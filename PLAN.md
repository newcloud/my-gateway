# my_gateway 开发计划

Rust + Nginx 动态安全网关。后端声明契约，Nginx 插件零信任拦截；路由/方法/参数不合约定即拒绝。

本文件是落地计划（不含代码）。目标是让每一步都有可验证的退出条件，而不是一次性设计完协议。

> **当前进度（2026-10-26）**：阶段 1、1.5 已完成并有可运行 demo（阶段 2–4 的核心提前落地）；
> 阶段 5 未开始。前后端共同遵守的契约约定见 [docs/PROTOCOL.md](docs/PROTOCOL.md)，
> 状态、验收数据与命令见 [docs/PROGRESS.md](docs/PROGRESS.md)，阶段 1 细节见
> [docs/PHASE1_PLAN.md](docs/PHASE1_PLAN.md)。本文件保留原始设计意图，实现与计划的差异在
> 相关小节用「实现备注」标出。

---

## 1. 总体架构

```
                 ┌──────────────────────────────┐
   client ─────► │  Nginx (access phase)        │
                 │  └─ ngx_gateway.so (Rust)    │──► 上游业务服务
                 │      ├─ 契约缓存 (ArcSwap)   │
                 │      └─ 后台刷新线程 (TTL)   │
                 └───────────┬──────────────────┘
                             │ GET /info (JSON)
                             ▼
                    业务后端 / 契约服务
```

关键设计约束（决定后面所有阶段）：

1. **核心逻辑与 Nginx 完全解耦**。纯 Rust crate `gateway-core` 只接收一个精简的请求上下文，返回一个决策枚举；不依赖任何 nginx 类型。这是阶段 2 能秒级测试的前提。
2. **access 阶段不做网络 I/O**。Nginx worker 是事件驱动的，在请求路径上同步拉契约会拖垮吞吐。契约由**独立线程按 TTL 刷新**，请求路径只读内存快照。
3. **默认拒绝（fail-closed）**。未声明路由 404；方法不符 405；参数非法 400；契约从未加载成功 503。fail-open 只作为可配置的应急开关。
4. **契约只增字段、不破字段**。所有新增校验项设为可选，旧版后端上报的契约仍能正常解析。

### 1.1 代码结构（阶段 1 就定型，后续只填内容）

```
my_gateway/
├── Cargo.toml                 # workspace
├── PLAN.md                    # 本文（总计划）
├── docs/                      # 进度与阶段文档
│   ├── PROTOCOL.md            # 网关协议（契约字段/语义/演进，前后端唯一约定）
│   ├── PROGRESS.md            # 当前进度、验证记录、命令、待办
│   └── PHASE1_PLAN.md         # 阶段 1 计划与验收
├── crates/
│   ├── gateway-core/          # 纯逻辑，可单测，无 unsafe / 无 nginx
│   │   ├── contract.rs        # 契约 JSON 结构（v1：protocol/version/ttl/meta/unmatched/routes）
│   │   ├── gateway.rs         # matchit 路由编译 + 方法判定 + Decision/Outcome
│   │   └── lib.rs             # 对外导出（Gateway / Decision / Outcome / RequestCtx / ContractError）
│   └── ngx-gateway/           # cdylib，只做适配
│       └── src/lib.rs         # 指令解析、刷新线程、access handler、写响应
├── demo_server/               # 返回 /info、/info_v2 的 demo 后端 + 业务接口
│   └── src/main.rs
├── deploy/nginx.conf          # 运行时配置（构建脚本会拷到 nginx/conf/）
├── scripts/                   # build-nginx.sh / build-module.sh / dev.sh
├── tests/phase1.sh            # curl 端到端验收（78 项）
└── nginx/                     # 本地 nginx 前缀（构建产物）
```

