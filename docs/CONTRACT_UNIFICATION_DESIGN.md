# 统一契约来源设计：ContractManager + 共享内存

## 1. 背景（改造前）

`my_gateway` 最初支持两种契约来源，实现方式并不统一：

- `gateway_info_file`：在 `postconfiguration` 阶段由 **master** 进程读取并解析，随后通过 `fork` 继承到各个 worker；
- `gateway_info_url`：在每个 worker 的 `init_process` 中为每个被引用的网关都起一个刷新线程，按 TTL 各自独立地去 `GET /info`。

未来还会增加 `etcd` watch 这类**推送型**来源。如果继续沿用现在“分散到 worker”的思路，会导致：

- **HTTP 来源**：`worker_processes = N` 时就会有 N 个并发请求去拉取同一份契约
- **etcd 来源**：每个 worker 都去建立一条 watch 长连接，etcd 会承受 N 倍的 watch 数量
- **逻辑割裂**：文件、HTTP、etcd 各用一套加载、刷新、日志逻辑，后续维护和扩展都极为混乱

## 2. 目标

将三种来源（本地文件、HTTP `/info`、未来的 etcd）**统一抽象**成同一套模型：

1. **全局单点**：只允许 **一个“主”** 去负责“获取”契约（读取文件、发起 HTTP 请求、建立 etcd watch）
2. **统一出口**：该“主”先 `Gateway::parse` 校验，再把**原始契约字节**写入 **共享内存（ngx_shm_zone）**；
   worker 侧按 `generation` 变化各自编译（网络只拉一次，但不跨进程共享 Rust 结构体）
3. **worker 只读**：所有 worker 都只从共享内存读取快照，不再各自起刷新线程、也不再各自去拉取或 watch
4. **不做周期轮询**：URL 契约只在启动/接管时拉一次，后续更新由 `nginx -s reload` 触发；
   首次失败按固定间隔重试，避免永久 503
5. **为 etcd 做预留**：推送型（watch）和拉取型（HTTP）都能无缝接入同一套架构

## 3. 整体架构

```text
┌─────────────────────────────────────────────────────────────────────────┐
│ leader worker（共享内存里的 leader pid；其余 worker 只读）                     │
│                                                                           │
│  ┌─────────────────────────────────────────────────────────────────────┐  │
│  │                        ContractManager                               │  │
│  │                                                                       │  │
│  │  ┌─────────────┐   ┌─────────────┐   ┌─────────────┐                │  │
│  │  │ FileSource   │   │ HttpSource   │   │ EtcdSource   │ （未来）        │  │
│  │  │ 读本地文件    │   │ 启动时拉取    │   │ watch 长连接 │                 │  │
│  │  └─────────────┘   └─────────────┘   └─────────────┘                │  │
│  │           │                 │                 │                      │  │
│  │           └───────────┬─────┴─────────────────┘                      │  │
│  │                       │ 原始契约字节                                   │  │
│  │                       ▼                                               │  │
│  │                gateway-core::Gateway::parse()                         │  │
│  │                       │ 编译校验                                       │  │
│  │                       ▼                                               │  │
│  │                写入 ngx_shm_zone（seqlock）                            │  │
│  │                更新 generation / heartbeat / leader / config_epoch     │  │
│  └─────────────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────┘
                                │  mmap（只读）
                        ┌───────┼───────┐
                      worker1 worker2 worker3
                    （仅从共享内存读快照并做判定）
```

## 4. 来源抽象（ContractSource）

实际实现（`crates/ngx-gateway/src/contract_source.rs`）：

```rust
pub struct RawContract {
    pub bytes: Vec<u8>,
}

pub trait ContractSource: Send {
    /// 取一次原始契约字节；校验交给 `gateway_core::Gateway::parse`。
    fn fetch(&mut self) -> anyhow::Result<RawContract>;
}
```

### 各 Source 职责

