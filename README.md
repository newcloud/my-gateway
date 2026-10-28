# my_gateway

Rust 写的 **nginx 动态安全网关**：把后端声明的 API 契约变成 nginx 侧的强制校验规则。
请求在进入后端之前完成路由、方法、参数、过滤器和响应策略判定，**契约没写的就拒绝**。

```text
客户端 ──> nginx + libmy_gateway.so ──> 后端
                 │
                 ├─ 契约（JSON v1）：URL 启动时拉取或本地文件
                 ├─ 404 / 405 / 400 / 413 / 503（fail-closed）
                 └─ 诊断头 X-Gateway-*（仅测试模式）+ X-Request-Id（始终）
```

当前验证规模：

| 测试 | 规模 |
| --- | --- |
| `gateway-core` 纯逻辑单测 | 196 项 |
| `tests/phase1.sh` 主集成 | 524 项 |
| `tests/httpbin.sh` 公网 httpbin 演示 | 170 项 |
| `tests/tls-local.sh` 本地自签 HTTPS | 24 项 |

---

## 1. 功能

### 契约来源

- `gateway_info_url <name> <url>`：leader 启动/接管时拉取一次，成功才替换快照，失败保留上一份；
  之后不做周期轮询；修改配置后 `nginx -s reload` 会重新选主并全量拉取；
- `gateway_info_file <name> <path>`：`nginx -t` / 启动 / reload 时读一次，规则由发布流程控制；
- HTTPS 支持（ureq + rustls，公共根证书）；
- `gateway_info_url ... allow-localhost-ca`：仅限 `https://localhost` / `127.0.0.1` / `[::1]`，
  为本地自签证书关闭该网关的证书校验（生产不要用）；
- 不跟随重定向，只接受 `2xx` 契约响应；响应体上限 1 MiB（超限按拉取失败处理，保留旧快照）；
- 从未成功加载的网关直接 `503 contract-not-ready`（fail-closed）。

### 路由与判定

- 严格路径匹配（不合并 `//`、不忽略尾斜杠）；
- 路径参数 `{id}`、通配 `{*rest}`；
- 路由级 / 分组级 `path_regex`（AND）；
- `priority`：高优先级路由先匹配，同优先级保持静态 > 参数 > 通配；
- 分组 `prefix` / `router`，支持 `headers` / `methods` / `query` / `body` / `path_params` /
  `filters` 继承，`unmatched` 按最长前缀生效；
- 方法不符返回 `405` + `Allow`；
- 一个 location 可绑定多个网关（AND 语义，任一拒绝即拒绝）。

### 参数校验

四个维度：`headers` / `query` / `body` / `path_params`：

- `required`、`required_if`（字符串或 `{field,equals}`）；
- 类型：`char` / `int` / `bool` / `enum`；
- 长度：`min_len` / `max_len` / `len`；
- 范围：`min` / `max`；
- 枚举：`values` + `ignore_case`；
- 正则：`regex`（全匹配）；
- 未声明字段：`unknown: "ignore"` / `"reject"`（body 默认 reject）；
- Body：JSON 与 form，`content_types` 白名单，支持临时文件 body；
- `body.max_bytes`：原始 body 字节上限，分组继承、子级可收紧或写 `0` 关闭；超限在读 body 前返回 413。
- JSON 结构扫描：`max_depth`（默认 32）、`max_string_length`（默认 8192 字节）、
  `max_array_entries`（默认 500）；解析前单遍扫描，防深层嵌套/长字符串/大数组。

### 其他

- `filters`：SQLi / XSS，基于 `libinjectionrs`，可按顶层/分组/路由开启；
- `filters.scope`：限制只检查 `path` / `query` / `header` / `body` 中的若干维度，默认四个全开：

  ```json
  { "filters": { "sqli": true, "xss": true, "scope": ["query", "body"] } }
  ```

  `scope` 只对同一层开启的过滤器生效；跨层取并集，子层不能收窄父层已开启的维度。
