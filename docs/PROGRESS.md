# 开发进度与文档目录

最后更新：2026-10-27 · 当前：**阶段 1 + 阶段 1.5 已完成**，阶段 5 已落地 Header / Query / Body / 路径参数、分组、filters、body.max_bytes、JSON 结构扫描与统一契约管理器（leader worker + 共享内存分发）（剩余 contract_not_ready / 预检 413 时机与工程加固）

## 1. 文档目录

| 文档 | 内容 |
| --- | --- |
| [../AGENTS.md](../AGENTS.md) | **开发约定**：先写 `gateway-core` 测试，再接入 nginx 并补集成测试 |
| [../README.md](../README.md) | **项目介绍**：功能、快速开始、demo 示例、契约生成与 AI Agent 提示词 |
| [../PLAN.md](../PLAN.md) | 总计划：架构约束、5 个阶段、里程碑、明确不做的范围 |
| [PROTOCOL.md](PROTOCOL.md) | **网关协议 v1（契约接口的唯一约定）**：字段、未声明路由策略、匹配/方法语义、拒绝响应、演进规则、草案 |
| [PHASE1_PLAN.md](PHASE1_PLAN.md) | 阶段 1 详细计划：环境结论、交付物、任务表、验收命令、风险 |
| [PROTOCOL_TEST_PLAN.md](PROTOCOL_TEST_PLAN.md) | **协议测试清单**：纯 `gateway-core` 196 项用例、写法约束、与 `phase1.sh` 的分工 |
| [PROGRESS.md](PROGRESS.md) | 本文：当前进度、验证记录、常用命令、待办 |
| [ISSUES.md](ISSUES.md) | **问题清单**：契约体积、多级数组/嵌套 JSON、etcd 等未实现项（只记问题，不写方案） |
| [AI_PROPOSALS.md](AI_PROPOSALS.md) | **AI 提议清单**：路由分级、动态路由、嵌套校验等建议（P-04 已确认） |
| [REFRESH_ENDPOINT_DESIGN.md](REFRESH_ENDPOINT_DESIGN.md) | **主动刷新接口设计**：`/_gateway_refresh`、`mode=auto\|check\|force`、leader 刷新流程（待实现） |
| [CLIENT_SOLUTION.md](CLIENT_SOLUTION.md) | **客户方案（政企版）**：现状问题、使用场景、能力清单、实施路径、POC 验收、FAQ（可直接对外） |
| [PRODUCT.md](PRODUCT.md) | **产品化方向（内部）**：定位、竞品、护城河、差距清单与阶段路线图 |
| [CONTRACT_UNIFICATION_DESIGN.md](CONTRACT_UNIFICATION_DESIGN.md) | **统一契约来源设计**：ContractSource 抽象、leader worker 选举、共享内存布局（已实现） |

代码索引：

| 位置 | 作用 |
| --- | --- |
| `crates/gateway-core/src/gateway.rs` | 纯逻辑：契约解析、分组展开、matchit 路由树、404/405/400 判定、四维度判定顺序 |
| `crates/gateway-core/src/params.rs` | 三维度共用的参数规则编译/校验（required / required_if / 重复 / 值 / unknown） |
| `crates/gateway-core/src/query.rs` | Query 与 form 共用的百分号解码、重复与数量上限 |
| `crates/gateway-core/src/body.rs` | JSON / form body 解析、媒体类型 / charset / Content-Encoding |
| `crates/gateway-core/src/value.rs` | 通用值规则：`char` / `int` / `bool` / `enum`、长度/范围、`regex` 编译与校验 |
| `crates/gateway-core/src/contract.rs` | 契约 JSON 结构（v1：`protocol` / `version` / `meta` / `unmatched` / `routes[].path/methods/headers/query/body`） |
| `crates/gateway-core/tests/protocol_v1.rs` | 196 项协议单测（§2 顶层 / §2.1 未声明策略 / §3.1–3.5 路径与分组 / §4 状态码 / §5.1–5.4 参数校验） |
| `crates/ngx-gateway/src/lib.rs` | nginx 适配层：具名网关注册、契约来源、access 判定、按需读取 body（同步/异步）与响应头 |
| `demo_server/src/main.rs` | demo 后端：登录/用户/商品/订单 + 两份固定契约 `GET /info`、`GET /info_v2` |
| `deploy/nginx.conf` | 运行时 nginx 配置（源文件，脚本会拷到 `nginx/conf/`） |
| `deploy/httpbin/` | 独立演示：一个 server 代理到 `https://httpbin.org`，契约来自本地 `contract.json` |
| `scripts/build-nginx.sh` | 下载并编译本地 nginx 1.28.0 到 `nginx/` |
| `scripts/build-module.sh` | 编译 `.so` → `nginx/modules/libmy_gateway.so` |
| `scripts/dev.sh` | `up / down / restart / status / logs`（含孤儿进程清理） |
| `tests/phase1.sh` | curl 验收，**524 项断言**（含独立实例的契约文件模式、重定向拉取、静态校验、诊断头关闭形态与 Header/Query/Body/路径参数/分组集成） |
| `tests/httpbin.sh` | httpbin 独立实例验收（**170 项断言**，按协议验证本地契约 + HTTPS 契约来源 + 真实公网代理） |
| `tests/tls-local.sh` | 本地自签 HTTPS 契约源验收（**24 项断言**，离线；覆盖 `allow-localhost-ca`） |

## 2. 阶段状态

