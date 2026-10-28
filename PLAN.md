# my_gateway 开发计划

Rust + Nginx 动态安全网关。后端声明契约，Nginx 插件零信任拦截；路由/方法/参数不合约定即拒绝。

本文件是落地计划（不含代码）。目标是让每一步都有可验证的退出条件，而不是一次性设计完协议。

> **当前进度（2026-10-26）**：阶段 1、1.5 已完成并有可运行 demo（阶段 2–4 的核心提前落地）；
> 阶段 5 已落地 Header / Query / Body / 路径参数，以及分组 headers / unmatched / path_regex。后续开发必须遵循 [AGENTS.md](AGENTS.md) 与 §7.1：
> **先写 `gateway-core` 测试，再接入 nginx 并补集成测试。**
> 前后端共同遵守的契约约定见 [docs/PROTOCOL.md](docs/PROTOCOL.md)，
> 状态、验收数据与命令见 [docs/PROGRESS.md](docs/PROGRESS.md)，阶段 1 细节见
> [docs/PHASE1_PLAN.md](docs/PHASE1_PLAN.md)。本文件保留原始设计意图，实现与计划的差异在
> 相关小节用「实现备注」标出。

---

## 1. 总体架构

```
                 ┌──────────────────────────────┐
   client ─────► │  Nginx (access phase)        │
                 │  └─ ngx_gateway.so (Rust)    │──► 上游业务服务
                 │      ├─ 契约快照（只读）     │
                 │      └─ 共享内存 ngx_shm_zone│
                 └───────────┬──────────────────┘
                             │ GET /info (JSON)
                             ▼
                    业务后端 / 契约服务
```

关键设计约束（决定后面所有阶段）：

1. **核心逻辑与 Nginx 完全解耦**。纯 Rust crate `gateway-core` 只接收一个精简的请求上下文，返回一个决策枚举；不依赖任何 nginx 类型。这是阶段 2 能秒级测试的前提。
2. **access 阶段不做网络 I/O**。Nginx worker 是事件驱动的，在请求路径上同步拉契约会拖垮吞吐。契约由**唯一的 leader worker 在启动/接管时拉取**并写入共享内存，后续更新走 reload；请求路径只读内存快照。
3. **默认拒绝（fail-closed）**。未声明路由 404；方法不符 405；参数非法 400；契约从未加载成功 503。fail-open 只作为可配置的应急开关。
4. **契约只增字段、不破字段**。所有新增校验项设为可选，旧版后端上报的契约仍能正常解析。

### 1.1 代码结构（阶段 1 就定型，后续只填内容）

```
my_gateway/
├── Cargo.toml                 # workspace
├── AGENTS.md                  # Agent 开发约定（先 core 测试，再 nginx 集成测试）
├── PLAN.md                    # 本文（总计划）
├── docs/                      # 进度与阶段文档
│   ├── PROTOCOL.md            # 网关协议（契约字段/语义/演进，前后端唯一约定）
│   ├── PROGRESS.md            # 当前进度、验证记录、命令、待办
│   └── PHASE1_PLAN.md         # 阶段 1 计划与验收
├── crates/
│   ├── gateway-core/          # 纯逻辑，可单测，无 unsafe / 无 nginx
│   │   ├── contract.rs        # 契约 JSON 结构（v1：protocol/version/meta/unmatched/routes）
│   │   ├── gateway.rs         # matchit 路由编译 + 方法判定 + Decision/Outcome
│   │   ├── params.rs          # 三维度共用的规则编译/校验（required / required_if / 值 / unknown）
│   │   ├── query.rs           # Query 与 form 解码
│   │   ├── body.rs            # JSON / form 解析与媒体类型校验
│   │   ├── value.rs           # 通用值规则（类型/长度/范围/枚举/regex）
│   │   └── lib.rs             # 对外导出（Gateway / Decision / Outcome / RequestCtx / ContractError）
│   └── ngx-gateway/           # cdylib，只做适配
│       ├── src/lib.rs         # 指令解析、access handler、body 读取、写响应
│       ├── src/contract_manager.rs  # leader 选举 + 首次加载/失败重试
│       ├── src/contract_source.rs   # FileSource / HttpSource
│       └── src/contract_shm.rs      # ngx_shm_zone 快照分发
├── demo_server/               # 返回 /info、/info_v2 的 demo 后端 + 业务接口
│   └── src/main.rs
├── deploy/nginx.conf          # 运行时配置（构建脚本会拷到 nginx/conf/）
├── scripts/                   # build-nginx.sh / build-module.sh / dev.sh
├── tests/phase1.sh            # curl 端到端验收（524 项）
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
4. 配置 `nginx.conf`：`load_module` + http 级 `gateway_name` / 契约来源（`gateway_info_url` 或 `gateway_info_file`） /
   location 里 `enable_gateway_by <name>`。
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
> `crates/gateway-core/tests/protocol_v1.rs` 覆盖（当前 196 项，按协议章节组织）。

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
  "routes": [
    {
      "path": "/user/{id}",
      "methods": ["GET", "POST"]
    }
  ]
}
```