| Source | 行为 | 说明 |
| --- | --- | --- |
| **FileSource** | “配置加载时读取” | 配置加载阶段先由 `postconfiguration` 读取并校验（失败拒绝配置）；leader 启动时再读一次写入共享内存 |
| **HttpSource** | “启动时拉取 + reload 更新” | leader 启动时拉一次；`max_redirects(0)`，只接受 2xx，响应体 ≤1 MiB；首次失败每 2s 重试 |
| **EtcdSource**（未实现） | “长连接推送” | 启动时 `GET` 全量 + `revision`，随后 `watch`；复用同一套发布路径 |

## 5. ContractManager 设计

> **实现状态（2026-10-27）**：已落地，代码见 `crates/ngx-gateway/src/contract_manager.rs`
> 与 `contract_shm.rs`。运行位置从“master 后台线程”改为 **leader worker**，原因见 5.1。

### 5.1 为什么不是 master 线程

最初方案是“master 进程的后台线程”。实现时发现不可行：

- `ngx_init_cycle()` 里 zone 的 `init` 回调（能拿到共享内存地址的最早时机）执行完之后，
  nginx 才调用 `daemon()`；`daemon()` 内部 `fork` 后只有调用线程存活，master 里起的线程会被丢掉；
- 放到 `postconfiguration` 更早，但那时共享内存还没有分配。

因此改为 **leader worker**：仍然只有一个进程去“获取”，对上层语义不变。

### 5.2 leader 选举

共享内存头部有一个 `leader` 字段（pid）：

1. 每个 worker 在 `init_process` 调 `try_claim_leader()`，CAS `0 → 自己的 pid`；
2. 抢到的 worker **同步**完成首次加载并 `publish`，然后进入后台循环（每秒心跳 + 失败重试）；
3. 其余 worker 直接对外服务，只从共享内存读快照；
4. 请求路径上顺带检查：leader 心跳超过 10s 未更新且 `/proc/<pid>` 已不存在 → 用 CAS 把**失效的旧 pid** 换成自己，
   接管走后台线程，避免第一个请求等一次网络拉取；
5. reload 时 zone 被 nginx 复用 → `attach()` 把 `config_epoch + 1`、`leader = 0`；
   旧周期 leader 在循环里看到 epoch 变化即退出，新 worker 重新选主并全量加载。

### 5.3 职责

1. 获取：按来源读取文件 / 发起 HTTP 请求（未来建立 etcd watch）
2. 调度：启动/接管时全量加载一次；**不做周期轮询**。首次失败的来源每 2s 重试，成功过的来源不再自动拉取
3. 编译：调用 `gateway_core::Gateway::parse` 校验，失败即视为本次加载失败
4. 写入共享内存：把通过的**原始契约文本**写入 `ngx_shm_zone`，更新 `generation`
5. 日志：`[name] contract loaded from file|url protocol=… routes=… unmatched=…`；
   失败为 `[name] contract refresh failed: … (no snapshot yet, requests fail-closed)`，
   同一条消息只在状态变化时输出
6. 容灾：首次失败 → 不发布快照（该网关请求 503）；已成功的快照一直保留

### 5.4 更新触发机制

**没有 `gateway_url_ttl` / TTL 轮询。** URL 契约的更新通过 `nginx -s reload` 触发：

- 当前可用方式：`nginx -s reload`（新配置周期重新选主并全量加载）；
- 首次加载失败：后台每 2s 重试，成功即发布，避免后端晚启动导致永久 503；
- **提供 `POST /_gateway_refresh`**：用于“配置不变、契约单独更新”的场景；
  任意 worker 只登记请求，由 leader 按名单刷新，设计见 `REFRESH_ENDPOINT_DESIGN.md`（待实现）。
  配置变更仍然走 `nginx -s reload`。

## 6. 共享内存设计

只用一个 zone：`my_gateway_contracts`（1 MiB，`ngx_shared_memory_add` 在 `postconfiguration` 注册）。
不再单独拆控制区——leader 心跳和 epoch 都放在同一个头部里。