| 阶段 | 内容 | 状态 |
| --- | --- | --- |
| 1 | 最小可行性 Demo（Rust 模块 + 本地 nginx + demo 后端 + curl） | ✅ 完成 |
| 1.5 | 配置真正生效：具名网关 + 契约拉取 + 404/405 | ✅ 完成（提前吃掉阶段 2–4 的核心） |
| 2 | 纯本地 TDD（`gateway-core`，脱离 nginx） | ✅ 完成（196 项协议单测） |
| 3 | 方法校验 + 契约加载（leader 单点拉取 + 只读快照） | ✅ 完成 |
| 4 | 动态路由（matchit）+ 契约 v1 | ✅ 基础完成（严格匹配、`Allow` 回写） |
| 5 | Query / Body / Header 参数校验 | 🟨 进行中（四维度参数、分组继承、`filters`、`path_regex`/`priority`/`responses`、`body.max_bytes`、JSON 结构扫描已接入；剩余 `contract_not_ready` / 预检 413 时机与性能项） |

location 通过 `enable_gateway_by <网关名>` 选择网关，可写多条（与语义）：

```
写日志 → 绑定的每个网关依次判定 → 放行到后端
                               ├─ 该网关契约未加载成功 → 503 contract-not-ready
                               ├─ 路由未声明          → 404 route-not-declared
                               ├─ 方法不符            → 405 + Allow
                               └─ 全部网关放行        → 放行
```

拒绝时响应头 `X-Gateway-Name` 标明是哪个网关拦的。这些 `X-Gateway-*` 是**诊断头**，
只在 `gateway_diagnostics on` 时返回（默认 `off`）；生产形态下所有拒绝响应仍带 `X-Request-Id`，
与 `error_log` 里的 `rid=` 一一对应。

不写 `enable_gateway_by` 的 location 完全不参与判定，`/passthrough/` 就是这种对照。

## 3. 配置形态

`deploy/nginx.conf`：

```nginx
http {
    # 诊断响应头（X-Gateway-Name/Reason/Field）：
    # 默认 off，只在测试/联调环境打开；打开的配置 nginx -t 会打 WARN。
    # 本仓库的 deploy/nginx.conf 是本地测试配置，所以写 on。
    gateway_diagnostics on;

    # 网关注册表：声明名字 → 契约来源（URL 或文件，二选一）
    gateway_name     myrustwebdemo1;
    gateway_info_url myrustwebdemo1 http://127.0.0.1:9000/info;

    # 第二个网关指向**另一份**契约：内容只有部分路由重叠
    gateway_name     myrustwebdemo2;
    gateway_info_url myrustwebdemo2 http://127.0.0.1:9000/info_v2;

    # 也可以从本地文件读契约（后端被入侵也改不了规则）：
    #   - 配置加载时读一次，改完要 nginx -s reload
    #   - 相对路径按 nginx prefix 解析，发布用 mv 原子替换
    gateway_name      filegw;
    gateway_info_file filegw conf/contracts/demo.json;

    server {
        location / {
            enable_gateway_by myrustwebdemo1;      # 绑定一个网关
            proxy_pass http://demo_backend;
        }
        location = /info {
            enable_gateway_by myrustwebdemo1;      # 绑定多个：与语义
            enable_gateway_by myrustwebdemo2;
        }
        location = /info_v2        { enable_gateway_by myrustwebdemo2; ... }  # 只在 /info_v2 契约
        location = /api/user/99    { enable_gateway_by myrustwebdemo2; ... }  # 只在 v1 契约 → 404
        location = /api/audit      { enable_gateway_by myrustwebdemo1;        # 只在 /info_v2 契约
                                     enable_gateway_by myrustwebdemo2; ... }  # v1 先判 → 404
        location /passthrough/ {
            # 不写 enable_gateway_by = 只透传，不做判定
            proxy_pass http://demo_backend/;
        }
    }
}
```

demo_server 提供两份内容不同的契约，专供多网关对照：

| 契约地址 | `version` | 只在它里面声明的路径 |
| --- | --- | --- |
| `http://127.0.0.1:9000/info` | `phase1-fixed` | `/api/login`、`/api/user/{id}`、`/api/user/{id}/profile`、`/api/header-check`（Header 存在性）、`/api/header-value-check`（Header 值规则）、`/api/header-required-if`（Header `required_if`）、`/api/query-check`（Query）、`/api/query-strict`（Query unknown=reject）、`/api/body-check`（Body）、`/api/group/*`（分组：headers/unmatched/path_regex）、`/api/user/{id}`（路径参数）、`/api/v1/*`（登录 header / 表单 / 嵌套 admin）、`/api/public/survey`（公开表单） |
| `http://127.0.0.1:9000/info_v2` | `phase1-forward` | `/info_v2`、`/api/metrics`、`/api/reports/{year}` |

两边都有：`/info`、`/api/health`、`/api/products`、`/api/orders`、`/api/echo`。

两份契约的**未声明路由策略**不同，这也是它们存在的意义：

| 契约 | `protocol` | `unmatched` | 未声明路径的行为 |
| --- | --- | --- | --- |
| `/info` | 1 | 省略 = `deny` | 网关直接 404，不转发 |
| `/info_v2` | 1 | `forward` | 交给后端处理（`/api/user/99` 直连 200，经它也是 200） |

`/api/user/99` 在 `/info_v2` 里没有声明，但它是 `forward` → 请求到后端并返回 200；
同一个路径如果绑 `/info` 网关则是 404。`/api/audit` 同时绑两个网关时仍被 `/info` 判 404（它先判）。

- 定义都是 **http(main) 级**，每个配置周期在 `preconfiguration` 重置后重新解析；
- **顺序要求**：`gateway_name` → 契约来源，且定义写在引用它的 location 之前；
  未知名字、重复定义、缺契约来源、同时写 URL+文件都会被 `nginx -t` 拒绝；
  已移除的 `gateway_url_ttl` 再写会被当成未知指令拒绝；
- 契约来源二选一：`gateway_info_url`（HTTP 拉取）或 `gateway_info_file`（本地文件）。
  文件模式是为了**后端被入侵也改不了网关规则**：URL 模式下改自己的 `/info` 就能改规则；
  文件由发布流程写入、后端只读（root + 0640、`mv` 原子替换），规则变更走发布而不是联网。