2. 校验：路由命中后检查 method 是否在 `methods` 中，不在则 405（响应头带 `Allow`）。
3. `contract_manager.rs`：**只有一个进程**在启动时拉 `/info`（`ureq`），成功后把原始契约写进共享内存；首次失败每 2s 重试，已成功的快照不再自动刷新（更新走 reload）。不引入 tokio。
4. 启动时由 leader worker 同步拉一次，拿不到就进入 503 状态，避免冷启动放行。
5. 指标先只做日志：`contract_version`、`refresh_ok/fail`、`age_seconds`。

> **实现备注（1.5 已落地，2026-10-27 改为统一契约管理器）**：
> - `ContractSource`（`FileSource` / `HttpSource`，etcd 预留）→ `Gateway::parse` → `ngx_shm_zone`；
>   worker 只读共享内存，不再各自起刷新线程；
> - **单点获取**：zone init 时（`postconfiguration` 注册）分配 `my_gateway_contracts`（1 MiB），
>   worker 在 `init_process` 通过共享内存的 `leader` pid CAS 选主，抢到的同步完成首次加载；
> - 为什么不是 master 线程：`ngx_init_cycle` 的 zone init 之后 nginx 才 `daemon()`，master 里的
>   线程会被 `fork` 掉，因此改为 leader worker（心跳 10s 超时 / `/proc/<pid>` 消失后接管）；
> - 快照按网关名存在 `RwLock<BTreeMap<..>>`，worker 侧在 `generation` 变化时才重新 `parse`；
> - reload 时 zone 被复用 → `config_epoch + 1`、`leader = 0`，新 worker 重新选主并全量加载；
> - 未拿到第一份契约前返回 `503 contract-not-ready`（fail-closed）；
> - 首次拉取失败每 2s 重试；已成功的快照一直保留，日志只在状态变化时写一条，避免刷屏；
> - 契约来源二选一：`gateway_info_url`（HTTP，leader 启动时拉一次，之后靠 reload 更新）或
>   `gateway_info_file`（本地文件，**配置加载/reload 时读一次**，与 `nginx.conf` 一致）。
>   文件模式用于"不允许后端决定规则"的场景——URL 模式下后端被入侵即可改自己的 `/info`，
>   等于改网关规则；文件模式把信任根移到文件权限，详见 `docs/PROTOCOL.md` 第 1.1 / 1.2 节；
> - leader 的后台循环直接写进程级 `error_log`（不再走请求线程队列）。

退出条件：

- 后端改 `/info` 后，通过reload让网关行为随之改变。
- 后端挂掉，网关继续用旧契约服务，日志有告警。
- 契约 JSON 损坏不会导致 worker 崩溃或放行。

---

## 5. 阶段 4：动态路由 + 协议 v1 定稿

**目的**：支持 `/user/{id}` 这类占位符路由，并与后端敲定第一版通用契约。此阶段结束即具备生产可用性。

任务：

1. 集成 `matchit`：把契约里的 `path` 逐条 `insert` 进 Router；`/user/{id}` 自动支持任意段，`{*rest}` 支持前缀。注意尾斜杠策略要显式定一个（建议：严格匹配，不做自动重定向）。
2. 命中后把路径参数以 `Vec<(name, value)>` 交给校验层，供阶段 5 使用。
3. 未命中一律 404，且**不暴露**"路由存在但方法错"以外的信息（405 仅在路径存在时返回）。
4. 契约 v1 定稿评审（与后端一起过一遍）：字段命名、type 表达、必填语义、错误码约定、版本与灰度（`version`）。
5. 契约加载失败的可观测性补齐：错误日志包含第几条路由、哪个字段。

