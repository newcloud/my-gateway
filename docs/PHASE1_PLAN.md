# 阶段 1 实施计划：最小可行性 Demo

> 总计划见 [../PLAN.md](../PLAN.md)。本文只覆盖阶段 1：把「Rust 写 nginx 模块 → 加载 → 拦住请求」这条链路跑通。
> 状态：**已完成**（2026-10-26），验收结果见 [PROGRESS.md](PROGRESS.md)。
>
> 后续补充（2026-10-26）：阶段 1 完成后，用户要求 demo「真正读取并启用配置」，因此把阶段 3/4 的核心
> 提前落地为**阶段 1.5**：http 级具名网关（`gateway_name` / `gateway_info_url <name> <url>`
> 或 `gateway_info_file <name> <path>`）+ location `enable_gateway_by <name>`
> + URL 启动时单点拉取（之后靠 reload；文件在配置加载时读）+ `gateway-core` 判定（matchit 路由、404/405 + `Allow`）。本文第 1 节
> 「不拉取契约」的非目标**已被 1.5 覆盖**，
> 验收断言从 29 项增至 78 项（新增契约文件模式后 100 项，Header 存在性接入后 117 项，
> Header 值规则第一批接入后 163 项，Header `required_if` 接入后 193 项，Query/Body 接入后 268 项，路径参数/分组接入后 303 项，分组业务示例接入后 351 项，分组剩余继承接入后 369 项，filters 接入后 397 项，path_regex/priority/responses 接入后 427 项，Body 临时文件接入后 428 项，blocked 覆盖接入后 429 项，拉取不跟随重定向后 433 项，filters scope 后 444 项，严格模式补强后 461 项，body max_bytes 后 477 项，JSON 结构扫描后 486 项，诊断头上线开关后 524 项；统一契约管理器后 524 项）；详见 [PROGRESS.md](PROGRESS.md) 第 2、3 节。

## 1. 目标与非目标

目标（必须全部可手工复现）：

1. 本地目录里有一份能跑的 nginx，能 `load_module` 加载 Rust 编出来的 `.so`；
2. 有一个 demo 后端，提供登录等常用接口，外加一个**内容写死**的契约接口 `GET /info`；
3. 插件在 access 阶段能读到 method/uri 并写日志；
4. 插件能直接返回自定义状态码，被拦的请求不得到达后端；
5. 用 curl 能分别观察到「放行 / 拦截 / 开关关闭时透传」三种结果；
6. 全程不需要 root。

非目标（本阶段明确不做）：
<!-- 注：第 1 条已被阶段 1.5 提前实现，保留原文用于对照 -->

- ~~不拉取契约、不解析 JSON、不做路由树、不校验参数（阶段 3–5）~~ → 1.5 已实现契约拉取、JSON 解析、matchit 路由树、方法校验；仅参数校验留给阶段 5；
- ~~不做配置热加载、不做多 worker 共享状态~~ → 1.5 已用 leader worker + 共享内存实现单点获取；不做性能压测；
- demo 后端的接口只求"够用、能覆盖后续阶段的测试形状"，不追求完整业务。

## 2. 环境结论（决定实现方式）

本机实测：Fedora 43，无系统 nginx、无 clang；有 gcc/make、pcre2/zlib/openssl 头文件、Rust 1.96。

| 问题 | 结论 |
| --- | --- |
| 没有 nginx | 从源码编译到 `nginx/` 前缀，带 `--with-compat`，无需 root |
| 没有 libclang（bindgen 需要） | 用 `dnf download` 取 `clang-libs`/`llvm-libs` 的 rpm，解包到 `.build/llvm`，设 `LIBCLANG_PATH` |
| 解包后的 clang 找不到 `stddef.h` | `BINDGEN_EXTRA_CLANG_ARGS=-I.build/llvm/usr/lib/clang/21/include` |
| crates.io 的 `ngx` 0.5.x 没有 `HttpRequestHandler` | 该 API 只在 git main，锁 rev `15719544` |
| 代理只用于下载 | 脚本内 `NO_PROXY=127.0.0.1,localhost`，本地 curl 一律直连 |

nginx 版本固定 **1.28.0**，因为 `nginx-sys` 的 vendored 源是 1.28.x；模块必须与运行中的 nginx 同源同配置编译。