- **统一契约管理器（ContractManager）**：文件与 URL 共用一条加载路径
  `ContractSource::fetch → Gateway::parse → 写共享内存`，适配层不重复实现规则；
- **单点获取**：所有 worker 通过共享内存里的 `leader` pid 选主，只有 leader 去读文件 / 发 HTTP，
  其余 worker 只读 `ngx_shm_zone`（`my_gateway_contracts`，1 MiB）里的快照；worker 数量增加
  不会变成 N 倍请求（将来接 etcd 时也只有一条 watch）；
  - **为什么不是 master 线程**：nginx 在 `ngx_init_cycle`（zone init）之后才 `daemon()`，
    master 里起的线程会被 `fork` 掉，所以选择 leader worker 方案；
  - leader 每秒刷心跳；退出后心跳超时（10s）或 `/proc/<pid>` 消失，由其它 worker 在请求路径上接管；
  - reload 时 zone 被复用 → `config_epoch + 1`、`leader = 0`，新周期 worker 重新选主并全量加载；
- **读取时机**：URL 模式由 leader 在启动/接管时拉一次；**没有 `gateway_url_ttl` / TTL 轮询**，
  后续更新通过 `nginx -s reload`（重新选主并全量拉取）；首次失败每 2s 重试；
  文件模式在**配置加载时读一次**（`nginx -t` / 启动 / reload），与 `nginx.conf` 完全一致——
  改文件不 reload 不生效；文件读不到或 JSON 非法直接拒绝本次配置，
  reload 失败时旧 worker 继续用旧契约。
  详见 [PROTOCOL.md](PROTOCOL.md) 第 1.1 / 1.2 节；
- 路径判定用**原始** request target（去掉 `?query`，不合并 `//`、不做百分号解码），
  nginx 的 `merge_slashes` 只影响 location 匹配；`//api/health` 与 `/api/health` 是两条不同路径；
- 未声明路由默认 `deny`（404）；契约写 `"unmatched": "forward"` 时改为转发给后端，
  已声明路由的方法校验仍然严格（405），详见 [PROTOCOL.md](PROTOCOL.md) 第 2.1 节；
- access 阶段只读共享内存快照（generation 变化时才重新 `Gateway::parse`），不做网络 I/O；
  同一条消息（成功/失败）只在状态变化时写一次日志；
- 契约任一路由非法（不以 `/` 开头、尾斜杠、重复、空方法）→ 该网关整份契约拒绝加载，继续用上一份好快照；
- **首次加载失败**（没有可用快照）→ 该网关的请求一律 503，fail-closed；后台每 2s 重试；
- 已加载成功的快照一直保留；只有 reload（重新选主并全量加载）才会重新拉取；
- URL 契约**禁止跟随重定向**：`max_redirects(0)` + 非 2xx 一律按失败处理（3xx 可能是劫持信号），
  见 `tests/phase1.sh` 第 12 节独立实例；
- 请求体在 access 阶段用 `ngx_http_read_client_request_body` 读取（temp file 用 `ngx_read_file`，
  `temp_file.file.offset` 即长度，不依赖路径）；读完回调恢复 phase 让 handler 重入，
  module ctx 里的 `header_added` 保证 `X-Request-Id` 每请求只出现一次；

### 契约格式（协议 v1）

以 **[PROTOCOL.md](PROTOCOL.md)** 为唯一约定：`protocol` / `version` / `meta` / `unmatched` / `routes[].path|methods|headers|query|body`。
当前解析是**严格模式**：

- `protocol` 不等于 `1`（插件支持的版本）→ 整份拒绝，日志提示升级插件或降低后端版本；
- 顶层或 `routes[]` 出现**未定义**字段 → 整份拒绝（已支持的 `headers` / `query` / `body` / `path_params`
  / `priority` / `path_regex` / `filters` / `responses` 见 [PROTOCOL.md](PROTOCOL.md)）；
- `meta` 是自由 JSON，插件忽略，不需要递增协议版本。

demo 后端两份契约都按文档带 `meta` 且 `protocol: 1`：`/info` 是默认 `deny`，`/info_v2` 写了
`unmatched: "forward"`。插件日志分别输出
`[myrustwebdemo1] contract loaded from url protocol=1 version=phase1-fixed routes=36 unmatched=deny` 和
`[myrustwebdemo2] contract loaded from url protocol=1 version=phase1-forward routes=8 unmatched=forward`。

## 4. 验证记录

运行环境：Fedora 43 / Rust 1.96.0 / nginx 1.28.0（本地源码编译）/ `ngx` git rev `15719544`。

```
$ cargo test -p gateway-core
test result: ok. 196 passed; 0 failed
```

```
$ ./tests/phase1.sh
0. 连通性                         2/2
1. 正常业务请求（放行）            9/9
2. 登录接口（JSON/form）           5/5
3. 未声明路径（含扫描器路径）     15/15
4. 对照：证明拦截来自网关          4/4
5. 契约生效（未声明/方法/尾斜杠/重复斜杠） 16/16
5.5 Header 存在性校验             17/17
5.6 Header 值规则                 46/46
5.7 Header required_if            30/30
5.8 Query 参数校验                37/37
5.9 Body 参数校验                 42/42
5.10 路径参数校验                 11/11
5.11 分组路由                     24/24
5.12 分组业务示例                 48/48
5.13 分组剩余继承                 18/18
5.14 filters（SQLi/XSS）           39/39
5.15 响应覆盖（responses）         17/17
5.16 路由级 path_regex / priority  13/13
5.17 Body 大小上限与 JSON 结构       25/25
6. 多网关：不同契约源             11/11
7. 未声明路由策略（deny/forward）  14/14
8. 日志                            5/5
9. 配置校验（nginx -t）             10/10
10. 契约文件模式（独立实例）        18/18
11. 来源与指令互斥                  2/2
12. 拉取不跟随重定向（独立实例）     6/6
13. 契约静态校验（methods/参数名/规则上限） 7/7
14. 诊断头默认关闭 + X-Request-Id    33/33
结果：524 通过，0 失败
```