- `responses`：覆盖 404/405/400/413/415 等状态码、body、`Retry-After`；
- **诊断头默认关闭**：`X-Gateway-Name` / `-Reason` / `-Field` 会泄露契约字段名和网关拓扑，
  只在 `gateway_diagnostics on`（测试配置）时返回；生产保持默认 `off`。所有拒绝响应**始终**带
  `X-Request-Id`（= nginx `$request_id`），`error_log` 每条判定日志以 `rid=<同一个值>` 结尾。
- `error_log` 记录 `reason=... field=... rule=... gateway=... rid=...`。

---

## 2. 快速开始

### 环境

- Linux（仓库在 Fedora 43 验证）；
- Rust stable；
- 编译 nginx 需要：`gcc`、`make`、`perl`、`pcre2-devel`、`zlib-devel`、`openssl-devel`；
- `curl`、`python3`（部分测试用）。

### 首次构建

```bash
# 1. 本地编译 nginx 1.28（下载源码，不装到系统，不需要 root）
./scripts/build-nginx.sh

# 2. 编译网关模块 -> nginx/modules/libmy_gateway.so
./scripts/build-module.sh

# 3. 启动 demo 后端 (:9000) + nginx (:8080)
./scripts/dev.sh up
```

> 只改了 Rust 代码时，执行 `./scripts/build-module.sh && ./scripts/dev.sh restart`。
> `nginx -s reload` 不会重新加载 `.so`。

### 看状态 / 日志 / 停止

```bash
./scripts/dev.sh status
./scripts/dev.sh logs
./scripts/dev.sh down
```

### 30 秒体验

```bash
# 放行
curl -i http://127.0.0.1:8080/api/health

# 未声明路由：网关自己拒绝，请求不会到后端
curl -i http://127.0.0.1:8080/api/admin/users

# 方法不符：405 + Allow
curl -i -X POST http://127.0.0.1:8080/api/health

# 缺少必填 Header：400 + 字段定位
curl -i http://127.0.0.1:8080/api/header-check

# 扫描器路径没有特殊规则：按契约未声明 → 404
curl -i http://127.0.0.1:8080/phpinfo.php
```

---

## 3. Demo 例子

主配置在 `deploy/nginx.conf`，运行时会复制到 `nginx/conf/nginx.conf`。
契约由 `demo_server` 的 `/info` 提供（URL 模式，默认 `deny`）。

### 3.1 路径 / 方法 / 参数

| 场景 | 命令 | 预期 |
| --- | --- | --- |
| 健康检查 | `curl -i localhost:8080/api/health` | 200 |
| 路径参数 | `curl -i localhost:8080/api/user/99` | 200 |
| 路径参数非法 | `curl -i localhost:8080/api/user/abc` | 400 `field=path.id` |
| 未声明路由 | `curl -i localhost:8080/nope` | 404 `route-not-declared` |
| 方法不符 | `curl -i -X POST localhost:8080/api/health` | 405 + `Allow: GET` |

### 3.2 Header / Query / Body

```bash
# Header 必填
curl -i localhost:8080/api/header-check
curl -i localhost:8080/api/header-check -H 'x-demo-token: demo-token'

# Header 值规则（长度 / int / enum / regex）
curl -i localhost:8080/api/header-value-check \
  -H 'x-demo-token: ab' -H 'x-demo-count: 10' \
  -H 'x-demo-mode: ALPHA' -H 'x-demo-code: AB12'

# Query：page 必填；mode=secure 时 token 条件必填
curl -i 'localhost:8080/api/query-check?page=1'
curl -i 'localhost:8080/api/query-check?page=1&mode=secure&token=ok'
curl -i 'localhost:8080/api/query-check?page=0'          # 400 min
curl -i 'localhost:8080/api/query-strict?page=1&extra=1' # 400 unknown

# Body：sku/qty 必填，未知字段拒绝
curl -i -X POST localhost:8080/api/body-check \
  -H 'Content-Type: application/json' \
  -d '{"sku":"A1","qty":2}'

curl -i -X POST localhost:8080/api/body-check \
  -H 'Content-Type: application/json' \
  -d '{"sku":"A1","qty":2,"is_admin":true}'   # 400 unknown

curl -i -X POST localhost:8080/api/body-check \
  -H 'Content-Type: text/plain' -d 'hello'    # 415（responses 覆盖）
```

