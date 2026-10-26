# 开发进度与文档目录

最后更新：2026-10-26 · 当前：**阶段 1 + 阶段 1.5 已完成**（契约真正被读取并生效），下一步阶段 5（Query/Body/Header 校验）

## 1. 文档目录

| 文档 | 内容 |
| --- | --- |
| [../PLAN.md](../PLAN.md) | 总计划：架构约束、5 个阶段、里程碑、明确不做的范围 |
| [PROTOCOL.md](PROTOCOL.md) | **网关协议 v1（契约接口的唯一约定）**：字段、未声明路由策略、匹配/方法语义、拒绝响应、演进规则、草案 |
| [PHASE1_PLAN.md](PHASE1_PLAN.md) | 阶段 1 详细计划：环境结论、交付物、任务表、验收命令、风险 |
| [PROGRESS.md](PROGRESS.md) | 本文：当前进度、验证记录、常用命令、待办 |

代码索引：

| 位置 | 作用 |
| --- | --- |
| `crates/gateway-core/src/gateway.rs` | 纯逻辑：契约解析、matchit 路由树、404/405 判定（无 nginx 依赖） |
| `crates/gateway-core/src/contract.rs` | 契约 JSON 结构（v1：`protocol` / `version` / `ttl` / `meta` / `unmatched` / `routes[].path/methods`） |
| `crates/gateway-core/tests/contract_cases.rs` | 25 项核心单测（命中、方法、尾斜杠、重复路由、协议版本、`unmatched`、坏 JSON…） |
| `crates/ngx-gateway/src/lib.rs` | nginx 适配层：具名网关注册、每网关刷新线程、access 判定与响应头 |
| `demo_server/src/main.rs` | demo 后端：登录/用户/商品/订单 + 两份固定契约 `GET /info`、`GET /info_v2` |
| `deploy/nginx.conf` | 运行时 nginx 配置（源文件，脚本会拷到 `nginx/conf/`） |
| `scripts/build-nginx.sh` | 下载并编译本地 nginx 1.28.0 到 `nginx/` |
| `scripts/build-module.sh` | 编译 `.so` → `nginx/modules/libmy_gateway.so` |
| `scripts/dev.sh` | `up / down / restart / status / logs`（含孤儿进程清理） |
| `tests/phase1.sh` | curl 验收，**78 项断言** |

## 2. 阶段状态

| 阶段 | 内容 | 状态 |
| --- | --- | --- |
| 1 | 最小可行性 Demo（Rust 模块 + 本地 nginx + demo 后端 + curl） | ✅ 完成 |
| 1.5 | 配置真正生效：具名网关 + 契约拉取 + 404/405 | ✅ 完成（提前吃掉阶段 2–4 的核心） |
| 2 | 纯本地 TDD（`gateway-core`，脱离 nginx） | ✅ 完成（25 项单测） |
| 3 | 方法校验 + 契约动态拉取（TTL 线程 + 只读快照） | ✅ 完成 |
| 4 | 动态路由（matchit）+ 契约 v1 | ✅ 基础完成（严格匹配、`Allow` 回写） |
| 5 | Query / Body / Header 参数校验 | ⬜ 未开始 |

location 通过 `enable_gateway_by <网关名>` 选择网关，可写多条（与语义）：

```
写日志 → 内置探针（403 / 503）→ 绑定的每个网关依次判定 → 放行到后端
                               ├─ 该网关契约未加载成功 → 503 contract-not-ready
                               ├─ 路由未声明          → 404 route-not-declared
                               ├─ 方法不符            → 405 + Allow
                               └─ 全部网关放行        → 放行
```

拒绝时响应头 `X-Gateway-Name` 标明是哪个网关拦的（探针拦截不带）。

不写 `enable_gateway_by` 的 location 完全不参与判定（探针也不跑），`/passthrough/` 就是这种对照。

## 3. 配置形态

`deploy/nginx.conf`：