```
$ ./tests/httpbin.sh     # 本地契约 + HTTPS 契约源 + 真实公网代理
结果：170 通过，0 失败

$ ./tests/tls-local.sh   # 本地自签 HTTPS + allow-localhost-ca（离线）
结果：24 通过，0 失败
```

> `phase1.sh` 启动时会裁掉最近一次启动之前的 `error.log`（原文件备份到
> `logs/error.log.before-phase1`）。`dev.sh restart` 是追加写，不裁会让上一次运行留下的
> `filter=sqli` / `rule=min` 等日志把断言“假通过”——`httpbin.sh` 用全新 prefix，才暴露过这类问题。

手工复验过的关键行为：

| 场景 | 期望 | 实测 |
| --- | --- | --- |
| `POST /api/body-check` 5 层 JSON | 400 `rule=json-depth` | 一致 |
| 33 字节 JSON 字符串 / 6 元素数组 | 400 `rule=json-string-length` / `json-array-entries` | 一致 |
| 字符串内 `{}[]` 与 `\"` | 不误判深度/结构 | 一致 |
| `GET /api/health` | 200 | 200 |
| `GET /api/admin/users`（未声明） | 404 + `X-Gateway-Reason: route-not-declared` + `X-Gateway-Name` | 一致 |
| `POST /api/health` | 405 + `Allow: GET` + `X-Gateway-Name` | 一致 |
| `GET /api/products/`（尾斜杠） | 404（严格匹配） | 一致 |
| `GET //api/health`（重复斜杠） | 404（按原始路径判定，不再落到后端） | 一致 |
| `GET /api/health?verbose=1` | 200（query 不参与匹配） | 一致 |
| `GET /phpinfo.php` | 404 + `X-Gateway-Reason: route-not-declared`（无内置探针） | 一致 |
| `/passthrough/api/admin/users` | 200（未绑定网关） | 一致 |
| `POST /info`（绑定 demo1 + demo2） | 405 + `X-Gateway-Name: myrustwebdemo1` | 一致 |
| `GET /info_v2`（只绑 demo2） | 200，响应体 `version: phase1-forward` | 一致 |
| `GET /api/user/99`（`/info_v2` 契约未声明，且是 `forward`） | 200，交给后端（`x-demo-backend: 1`） | 一致 |
| `POST /api/metrics`（`/info_v2` 已声明 GET） | 405 + `Allow: GET` + `X-Gateway-Name: myrustwebdemo2` | 一致 |
| `GET /api/audit`（只在 `/info_v2`；同时绑两个网关） | 404 + `X-Gateway-Name: myrustwebdemo1`（`/info` 先判） | 一致 |
| `GET /api/reports/2026`（`/info_v2` 动态路由） | 200 + `"year":"2026"` | 一致 |
| `GET /api/header-check`（无 `x-demo-token`） | 400 + `X-Gateway-Reason: missing-parameter` + `X-Gateway-Field: header.x-demo-token` | 一致 |
| `GET /api/header-check` + `X-Demo-Token` | 200 + `x-demo-backend: 1`（头名大小写不敏感） | 一致 |
| 同一 `x-demo-token` 两行 | 400 + `X-Gateway-Reason: invalid-parameter` + `X-Gateway-Field: header.x-demo-token` | 一致 |
| 显式可选 `x-demo-note` 存在/缺失 | 都是 200（`{ "required": false }` 不做检查） | 一致 |
| 携带未声明的头 | 200（未声明头不影响判定） | 一致 |
| **只起 nginx、后端未起** | 503 + `X-Gateway-Reason: contract-not-ready` | 一致 |
| 后端起来后（不重启 nginx） | 自动恢复 200 | 一致（首次失败每 2s 重试） |

每次改动请重跑（开发顺序见 [PLAN.md](../PLAN.md) §7.1：**先 `gateway-core` 测试，再 nginx 集成测试**）：

```bash
cargo test --offline -p gateway-core   # 1. 纯逻辑，先全绿
./scripts/build-module.sh && ./scripts/dev.sh restart
./tests/phase1.sh                      # 2. 真实 nginx 集成
./tests/httpbin.sh                     # 3. httpbin 独立实例（需要公网）
./tests/tls-local.sh                   # 4. 本地自签 HTTPS 契约源（离线）
```

### httpbin 独立演示（`deploy/httpbin/`）

一个 server 把 `/` 全部代理到 `https://httpbin.org`，网关契约来自本地文件
`deploy/httpbin/contract.json`（`gateway_info_file`，不依赖后端返回规则）。`tests/httpbin.sh`
会把配置和模块复制到临时 prefix 后启动，避免影响主实例：

| 契约能力 | 用例 |
| --- | --- |
| 放行后真实代理 | `/get?name=ab`、`/anything`、`/anything/{slug}`、`/status/200` 回显 |
| Query | `name` 必填/长度、`age` int 范围、重复、`unknown: reject` |
| Header | `x-demo-token` 必填/长度/大小写/多行重复 |
| Body | JSON `username`/`age`/`role`/`note`/`active`，缺字段、范围、`required_if`、枚举大小写、bool、未知字段、媒体类型、charset |
| 路径参数 | `{code}` 范围、`{slug}` regex、`{*rest}` 跨段通配（原始未解码值） |
| 方法 / 未声明路由 | 405 + `Allow`（含分组交集）、404 `route-not-declared` |
| 分组 | `methods` / `query` / `headers` / `body` / `path_params` / `filters` 继承；嵌套组；`unmatched: forward` 与更长子组 `deny`；组级 `path_regex` |
| `priority` / 路由正则 | `priority=10` 参数路由先匹配；路由级 `path_regex` 失败按未匹配处理 |
| `responses` | 404/405/400/413/415 自定义 body / `Retry-After` |
| `filters` | query 上的 SQLi / XSS（`rule=filter filter=sqli|xss`），组级开关不能被路由 `false` 关闭 |
| HTTPS 契约来源 | `gateway_info_url https://httpbin.org/base64/...`：TLS 拉取成功、契约生效（声明的 `/get` 放行、未声明 `/post` 404） |