## 3. 交付物与目录

```
my_gateway/
├── Cargo.toml                     # workspace: gateway-core + ngx-gateway + demo_server
├── PLAN.md                        # 总计划
├── docs/
│   ├── PROTOCOL.md               # 网关协议 v1（前后端唯一约定）
│   ├── PHASE1_PLAN.md            # 本文
│   └── PROGRESS.md               # 进度 + 文档目录
├── crates/gateway-core/           # 纯逻辑：契约解析 / 路由 / 判定（1.5 新增）
│   ├── src/{contract,gateway,value,lib}.rs
│   └── tests/protocol_v1.rs       # 196 项协议单测（原 contract_cases.rs）
├── crates/ngx-gateway/            # nginx 动态模块（cdylib）
│   ├── src/lib.rs                 # 指令 + access handler + body 读取 + 写响应
│   ├── src/contract_manager.rs    # leader 选举 + 首次加载/失败重试
│   ├── src/contract_source.rs     # FileSource / HttpSource
│   └── src/contract_shm.rs        # ngx_shm_zone 快照分发
├── demo_server/                   # 被保护的后端 + 固定契约
│   └── src/main.rs
├── deploy/nginx.conf              # 运行时配置源文件（拷贝到 nginx/conf/）
├── scripts/
│   ├── build-nginx.sh             # 下载/编译/安装本地 nginx
│   ├── build-module.sh            # 编译 .so 并部署到 nginx/modules/
│   └── dev.sh                     # up/down/restart/status/logs
├── tests/phase1.sh                # curl 验收（524 项断言）
├── nginx/                         # 构建产物：sbin/ conf/ modules/ logs/
└── .build/                        # nginx 源码、本地 libclang（体积大，可随时重建）
```

运行端口：nginx `127.0.0.1:8080`，demo_server `127.0.0.1:9000`。

## 4. 任务拆解与实际结果