### 3.3 分组与继承

```bash
# /api/v1 组级 authorization 必填；/me 是组内路由
curl -i localhost:8080/api/v1/me
curl -i localhost:8080/api/v1/me -H 'Authorization: Bearer demo-token-0001'

# 组级 path_params：org > 0
curl -i localhost:8080/api/org/7/profile
curl -i localhost:8080/api/org/0/profile   # 400 field=path.org

# 组级 filters：SQLi 拦截
curl -i 'localhost:8080/api/filter/search?kw=union+select+1'   # 400 filter=sqli
curl -i 'localhost:8080/api/filter/search?kw=hello'            # 200
```

### 3.4 路由正则 / priority

```bash
curl -i localhost:8080/api/route-regex/2026   # 200
curl -i localhost:8080/api/route-regex/12     # 404（正则不通过按未匹配处理）

# priority=10 的参数路由先于静态 /me
curl -i localhost:8080/api/priority/me        # 400 path.id
curl -i localhost:8080/api/priority/x         # 200

curl -i localhost:8080/phpinfo.php            # 404 + X-Gateway-Reason: route-not-declared
```

### 3.5 未声明策略 / 多网关

- `/info` 默认 `unmatched: "deny"`；
- `/info_v2` 为 `"forward"`，未声明路径交给后端；
- `location /` 绑定 `myrustwebdemo1`；`location = /info` 与 `/api/audit` 同时绑定两个网关（任一拒绝即拒绝）；
- `/passthrough/...` 不绑定网关，用于对照（完全不参与判定）。

```bash
curl -i localhost:8080/api/audit             # 404：demo1 未声明（先判）
curl -i localhost:8080/api/metrics           # 200：只在 demo2 的 /info_v2 声明
curl -i -X POST localhost:8080/api/metrics   # 405 + Allow: GET
curl -i localhost:8080/passthrough/api/admin/users  # 200：完全绕过网关
```

### 3.6 Body 大小上限

```bash
# /api/body-check 配置 max_bytes=64K；超过时读 body 前直接 413
python3 -c 'import json;print(json.dumps({"sku":"A1","qty":2,"note":"x"*100000}))' > /tmp/big.json
curl -i -X POST localhost:8080/api/body-check \
  -H 'Content-Type: application/json' --data-binary @/tmp/big.json
# 413 + X-Gateway-Reason: body-too-large + {"error":"body-too-large"}

# 分组继承：/api/limit 组级 2K；子级可收紧或关闭
curl -i -X POST localhost:8080/api/limit/inherit \
  -H 'Content-Type: application/json' -d '{"data":"'$(python3 -c 'print("x"*3000)')'"}'
# 413

curl -i -X POST localhost:8080/api/limit/disabled \
  -H 'Content-Type: application/json' -d '{"data":"'$(python3 -c 'print("x"*3000)')'"}'
# 200
```

建议值（原始字节，给 JSON 空白/转义留余量）：

| 场景 | 建议 `max_bytes` |
| --- | --- |
| 登录 / 注册 JSON | 1–4 KiB |
| 简单表单 | 4–16 KiB |
| 普通 CRUD JSON | 16–64 KiB |
| 批量 / 富文本 | 64–256 KiB |
| 文件上传 | 不设，交给 nginx `client_max_body_size` |

JSON body 还会在解析前做一次结构扫描，默认：

```json
{
  "max_depth": 32,
  "max_string_length": 8192,
  "max_array_entries": 500
}
```

超过时返回 `400`，日志 `rule=json-depth` / `json-string-length` / `json-array-entries`；
三个值都可以在 `body` 块里调整，写 `0` 关闭（`max_depth` 仍有 128 层硬上限）。
base64/大文本字段要显式调大 `max_string_length`。

扫描在 `serde_json` 解析之前执行：单遍遍历、固定 128 层栈、无堆分配，字符串内的括号和
`\"` 会被正确忽略。它只负责挡结构异常，不是 JSON 校验器；语法错误仍由 serde 返回 400。

---

## 4. 契约长什么样