退出条件：

- 静态路由 + 占位符路由混合的 fixture 全部通过。
- 未声明路由扫描（如 `/phpinfo.php`、`/.env`）稳定 404，日志可统计。
- 契约 v1 文档化，后端能照着产出。

---

## 6. 阶段 5：多维度参数过滤（分三步）

> 字段与语义的权威定义见 [docs/PROTOCOL.md](docs/PROTOCOL.md) 第 5 节（协议 v1 已定稿）；
> 本节只记任务拆解。插件是严格模式，草案字段出现即整份拒绝，避免"以为校验了其实没校验"。
>
> **实际进度（2026-10-26）**：Header、Query、Body 三个维度已落地：
> 存在性 / 值规则 / `required_if` / 重复检查 / `unknown`；Query 支持原始 query 解码，
> Body 支持 JSON 与 form（含 `content_types` 与媒体类型校验）；
> 路径参数与分组 `headers` / `unmatched` / `path_regex` 已落地。
> `responses` 的 contract_not_ready 时机、nginx 预检 413、契约来源其他加固与性能优化仍未开始。

原则：每步都是"加一个可选字段 + 加一组校验 + 加一组测试"，不重构。

### Step 5-1 Query 参数

契约新增：

```json
{
  "path": "/user/{id}",
  "methods": ["GET"],
  "query": {
    "params": {
      "page": { "type": "int", "required": false, "min": 1 },
      "kw":   { "type": "char", "len": 32 },
      "flag": { "type": "bool" },
      "sort": { "type": "enum", "values": ["asc", "desc"] }
    },
    "unknown": "ignore"
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
  "params": {
    "authorization": true,
    "x-api-version": { "type": "enum", "values": ["1", "2"], "required": true }
  }
}
```

只校验**显式声明**的 header，大小写不敏感；不声明的不看，避免影响正常业务头。
存在性、值规则与 `required_if` 均已实现：`x-api-version` 的 `enum` 会按 `values` 校验；
`filters` 与分组继承仍属后续工作。

每步独立退出条件：新增字段缺省时，旧契约行为完全不变（回归测试守这条）。

---

## 7. 里程碑与验收

| 阶段 | 产出 | 验收方式 | 状态 |
| --- | --- | --- | --- |
| 1 | 可加载的 .so + demo 后端 /info | curl 三态可见 | ✅ |
| 1.5 | 契约真正生效（具名多网关 + URL 启动拉取 / 文件按 reload 加载 + 404/405 + fail-closed） | `tests/phase1.sh` 524/524 | ✅ |
| 2 | `gateway-core` + 测试骨架 | `cargo test` 全绿，无需 nginx | ✅（196 项） |
| 3 | 方法校验 + 契约更新 | reload后生效 | ✅（reload 验证） |
| 4 | Radix 路由 + 契约 v1 | 扫描类请求 404，动态路由通过 | ✅（基础部分） |
| 5 | Query/Body/Header 校验 | 三类非法参数各一组用例 | 🟨 进行中（四维度参数与分组 headers/unmatched/path_regex 已接入；`filters` / 分组其他继承待做） |

统一验收口径（每阶段都跑）：`nginx -t` → `scripts/dev.sh up` → `tests/phase1.sh` → 检查 `nginx/logs/error.log` 无 panic。
详细命令与最近一次结果见 [docs/PROGRESS.md](docs/PROGRESS.md)。

### 7.1 后续开发方式（强制流程）

以本次 **Header 存在性** 切片为模板。每个能力切片（Header `filters`、Query 必填、Body 白名单等）都必须按下面顺序做，
**不允许先改 nginx 适配层、再回头补 core 测试**：

1. **定协议**：先改 [docs/PROTOCOL.md](docs/PROTOCOL.md)，写清字段、状态码、`reason` / `field` / `rule`、失败优先级，
   未实现的能力继续标 🧪。
2. **先写 `gateway-core` 测试**：在 `crates/gateway-core/tests/protocol_v1.rs` 增加 `s<章节>_*` 用例，
   覆盖正常、缺失/非法、优先级、大小写、重复、契约拒绝等边界；先跑出红色。