> 该测试会真实访问公网 `https://httpbin.org`，网络不可达时会在前置检查处失败；
> 主验收 `tests/phase1.sh` 仍然是离线可跑的。

| 日期 | 改动 | 结果 |
| --- | --- | --- |
| 2026-10-26 | 阶段 1 首次落地：模块加载、日志、探针拦截、demo 后端、curl 验收 | 29/29 通过 |
| 2026-10-26 | 阶段 1.5：`gateway-core` + 契约拉取 + 404/405/Allow + fail-closed；`dev.sh` 修孤儿进程 | 22 单测 + 36/36 通过 |
| 2026-10-26 | `dev.sh` 收尾：`setsid --fork`、按 `/proc/exe` 认领 pid、启动等 `/api/health` 就绪；同步 PLAN/PROGRESS | 36/36 复跑通过 |
| 2026-10-26 | 新增 `docs/PROTOCOL.md`（协议 v1）+ 严格解析：`protocol` 版本校验、未知字段拒绝、`meta` 忽略 | 22 单测 + 36/36 通过 |
| 2026-10-26 | `dev.sh` 以真实 master 为准（pid 文件丢失自动回写、确认无进程才删）；worker `exit_process` join 刷新线程 | restart ×20 + reload ×20 无新增 SIGSEGV；22 单测 + 36/36 |
| 2026-10-26 | 配置模型改为具名多网关：http 级 `gateway_name` / `gateway_info_url <name> <url>` / `gateway_ttl <name> <n>`，location 用 `enable_gateway_by <name>`（删除 `gateway on\|off`） | 49/49 通过；独立契约源（9001）验证多网关互不影响 |
| 2026-10-26 | demo_server 新增 `GET /info_v2`（内容与 `/info` 部分重叠）；`myrustwebdemo2` 改指向它，新增 4 个差异对照 location；`dev.sh` 在 demo_server 源码更新后自动重编 | 66/66 通过 |
| 2026-10-26 | 清理死代码：删除 `EXAMPLE_CONTRACT`、未使用的 re-export、`Decision::status`；`meta` 改为 `_meta` 消除 dead_code 警告 | 22 单测 + 66/66，编译无警告 |
| 2026-10-26 | 修复路径判定与上游不一致：改用原始 request target 取 path（`//api/health` 以前被 nginx `merge_slashes` 归一后放行，后端却收到原始路径） | 73/73 通过 |
| 2026-10-26 | 未声明路由策略：顶层可选 `unmatched`（`deny` 默认 / `forward` 交给后端）。开发期协议仍是 v1，不升版本号；demo 的 `/info_v2` 用 `unmatched: forward` 做对照 | 25 单测 + 78/78 通过 |
| 2026-10-26 | 测试按协议重写为 `protocol_v1.rs`（42 项，自包含、无共享夹具）；新增 `Decision::status_code` 并让 nginx 适配层复用，测试可直接断言 404/405 | 42 单测 + 78/78 通过 |
| 2026-10-26 | 新增契约来源 `gateway_info_file`（本地文件，与 `gateway_info_url` 二选一）：每个 TTL 只 `stat`，mtime/长度/inode 变化才重读；坏文件保留 last-known-good；相对路径按 nginx prefix 解析。动机是**后端被入侵时不能改网关规则** | 42 单测 + 94/94 通过 |
| 2026-10-26 | 文件来源改为**配置加载时读一次**（与 `nginx.conf` 一致），删除文件刷新线程/`FileStamp`；读不到或 JSON 非法 → 拒绝本次配置，reload 失败旧 worker 继续跑。`gateway_ttl` 改名 `gateway_url_ttl`（只对 URL 来源有效，文件来源写了报错） | 42 单测 + 100/100 通过 |
| 2026-10-26 | 阶段 5 第一切片：Header 存在性（`true` / `{ "required": true\|false }`）；缺失 `400 missing-parameter`、同一头多行 `400 invalid-parameter`，回写 `X-Gateway-Field` 与 `rule=required\|duplicate`；适配层从 `headers_in_iterator` 收集请求头。Query/Body 与 Header 值规则仍未实现 | 60 单测通过；`cargo check -p my-gateway` 通过 |
| 2026-10-26 | Header 存在性接入 nginx 运行链路：`/info` 新增 `/api/header-check`（`x-demo-token` 必填、`x-demo-note` 显式可选），`deploy/nginx.conf` 单独绑定 `myrustwebdemo1`；`phase1.sh` 新增 17 项集成断言（缺失/放行/大小写/可选/未声明/重复行/日志） | 60 单测 + 117/117 集成通过 |
| 2026-10-26 | 固化后续开发方式：新增 `AGENTS.md`，`PLAN.md` §7.1 与 `PROTOCOL_TEST_PLAN.md` §1.1 明确“先 `gateway-core` 测试 → 实现 core → 再接入 nginx → 补 `phase1.sh` 集成测试”，并列出硬性约束与全量验收命令 | 文档更新 |
| 2026-10-26 | 阶段 5 第二切片：Header 值规则第一批（`type` `char/int/bool/enum`、`min/max`、`min_len/max_len/len`、`values/ignore_case`、`regex` 全匹配）。Header 值去首尾 OWS；非 UTF-8 仅在有值规则时 `400 invalid-parameter`；顺序为必填 → 重复 → 值校验；适配层改传 `Option<&str>` | 80 单测 + 163/163 集成通过 |
| 2026-10-26 | 阶段 5 第三切片：Header `required_if`（字符串形式按被引用头是否出现；对象形式按去 OWS 后区分大小写的完全相等）。启用顺序：无条件必填 → 条件必填 → 重复 → 值校验；契约拒绝自引用、引用未声明头、`required: true` 同用及非法形态；新增 `/api/header-required-if` 与集成断言 | 97 单测 + 193/193 集成通过 |
| 2026-10-26 | 阶段 5 第四/五切片：Query 与 Body。抽出 `params.rs` 统一三维度的 required / required_if / 重复 / 值 / unknown；新增 `query.rs`（原始 query 一次解码、UTF-8、空 key / 同名 / 256 对上限）与 `body.rs`（JSON/form、content_types、charset、Content-Encoding、重复键、标量类型）；适配层在 access 阶段按需读取 body（同步/异步 post handler），proxy 复用缓冲；新增 `/api/query-check`、`/api/query-strict`、`/api/body-check` | 119 单测 + 268/268 集成通过 |
| 2026-10-26 | 阶段 5 第六/七切片：路径参数与分组。`path_params` 使用 matchit 原始路径值，规则名必须存在于路由，禁止 required/required_if/true；分组 `prefix`/`router` 递归展开，继承 `headers`，按最长前缀+结构父链选择 `unmatched`，分组 `path_regex` AND 全匹配；分组其他字段与路由级 `path_regex` 仍拒绝 | 140 单测 + 303/303 集成通过 |
| 2026-10-26 | 增加分组业务示例：`/api/v1` 组级 `authorization`（登录 header）+ `path_regex`，`/feedback` 表单（form/JSON、`required_if`），嵌套 `/admin` 追加 `x-admin-token`；`/api/public/survey` 公开表单（email 正则 / score 范围 / consent）；`tests/phase1.sh` 补 48 项 | 351/351 集成通过 |
| 2026-10-26 | 分组剩余继承：`methods` 交集与 `Allow` 顺序、`query`/`body`/`path_params` 合并、`unknown` 更严格者、`body.content_types` 交集；新增 `/api/org` 组级 path_params 与 `/api/v1` 组级 query/body/methods；core 补 9 项、集成补 18 项 | 149 单测 + 369/369 集成通过 |
| 2026-10-26 | 实现 filters（SQLi / XSS）：顶层/分组/路由开关、或继承；按维度在参数校验后扫描，SQLi/XSS 使用第三方 `libinjectionrs`；命中 `400 invalid-parameter`，日志 `rule=filter filter=sqli|xss`；新增 `/api/filter` 示例；core 补 11 项、集成补 28 项 | 160 单测 + 397/397 集成通过 |
| 2026-10-26 | 实现路由级 `path_regex`、`priority`、`responses`：正则与分组 AND；priority 分组路由树高优先级先匹配；responses 自定义状态/body/Retry-After；新增 `/api/route-regex/*`、`/api/priority/*` 示例；core 补 11 项、集成补 30 项 | 171 单测 + 427/427 集成通过 |
| 2026-10-26 | Body 临时文件支持：`client_body_buffer_size` 调小后大 body 落临时文件，插件按 `temp_file->offset` 读回再校验；`/api/body-check` 的 16KB 异步用例同时覆盖 temp file；超 `client_max_body_size` 的 413 仍可能由 nginx 预检直接返回 | 428/428 集成通过 |
| 2026-10-26 | `responses.blocked` 接入探针：`/phpinfo.php` 等探针命中时返回自定义 403 body；`contract_not_ready` / `body_too_large` 仍受运行时时机限制 | 429/429 集成通过 |
| 2026-10-26 | JSON 结构扫描：`max_depth` 默认 32（上限 128）、`max_string_length` 默认 8192 字节、`max_array_entries` 默认 500；解析前单遍扫描、组继承、`0` 关闭；core 补 6 项、集成补 9 项 | 196 单测 + 486/486 集成通过 |
| 2026-10-26 | Body 增加 `max_bytes`：原始字节上限，分组继承、子级取更小或写 0 关闭；Content-Length 超限在读 body 前 413，chunked 读后检查；新增 `/api/limit` 示例；core 补 5 项、集成补 16 项 | 190 单测 + 477/477 集成通过 |
| 2026-10-26 | filters 增加 `scope`：默认四个维度全开，可限制只检查 `path` / `query` / `header` / `body`；跨层并集、子层不能收窄；新增 `/api/filter/scope` 示例；core 补 8 项、集成补 11 项 | 179 单测 + 444/444 集成通过 |
| 2026-10-26 | `gateway_info_url` 新增可选第三参数 `allow-localhost-ca`：仅限 `https://localhost` / `127.0.0.1` / `[::1]`，关闭该网关证书校验并输出 WARN；非本地地址、http、未知参数在 `nginx -t` 拒绝；新增 `tests/tls-local.sh` 24 项 | 24/24 离线集成通过 |
| 2026-10-26 | 契约拉取不跟随重定向：`max_redirects(0)`，只接受 `2xx`；302 等状态按 `bad-status` 失败并保留旧快照；集成补 4 项 | 433/433 集成通过 |
| 2026-10-26 | 严格模式补强：`methods` 必须是 RFC 9110 token（1–32 字符，修复 `Allow` 响应头/日志注入与 `"GET, POST"` 被拆成两个方法）；路径参数名按 `[A-Za-z_][A-Za-z0-9_]{0,63}` 校验、重复名无条件拒绝；单路由（含继承）规则合计 ≤256；body 未声明字段为 `null` 不再绕过 `unknown: "reject"`；内置探针改按路径段匹配（任意 `.env`、`.git` 段、`/wp-*` 根前缀，不再误伤 `/.github`）；移除演示钩子 `/__gateway/deny`；URL 契约响应体上限 1 MiB；日志队列满时输出 dropped 计数；body 状态分配失败回 500 而不是 abort worker；core 补 6 项、集成补 17 项 | 185 单测 + 461/461 集成通过 |
| 2026-10-27 | **统一契约管理器 + 共享内存分发**：`ContractSource`（File/Http，etcd 预留）→ `Gateway::parse` → `ngx_shm_zone`（`my_gateway_contracts`，seqlock）；worker 不再各自刷新，由共享内存里的 `leader` pid 选主，leader 每秒心跳、退出后 10s 内由其它 worker 接管；reload 时 `config_epoch+1`、`leader=0` 重新选主。master 线程方案被否（`daemon()` 在 zone init 之后 `fork`，线程会被丢掉）。同时接入 **请求体**（Content-Length 预检 + `ngx_http_read_client_request_body` + temp file 用 `ngx_read_file`）与 **响应覆盖**（`responses.*` 自定义 status/body），`X-Request-Id` 改为每请求只回写一次。集成补 Body/响应覆盖/重定向相关断言至 524 项 | 196 单测 + 524/524 集成通过 |
| 2026-10-27 | **回归修复（`tests/httpbin.sh` / `tests/tls-local.sh` 发现）**：适配层重写时丢了三处协议要求的日志字段，已恢复为文档格式——参数拒绝行补 `method` 与 `filter=sqli|xss`，forward 行补 `scope=<prefix>`，413 行改为 `reason=body-too-large limit=<max>`；`gateway_info_url` 第三参数 `allow-localhost-ca` 的解析/适用范围校验/WARN 一并恢复（`is_local_https_url` 只放行 `https://localhost`、`127.0.0.1`、`[::1]`）。另修 `phase1.sh`：启动时裁掉最近一次启动之前的 `error.log`（备份 `.before-phase1`），避免旧日志让断言假通过 | 196 单测 + 524/524、httpbin 170/170、tls-local 24/24 |
| 2026-10-26 | **诊断头上线开关**：新增 http 级 `gateway_diagnostics on\|off`（默认 `off`，每配置周期重置）；`X-Gateway-Name` / `-Reason` / `-Field` / `-Blocked` 只在打开时回写，非法值/重复声明 `nginx -t` 拒绝，打开时打 WARN。诊断关闭后拒绝响应统一带 `X-Request-Id`（复用 nginx `$request_id`），网关日志以 `rid=` 结尾，可用同一 ID 关联。本切片只在适配层，`gateway-core` 无对应单测；`phase1.sh` 新增独立「上线形态」实例与双模式断言 38 项 | 196 单测 + 524/524 集成通过；httpbin 170/170、tls-local 24/24 |
| 2026-10-27 | **清理 TTL 与旧架构残留**：删除契约 `ttl` 字段与 `gateway_url_ttl` 指令、`GatewayDef.url_ttl`、`GatewaySourceConfig` 的 Option 双字段（改为 File/Url 枚举）与按 TTL 轮询；URL 契约改为 leader 启动/接管时拉一次，首次失败每 2s 重试，更新通过reload。恢复 URL 契约 1 MiB 响应体上限；未被 location 引用的网关不再拉取；请求路径先比 `generation` 再复制 payload；心跳节流到每秒；leader 线程启动失败/epoch 变化时释放 leader；接管改为 CAS 失效 pid（原先只能抢 `0`，leader 进程死掉后无法接管）。文档同步删除 TTL 描述、修正架构与计数 | 196 单测 + 524/524、httpbin 170/170、tls-local 24/24 |
| 2026-10-27 | **移除内置探针**：删除 `.php` / `.env` / `.git` / `/wp-*` 硬编码规则、`probe_block_reason`、`X-Gateway-Blocked` 诊断头与 `responses.blocked` 覆盖；这些路径改由契约判定（未声明 → 404 `route-not-declared`，或按 `unmatched: forward` 交给后端） | 196 单测 + 524/524、httpbin 170/170、tls-local 24/24 |