| # | 任务 | 结果 |
| --- | --- | --- |
| 1 | 记录 `nginx -V` 等环境信息 | 系统无 nginx → 改为源码构建，见第 2 节 |
| 2 | 本地编译 nginx（`--with-compat`） | `scripts/build-nginx.sh`，产物 `nginx/sbin/nginx` |
| 3 | 搭 ngx 模块骨架，注册 access handler | `crates/ngx-gateway/src/lib.rs` |
| 4 | 加 `gateway on\|off` 指令 | 阶段 1 实现；**1.5 已替换为具名多网关**（`enable_gateway_by`，不再有 on/off） |
| 5 | 访问日志（notice 级） | 每个请求一行 `gateway: METHOD URI (path=...)` |
| 6 | ~~内置探针拦截~~ | ~~`.php` 后缀 / 任意段 `.env` / `.git` 段 / 根前缀 `/wp-` → 403~~；已移除，现由契约按未声明路由处理 |
| 7 | demo_server + 固定契约 | 业务接口 + `GET /info`（36 条叶子路由（含分组展开），含 Header / Query / Body / 路径参数 / 分组 / 登录与表单示例）；1.5 追加 `GET /info_v2` |
| 8 | nginx 反代 + 对照 location | `/` 网关接管，`/passthrough/` 关闭网关 |
| 9 | curl 验收脚本 | `tests/phase1.sh`，29 通过 / 0 失败（1.5 扩到 36 项，多网关后 49 项，双契约后 78 项，契约文件模式后 100 项，Header 存在性接入后 117 项，Header 值规则第一批接入后 163 项，Header `required_if` 接入后 193 项，Query/Body 接入后 268 项，路径参数/分组接入后 303 项，分组业务示例接入后 351 项，分组剩余继承接入后 369 项，filters 接入后 397 项，path_regex/priority/responses 接入后 427 项，Body 临时文件接入后 428 项，blocked 覆盖接入后 429 项，拉取不跟随重定向后 433 项，filters scope 后 444 项，严格模式补强后 461 项，body max_bytes 后 477 项，JSON 结构扫描后 486 项，诊断头上线开关后 524 项；统一契约管理器后 524 项） |
| 10 | 进度与文档目录 | `docs/PROGRESS.md` |
| 11 | **1.5**：抽 `gateway-core`，196 项单测 | `cargo test -p gateway-core` 全绿 |
| 12 | **1.5**：具名网关 + 统一契约管理器 | 文件/HTTP 共用 `ContractSource` → `Gateway::parse` → `ngx_shm_zone`；单点（leader worker）启动时拉取，worker 只读；日志 `[myrustwebdemo1] contract loaded ... routes=36` |
| 13 | **1.5**：契约判定（404 / 405 + `Allow` / 503 未就绪） | `tests/phase1.sh` 扩到 36 项，全部通过 |
| 14 | **1.5**：配置模型改为 http 级具名多网关 | `enable_gateway_by` + `X-Gateway-Name`；`tests/phase1.sh` 扩到 49 项 |
| 15 | **1.5**：demo_server 第二份契约 `GET /info_v2` | `myrustwebdemo2` 指向 `/info_v2`；新增 4 个差异对照 location；`tests/phase1.sh` 扩到 66 项，原始路径判定后 78 项 |
| 16 | **阶段 5**：Header 存在性接入 | `/info` 新增 `/api/header-check`（`x-demo-token` 必填 / `x-demo-note` 显式可选）+ 独立 location；`tests/phase1.sh` 补 17 项，共 117 项 |
| 17 | **阶段 5**：Header 值规则第一批 | `/info` 新增 `/api/header-value-check`（`char` 长度、`int` 范围、`enum` 大小写、`regex` 全匹配）+ 独立 location；core 补 20 项、`tests/phase1.sh` 补 46 项，共 80 单测 / 163 集成 |
| 18 | **阶段 5**：Header `required_if` | `/info` 新增 `/api/header-required-if`（对象/字符串两种条件、条件成立后仍做值校验）+ 独立 location；core 补 17 项、`tests/phase1.sh` 补 30 项，共 97 单测 / 193 集成 |
| 19 | **阶段 5**：Query | 抽出 `params.rs` 复用三维度规则；`/info` 新增 `/api/query-check`、`/api/query-strict`；core 补 10 项、`tests/phase1.sh` 补 37 项，共 107 单测 / 230 集成 |
| 20 | **阶段 5**：Body | 新增 `body.rs`（JSON/form、content_types、charset/encoding）；access 阶段按需读 body（同步/异步）；`/info` 新增 `/api/body-check`；core 补 12 项、`tests/phase1.sh` 补 38 项，共 119 单测 / 268 集成 |
| 21 | **阶段 5**：路径参数 | `path_params` 使用原始路径值，规则名必须存在于路由，禁止 required/required_if；core 补 8 项、`tests/phase1.sh` 补 11 项，共 127 单测 / 279 集成 |
| 22 | **阶段 5**：分组基础 | `prefix`/`router` 展开、`headers` 继承、`unmatched` 作用域、分组 `path_regex`；core 补 13 项、`tests/phase1.sh` 补 24 项，共 140 单测 / 303 集成 |
| 23 | **阶段 5**：分组业务示例 | `/api/v1` 登录 header + 路径参数 + form/JSON 表单 + 嵌套 admin；`/api/public/survey` 公开表单；`tests/phase1.sh` 补 48 项，共 140 单测 / 351 集成 |
| 24 | **阶段 5**：分组剩余继承 | `methods` / `query` / `body` / `path_params` 合并、`unknown` 严格化、`content_types` 交集；`/api/org` 组级 path_params；core 补 9 项、集成补 18 项，共 149 单测 / 369 集成 |
| 25 | **阶段 5**：filters | 顶层/分组/路由 `filters`、或继承；SQLi/XSS 使用 `libinjectionrs`；按维度在参数校验后扫描；`/api/filter` 示例；core 补 11 项、集成补 28 项，共 160 单测 / 397 集成 |
| 26 | **阶段 5**：路由级 `path_regex` | 与分组正则 AND；core 补 3 项、集成补 5 项 |
| 27 | **阶段 5**：`priority` | 按 priority 分组路由树，高优先级先匹配；同优先级保留具体度；core 补 4 项 |
| 28 | **阶段 5**：`responses` | 覆盖 core 拒绝状态码 / body / `Retry-After`；core 补 4 项、集成补 17 项，共 171 单测 / 427 集成 |
| 29 | **阶段 5**：Body 临时文件 | 从 `temp_file->offset` 读回大 body；`/api/body-check` 16KB 用例覆盖 temp file + 异步；集成补 1 项，共 428 集成 |
| 30 | **阶段 5**：拉取不跟随重定向 | `max_redirects(0)`，只接受 `2xx`；302 按 `bad-status` 失败；集成补 4 项，共 171 单测 / 433 集成 |
| 31 | **阶段 5**：filters scope | 默认四个维度全开，可限制 `path` / `query` / `header` / `body`；跨层并集、子层不能收窄；core 补 8 项、集成补 11 项，共 179 单测 / 444 集成 |
| 32 | **严格模式补强** | `methods` 必须是 RFC 9110 token（修复 `Allow` 头/日志注入）、路径参数名语法与重复、单路由规则合计 ≤256、body 未声明 `null`；适配层探针按段匹配、移除 `/__gateway/deny`、契约 1 MiB 上限、日志 dropped 计数；core 补 6 项、集成补 17 项，共 185 单测 / 461 集成 |
| 33 | **阶段 5**：body.max_bytes | 原始 body 字节上限，分组继承、子级取更小或写 0 关闭；Content-Length 预检 + chunked 读后检查；core 补 5 项、集成补 16 项，共 190 单测 / 477 集成 |
| 34 | **阶段 5**：JSON 结构扫描 | `max_depth` / `max_string_length` / `max_array_entries`，解析前单遍扫描、组继承、`0` 关闭；core 补 6 项、集成补 9 项，共 196 单测 / 486 集成 |
| 35 | **诊断头上线开关** | `gateway_diagnostics on\|off`（默认 off）；诊断头只在测试环境回写，非法值/重复声明拒绝加载、开启时打 WARN；拒绝响应统一带 `X-Request-Id`，日志以 `rid=` 结尾。纯适配层切片，无 core 用例；`phase1.sh` 新增默认 off 实例与双模式断言 38 项，共 196 单测 / 524 集成 |