3. **实现 `gateway-core`**：只依赖公开 API，不引入 nginx 类型；直到 `cargo test -p gateway-core` 全绿，
   且 `cargo clippy -p gateway-core --all-targets`、`cargo fmt --all -- --check` 干净。
4. **再接入 nginx**：`crates/ngx-gateway/src/lib.rs` 收集所需请求数据并把 `Decision` 映射为状态码/响应头/日志；
   `demo_server/src/main.rs` 增加只使用该切片的契约路由；`deploy/nginx.conf` 增加专用 location。
5. **补 nginx 集成测试**：在 `tests/phase1.sh` 增加真实 nginx 断言，至少覆盖
   拒绝状态码、`X-Gateway-Name` / `X-Gateway-Reason` / `X-Gateway-Field`、请求未到后端、日志字段、
   未实现能力仍被拒绝。**只有 core 单测、没有集成测试的切片不算完成。**
   纯适配层开关（如 `gateway_diagnostics`）没有 core 可测，按
   `docs/PROTOCOL_TEST_PLAN.md` §7 的例外处理：登记原因，并用“开 / 关”两组实例各跑一遍。
6. **跑全量验收**：

   ```bash
   cargo fmt --all -- --check
   cargo clippy --offline -p gateway-core --all-targets
   cargo test --offline -p gateway-core
   ./scripts/build-module.sh && ./scripts/dev.sh restart
   ./tests/phase1.sh
   ```

7. **同步文档**：更新 `docs/PROGRESS.md` 的验证结果与历史、`docs/PROTOCOL_TEST_PLAN.md` 的用例数/集成分工，
   以及本文件与 `docs/PHASE1_PLAN.md` 中过期的断言数、路由数、日志样例。

硬性约束：

- **只开对应切片**：接入 Header 存在性就只启用存在性，接入 Header 值规则才启用值规则；Query / Body / `filters` 不得顺带打开。
- **未实现 = 拒绝加载**：契约里出现尚未实现的字段时整份拒绝，不能静默忽略。
- **fail-closed**：缺失/非法一律拒绝并给出稳定 reason，不能放行到后端。
- **两层都要测**：`gateway-core` 测纯判定；`tests/phase1.sh` 测真实 nginx 的响应头、日志和“未到后端”。
- 新增/删除 demo 契约路由时，同步更新 `tests/phase1.sh` 里的 `routes=N` 与日志断言。

---

## 8. 明确不做（避免过度设计）

- 不做管理后台、不做规则可视化编辑。
- 不做 JWT/鉴权体系，只做契约一致性校验（Token 检查仅作为 header 校验的一个用例）。
- ~~不做跨 worker 共享内存字典，先每 worker 各自拉取~~ → 已改为 leader worker 单点获取 + 共享内存分发（见 §4 实现备注）。
- 不做热更新契约以外的配置热加载。
- 不做 HTTP/3、gRPC、WAF 语义（SQL 注入特征匹配等）。

---

## 9. 下一步（当前状态）

阶段 1–4 的目标已经具备可用 demo（Rust 模块 + 本地 nginx + demo 后端 + 契约拉取 + matchit 路由 +
404/405/`Allow` + 冷启动 fail-closed + 具名多网关 + 契约文件来源 + 诊断头上线开关），
`phase1.sh` 524 项、`httpbin.sh` 170 项、`tls-local.sh` 24 项全绿。接下来按顺序：

1. **阶段 5**：契约新增 `query` / `body` / `headers`，`RequestCtx` 带上对应数据，`Gateway::check`
   返回统一的 `400` + 具体原因；nginx 层负责有上限地读取请求体（只对已声明路由）。
   四维度参数与分组基础能力已落地；下一步补 `contract_not_ready` / 预检 413 与契约来源加固。
2. 契约来源加固：URL 凭据/fragment/长度校验、内容摘要/签名钉死、只允许回环/白名单地址、拉取失败告警。
3. 去掉每请求不必要的字符串分配；评估多 worker 下的性能与接管窗口。

每一步的退出条件沿用第 7 节：先加可选字段，缺省时旧契约行为完全不变（回归测试守这条）。