## 5. 常用命令

```bash
# 首次
./scripts/build-nginx.sh
./scripts/build-module.sh

# 日常
./scripts/dev.sh up        # 构建(如需)并启动 demo_server + nginx
./scripts/dev.sh status
./scripts/dev.sh logs
./scripts/dev.sh down

# 改完 Rust 模块后必须 restart（nginx 不会 reload 已加载的 .so）
./scripts/build-module.sh && ./scripts/dev.sh restart
```

手工验证：

```bash
curl -i http://127.0.0.1:8080/api/health                  # 200
curl -i http://127.0.0.1:8080/api/admin/users             # 404 route-not-declared
curl -i -X POST http://127.0.0.1:8080/api/health          # 405 + Allow: GET
curl -i http://127.0.0.1:8080/phpinfo.php                 # 404 route-not-declared
curl -i http://127.0.0.1:8080/passthrough/phpinfo.php     # 200（未绑定网关）
curl -i -X POST http://127.0.0.1:8080/info                # 405（demo1 + demo2 都绑定）
curl -s http://127.0.0.1:9000/info                        # 看当前契约
tail -f nginx/logs/error.log .run/demo_server.log
```

复验 fail-closed（冷启动）：

```bash
./scripts/dev.sh down
./nginx/sbin/nginx -p "$PWD/nginx" -c conf/nginx.conf      # 只起网关
curl -i http://127.0.0.1:8080/api/health                    # 503 contract-not-ready
./scripts/dev.sh up                                        # 起后端
curl -i http://127.0.0.1:8080/api/health                    # 200（后台重试已恢复）
```