```nginx
http {
    # 网关注册表：声明名字 → 契约地址 → 可选 TTL（默认 60）
    gateway_name     myrustwebdemo1;
    gateway_info_url myrustwebdemo1 http://127.0.0.1:9000/info;
    gateway_ttl      myrustwebdemo1 3;

    # 第二个网关指向**另一份**契约：内容只有部分路由重叠
    gateway_name     myrustwebdemo2;
    gateway_info_url myrustwebdemo2 http://127.0.0.1:9000/info_v2;
    gateway_ttl      myrustwebdemo2 5;

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
| `http://127.0.0.1:9000/info` | `phase1-fixed` | `/api/login`、`/api/user/{id}`、`/api/user/{id}/profile` |
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
- **顺序要求**：`gateway_name` → `gateway_info_url` → 可选 `gateway_ttl`，且定义写在引用它的
  location 之前；未知名字、重复定义、缺 `gateway_info_url`、非法 TTL 都会被 `nginx -t` 拒绝；
- 每个被 location 引用的网关在 worker 里各起一个刷新线程 + 独立快照；只声明没引用的不拉取；
- 路径判定用**原始** request target（去掉 `?query`，不合并 `//`、不做百分号解码），
  nginx 的 `merge_slashes` 只影响 location 匹配；`//api/health` 与 `/api/health` 是两条不同路径；
- 未声明路由默认 `deny`（404）；契约写 `"unmatched": "forward"` 时改为转发给后端，
  已声明路由的方法校验仍然严格（405），详见 [PROTOCOL.md](PROTOCOL.md) 第 2.1 节；
- access 阶段只读对应网关的快照，不做网络 I/O；同一条消息（成功/失败）只在状态变化时写一次日志；
- 契约任一路由非法（不以 `/` 开头、尾斜杠、重复、空方法）→ 该网关整份契约拒绝加载，继续用上一份好快照；
- 后端挂掉但快照还在 → 继续用 last-known-good（请求会由 upstream 返回 502），不会把所有请求打成 503；
- worker 退出时（`exit_process`）先置停止标记再 join 所有刷新线程（sleep 50ms 粒度，最坏等一次 HTTP 超时），
  避免进程 teardown 与 detached 线程并发造成关闭期 `exited on signal 11`。

### 契约格式（协议 v1）

以 **[PROTOCOL.md](PROTOCOL.md)** 为唯一约定：`protocol` / `version` / `ttl` / `meta` / `unmatched` / `routes[].path|methods`。
当前解析是**严格模式**：

- `protocol` 不等于 `1`（插件支持的版本）→ 整份拒绝，日志提示升级插件或降低后端版本；
- 顶层或 `routes[]` 出现未定义字段（如提前写 `priority` / `query` / `responses`）→ 整份拒绝；
- `meta` 是自由 JSON，插件忽略，不需要递增协议版本。

demo 后端两份契约都按文档带 `meta` 且 `protocol: 1`：`/info` 是默认 `deny`，`/info_v2` 写了
`unmatched: "forward"`。插件日志分别输出
`[myrustwebdemo1] contract loaded protocol=1 version=phase1-fixed routes=8 unmatched=deny` 和
`[myrustwebdemo2] contract loaded protocol=1 version=phase1-forward routes=8 unmatched=forward`。

## 4. 验证记录

运行环境：Fedora 43 / Rust 1.96.0 / nginx 1.28.0（本地源码编译）/ `ngx` git rev `15719544`。

```
$ ./tests/phase1.sh
0. 连通性                         2/2
1. 正常业务请求（放行）            9/9
2. 登录接口（JSON/form）           5/5
3. 网关拦截                       7/7
4. 对照：证明拦截来自网关          4/4
5. 契约生效（未声明/方法/尾斜杠/重复斜杠） 16/16
6. 多网关：不同契约源             11/11
7. 未声明路由策略（deny/forward）  14/14
8. 日志                            5/5
9. 配置校验（nginx -t）             5/5
结果：78 通过，0 失败
```

手工复验过的关键行为：