> 实际实现比原计划更紧凑：`router.rs` / `decision.rs` / `store.rs` 暂时合并进 `gateway.rs`，
> `validate.rs` 留给阶段 5；`mock-server` 与业务后端合并为 `demo_server`。
> 每个网关一份契约快照，放在 `RwLock<BTreeMap<String, Arc<Gateway>>>` 里（读多写少，够用；
> 压测有争用再换 `ArcSwap`）。配置用 http 级具名网关 + location `enable_gateway_by`，见
> [docs/PROGRESS.md](docs/PROGRESS.md) 第 3 节。

---

## 2. 阶段 1：最小可行性 Demo

**目的**：先证明"Rust 能编出能被这个 nginx 加载的 .so，并在 access 阶段拦住请求"。这一步只验证链路，不做任何判断逻辑。

任务：

1. 记录本机 nginx 版本、编译参数（`nginx -V`），确认 `--with-compat` 与 ngx-rust 支持范围。**这是全项目最大的不确定项，必须第一步做掉。**
2. 用 ngx-rust 写出模块骨架：注册 access handler，打印 method + uri 到 error_log，固定返回 418 或原样放行（用 nginx 指令切换）。
3. `mock-server` 提供一个写死的 `GET /info`，返回 2 条静态路由的 JSON。
4. 配置 `nginx.conf`：`load_module` + http 级 `gateway_name` / `gateway_info_url <name> <url>` /
   `gateway_ttl <name> <n>`，location 里 `enable_gateway_by <name>`。
5. 用 curl 验证：放行、拦截、error_log 三种可见结果。

退出条件：

- `.so` 编译通过，nginx `-t` 通过，worker 启动无报错。
- `curl` 能看到自定义拦截响应，日志里能看到请求行。
- 形成一份"编译 + 部署 + 重启"的最小命令记录，后续每阶段复用。

风险与应对：

| 风险 | 应对 |
| --- | --- |
| ngx-rust 与 nginx 版本 ABI 不匹配 | 阶段 1 就试；不行则降级方案：OpenResty + Rust 旁路进程，或 Nginx `auth_request` 到 Rust 小服务 |
| 头文件/工具链缺失 | 用 nginx 源码目录编译，记录 `--add-dynamic-module` 路径 |
| 调试靠 error_log 太低效 | 阶段 1 就把日志级别指令做出来（`gateway_log_level`） |

---

## 3. 阶段 2：本地 TDD（脱离 Nginx）

**目的**：把 80% 的后续开发从"改代码 → 编 so → 重启 nginx → curl"变成 `cargo test`。

任务：

1. 定义上下文结构，只保留校验需要的东西：

```rust
pub struct RequestCtx<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub query: &'a str,
    pub headers: Vec<(&'a str, &'a str)>,
    pub body: Option<&'a [u8]>,
}
```

2. 定义决策枚举，状态码在 core 里定死，适配层不再判断：

```rust
pub enum Decision {
    Allow,
    Reject { status: u16, reason: &'static str, detail: String },
}
```

3. 用一个 `Gateway` 门面串起"解析契约 → 编译路由 → 校验"，签名稳定为
   `fn check(&self, ctx: &RequestCtx) -> Decision`，内部实现随便换。
4. 建测试骨架：表驱动用例 + 契约 JSON fixture 目录 `tests/fixtures/*.json`，一个 fixture 一个场景文件。
5. 给 core 写一个 `cargo run --example check -- --contract x.json --method GET --path /x` 的 CLI，方便手工验证和贴 bug 复现。

> **实现备注（1.5 已落地）**：`Decision` 目前是 `Allow / RouteNotDeclared(404) / MethodNotAllowed(405)`
> 三个语义化变体 + `Outcome { decision, allow }`（405 回写 `Allow`）；等阶段 5 加参数校验时再引入
> `Reject { status, reason, detail }` 统一承载 400/413。`examples/check` CLI 尚未做，当前用
> `crates/gateway-core/tests/protocol_v1.rs` 覆盖（42 项，按协议章节组织）。

退出条件：

- `cargo test` 覆盖：路由命中/未命中、方法不符、契约解析失败。
- 同样的输入在 CLI 与单测中结果一致。
- 改逻辑不需要接触 nginx。