### dev.sh 行为说明

- `up`：pid 文件失效或 9000 被本仓库残留进程占用时，先按 `/proc/<pid>/exe` 认领并清理，再启动；
  启动后轮询 `/api/health`，拿到 200 才打印“就绪”（冷启动的短暂 503 不会误报失败）。
- `down`：先停 nginx 再停 demo_server；pid 文件失效时按可执行文件路径兜底清理；若端口仍被**外部**
  进程占用，会打印 pid 但不会误杀。
- **nginx 以真实进程为准**：`nginx_running` 扫 `/proc/<pid>/exe` + cmdline 找本仓库 master，不信任
  pid 文件。pid 文件只是 `-s reload/quit` 的入参缓存，丢失时会自动回写；只有确认没有 master 后才删。
  这样即使 pid 文件被旧 master 退出时 unlink，`down` 也不会漏停、`up` 也不会起第二个 master。
- 进程用 `setsid --fork` 启动，pid 文件记录的是真实进程（不依赖 `$!`）；因此终端或 harness 退出
  不会留下半死进程，`dev.sh up | tail` 之类的管道也不会被后台进程挂住。
- `status` 会额外提示“不在 pid 文件里”的本仓库残留进程；pid 文件缺失时会注明已按真实进程回写。

日志位置：