### 6.1 头部（48 字节，全部小端）

| 偏移 | 字段 | 说明 |
| --- | --- | --- |
| 0 | `magic: u32` | `"GWCT"`（`0x54435747`），用来区分首次分配与 reload 复用 |
| 4 | `payload_len: u32` | 有效 payload 字节数 |
| 8 | `generation: u64` | seqlock：奇数 = 写入中，偶数 = 可读，`0` = 未写过 |
| 16 | `heartbeat: u64` | leader 约每秒刷新一次（unix 秒，循环 1s + 秒级节流），用于失效接管 |
| 24 | `leader: u64` | leader 的 pid，`0` = 无 leader |
| 32 | `config_epoch: u64` | 每个配置周期 +1，旧 leader 据此让位 |
| 40 | `reserved: u64` | 预留（etcd revision 等） |

### 6.2 payload

若干条记录顺序拼接：

```text
name_len: u16 | name: bytes | body_len: u32 | body: bytes        (little endian)
```

`body` 是原始契约文本，worker 侧在 generation 变化时才 `Gateway::parse` —— 网络只拉一次，
但每个 worker 各自编译，避免共享内存里放 Rust 结构体的生命周期问题。

### 6.3 并发

写方（leader）在写 payload 前把 `generation` 置为奇数，写完置为下一个偶数；
读方前后各读一次 `generation`，不一致或为奇数就放弃本次读取（下一个请求重试）。
内容与上次完全相同时不发布，避免 worker 无谓地重新解析。

## 7. Worker 端改动

1. **初始化**：`init_process` 里抢 leader（见 5.2），不再起“每个网关一个刷新线程”
2. **判定时读取**：请求路径上先比较 `shm.generation` 与本地 `cached_generation`，
   相同就直接返回（不复制 payload）；不一致时才反序列化更新 `SNAPSHOTS`，
   顺带做一次 leader 失效检查

## 8. 待办

1. **EtcdSource**：实现 watch 后复用同一套 `publish` 路径。
2. **更快接管**：评估 `NGX_PROCESS_HELPER` 专用进程替代 leader worker，缩短 leader 退出后的接管窗口。
3. **运行期独立刷新**：实现 `POST /_gateway_refresh`，按 `REFRESH_ENDPOINT_DESIGN.md`，
   支持 `mode=auto|check|force`；配置变更仍然走 reload。

## 9. 迁移计划

| 步骤 | 内容 | 状态 |
|---|---|---|
| **1. 设计定稿** | 抽象 `ContractSource`、ContractManager、shm 布局 | ✅ 完成（运行位置由 master 线程改为 leader worker） |
| **2. 搭建骨架** | `contract_shm.rs` + `ngx_shm_zone` 注册 | ✅ 完成 |
| **3. 迁移文件来源** | 文件读取走 `FileSource`，配置加载时仍做强校验 | ✅ 完成 |
| **4. 迁移 HTTP 来源** | worker 刷新线程 → leader 单点拉取 + 共享内存分发 | ✅ 完成（`tests/phase1.sh` 第 12 节验证 3xx 不跟随） |
| **5. 更新触发** | 配置变更走 reload；运行期独立刷新 `/_gateway_refresh` | ⬜ 待实现（设计见 `REFRESH_ENDPOINT_DESIGN.md`） |
| **6. 验证回归** | 196 单测 + 524 项集成断言 | ✅ 全绿 |
| **7. 预留 etcd** | 新增 `EtcdSource` 骨架 | ⬜ 未开始（接口已就位） |

## 10. 总结

核心思想：**“集中获取，统一分发”**。worker 永远不知道来源是什么，只从共享内存读取。统一后不仅能省去大量无意义的请求和 watch，更重要的是能把架构收束到一个清晰的模型，未来新增来源只需实现 `ContractSource` 接口即可。