---

## 4. 阶段 3：方法校验 + 契约动态拉取

**目的**：第一次真正"动态"起来——规则来自后端，不是代码。

任务：

1. `contract.rs` 第一版结构（字段后续只增不改）：

```json
{
  "protocol": 1,
  "version": "2026-10-26T12:00:00Z",
  "ttl": 60,
  "routes": [
    {
      "path": "/user/{id}",
      "methods": ["GET", "POST"]
    }
  ]
}
```

2. 校验：路由命中后检查 method 是否在 `methods` 中，不在则 405（响应头带 `Allow`）。
3. `refresh.rs`：每个 worker 起一个线程，每 `ttl` 秒 `ureq` 拉一次 `/info`；**成功才替换**快照，失败保留旧快照并计数；用 `ArcSwap` 做无锁读。不引入 tokio。
4. 启动时同步拉一次（超时 2s），拿不到就进入 503 状态，避免冷启动放行。
5. 指标先只做日志：`contract_version`、`refresh_ok/fail`、`age_seconds`。

> **实现备注（1.5 已落地）**：
> - 刷新线程在 `init_process` 启动（每个 worker 一份），access 阶段只读快照；
> - 快照按网关名存在 `RwLock<BTreeMap<..>>` 而非 `ArcSwap`，`preconfiguration` 清空，保证 reload 后新 worker 重新拉取；
> - 每个被引用的网关一个刷新线程（各自 TTL），未引用的定义不拉取；
> - 启动不做同步拉取：未拿到第一份契约前返回 `503 contract-not-ready`（fail-closed 效果一致，且不阻塞 worker 启动）；
> - 拉取失败保留 last-known-good 快照，日志只在状态变化时写一条，避免 TTL 小时刷屏；
> - 后台线程不直接调 nginx logger，消息排队后由请求线程写 `error_log`。

退出条件：

- 后端改 `/info` 后，≤ TTL 网关行为随之改变，无需 reload nginx。
- 后端挂掉，网关继续用旧契约服务，日志有告警。
- 契约 JSON 损坏不会导致 worker 崩溃或放行。

---

## 5. 阶段 4：动态路由 + 协议 v1 定稿

**目的**：支持 `/user/{id}` 这类占位符路由，并与后端敲定第一版通用契约。此阶段结束即具备生产可用性。

任务：

1. 集成 `matchit`：把契约里的 `path` 逐条 `insert` 进 Router；`/user/{id}` 自动支持任意段，`{*rest}` 支持前缀。注意尾斜杠策略要显式定一个（建议：严格匹配，不做自动重定向）。
2. 命中后把路径参数以 `Vec<(name, value)>` 交给校验层，供阶段 5 使用。
3. 未命中一律 404，且**不暴露**"路由存在但方法错"以外的信息（405 仅在路径存在时返回）。
4. 契约 v1 定稿评审（与后端一起过一遍）：字段命名、type 表达、必填语义、错误码约定、版本与灰度（`version` + `ttl`）。
5. 契约加载失败的可观测性补齐：错误日志包含第几条路由、哪个字段。

退出条件：

- 静态路由 + 占位符路由混合的 fixture 全部通过。
- 未声明路由扫描（如 `/phpinfo.php`、`/.env`）稳定 404，日志可统计。
- 契约 v1 文档化，后端能照着产出。

---

## 6. 阶段 5：多维度参数过滤（分三步）

> 字段与语义的权威定义见 [docs/PROTOCOL.md](docs/PROTOCOL.md) 第 5 节（草案，协议仍为 v1）；
> 本节只记任务拆解。插件是严格模式，草案字段出现即整份拒绝，避免"以为校验了其实没校验"。

原则：每步都是"加一个可选字段 + 加一组校验 + 加一组测试"，不重构。

### Step 5-1 Query 参数

契约新增：