协议是 `protocol: 1`，完整字段与语义见 [docs/PROTOCOL.md](docs/PROTOCOL.md)。
**严格模式**：出现未知字段、未实现字段、非法路由/规则，整份契约拒绝加载，不会“部分生效”。

### 最小契约

```json
{
  "protocol": 1,
  "version": "2026-10-26",
  "unmatched": "deny",
  "routes": [
    { "path": "/api/health", "methods": ["GET"] }
  ]
}
```

### 带参数与分组

```json
{
  "protocol": 1,
  "version": "2026-10-26",
  "unmatched": "deny",
  "responses": {
    "route_not_declared": { "status": 404, "body": "{\"error\":\"route-denied\"}" },
    "method_not_allowed": { "status": 405, "body": "{\"error\":\"method-denied\"}" }
  },
  "routes": [
    {
      "path": "/api/orders/{id}",
      "methods": ["GET"],
      "path_params": { "params": { "id": { "type": "int", "min": 1 } } },
      "headers": {
        "params": {
          "authorization": { "required": true, "type": "char", "min_len": 8 }
        }
      }
    },
    {
      "path": "/api/feedback",
      "methods": ["POST"],
      "body": {
        "unknown": "reject",
        "content_types": ["application/json"],
        "params": {
          "rating": { "required": true, "type": "int", "min": 1, "max": 5 },
          "topic": { "type": "enum", "values": ["bug", "feature"] },
          "comment": {
            "required_if": { "field": "topic", "equals": "bug" },
            "type": "char",
            "max_len": 200
          }
        }
      }
    },
    {
      "prefix": "/api/v1",
      "methods": ["GET", "POST"],
      "headers": {
        "params": {
          "authorization": { "required": true, "type": "char", "min_len": 8 }
        }
      },
      "query": {
        "params": { "trace": { "type": "char", "max_len": 8 } }
      },
      "router": [
        { "path": "/me", "methods": ["GET"] },
        {
          "path": "/orders/{id}",
          "methods": ["GET"],
          "path_params": { "params": { "id": { "type": "int", "min": 1 } } }
        }
      ]
    }
  ]
}
```

可以直接参考：

- `deploy/httpbin/contract.json`：可运行的完整示例（分组/继承/priority/正则/过滤器）；
- `demo_server/src/main.rs` 的 `info()`：demo 后端实际返回的契约；
- `docs/PROTOCOL.md` §9：更多示例。

---

## 5. 如何生成“契约接口”

契约就是一个返回 JSON 的接口，通常放在后端 `GET /info`；也可以落成本地文件。
推荐流程：

### 5.1 选择来源

| 方式 | 配置 | 适合 |
| --- | --- | --- |
| 后端 `/info` | `gateway_info_url gw http://backend/info;` | 契约随服务版本更新（reload 后生效） |
| 本地文件 | `gateway_info_file gw conf/contracts/web.json;` | 规则不能由被保护后端决定（更安全） |

nginx 侧示例：

```nginx
gateway_name      mygw;
gateway_info_url  mygw http://127.0.0.1:9000/info;

server {
    listen 8080;
    location / {
        enable_gateway_by mygw;
        proxy_pass http://backend;
    }
}
```

改本地文件后需要 `nginx -t && nginx -s reload`。

### 5.2 先列出真实路由

生成契约前先准备：

- 每条路由的路径、方法；
- 路径参数（`/orders/{id}` 的 `id` 类型/范围）；
- 必填 Header / Query / Body 字段与类型、长度、枚举、正则；
- 哪些路径允许未声明（`unmatched: "forward"`），哪些必须拒绝；
- 是否需要 SQLi/XSS 过滤。

### 5.3 让 AI Agent 直接生成

把 `docs/PROTOCOL.md` 和真实路由清单交给 agent，让它输出契约 JSON。
可直接复制下面的提示词：