| 场景 | 期望 | 实测 |
| --- | --- | --- |
| `GET /api/health` | 200 | 200 |
| `GET /api/admin/users`（未声明） | 404 + `X-Gateway-Reason: route-not-declared` + `X-Gateway-Name` | 一致 |
| `POST /api/health` | 405 + `Allow: GET` + `X-Gateway-Name` | 一致 |
| `GET /api/products/`（尾斜杠） | 404（严格匹配） | 一致 |
| `GET //api/health`（重复斜杠） | 404（按原始路径判定，不再落到后端） | 一致 |
| `GET /api/health?verbose=1` | 200（query 不参与匹配） | 一致 |
| `GET /phpinfo.php` | 403 + `X-Gateway-Blocked: php-suffix` | 一致 |
| `/passthrough/api/admin/users` | 200（未绑定网关） | 一致 |
| `POST /info`（绑定 demo1 + demo2） | 405 + `X-Gateway-Name: myrustwebdemo1` | 一致 |
| `GET /info_v2`（只绑 demo2） | 200，响应体 `version: phase1-forward` | 一致 |
| `GET /api/user/99`（`/info_v2` 契约未声明，且是 `forward`） | 200，交给后端（`x-demo-backend: 1`） | 一致 |
| `POST /api/metrics`（`/info_v2` 已声明 GET） | 405 + `Allow: GET` + `X-Gateway-Name: myrustwebdemo2` | 一致 |
| `GET /api/audit`（只在 `/info_v2`；同时绑两个网关） | 404 + `X-Gateway-Name: myrustwebdemo1`（`/info` 先判） | 一致 |
| `GET /api/reports/2026`（`/info_v2` 动态路由） | 200 + `"year":"2026"` | 一致 |
| **只起 nginx、后端未起** | 503 + `X-Gateway-Reason: contract-not-ready` | 一致 |
| 后端起来后（不重启 nginx） | 自动恢复 200 | 一致（TTL 3s 内） |

每次改动请重跑：

```bash
cargo test --offline -p gateway-core   # 纯逻辑
./scripts/build-module.sh && ./scripts/dev.sh restart
./tests/phase1.sh
```

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
curl -i http://127.0.0.1:8080/phpinfo.php                 # 403 php-suffix
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
curl -i http://127.0.0.1:8080/api/health                    # 200（TTL 内自动恢复）
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
- `ureq` 用 `default-features = false`（只要明文 HTTP，不带 TLS/gzip），契约地址目前只允许
  `http://`；要拉 `https://` 需重新开 TLS 特性。
- 建议不要 `cargo update`，升级前先确认 `examples/*.rs` 的 API 是否变化。

## 7. 待办（优先级顺序）

1. **阶段 5**：契约里加 `query` / `body` / `headers` 校验（先做 `required` + 类型/长度，错误统一 400 + `X-Gateway-Reason`）。
2. 把内置探针规则（`.php` / `.env` / `.git*` / `/wp-*`）改成数据驱动，作为契约的默认兜底规则。
3. 去掉每请求 `to_ascii_lowercase` 的分配（探针匹配用 `eq_ignore_ascii_case` 或预编译表）。
4. 多 worker 评估：当前每个 worker 各拉一份契约（N 个 worker = N 次 TTL 请求）；量大时改共享内存或 master 拉取。
5. 契约来源加固：可选签名校验 / 仅允许本机回环地址 / 拉取失败告警。

## 8. 已知限制

- 修改模块后必须 `dev.sh restart`，`nginx -s reload` 不会重新加载 `.so`。
- 冷启动时后端不可用 → 绑定了网关的 location 全站 503（这是刻意的 fail-closed）；不需要此行为就去掉
  对应的 `enable_gateway_by`（或删掉该网关定义）。
- 契约路径**严格匹配**：`/api/products/` 与 `/api/products` 不是同一条；契约里写尾斜杠会直接报错。
- `methods: []` 表示不限方法；`Allow` 头按契约里写的顺序输出。
- 当前拦截规则仍有阶段 1 的硬编码探针（在契约之前执行），不代表最终能力。
- `unmatched: "forward"` 是迁移期开关：未声明路径（含扫描器路径、拼错路径）都会交给后端，
  后端必须自己兜底 404；契约覆盖完整后建议改回默认的 `deny`。
- `dev.sh` 会清理本仓库路径下的 demo_server 残留进程（按 `/proc/<pid>/exe` 认领），不会动其他进程；
  `setsid --fork` 启动，终端退出不会留下半死状态。
- `.build/`、`target/`、`nginx/`、`.run/` 都是本地产物，重建成本低，不要提交到版本库。