## 5. 关键设计取舍

1. **插件只做"能看见、能拦截"**。阶段 1 曾用硬编码探针验证拦截链路；该层已移除，现在所有拒绝都来自契约判定。
2. **两套日志证据**。插件写 `error_log`，后端写 stdout；被拦请求只在 error_log 出现，是"请求没到后端"的直接证据。
3. **`/passthrough/` 对照 location**。不重启就能证明拦截来自插件，而不是后端自己的响应。
4. **契约内容与后端路由同源**。`/info` 里的路由声明（含分组展开共 36 条）与 demo_server 的接口/示例对应，1.5 接动态拉取时不用改测试数据。
5. **`/api/admin/users` 故意不进契约**。留着验证「未声明路由 → 404 + `route-not-declared`」（1.5 已生效）。
6. **`/phpinfo.php` 后端真的返回 200**。模拟"漏洞文件真实存在"，网关必须挡住；后端直连能拿到内容，形成对照。
7. **判定与适配分层**（1.5）。状态码、`Allow`、路由匹配都在 `gateway-core`，nginx 层只组装 `RequestCtx` 并写响应；核心逻辑可用 `cargo test` 覆盖。
8. **fail-closed**（1.5）。location 绑定了网关、但该网关的快照还没拉到时返回 503，绝不因为"没读到契约"而放行；拉取成功后自动恢复，不需要重启 nginx。

## 6. demo_server 接口清单

| 方法 | 路径 | 说明 | 在契约 |
| --- | --- | --- | --- |
| GET | `/info` | 写死的路由契约（version/routes） | — |
| GET | `/api/health` | 健康检查 | ✓ |
| POST | `/api/login` | `username`+`password` 必填；`remember`/`otp`/`redirect` 可选；支持 JSON 与 form；正确凭据 `demo/demo123` | ✓ |
| GET | `/api/user/{id}` | 路径参数回显 | ✓ |
| GET/PUT | `/api/user/{id}/profile` | 读取/更新资料 | ✓ |
| GET | `/api/products` | 列表 | ✓ |
| POST | `/api/orders` | 创建订单，回显 body | ✓ |
| POST | `/api/echo` | 原样返回 content-type/body | ✓ |
| GET | `/api/admin/users` | 故意未声明的管理接口 | ✗（1.5 起返回 404） |
| GET | `/phpinfo.php` | 模拟漏洞文件，直接返回 200 | ✗（网关按未声明路由 404） |