```text
你是 my_gateway 的契约生成助手。

先阅读：
- docs/PROTOCOL.md（协议 v1 与“已实现/未实现”状态）
- deploy/httpbin/contract.json（完整可运行示例）
- demo_server/src/main.rs（/info 实际契约）

我的后端路由/参数如下：
<在这里粘贴 OpenAPI、路由表或控制器代码>

请生成一份 protocol=1 的契约 JSON，要求：
1. 只使用 docs/PROTOCOL.md 中标记为已实现的字段；出现未实现/未知字段会被整份拒绝；
2. 顶层 unmatched 使用 "deny"；确实需要放行未声明路径的分组再单独写 "forward"；
3. 所有写接口：body.unknown = "reject"，并声明 content_types；
4. 路径参数写在 path_params.params，规则名必须出现在路由 path 里；
5. 有登录要求的接口用 headers 的 authorization 必填 + min_len；
6. 需要防注入的读接口打开 filters.sqli / filters.xss；
7. 同一路径不要重复声明；分组 prefix/router 的完整路径要能拼出真实路由；
8. 输出仅 JSON，不要解释；生成后再用 python3 -m json.tool 自检。

不要修改仓库里的其他文件。
```

生成后：

```bash
# JSON 语法
python3 -m json.tool /path/to/contract.json >/dev/null && echo ok

# 放进本地文件模式并校验契约（会拒绝未知字段/非法规则）
# 先在 nginx.conf 里配置 gateway_name / gateway_info_file（见 5.1）
cp /path/to/contract.json nginx/conf/contracts/web.json
./nginx/sbin/nginx -p nginx -c conf/nginx.conf -t

# 或让后端 /info 返回它，nginx -s reload 后看日志
grep 'contract loaded' nginx/logs/error.log
```

> 契约是**安全边界**，AI 生成后必须人工复核：尤其是 `unmatched`、`unknown`、
> `authorization`、路径参数范围和 `responses`。不要因为“测试能跑”就直接上线。

### 5.4 验收契约

```bash
# 1. 正常请求必须放行
curl -i http://127.0.0.1:8080/<声明的路由>

# 2. 未声明路由 / 方法不符 / 缺参数必须被网关拒绝，且请求未到后端
curl -i http://127.0.0.1:8080/not-declared
curl -i -X POST http://127.0.0.1:8080/<只允许 GET 的路由>
curl -i http://127.0.0.1:8080/<缺必填参数的路由>

# 3. 看 error_log 的 reason/field/rule 是否稳定
tail -f nginx/logs/error.log
```

拒绝响应会带（`X-Gateway-*` 只在测试配置 `gateway_diagnostics on` 时出现）：

```text
X-Request-Id: 6c46c02f02064ef99296c5ea8a2c1242          # 始终返回；= nginx $request_id
Allow: GET                                              # 405 协议头，始终返回

# 以下仅 gateway_diagnostics on（测试环境）：
X-Gateway-Name: mygw
X-Gateway-Reason: missing-parameter | invalid-parameter | route-not-declared |
                  method-not-allowed | unsupported-media-type | contract-not-ready
X-Gateway-Field: query.page / body.age / header.authorization / path.id
```

上线前确认生产配置里**没有** `gateway_diagnostics on;`（或显式写 `off`）；打开时 `nginx -t` 会打 WARN。
诊断关闭后，用用户提供的 `X-Request-Id` 去 `error_log` 搜 `rid=` 就能拿到完整的 `reason/field/rule`。
本地 `deploy/nginx.conf` 是测试配置，所以默认打开。

---

## 6. 运行测试

```bash
# 纯逻辑（不需要 nginx）
cargo test --offline -p gateway-core          # 196

# 主集成（需要 ./scripts/dev.sh up）
./tests/phase1.sh                             # 524

# 公网 httpbin 演示实例（需要能访问 https://httpbin.org）
./tests/httpbin.sh                            # 170

# 本地自签 HTTPS 契约源（离线）
./tests/tls-local.sh                          # 24
```

---

## 7. 项目结构

```text
crates/gateway-core/       纯逻辑：契约解析、路由/分组展开、参数/过滤判定
crates/ngx-gateway/        nginx 适配：配置指令、统一契约管理器（共享内存分发）、access handler、响应头
demo_server/               demo 后端 + GET /info、/info_v2
deploy/nginx.conf          主运行配置
deploy/httpbin/            httpbin 独立演示（本地契约 + 一个 server）
tests/                     curl 集成验收
docs/PROTOCOL.md           协议与实现状态（契约字段以此为准）
docs/PROGRESS.md           验证记录与已知限制
```