```json
{
  "path": "/user/{id}",
  "methods": ["GET"],
  "query": {
    "page":  { "type": "int", "required": false, "min": 1 },
    "kw":    { "type": "char", "len": 32 },
    "flag":  { "type": "bool" },
    "sort":  { "type": "enum", "values": ["asc", "desc"] }
  }
}
```

规则：缺失必填 → 400；类型/长度/枚举不符 → 400；**未声明的 query 参数**按配置处理（默认忽略，可开 strict 拒绝）。`type` 先只做 `int / char(len) / bool / enum`，够用。

### Step 5-2 Body 参数（防 Mass Assignment）

- 支持 `application/x-www-form-urlencoded` 与 `application/json` 两种。
- 契约用 `body` 字段声明白名单，语义同 query。
- 严格模式：出现未声明字段直接 400，这就是 Mass Assignment 的拦截点。
- Body 大小上限与解析上限由 nginx 配置控制，core 只处理已读到的字节；超过上限返回 413，不做部分解析。

### Step 5-3 Header 校验

```json
"headers": {
  "x-api-version": { "required": true, "enum": ["1", "2"] }
}
```

只校验**显式声明**的 header，大小写不敏感；不声明的不看，避免影响正常业务头。

每步独立退出条件：新增字段缺省时，旧契约行为完全不变（回归测试守这条）。

---

## 7. 里程碑与验收

| 阶段 | 产出 | 验收方式 | 状态 |
| --- | --- | --- | --- |
| 1 | 可加载的 .so + demo 后端 /info | curl 三态可见 | ✅ |
| 1.5 | 契约真正生效（具名多网关 + TTL 拉取 + 404/405 + fail-closed） | `tests/phase1.sh` 78/78 | ✅ |
| 2 | `gateway-core` + 测试骨架 | `cargo test` 全绿，无需 nginx | ✅（42 项） |
| 3 | 方法校验 + 动态刷新 | 改后端契约即时生效，无需 reload | ✅（TTL 3s 验证） |
| 4 | Radix 路由 + 契约 v1 | 扫描类请求 404，动态路由通过 | ✅（基础部分） |
| 5 | Query/Body/Header 校验 | 三类非法参数各一组用例 | ⬜ |

统一验收口径（每阶段都跑）：`nginx -t` → `scripts/dev.sh up` → `tests/phase1.sh` → 检查 `nginx/logs/error.log` 无 panic。
详细命令与最近一次结果见 [docs/PROGRESS.md](docs/PROGRESS.md)。

---

## 8. 明确不做（避免过度设计）

- 不做管理后台、不做规则可视化编辑。
- 不做 JWT/鉴权体系，只做契约一致性校验（Token 检查仅作为 header 校验的一个用例）。
- 不做跨 worker 共享内存字典，先每 worker 各自拉取（后端接口要扛得住 N × worker 的 QPS，可加随机抖动）。
- 不做热更新契约以外的配置热加载。
- 不做 HTTP/3、gRPC、WAF 语义（SQL 注入特征匹配等）。

---

## 9. 下一步（当前状态）

阶段 1–4 的目标已经具备可用 demo（Rust 模块 + 本地 nginx + demo 后端 + 契约拉取 + matchit 路由 +
404/405/`Allow` + 冷启动 fail-closed + 具名多网关），验收 78 项全绿。接下来按顺序：

1. **阶段 5**：契约新增 `query` / `body` / `headers`，`RequestCtx` 带上对应数据，`Gateway::check`
   返回统一的 `400` + 具体原因；nginx 层负责有上限地读取请求体（只对已声明路由）。
2. 把内置探针规则（`.php` / `.env` / `.git*` / `/wp-*`）改成数据驱动，作为契约的兜底层。
3. 去掉每请求 `to_ascii_lowercase` 分配；评估多 worker 下的契约拉取策略。
4. 契约来源加固：可选签名校验、只允许回环/内网地址、拉取失败告警。

每一步的退出条件沿用第 7 节：先加可选字段，缺省时旧契约行为完全不变（回归测试守这条）。