## 7. 验收命令

```bash
./scripts/build-nginx.sh      # 首次：下载并编译本地 nginx
./scripts/build-module.sh     # 编译 .so → nginx/modules/
./scripts/dev.sh up           # 起 demo_server(9000) + nginx(8080)
./tests/phase1.sh             # 524 项断言
./scripts/dev.sh down
```

手工观察四种状态：

```bash
# 放行（响应头有 x-demo-backend: 1）
curl -i http://127.0.0.1:8080/api/health

# 拒绝（404 + x-gateway-reason: route-not-declared；/phpinfo.php 不在契约里）
curl -i http://127.0.0.1:8080/phpinfo.php
tail -n 5 .run/demo_server.log

# 契约拒绝（404 + x-gateway-reason: route-not-declared；405 + Allow）
curl -i http://127.0.0.1:8080/api/admin/users
curl -i -X POST http://127.0.0.1:8080/api/health

# 开关关闭后透传（200，拿到后端内容）
curl -i http://127.0.0.1:8080/passthrough/phpinfo.php
```

fail-closed（冷启动只有网关、没有后端）：

```bash
./scripts/dev.sh down
./nginx/sbin/nginx -p "$PWD/nginx" -c conf/nginx.conf
curl -i http://127.0.0.1:8080/api/health   # 503 + contract-not-ready
./scripts/dev.sh up
curl -i http://127.0.0.1:8080/api/health   # 200（首次失败的后台重试已恢复）
```

## 8. 退出条件（已完成）

- [x] `nginx -t` 通过，worker 启动无 panic；
- [x] `curl` 能看到放行 / 404 / 405 / 400 / 503 / 透传等响应；
- [x] `error_log` 有插件日志，后端日志能证明被拦请求未到达；
- [x] 契约由具名网关的 `gateway_info_url` 实际拉取（日志 `[<name>] contract loaded from url ...`），未就绪时 fail-closed；
- [x] 契约也可由 `gateway_info_file` 从本地文件读取（文件模式，配置加载/reload 时读一次，后端被入侵也改不了规则）；
- [x] `tests/phase1.sh` 全绿，且脚本可重复执行；
- [x] 全过程无 root、无手动改系统配置。

## 9. 风险与遗留

| 项 | 现状 | 处理 |
| --- | --- | --- |
| `ngx` 依赖 git rev | 锁 `15719544`，crates.io 版本无所需 API | 升级前先 diff `examples/*.rs`；不要用 `cargo update` 盲升 |
| bindgen / syn 版本冲突 | 已锁 `prettyplease 0.3.0`（syn 3），否则 bindgen 编译失败 | 不要单独降级 prettyplease；`cargo tree -e build -p nginx-sys \| grep syn` 自检 |
| `.build/` 体积大 | 含 nginx 源码 + llvm 解包 | 可随时用 `build-nginx.sh`/`download llvm` 重建，不进版本库 |
| 每请求 `to_ascii_lowercase` | 演示足够 | 后续改成 `eq_ignore_ascii_case` 或预编译匹配表 |
| `panic = abort` | release 配置 | 符合"不把 panic 展开进 C"的预期；core 逻辑靠 196 项单测兜住 |
| 多 worker | 已改为 leader worker 单点获取 + 共享内存分发；leader 退出后 10s 内接管 | 需要更快接管时考虑 `NGX_PROCESS_HELPER` 专用进程 |
| `dev.sh` 残留进程 | 已按 `/proc/<pid>/exe` 认领并清理，`setsid --fork` 启动 | 若手工 `kill` 过进程，跑一次 `dev.sh down` 同步状态 |

## 10. 下一步（阶段 5 入口）

`gateway-core` 与契约管道已就绪，下一阶段只扩展判定维度：在 `Route` 上加 `query` / `body` / `headers` 规则，
在 `RequestCtx` 上带请求体与查询串，`Gateway::check` 返回统一的 `400` + 具体原因；
nginx 层负责收集小块请求体（限制大小、只对已声明路由生效），校验逻辑仍保持纯函数、可单测。