---

## 8. 安全模型与已知限制

- **fail-closed**：契约没加载成功 → 503；路由没声明 → 404；参数不合法 → 400；
  任何拒绝都不会到后端。
- **URL 模式的信任前提**是“后端返回的就是规则”。后端被攻陷时可以改 `/info`；
  高安全场景请用 `gateway_info_file`，文件对后端进程只读，发布时原子替换 + reload。
- 本地自签 HTTPS 使用 `allow-localhost-ca`，只允许本机地址，且会在 `nginx -t` 打 WARN。
- 尚未完全覆盖：
  - `responses.contract_not_ready`：契约没加载时没有快照可读；
  - 契约加载/重试由 leader worker 负责，leader 退出后要等心跳超时（10s）才会接管；
    这段时间里已有快照继续可用，只是失败重试暂时停止；
  - nginx 在插件前预检 413 时无法补 `X-Gateway-*`，也不会有 `X-Request-Id`（请求还没进插件）；
  - `X-Request-Id` 只加在网关自己生成的响应上；上游 `proxy_pass` 的响应不加，避免和业务自己的请求 ID 撞车；
  - 诊断头受 `gateway_diagnostics` 控制（默认 off）；关闭后 `field/rule` 只在 `error_log` 里，
    若线上 `error_log` 级别高于 `notice` 就查不到，暂未提供 `gateway_log_level` 指令；
  - `HEAD` 语义未定；
- 每次改模块后必须 `./scripts/dev.sh restart`，`reload` 不会重载 `.so`。

---

## 9. 文档

- [docs/PROTOCOL.md](docs/PROTOCOL.md)：协议 v1、判定顺序、字段、示例、变更记录
- [docs/PROGRESS.md](docs/PROGRESS.md)：实现进度、验证结果、已知限制
- [docs/ISSUES.md](docs/ISSUES.md)：问题 / 未实现能力清单（契约体积、多级数组与嵌套 JSON、etcd 等）
- [docs/AI_PROPOSALS.md](docs/AI_PROPOSALS.md)：AI 提议清单（P-04 主动刷新接口已确认）
- [docs/REFRESH_ENDPOINT_DESIGN.md](docs/REFRESH_ENDPOINT_DESIGN.md)：`/_gateway_refresh` 设计（待实现）
- [docs/PROTOCOL_TEST_PLAN.md](docs/PROTOCOL_TEST_PLAN.md)：core 用例清单与测试写法
- [docs/CONTRACT_UNIFICATION_DESIGN.md](docs/CONTRACT_UNIFICATION_DESIGN.md)：统一契约管理器（单点获取 + 共享内存分发）
- [docs/CLIENT_SOLUTION.md](docs/CLIENT_SOLUTION.md)：**客户方案**（政企版，市场/场景/实施/POC 验收，可直接对外）
- [docs/PRODUCT.md](docs/PRODUCT.md)：产品化方向（内部：定位、竞品、差距与路线图）
- [PLAN.md](PLAN.md) / [docs/PHASE1_PLAN.md](docs/PHASE1_PLAN.md)：阶段计划
- [AGENTS.md](AGENTS.md)：开发约定（协议先行、先 core 测试、再接 nginx）

---

## 10. 许可证

[Apache License 2.0](LICENSE)，Copyright 2026 newcloud。

- 仓库内源码（`gateway-core` / `ngx-gateway` / `demo_server` / `scripts` / `tests` / `docs`）均适用该许可证；
- 本项目以动态模块形式加载进 nginx，**不包含也不分发 nginx 源码**（`nginx/` 由 `scripts/build-nginx.sh`
  在本机生成，已被 `.gitignore` 忽略）；nginx 自身为 BSD-2-Clause；
- 若对外分发自带编译 nginx 的二进制包，需一并附带 nginx 的 `LICENSE`（BSD-2-Clause）以及
  `libinjectionrs`（BSD-3-Clause）等第三方依赖的许可声明。