- nginx：`nginx/logs/error.log`（契约加载/刷新、404/405 证据）、`nginx/logs/access.log`
- demo_server：`.run/demo_server.log`（每请求一行 `[demo_server] METHOD URI`）

## 6. 环境备注（重建时需要）

- **代理只用于下载**：`cargo`/`curl` 下载依赖时用 `http_proxy=http://127.0.0.1:10808`；
  `scripts/dev.sh` 与 `tests/phase1.sh` 内部已设 `NO_PROXY=127.0.0.1,localhost`，本地测试不走代理。
- **无 root**：nginx 装到 `nginx/`，libclang 解包到 `.build/llvm`。
- 若 `.build/llvm` 丢失，重新解包：

```bash
dnf download --destdir /tmp/llvmdl clang-libs llvm-libs
mkdir -p .build/llvm
for r in /tmp/llvmdl/*.x86_64.rpm; do rpm2cpio "$r" | cpio -idmu -D .build/llvm; done
```

- 版本锁定：nginx `1.28.0`（与 `nginx-sys` 对齐）；`ngx`/`nginx-sys` 锁 git rev
  `15719544cfaf4322d602b0a8c2a8f85252ac6559`（crates.io 的 0.5.x 缺少 handler API）。
- **bindgen / syn 坑**：bindgen 0.73.2 允许 `syn >=2,<4`，默认会选到 syn 3，而旧 `prettyplease 0.2`
  仍依赖 syn 2，导致 `Parse` trait 不匹配、编译失败。已把 `prettyplease` 锁到 **0.3.0**（依赖 syn 3），
  现在整棵树只有一个 syn。可用 `cargo tree -e build -p nginx-sys | grep syn` 自检。
- `ureq` 用 `default-features = false, features = ["rustls"]`：支持 `http://` 与 `https://` 契约地址，
  gzip 仍关闭。版本锁在 `=3.4.0`（3.5 要求 `rustls ^0.23.46`，当前 registry 镜像只有 0.23.43）；
  HTTPS 默认使用 `webpki-roots` 公共根证书；本地自签证书用
  `gateway_info_url <name> <url> allow-localhost-ca`（仅限 localhost/127.0.0.1/[::1]，见协议 §1.4）。
- 建议不要 `cargo update`，升级前先确认 `examples/*.rs` 的 API 是否变化。

## 7. 待办（优先级顺序）

1. **`responses` 剩余时机**：`contract_not_ready` 在无快照时没有配置可读；nginx 预检 413 早于插件。
   需要更早的钩子或响应后处理，保持 fail-closed。
2. 多 worker 已完成单点获取（leader worker + 共享内存）；后续可评估把 leader 心跳改成
   `shmtx` 锁 + `ngx_process_helper` 专用进程，进一步缩短 leader 退出后的接管窗口。
3. 契约来源加固（未开发）：URL 凭据/fragment/长度校验、内容摘要/签名钉死、
   可选仅回环/白名单地址、拉取失败告警；高安全场景优先用 `gateway_info_file`。

## 8. 已知限制

- 修改模块后必须 `dev.sh restart`，`nginx -s reload` 不会重新加载 `.so`。
- 冷启动时后端不可用 → 绑定了网关的 location 全站 503（这是刻意的 fail-closed）；不需要此行为就去掉
  对应的 `enable_gateway_by`（或删掉该网关定义）。
- 契约路径**严格匹配**：`/api/products/` 与 `/api/products` 不是同一条；契约里写尾斜杠会直接报错。
- `methods: []` 表示不限方法；`Allow` 头按契约里写的顺序输出。
- `unmatched: "forward"` 是迁移期开关：未声明路径（含扫描器路径、拼错路径）都会交给后端，
  后端必须自己兜底 404；契约覆盖完整后建议改回默认的 `deny`。
- 诊断头默认关闭：`X-Gateway-Name` / `-Reason` / `-Field` 只在 `gateway_diagnostics on`
  时返回；关闭后 `reason/field/rule` 仅存在于 `error_log`（级别需为 `notice`），没有 `gateway_log_level` 指令。
- `X-Request-Id` 由插件生成（`<pid-hex><8 位序号>`），只加在网关自己产生的响应上
  （404/405/400/413/415/503）；上游响应不加。handler 因读 body 重入时不会重复回写。
- leader worker 退出后，其它 worker 要等心跳超时（10s）或 `/proc/<pid>` 消失才会接管；
  这段时间内已有快照仍然可用（fail-open 到 last-known-good），只是失败重试暂时停止。
- `dev.sh` 会清理本仓库路径下的 demo_server 残留进程（按 `/proc/<pid>/exe` 认领），不会动其他进程；
  `setsid --fork` 启动，终端退出不会留下半死状态。
- `.build/`、`target/`、`nginx/`、`.run/` 都是本地产物，重建成本低，不要提交到版本库。
