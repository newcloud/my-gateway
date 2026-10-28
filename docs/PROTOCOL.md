# 网关协议 v1（契约接口）

> 本文是**唯一约定**：后端生成 `GET /info` 的返回值、插件解析契约、以及拒绝请求时的行为，都以本文为准。
> 代码对应：`crates/gateway-core/src/contract.rs`（结构）、`crates/gateway-core/src/gateway.rs`（校验与判定）、
> `crates/ngx-gateway/src/lib.rs`（契约来源拉取/读文件、响应）、`demo_server/src/main.rs`（参考实现）。

## 0. 状态与版本

### 0.1 实现状态

项目仍在开发中，协议只有 v1：加字段直接改本版本，不递增版本号。

| 能力 | 协议条款 | 插件状态 |
| --- | --- | --- |
| 契约结构、严格解析、`unmatched` | 第 0–2 节 | ✅ 已实现 |
| 路径匹配、方法匹配 | 第 3.1–3.2 节 | ✅ 已实现（路径语法、方法 token 均在加载期校验） |
| 路径参数提取与值校验（`path_params`） | 第 3.3、5.4 节 | ✅ 已实现（原始路径值） |
| Header / Query / Body 参数校验：存在性、`required_if`、值规则、`unknown`、Body 媒体类型 | 第 5.0–5.3 节 | ✅ 已实现 |
| 规则简写 `true` / `required_if` / 字段 `regex` | 第 5.0–5.0.2 节 | ✅ 三个维度均已实现 |
| 分组路由：`prefix` / `router` / `headers` / `methods` / `query` / `body` / `path_params` 继承 / `unmatched` / 分组 `path_regex` | 第 3.4–3.5 节 | ✅ 已实现 |
| 分组 `filters` 继承（或语义） | 第 3.4.2 节 | ✅ 已实现 |
| 路由级 `path_regex` / 路由 `priority` | 第 3.5、3.2 节 | ✅ 已实现 |
| `filters`（SQLi / XSS，按维度过滤；第三方 libinjectionrs） | 第 5.9 节 | ✅ 已实现 |
| JSON 结构扫描：`max_depth` / `max_string_length` / `max_array_entries` | 第 5.2 节 | ✅ 已实现 |
| 响应覆盖 `responses` | 第 6 节 | ✅ 已实现（当前 core 拒绝类响应） |
| 契约拉取不跟随重定向 | 第 1 节 | ✅ 已实现 |
| 诊断响应头开关 `gateway_diagnostics`（默认 off）与 `X-Request-Id` 关联 | 第 4.2–4.3 节 | ✅ 已实现 |

> **🧪 的含义**：条款已经定稿，但插件尚未实现。实现前如果这些字段出现在契约里，**整份契约会被拒绝加载**
> （严格模式 + fail-closed）。这是刻意的：避免后端“以为参数已经校验了，其实没有”。

> **Header 切片的边界（2026-10-26）**：实现分三批，当前都已接入 nginx：
> - 存在性：`headers.params.<name>: true` 或 `{ "required": true }` → 缺失返回 `400 missing-parameter`；
>   `{ "required": false }` → 显式可选；同一个声明头出现多行 → `400 invalid-parameter`。
> - 值规则第一批：`type`（`char` / `int` / `bool` / `enum`）、`min` / `max`、`min_len` / `max_len` / `len`、
>   `values` / `ignore_case`、`regex`（全匹配）。
> - 条件必填：`required_if` 的字符串形式与 `{ "field", "equals" }` 对象形式。
>
> **已实现范围**：分组 `methods` / `query` / `body` / `path_params` / `filters` 继承、路由级
> `path_regex` / `priority`、`responses` 的 `body_too_large`（插件读到 413 时）。
> **仍未实现**：`responses.contract_not_ready`（契约未加载时没有快照可读）；nginx 在插件前
> 预检 413 时仍可能没有网关头，也无法应用 `responses.body_too_large`。
>
> **Query / Body 边界（2026-10-26）**：
> - Query：`params`（`required` / `required_if` / 值规则）、`unknown`（默认 `ignore`），解析原始 query；
> - Body：`params`、`unknown`（默认 `reject`）、`content_types`（默认 JSON + form），支持 JSON 与 form；
> - Body 在 access 阶段读取；支持内存 buffer 与 nginx 临时文件（按 `temp_file->offset` 读回）；
>   不自动解压 `Content-Encoding`。

### 0.2 演进规则

| 概念 | 字段 | 含义 |
| --- | --- | --- |
| 协议版本 | 顶层 `protocol` | 契约**结构**的版本，整数。插件只接受自己支持的版本 |
| 内容版本 | 顶层 `version` | 后端契约的**内容修订号**，仅用于日志/排查，不参与解析 |

当前插件只支持 `protocol = 1`。演进规则：

1. **开发期不递增版本**：契约还没正式对外使用，加字段、改语义都直接改 v1；不存在需要兼容的旧契约。
2. **正式上线后**，只有需要同时兼容旧契约（新旧插件/后端混跑）时才递增 `protocol`，
   并明确旧版本在新插件里的行为（通常整份拒绝或按旧语义解析）。
3. 插件看到不支持的 `protocol` 会**拒绝整份契约**（不会“读一半”）。
4. **纯信息字段**放进顶层 `meta`（自由 JSON 对象），插件必须忽略 `meta`。
5. 解析是**严格模式**：顶层、`routes[]`、分组对象、以及参数规则内出现未定义的字段，整份契约拒绝加载，日志写明字段名。
   这样后端误发字段时会立刻暴露，而不是被静默忽略造成“以为生效了”。
6. 新增字段优先设计成**可选**：省略时行为保持合理默认值。
7. `routes: []` 合法，语义是“拒绝所有未在契约中声明的路由”（即全站 404，`/passthrough/` 之类关掉网关的 location 除外）。
8. **参数校验字段缺省 = 不校验**：没有写 `query` / `body` / `headers` / `path_params` 的路由，行为与阶段 4 完全一致，
   网关只做路径与方法判定。这条是参数校验上线时的回归底线。

---

## 1. 契约来源与传输

每个网关**必须且只能**有一个契约来源：`gateway_info_url`（HTTP 拉取）或 `gateway_info_file`
（本地文件）。两者取哪个是**信任模型的选择**，不只是部署方便与否，见 1.1。

| 项 | `gateway_info_url`（HTTP） | `gateway_info_file`（本地文件） |
| --- | --- | --- |
| 指令 | `gateway_info_url <网关名> <url> [allow-localhost-ca]`，必须是 `http://` 或 `https://`；HTTPS 默认用 `rustls` + `webpki-roots` 验证公共根证书 | `gateway_info_file <网关名> <path>`，绝对路径或相对 nginx prefix（`-p`）的路径 |
| 方法 | `GET` | 直接读本地文件（UTF-8 JSON） |
| 成功条件 | `200` + JSON 对象；建议 `Content-Type: application/json` | 文件可读且 JSON 合法 |
| 超时 | 单次 HTTP 请求 3s（`timeout_global`） | 无网络 |
| 读取时机 | **唯一的 leader worker**（共享内存选主）在启动/接管时拉一次，成功后写入共享内存，其余 worker 只读；**成功才替换**快照；之后不做周期轮询，更新由 `nginx -s reload` 触发 | **配置加载时读一次**（`nginx -t` / 启动 / `nginx -s reload`），与 `nginx.conf` 自身完全一致；改文件必须 reload |
| 失败处理 | 保留上一份成功契约（last-known-good）；从未成功过则绑定请求 `503 contract-not-ready`，后台每 2s 重试直到成功 | **拒绝本次配置**（`NGX_LOG_EMERG`）：启动失败，reload 失败则旧 worker 继续用旧契约 |
| 日志 | 成功/失败同一消息只在状态变化时写一条（带网关名前缀） | 加载成功记一条 `contract loaded from file ...`；失败直接进配置错误 |
| 更新触发 | 启动/接管时拉一次；之后由 `nginx -s reload` 触发（重新选主并全量拉取） | 改文件后 `nginx -s reload` |
| 重定向 / 状态码 | 只接受 `2xx`；**不跟随** `30x`，`30x`/`4xx`/`5xx` 均视为拉取失败（`bad-status`），保留旧快照 | 不适用 |

另外有一个与来源无关的 http 级开关：`gateway_diagnostics on|off`（默认 `off`），只控制
`X-Gateway-*` 诊断响应头是否回写给客户端，见 4.2。

### 1.1 为什么要有文件模式（安全原因，别忽略）

URL 模式的隐含前提是「**后端说的就是规则**」。后端一旦被拿下（RCE、配置被改、依赖被投毒、
供应链污染），攻击者只要改自己的 `/info` 返回值，下一次契约更新时就能：

- 给自己加路由、加方法（把本该 405 的接口打开）；
- 把 `unmatched` 改成 `forward`（未声明路径全部放行）；
- 静默删掉敏感路由的约束。

这些改动**完全合规**——插件会认为这是一次正常的契约更新。也就是说 URL 模式下，被攻陷的后端
可以把网关从"边界"变成"放大器"。

文件模式把信任根从「后端 HTTP 响应」移到「本地文件 + 文件权限」：

| 来源 | 谁能改规则 | 适合 |
| --- | --- | --- |
| `gateway_info_url` | **后端进程**（以及任何能劫持这条 HTTP 的人） | 契约由服务自己生成、与网关同属一个信任域 |
| `gateway_info_file` | **只有能写这个文件的人**（发布流程 / 运维 / root） | 不允许后端决定网关规则；等保、金融、多租户等要求"规则与被保护对象分离"的场景 |

代价：规则变更要走发布流程（写文件 + `nginx -s reload`），不再能"后端一改就生效"。
这也正是文件模式的**好处**：任何规则变化都会在 `error_log` 留下一次配置加载记录，可审计。

### 1.2 使用文件模式时必须注意（开发/运维清单）

1. **权限才是边界。** 契约文件应对后端进程**只读**：
   `chown root:<nginx 运行用户> contract.json && chmod 0640`，所在目录也不能被后端用户写。
   如果后端和 nginx 跑在同一用户下（本仓库 dev 模式就是），文件模式**不提供任何隔离**，
   只是验证了"从文件读契约"这条路径。
2. **原子替换发布。** 先写临时文件，再 `mv` 到目标路径（同目录、同文件系统），然后
   `nginx -t && nginx -s reload`。直接 `>` 截断写入会留下"半截 JSON"窗口——现在这个窗口
   不会让线上放行，但会让 reload 失败（见第 4 条）。
3. **路径规则**：绝对路径，或相对 **nginx prefix**（`-p`，默认编译 prefix）的路径，例如
   `conf/contracts/web.json`。注意不是相对 conf 文件所在目录（那是 `include` 的规则）。
4. **没有 TTL，改文件必须 reload。** 文件在**配置加载时**读一次（`nginx -t` / 启动 / reload），
   与 `nginx.conf` 的读取方式完全一样。好处是语义简单、规则变化可审计；代价是改完不 reload 不生效。
5. **读不到或 JSON 非法 → 拒绝本次配置**（`NGX_LOG_EMERG`）：
   - 启动时：nginx 起不来（不会带着空规则上线）；
   - reload 时：新配置被丢弃，**旧 worker 继续用旧契约**，线上不受影响。
   这跟写错 `nginx.conf` 的后果一致，符合"契约 = 配置"的定位。
6. **`gateway_url_ttl` 已移除**：旧配置里再写这个指令，`nginx -t` 会报
   `unknown directive "gateway_url_ttl"`，不会静默忽略。
7. **一个网关不能同时配两种来源**，`nginx -t` 会拒绝（报 `already has a contract source`）。
8. **两者可以在同一份配置里混用**：A 网关走 URL（启动时拉取，reload 更新）、B 网关走文件
   （reload 生效），各自独立。
9. **文件内容是同一份协议 v1 JSON**，没有额外包装。

### 1.3 契约拉取不跟随重定向（已实现）

契约地址是配置里写死的信任点。`gateway_info_url` 拉取时：

- 不跟随 `30x`（`max_redirects(0)`），避免被重定向带到任意主机；
- 只接受 `2xx`；`30x` / `4xx` / `5xx` 一律视为拉取失败，错误信息带 `bad-status`；
- 失败时保留上一份成功契约；从未成功过则继续 `503 contract-not-ready`。

### 1.4 本地自签证书（仅限 localhost）

开发/联调时契约源可能是本地自签 HTTPS。可以在 `gateway_info_url` 的 URL 后加第三个参数：

```nginx
gateway_info_url gw https://127.0.0.1:8443/info allow-localhost-ca;
```

规则：

- 只对 `https://` 生效；`http://` 加这个参数会被 `nginx -t` 拒绝；
- host 必须是 `localhost` / `127.0.0.1` / `[::1]`；其它地址（包括内网域名）一律拒绝；
- 打开后**该网关**的 TLS 证书校验被关闭（链、主机名都不校验），`nginx -t` 会输出 WARN；
- 只影响契约拉取，不影响 `proxy_pass` 到后端时的 TLS 校验；
- 生产环境不要使用；优先使用公共 CA，或改用 `gateway_info_file` 把规则放在 nginx 侧。

未知的第三个参数同样在 `nginx -t` 阶段拒绝。

`/info` 本身也应出现在 `routes` 里（后端自查用），是否放行由契约决定。

demo_server 同时提供 `/info`（`version: phase1-fixed`，默认 `deny`）与 `/info_v2`
（`version: phase1-forward`，`unmatched: "forward"`），两份内容部分重叠，用来让 nginx 里的
两个网关指向不同地址、验证各自独立的快照与不同的未声明策略；两者 `protocol` 都是 `1`。

---

## 2. 顶层结构（v1）

```json
{
  "protocol": 1,
  "version": "2026-10-26T12:00:00Z",
  "meta": { "owner": "user-service", "generated_by": "build-42" },
  "routes": [
    { "path": "/api/health", "methods": ["GET"] },
    { "path": "/api/login",  "methods": ["POST"] }
  ]
}
```

| 字段 | 类型 | 必填 | 默认 | 说明 |
| --- | --- | --- | --- | --- |
| `protocol` | int | 否 | `1` | 不支持的版本 → 拒绝契约 |
| `version` | string | 否 | — | 内容修订号，仅进日志 |
| `meta` | object | 否 | — | 自由信息，插件忽略 |
| `unmatched` | string | 否 | `"deny"` | 未声明路由策略：`deny` / `forward`，见 2.1 |
| `filters` | object | 否 | — | 全局字段过滤开关（`sqli` / `xss`），见 5.9；可被分组/路由继续开启 |
| `routes` | array | 否 | `[]` | 路由或分组声明，见第 3 节；省略等价于空数组（默认拒绝所有未声明路径）；每个元素只能是路由（`path`）或分组（`prefix`）之一 |
| 其他字段 | — | — | — | **拒绝整份契约** |

### 2.1 未声明路由策略

有些后端希望“契约只写重点路由，其余路径仍交给后端自己处理”（迁移期、旧客户端兼容）。
`unmatched` 就是控制这个行为的开关：

| 值 | 行为 |
| --- | --- |
| `"deny"`（默认，可省略） | 路径未在 `routes` 中声明 → 网关直接 `404 route-not-declared`，请求不会到后端 |
| `"forward"` | 路径未在 `routes` 中声明 → 交给后端（与命中路由一样放行），网关只写一条 notice 日志 |

```json
{
  "protocol": 1,
  "unmatched": "forward",
  "routes": [
    { "path": "/api/health", "methods": ["GET"] }
  ]
}
```

约束与注意：

- **只影响“路径未声明”**：已经声明的路由，方法不符仍然是 `405 + Allow`，不会转发。
  命中候选路由但 `path_regex` 不通过（🧪）也算“路径未声明”，按本节的 `unmatched` 处理。
- **分组可以覆盖**：分组上的 `unmatched` 只作用于该前缀范围，按 3.4.3 的“最长前缀 + 最近祖先”选取；
  分组的 `forward` 不会影响其他前缀。
- **未声明路径不做参数校验**：既然没有路由声明，就没有 query/body/header 规则可校验；`forward` 一律原样交给后端，
  `filters` 也不会生效（见 5.9）。
- 其他值（如 `"allow"`）→ JSON 解析失败 → 整份契约拒绝。
- 安全提示：`forward` 会把扫描器路径、拼错路径也交给后端，后端必须自己兜底（返回 404 而不是泄漏）。
  它定位是**迁移期开关**，不是长期默认值；契约覆盖完整后建议改回 `deny`。

---

## 3. 路由与分组对象

`routes` 数组的元素有两种：

- **路由**：必须有 `path`，不能有 `prefix` / `router`；
- **分组**：必须有 `prefix`，不能有 `path`；用 `router` 组织子路由/子分组。

分组本身不产生可访问的路由，只把前缀和公共检查下发给后代；展开规则见 3.4。直接写路由的形态保持不变。

路由对象示例：

```json
{
  "path": "/api/orders/{order_id}",
  "methods": ["POST"],
  "path_regex": "^/api/orders/[0-9]+$",
  "filters": { "sqli": true, "xss": true },
  "path_params": {
    "params": {
      "order_id": { "type": "int", "min": 1 }
    }
  },
  "query": {
    "params": {
      "dry_run": { "type": "bool" }
    },
    "unknown": "ignore"
  },
  "body": {
    "params": {
      "sku":  { "type": "char", "required": true, "max_len": 64 },
      "qty":  { "type": "int", "required": true, "min": 1, "max": 99 },
      "note": { "type": "char", "max_len": 200 }
    },
    "unknown": "reject",
    "content_types": ["application/json"]
  },
  "headers": {
    "params": {
      "x-api-version": { "type": "enum", "values": ["1", "2"], "required": true }
    }
  }
}
```

| 字段 | 类型 | 必填 | 默认 | 说明 |
| --- | --- | --- | --- | --- |
| `path` | string | 是 | — | 以 `/` 开头；不允许查询串、`#`、尾斜杠（`/` 本身除外），见 3.1 |
| `methods` | string[] | 否 | `[]` | 允许的方法；**空数组 = 不限方法**；元素不能是空串，见 3.2 |
| `path_regex` | string | 否 | — | 🧪 附加路径正则，见 3.5；与 `path` 同时满足才命中 |
| `filters` | object | 否 | — | 字段过滤开关（`sqli` / `xss`），见 5.9 |
| `path_params` | object | 否 | — | 路径参数值校验，见 3.3 与 5.4 |
| `query` | object | 否 | — | Query 参数校验，见 5.1 |
| `body` | object | 否 | — | Body 参数校验，见 5.2 |
| `headers` | object | 否 | — | Header 参数校验（存在性 / 值规则 / `required_if`），见 5.3 |
| `priority` | i32 | 否 | `0` | 越大越先匹配；同优先级内仍按具体度 |
| 其他字段 | — | — | — | **拒绝整份契约** |

`unmatched` 只允许出现在**顶层**和**分组**上；路由对象里写 `unmatched`（或 `router` / `prefix`）→ 整份拒绝。

四个参数块（`path_params` / `query` / `body` / `headers`）的公共约定：

- **块存在 = 开启该类校验；块省略 = 完全不看该类输入**（不解析、不读取，原样转发）。
- 块内结构统一为 `{"params": { "<名字>": <值规则> }, ...选项}`；选项见第 5 节。
- 参数块也可以写在**分组**上，由后代继承；同名规则跨层重复会拒绝加载，见 3.4.2。
- 规则只做**拒绝**，网关不会改写、补默认值或重新编码转发给后端。
- 参数规则的目标是“契约声明与后端实际接受的一致”，不是替代后端业务校验。

### 3.1 路径匹配

#### 3.1.1 判定输入

路径取自**原始 request target** 的 path 部分：

- 去掉第一个 `?` 及其后的 query；query 不参与路由匹配。
- 兼容 absolute-form（`GET http://host/path HTTP/1.1`）：取 authority 之后的 path。
- 空 path 按 `/` 处理。
- **不合并** `//`，**不做**百分号解码，**不做** `.` / `..` 归一化，**不转**大小写，**不自动**补/去尾斜杠。
- nginx 的 `merge_slashes on` 只影响 location 匹配与 `$uri`，不影响这里；因此 `//api/health` 与
  `/api/health` 是两条不同路径，前者通常是 `404 route-not-declared`（与上游实际收到的路径一致）。
- 非 UTF-8 的 request target 属于畸形请求，nginx 通常已经拒绝；若仍到达插件，按“未命中”处理（`deny` → 404，
  `forward` → 交给后端）。

#### 3.1.2 契约路径语法

| 写法 | 含义 | 示例 |
| --- | --- | --- |
| `/api/products` | 字面量（含 `-` `_` `.` 等） | 精确匹配 |
| `/api/user/{id}` | 单段参数，名字 `[A-Za-z_][A-Za-z0-9_]*`，最长 64 字符 | `/api/user/42` ✓，`/api/user/42/x` ✗ |
| `/static/{*rest}` | 通配剩余部分（可跨多段），只能出现在最后一段 | `/static/a/b/c.txt` ✓ |

约束：

- 必须以 `/` 开头；不能包含 `?` 或 `#`。
- 除 `/` 本身外不能以 `/` 结尾。
- `{name}` 必须占满**整段**，不支持 `/{id}.json` 这类动态后缀；参数段不能匹配空段，所以
  `/api/user/{id}` 不匹配 `/api/user/` 或 `/api/user`。
- `{*rest}` 匹配一个或多个字符，可以包含 `/`；不匹配空串。因此 `/static/{*rest}` 不匹配 `/static/` 或
  `/static`，`/{*rest}` 不匹配 `/`。需要根路径时单独声明 `/`。
- 字面量 `{` / `}` 按 matchit 语法写成 `{{` / `}}`；不建议在静态路径中使用花括号。
- 契约里写的百分号编码按**字面量**处理（`%2F` 只匹配真实的 `%2F` 三个字符）。
- 路径**区分大小写**，**严格尾斜杠**：`/api/products` 与 `/api/products/` 是两条不同路径，不做 301/308 重定向。
- 空段是字面量：`//api/health` 可以出现在契约里，但不建议；不要依赖 nginx 的归一化行为。

#### 3.1.3 匹配优先级

- **静态段 > 参数段 > 通配**，逐段比较；契约顺序**不**影响结果。
- 例：

  | 契约 | 请求 | 命中 |
  | --- | --- | --- |
  | `/a/{x}`、`/a/b` | `GET /a/b` | `/a/b`（静态优先） |
  | `/a/{x}`、`/a/b` | `GET /a/c` | `/a/{x}` |
  | `/a/{x}`、`/a/{*rest}` | — | **契约冲突，拒绝加载**（见 3.1.4） |
  | `/files/{*rest}` | `GET /files/a/b.txt` | `/files/{*rest}`，`rest = "a/b.txt"` |

- **`priority: i32`**（默认 0，越大越先匹配）：先按 priority 从高到低找候选，同一 priority 内仍按
  静态段 > 参数段 > 通配的具体度；完整路径重复（即使 priority 不同）仍拒绝加载。

#### 3.1.4 冲突与拒绝加载

以下情况**整份契约拒绝加载**，不会“保留一半路由”：

- 同一个 `path` 写两次。
- 分组展开后完整路径重复（包括与顶层直接写的路由重复）。
- 形状相同但参数名不同的路由，例如 `/u/{id}` 与 `/u/{name}`。
- 同一位置同时声明参数段与通配段，例如 `/a/{id}` 与 `/a/{*rest}`（matchit 视为冲突）。
- 通配段不在最后一段，例如 `/static/{*rest}/x`。
- `{name}` 非法（空名、含 `*`、含非法字符）、参数名重复（如 `/{id}/{id}`）。

合法且常见的组合：`/a/{id}` 与 `/a/{id}/b`、`/a/b` 与 `/a/{id}`、`/a/b` 与 `/a/{*rest}`。

> 冲突只看**展开后的完整路径**，与路由写在哪个分组里无关；`path_regex` 不参与冲突判定（它只是附加条件）。

> 实现备注：冲突检测主要依赖 matchit 的插入结果；参数名语法（`[A-Za-z_][A-Za-z0-9_]{0,63}`）与
> 参数名重复现在在加载期单独校验（无论有没有写 `path_params`），非法名字不会再被 matchit 静默
> 当成“能被匹配、但规则永远引用不到”的裸通配段。
> 本文是协议要求，代码按本文补齐后，第 0.1 节的状态才能改成“完全实现”。

### 3.2 方法匹配

- 方法比较**大小写不敏感**；契约里建议统一大写。方法名前后空白会在解析时去掉。
- `methods` 省略或 `[]` → 不限方法（放行所有方法，包括 `PATCH`、自定义方法）。
- 路径命中但方法不在列表 → `405`，响应头 `Allow` 按契约书写顺序输出（如 `Allow: GET, PUT`）。
- `methods` 中任意元素为空串或全空白 → 拒绝整份契约。
- `methods` 元素必须是 **RFC 9110 token**（`!#$%&'*+-.^_`|~` + ALPHA + DIGIT），最长 32 字符。
  方法名会原样写进 `Allow` 响应头和 error_log，所以带 CR/LF（响应头/日志注入）、空格、逗号
  （`"GET, POST"` 会被误当成两个方法）→ 拒绝整份契约。
- **方法按字面判定**：只声明 `GET` 时，`HEAD` 仍返回 `405`；需要 `HEAD` 必须在 `methods` 里显式写出。
  同理 `OPTIONS` 不会自动放行，网关不生成 CORS 预检响应。
- 未声明方法不会被自动映射为 `GET`，也不做方法重写。

### 3.3 路径参数提取（✅ 已实现）

路径命中后，参数值按下列规则提取（供 5.4 校验）：

- `{name}`：取匹配到的**单个原始段**；不包含 `/`，不为空。
- `{*rest}`：取匹配到的剩余原始路径（不含开头的 `/`），可以包含 `/`。
- **不做百分号解码**，**不把 `+` 转空格**：`/api/user/%31` 的 `id` 是 `%31`，不是 `1`。
- 值是原始 request target 的字节序列（按 UTF-8 解释）；契约作者应要求客户端发送未编码形式。
  如果客户端可能发送编码形式，请用 `char` 规则而不是 `int`，并把 `%` 计入长度。
- 路由参数名必须在同一条路由内唯一；参数规则里出现路由中不存在的名字 → 契约拒绝。
  路由里有参数但没写规则 → 合法，只匹配、不校验值。

之所以按原始值处理：路由匹配本身不解码，若校验前解码，会出现“网关按解码值判定、路由按原始值匹配”的
双重语义，`%2F`、`%2e%2e` 一类输入最容易产生绕过。网关不猜测后端框架的解码行为。

### 3.4 分组路由（✅ headers / methods / query / body / path_params / filters / unmatched / path_regex）

`routes` 的元素可以是**路由**（有 `path`）或**分组**（有 `prefix`）。分组不产生可访问的路由，
只把公共前缀、方法约束和公共检查下发给 `router` 里的后代；后代可以继续嵌套分组。

> **当前实现范围**：分组接受 `prefix` / `router` / `methods` / `headers` / `query` / `body` /
> `path_params` / `filters` / `unmatched` / `path_regex`。

```json
{
  "prefix": "/api",
  "methods": ["GET", "POST"],
  "unmatched": "forward",
  "path_regex": "^/api/.*$",
  "filters": { "sqli": true, "xss": true },
  "headers": {
    "params": {
      "authorization": { "type": "char", "required": true, "min_len": 8 }
    }
  },
  "router": [
    { "path": "/user", "methods": ["GET"] },
    {
      "prefix": "/admin",
      "headers": { "params": { "x-admin": { "type": "bool", "required": true } } },
      "router": [
        { "path": "/audit", "methods": ["GET"] }
      ]
    }
  ]
}
```

展开后的叶子路由：

| 完整路径 | 有效方法 | 继承的检查 |
| --- | --- | --- |
| `/api/user` | `GET`（组 `GET/POST` ∩ 路由 `GET`） | `authorization`、组正则、SQLi/XSS |
| `/api/admin/audit` | `GET` | `authorization`、`x-admin`、组正则、SQLi/XSS |

> 写法上注意：`router` 必须是**数组**（`[]`），不是对象；header 规则统一写在 `headers` 下，
> `header`（单数）不是别名，会被严格模式拒绝。

分组字段：

| 字段 | 类型 | 必填 | 默认 | 说明 |
| --- | --- | --- | --- | --- |
| `prefix` | string | 是 | — | 前缀路径；必须是字面量，见下 |
| `router` | array | 否 | `[]` | 子路由/子分组；元素规则与顶层 `routes` 相同 |
| `methods` | string[] | 否 | 不限 | 组级方法约束，与后代取交集，见“继承规则” |
| `unmatched` | string | 否 | 继承/顶层 | 本组范围内“路径未命中”的默认策略：`deny` / `forward` |
| `path_regex` | string | 否 | — | 组级路径正则，作用于**完整路径**（不是相对 `prefix`），后代全部追加校验，见 3.5 |
| `filters` | object | 否 | — | 组级 SQLi/XSS 开关（或继承） |
| `path_params` | object | 否 | — | 后代共用的路径参数规则 |
| `query` | object | 否 | — | 后代共用的 Query 规则 |
| `body` | object | 否 | — | 后代共用的 Body 规则 |
| `headers` | object | 否 | — | ✅ 后代共用的 Header 规则（同名跨层重复拒绝） |
| 其他字段 | — | — | — | **拒绝整份契约** |

#### 3.4.1 前缀与展开

- `prefix` 必须以 `/` 开头；除 `/` 本身外不能以 `/` 结尾；不能含 `?` / `#`；
  **只能是字面量，不能含 `{name}` / `{*rest}`**（v1 不支持参数化前缀）；最长 512 字符。
- 嵌套分组的 `prefix` 也是**相对前缀**，拼接规则与叶子 `path` 相同（见下）：父 `/api` + 子 `/v1` → `/api/v1`；
  父 `/api` + 子 `/` → `/api`；父 `/` + 子 `/v1` → `/v1`。
- 叶子路由的 `path` 仍必须以 `/` 开头，完整路径按下列规则拼接：
  - `prefix = "/"`：完整路径 = `path`；
  - `path = "/"`：完整路径 = `prefix`（表示分组根路径本身）；
  - 其他：完整路径 = `prefix` + `path`（两侧各有一个 `/`，但只保留一个）。
- 例：`/api` + `/user` → `/api/user`；`/api` + `/` → `/api`；`/` + `/user` → `/user`。
- 完整路径仍遵守 3.1 的全部规则（不能有尾斜杠、区分大小写等）。如果拼接结果非法 → 契约拒绝加载。
- **同一父级下不能有两个相同 `prefix` 的分组**；不同前缀允许重叠（如 `/api` 与 `/api/v1`）。
- 所有分组展开后的**完整路径不能重复**；与顶层直接写的路由重复也算冲突 → 契约拒绝加载。
- 分组嵌套深度上限 8 层；展开后的叶子路由总数上限 4096；超过 → 契约拒绝加载。
- 分组本身不隐式产生路由：`prefix: "/api"` 不会自动让 `/api` 可访问，需要子路由 `path: "/"`。
- `router: []` 合法。空分组 + `unmatched` 可以作为整段前缀的默认策略，例如迁移期把 `/legacy/**`
  全部转发给后端。

#### 3.4.2 继承规则

> **当前实现范围**：`methods` / `headers.params` / `query` / `body` / `path_params` / `filters` 的继承、
> `unmatched` 作用域与 `path_regex` 都已实现。

后代路由的有效配置按下列规则合并；所有规则在**契约加载时**展开成叶子路由，access 阶段不做合并：

| 配置 | 合并规则 |
| --- | --- |
| `methods` | 每一层的集合取**交集**；省略或 `[]` 表示“本层不限制”（全集）。交集为空 → 契约拒绝加载 |
| `path_regex` | **全部都要匹配**（AND）；子级不能移除父级正则，只能追加一条。正则一律匹配完整路径，不相对 `prefix` |
| `filters` | **或**语义：任一层开启即开启；子级写 `false` 不能关闭父级（见 5.9） |
| `path_params.params` / `query.params` / `body.params` / `headers.params` | 按名字合并；**同名规则只能在一个层级定义**，重复 → 契约拒绝加载（避免静默覆盖/放宽） |
| `query.unknown` / `body.unknown` | 每个出现的块先按自己的默认值补全（query 为 `ignore`、body 为 `reject`），再取更严格者：任一为 `reject` 即 `reject` |
| `body.content_types` | 只统计实际出现的 `body` 块；每个块省略时按“两种都接受”补全，再取**交集**；交集为空 → 契约拒绝加载 |
| `unmatched` | 不是逐路由继承，而是按 3.4.3 的“作用域”选取 |
| `path_params` 规则 | 每个生效规则的名字必须出现在该叶子路由的完整路径里，否则 → 契约拒绝加载 |

> 分组 `path_params` 适合约束多个后代共有的参数（如 `{tenant}`）。只要有一个后代叶子路由缺少该参数名，
> 整份契约就会拒绝加载；只适用于部分子路由的路径规则请写在具体路由上。

`methods` 例子：

- 组 `["GET", "POST"]` + 路由 `["GET"]` → 有效方法 `GET`。
- 组 `["GET"]` + 路由 `["POST"]` → 交集为空 → 契约拒绝加载。
- 组 `["GET"]` + 路由省略 / `[]` → 有效方法 `GET`（`[]` 表示“本层不额外限制”，不会把组约束清空）。
- `Allow` 头只输出**有效方法**；顺序取最内层声明了非空 `methods` 的那一层的书写顺序。

`params` 同名冲突例子：

```json
{
  "prefix": "/api",
  "headers": { "params": { "authorization": { "type": "char", "required": true } } },
  "router": [
    {
      "path": "/admin",
      "headers": { "params": { "authorization": { "type": "char", "required": false } } }
    }
  ]
}
```

上例会拒绝加载，因为 `authorization` 在组和子路由重复定义，且子级试图把 `required` 放宽。
需要不同规则时，把规则只写在真正需要的叶子路由上，不要在公共组里定义。

不同名字的规则可以分层合并。例如组里放公共的 `trace`，子路由放自己的 `page`：

```json
{
  "prefix": "/api",
  "query": {
    "params": { "trace": { "type": "char", "max_len": 32 } },
    "unknown": "ignore"
  },
  "router": [
    {
      "path": "/items",
      "methods": ["GET"],
      "query": { "params": { "page": { "type": "int", "min": 1 } } }
    }
  ]
}
```

`/api/items` 的有效 query 规则是 `trace` + `page`，`unknown` 为 `ignore`（两层都没写 `reject`）。
如果子路由再写一个 `trace`，即使规则完全相同也会拒绝加载——重复就是错误，不做“相同则合并”。

`unknown` / `content_types` 例子：

- 组 `query.unknown: "reject"` + 子路由 `"ignore"` → 最终仍是 `reject`。
- 组 `body.content_types: ["application/json"]` + 子路由 `["application/x-www-form-urlencoded"]`
  → 交集为空 → 契约拒绝加载。
- 组没有 `body` 块、子路由有 `body` 块 → 只按子路由的 body 规则校验，组不贡献限制。

#### 3.4.3 分组默认策略（`unmatched`）

分组上的 `unmatched` 决定：**请求路径落在该分组范围内，但没有任何叶子路由命中时**，交给后端还是 404。

选择顺序：

1. 先按完整路径匹配所有叶子路由（含 `path_regex`）。命中就走路由自己的方法/参数判定，不看分组
   `unmatched`。
2. 路径没有命中任何路由时，找**最长**的、按段包含该路径的分组前缀：
   - `/api` 只包含 `/api` 本身与 `/api/...`，不包含 `/apix`；
   - `/api/v1` 比 `/api` 更长，`/api/v1/x` 选 `/api/v1`。
3. 取该分组自身显式写的 `unmatched`；没写就沿**结构上的父分组**向上找最近的祖先；都没有则用顶层 `unmatched`
   （默认 `deny`）。不会横向切换到另一个前缀更短、但结构上无关的分组。
4. 用选中的策略执行：`deny` → `404 route-not-declared`；`forward` → 交给后端，日志的 `scope` 写
   **提供策略**的分组前缀（可能是祖先，不一定是第 2 步选中的最长前缀）。

例子（顶层默认 `deny`）：

| 分组配置 | 请求 | 结果 |
| --- | --- | --- |
| `/api`：`unmatched: "forward"` | `GET /api/unknown` | 转发给后端 |
| `/api`：`forward`；`/api/admin`：`deny` | `GET /api/admin/unknown` | `404`（更长的前缀优先） |
| `/api`：`forward`；`/api/admin`：未写 | `GET /api/admin/unknown` | 继承 `/api` → 转发 |
| 顶层 `deny`，没有分组包含 | `GET /other` | `404` |

约束：

- 只影响“未命中路由”的路径；已命中路由的方法不符仍是 `405`，参数不符仍是 `400`，不会因为
  `unmatched: forward` 而绕过。
- `path_regex` 不通过等同于“未命中”，按本节选择分组策略；不会回退到次优路由（见 3.5）。
- 相同父级下两个分组 `prefix` 相同 → 契约拒绝加载；`/api` 与 `/api/v1` 这种重叠是合法的。
- `unmatched` 只接受 `"deny"` / `"forward"`；其他值 → 契约拒绝加载（与顶层一致）。
- 继承只沿**结构上的父子分组**，不是按前缀字符串推断：顶层并列的 `/api` 与 `/api/v1` 之间没有继承关系；
  要让 `/api/v1` 继承 `/api` 的策略，就把前者写成后者的 `router` 成员。
- 未声明路径不进入参数校验；`forward` 原样交给后端。
- 日志里的 `scope` 写**最终提供策略**的分组前缀：上表第 3 行的 `forward` 来自 `/api`，就记 `scope=/api`，
  即使最长匹配的作用域是 `/api/v1`。

### 3.5 路径正则（✅ 分组级与路由级）

`path_regex` 是路由/分组的**附加路径条件**，不能替代路由的 `path`，也不能替代分组的 `prefix`：

```json
{
  "path": "/files/{*rest}",
  "methods": ["GET"],
  "path_regex": "^/files/[A-Za-z0-9._/-]+\\.(png|jpg|gif)$"
}
```

规则：

- 语法使用 Rust `regex` crate：支持常规字符类、分组、量词、`|`；**不支持反向引用和环视**
  （`(?=)`、`(?!<)` 等）。编译失败 → 契约拒绝加载。
- 模式长度上限 256 字符；Rust `regex` 是线性时间匹配，不存在传统回溯型 ReDoS；仍建议优先使用
  静态段/参数段，正则只作为补充约束。
- 作用于**展开后的完整原始 path**：不做百分号解码、不合并 `//`、不转大小写。
- **全匹配**语义：等价于 `^(?:<path_regex>)$`，不是“包含”或“前缀匹配”。需要前缀匹配时显式写
  `.*`，例如 `^/api/.*$`。示例里保留 `^` / `$` 只是书写习惯，重复锚点不影响结果。
- 路由级与分组级都支持；分组正则对**所有后代**生效，多层 `path_regex` 是 AND，子级只能追加，不能移除或覆盖父级正则。
- **正则始终匹配完整路径，不是相对 `prefix` 的剩余部分**。分组的 `prefix` 只负责拼接叶子路径，不参与正则匹配，
  也不会被自动加到正则前面。因此分组正则里要自己写出包含前缀在内的完整路径。
- 执行时机：路由树先按 `path` 选出候选，再校验该候选继承的所有正则。
  - 全部匹配 → 命中，继续方法/参数判定。
  - 任一不匹配 → 视为路径**未命中**：按 3.4.3 选分组 `unmatched`（或顶层策略），通常是 `404`
    或 `forward`；**不是 400**。
  - 不会回退到次优路由。例如 `/{*rest}` 上挂了正则，正则失败后不会再去尝试另一条参数路由。
    需要回退时请改用不同的静态前缀或参数段来区分。
- 正则只做匹配，不提取路径参数；路径参数仍由 3.3 提取、5.4 校验。
- 顶层不支持 `path_regex`（只允许分组/路由）；写了会被严格模式拒绝。

分组正则的常见写法（`prefix` 都是 `/api/v1`）：

| `path_regex` | 效果 |
| --- | --- |
| `^/api/v1/.*$` | **恒真**：所有后代路径本来就以 `/api/v1` 开头，等于没写 |
| `^/v1/.*$` | **恒假**：完整路径以 `/api` 开头，所有后代都会变成“未命中” |
| `^/api/v1/(user\|order)/[0-9]+$` | 有意义：只放行这两个资源 + 纯数字 ID |

> 一般规律：分组正则的**前缀部分必然与 `prefix` 重复**，这是“所有层级都用同一套全路径语义”的代价。
> 如果觉得重复，通常说明这条约束只适用于某个子路由，应该把 `path_regex` 写在路由对象上，而不是分组上。

```json
{
  "prefix": "/api",
  "path_regex": "^/api/v[0-9]+/(user|order)s/[0-9]+$",
  "router": [
    { "path": "/v1/users/{id}", "methods": ["GET"] },
    { "path": "/v1/orders/{id}", "methods": ["GET"] }
  ]
}
```

上例的叶子完整路径是 `/api/v1/users/{id}` 与 `/api/v1/orders/{id}`，只有版本号是数字、资源是 `users` / `orders`、
ID 是纯数字时才命中；`/api/v1/users/abc` 虽然能匹配路由形状，但正则不通过，按分组 `unmatched` 处理（不是 400）。

---

## 4. 判定顺序与响应

对绑定了网关的 location（`enable_gateway_by <name>`，可绑定多个），按下列顺序，**先命中先返回**，
被拒请求不会到达后端：

| # | 条件 | 状态码 | 响应头 | 说明 |
| --- | --- | --- | --- | --- |
| 1 | 该网关从未成功加载契约 | `503` | `X-Gateway-Reason: contract-not-ready` + `X-Gateway-Name` | fail-closed，冷启动不放行 |
| 2 | 路径未命中（未声明；或命中候选路由但 `path_regex` 不通过，🧪），且该作用域的 `unmatched` 为 `deny`（默认） | `404` | `X-Gateway-Reason: route-not-declared` + `X-Gateway-Name` | 含扫目录/扫漏洞路径（没有内置探针）；作用域见 3.4.3 |
| 2b | 路径未命中，但该作用域 `unmatched` 为 `"forward"` | — | — | 交给后端处理，日志 `gateway: forward ... (route-not-declared, unmatched=forward, scope=<prefix>)`（顶层策略无 `scope` 段）；不做参数校验 |
| 3 | 路径存在但方法不符 | `405` | `Allow: ...` + `X-Gateway-Reason: method-not-allowed` + `X-Gateway-Name` | `Allow` 只含该路由的**有效**方法（分组交集后） |
| 4a | Header 必填缺失（无条件或条件必填成立）/ 同一个声明头出现多行 | `400` | `X-Gateway-Reason: missing-parameter` 或 `invalid-parameter` + `X-Gateway-Name` + `X-Gateway-Field` | ✅ 见 5.3 |
| 4b | Header / Query / Body / 路径参数 字段校验不符 | `400` | `X-Gateway-Reason: invalid-parameter` + `X-Gateway-Name` + `X-Gateway-Field` | 见第 5 节 |
| 5 | 字段过滤命中 SQLi / XSS | `400` | `X-Gateway-Reason: invalid-parameter` + `X-Gateway-Name` + `X-Gateway-Field` | 见 5.9；**按维度**在该维度参数校验之后执行 |
| 6 | Body 处理失败：媒体类型 / 解析 / 字段（✅） | `400` | `X-Gateway-Reason: unsupported-media-type` 或 `invalid-parameter` + `X-Gateway-Name`（字段错误可带 `X-Gateway-Field`） | 见 5.2、5.5 |
| 7 | Body 超过上限（nginx 读取时处理） | `413` | 插件读 body 得到 413 时补 `X-Gateway-Reason: body-too-large` + `X-Gateway-Name`；nginx 预检时可能没有网关头 | 见 5.7 |
| 8 | 绑定多个网关时，全部允许 | — | — | 交给 `proxy_pass` 上游；任一拒绝即拒绝（转发也算允许） |

补充：

- 第 4a/4b/5 行是**状态码视角**的汇总；实际执行按 5.5 的维度顺序（path → query → header → body），
  每个维度自己的字段校验通过后立即执行该维度的 `filters`，再进入下一个维度。
- 路径判定使用**原始** request target 的 path（去掉 `?query`，不合并 `//`、不做百分号解码）。
- 未声明路由是 `deny` 还是 `forward` 由**各自网关的契约**决定（分组可覆盖，见 3.4.3）；一个 location 绑多个网关时，
  任一网关拒绝就拒绝，全部允许（含转发）才放行。
- **`path_regex` 不通过等同“路径未命中”**，不是 `400`，也不回退到次优路由；走 3 或 3b。
- **未绑定** `enable_gateway_by` 的 location 完全不参与判定（body 也不读），请求直接透传；
  `/passthrough/` 就是这种对照。
- `X-Gateway-Name` 标明是哪个网关做出的拒绝（多网关排查用）。
- 表中所有 `X-Gateway-*` 都是**诊断头**，只在 `gateway_diagnostics on` 时返回，默认关闭（见 4.2）；
  `Allow` / `Retry-After` 是协议头，始终返回。
- 响应体不做约定：当前返回 nginx 默认错误页；客户端应以**状态码**为准，不要依赖 `X-Gateway-Reason`
  （它默认不返回，只有测试环境才可见）。
- `X-Gateway-Reason` 是稳定的机器可读 token；以后新增原因只增不改。它默认只写进 `error_log`。

### 4.1 参数校验与过滤失败的响应头（Header / Query / Body / 路径参数 / filters 已实现）

参数校验失败时，除状态码与 `X-Gateway-Reason` 外，还会写：

| 响应头 | 何时出现 | 值 |
| --- | --- | --- |
| `X-Gateway-Field` | 能定位到具体字段时（缺失、类型/范围/长度/枚举不符、重复、未声明字段、SQLi/XSS 命中） | `<dimension>.<name>` |

- `dimension` 固定为 `path` / `query` / `header` / `body` 之一，小写。
- `name` 与契约声明一致；header 名统一小写。名字中不属于 `A-Z a-z 0-9 - . _ ~` 的字节按 UTF-8
  做百分号编码，保证响应头是纯 ASCII。第一个 `.` 是维度分隔符，名字本身可以再含 `.`。
- 一次请求只报**第一个**错误，不返回错误列表。Header / Query / Body 的缺失、重复、类型/范围/长度/枚举/正则、
  未声明字段、body 重复键、路径参数与过滤命中都会写 `X-Gateway-Field`。
- **不返回收到的值**，也不返回规则细节（例如 `max_len=32`、命中的是 `sqli` 还是 `xss`），
  避免把用户输入或内部策略带进响应。具体原因只写 `error_log`，格式见 5.6。
- `unsupported-media-type`、`body-too-large`、以及“body 整体读失败”“query 参数对总数超限”没有具体字段，不带
  `X-Gateway-Field`。

### 4.2 诊断响应头与上线开关（✅ 已实现）

`X-Gateway-Name` / `X-Gateway-Reason` / `X-Gateway-Field` 是**诊断信息**，
不能直接返回给真实用户：`-Field` 会给出契约里的参数名，`-Reason` 会把“路由存在但参数不对”和
“没有这条路由”区分开，两者合起来就是一台免费的契约枚举机；`-Name` 则暴露网关拓扑。

```nginx
http {
    gateway_diagnostics off;   # 默认值；只接受 on | off
}
```

| 取值 | 行为 |
| --- | --- |
| `off`（默认、不写） | 上述四个头**一律不写**；状态码、`Allow`、`Retry-After`、`responses` 自定义 body 全部不变 |
| `on` | 写四个诊断头；`nginx -t` / 启动时打一条 `NGX_LOG_WARN`，提醒不要带到生产 |

约束：

- **只能在 nginx 侧配置打开，契约里没有对应字段**。URL 模式下后端被拿下就能改自己的 `/info`，
  所以“调试开关”绝不能由契约决定，理由同 1.1。
- 每个配置周期重新判定：reload 时如果新配置没写 `gateway_diagnostics`，就回到 `off`，
  不会继承上一份配置的 `on`。
- 重复声明、`maybe` 之类非法取值 → `nginx -t` 直接拒绝（`must be on|off` / `duplicate gateway_diagnostics`）。
- 关掉之后仍然可排障：见 4.3 的 `X-Request-Id`，`reason` / `field` / `rule` 继续写进 `error_log`。

### 4.3 `X-Request-Id` 与日志关联（✅ 已实现）

- 网关自己产生的拒绝响应（404 / 405 / 400 / 413 / 415 / 503）**始终**带 `X-Request-Id`，
  值与 nginx 内置 `$request_id` 相同（32 位十六进制）；不受 `gateway_diagnostics` 影响，
  因为它只是关联 ID，不泄露契约内容。
- 网关的每条判定日志以 `rid=<同一个值>` 结尾，例如：

  ```text
  gateway: 400 GET /api/header-check reason=missing-parameter field=header.x-demo-token rule=required gateway=mygw rid=3f1c...
  ```

- 用户报障时只需要一个 `X-Request-Id`，运维就能在 `error_log` 里捞到完整原因，不需要把
  `X-Gateway-Field` 暴露给客户端。
- 上游（`proxy_pass`）自己返回的响应不加这个头，避免和业务自己的请求 ID 撞车。

---

## 5. 参数校验（协议已定稿，分步实现中）

> 本节字段在插件实现前**不允许出现在契约里**：出现即整份拒绝（严格模式）。
> 例外：`headers` / `query` / `body` 三个参数块已实现（存在性、`required_if`、值规则、`unknown`、Body 媒体类型，见 5.1–5.3）；
> 路径参数（`path_params`）、分组继承与 `filters` 仍按本节约定，未实现前写入会被拒绝。
> 实现顺序建议：路径参数 → Query → Header → Body → `filters`；每一步都满足“新增字段缺省时旧契约行为不变”。

参数块可以写在**顶层路由**、**分组**或两者都写；加载时按 3.4.2 合并成叶子路由的有效规则，
access 阶段不再做合并。分组只提供“公共部分”，同名参数规则不允许跨层重复。

### 5.0 通用值规则

四个参数块共用同一套值规则。规则值有两种形态：

- **完整对象**：按需写 `type` / `required` / `required_if` / `regex` 等；
- **`true` 简写**：只要求字段存在，不校验值。

```json
{
  "authorization": true,
  "x-request-id": { "required": true },
  "qty": { "type": "int", "required": true, "min": 1, "max": 99 },
  "coupon": { "regex": "^[A-Z0-9]{4,16}$" },
  "card_last4": { "required_if": { "field": "payment_method", "equals": "card" } }
}
```

- `true` 等价于 `{ "required": true }`。`false` / `null` / 字符串 / 数字 → **拒绝整份契约**
  （避免把 `false` 误读成“可选”）。因此 `path_params` 里也不能写 `true`（等价于被禁止的 `required`）。
- `type` 可省略：省略时**不校验类型/范围/长度**；存在性仍由 `required` / `required_if` 决定，
  写了 `regex` 时只对出现的值执行正则。
- 一个规则至少要声明一个字段：`required` / `required_if` / `regex` / `type` 之一；空对象 `{}` → 拒绝加载。
  `{ "required": false }` 是合法的**显式可选**声明（运行时不做检查，等价于省略该规则），
  保留它是为了让生成的契约能明确表达“这个字段可选”；但它不能是空对象。
- 只写 `true` 的 header 规则是“必须带这个头，值随便”，不需要再写 `type: "char"`。

| 字段 | 适用 | 必填 | 默认 | 说明 |
| --- | --- | --- | --- | --- |
| `type` | 全部 | 否 | — | `char` / `int` / `bool` / `enum`；省略 = 不校验值 |
| `required` | 全部 | 否 | `false` | 请求中必须出现；路径参数禁止写（总是存在） |
| `required_if` | 全部 | 否 | — | 条件必填，见 5.0.1；路径参数禁止写 |
| `regex` | 字符串 | 否 | — | 值必须**全匹配**，见 5.0.2；只能配 `char` 或省略 `type` |
| `min` / `max` | `int` | 否 | — | 闭区间；必须是 i64；`min <= max` |
| `min_len` / `max_len` | `char` | 否 | — | 闭区间，单位是 Unicode 码点（不是字节）；`min_len <= max_len` |
| `len` | `char` | 否 | — | 精确长度；不能与 `min_len` / `max_len` 同时出现 |
| `values` | `enum` | 是 | — | 非空字符串数组，1–64 个，元素之间不能重复 |
| `ignore_case` | `enum` | 否 | `false` | 为 `true` 时枚举值比较忽略 ASCII 大小写 |
| 其他字段 | — | — | — | **拒绝整份契约** |

类型语义：

| `type` | 接受的值 | 约束 |
| --- | --- | --- |
| `char` | UTF-8 字符串 | 长度按 Unicode 码点算；禁止 NUL（`\u0000`）；不做 trim、不做类型转换 |
| `int` | 十进制整数 | 语法 `-?(0\|[1-9][0-9]*)`；不接受 `+1`、`01`、空白、小数、指数；范围是 i64 |
| `bool` | `true` / `false` / `1` / `0` | ASCII 大小写不敏感；JSON 里也接受布尔字面量与数字 `1` / `0` |
| `enum` | 字符串 | 默认区分大小写；只接受字符串，数字/布尔不会转成字符串 |

#### 5.0.1 条件必填 `required_if`

> 实现状态：**Header / Query / Body / 路径参数均已实现**。
> Header 维度的引用只能指向同一个 `headers.params` 里的头，名字大小写不敏感。

```json
"card_last4": { "required_if": { "field": "payment_method", "equals": "card" } },
"coupon":     { "required_if": "use_coupon" }
```

- 引用的是**同一个参数块**里的另一个字段；不能跨维度（query 不能引用 header），不能引用自己。
  合并后该维度的有效规则里必须有被引用字段，否则拒绝加载。
- 两种写法：
  - **字符串**：`"required_if": "use_coupon"` → 被引用字段**出现**即成立（空串也算出现）。
  - **对象**：`{ "field": "...", "equals": "..." }` → 被引用字段出现，且值**完全相等**才成立。
- 对象形式是严格的：只允许 `field` / `equals` 两个键，缺键、多键、非字符串、空 `field` → 契约拒绝加载；
  `equals` 允许空串（表示“出现且值为空”）。字符串形式不能为空串。
- 比较规则（与各维度已有处理一致）：
  - query / form：先百分号解码，`+` 转空格后再比较；
  - header：名字大小写不敏感，值去 OWS 后比较；
  - path：用原始未解码值；
  - JSON body：只有**字符串值**能匹配 `equals`；数字/布尔不会转成字符串（`1` ≠ `"1"`）。
  - 默认大小写敏感；不做 trim / 解码以外的归一化。
- 条件成立且本字段缺失 → `400 missing-parameter`，`X-Gateway-Field` 指向**本字段**，日志 `rule=required_if`。
- 条件不成立 → 本字段可有可无；如果出现了，仍按自身的 `type` / `regex` 等规则校验。
- `required: true` 和 `required_if` 同时写 → **拒绝加载**（前者已经覆盖，避免两种语义打架）。
- 只做**一层**判定：不递归、不链式推导。被引用字段本身是不是 `required_if` 不影响条件，只看请求里它是否出现/等于。
  互相引用是合法的（两个条件都只看请求本身），自引用拒绝。
- **Header 维度的条件取值**：被引用头出现即算“出现”（空值、非 UTF-8 也算）；对象形式的 `equals` 只与
  去掉 OWS 后的 UTF-8 值比较，非 UTF-8 值一律不相等；同一头多行时取**第一行**做条件判定，
  重复行本身仍会在后续的重复检查中报错。
- 路径参数块禁止 `required_if`（路径参数总是存在，条件恒成立，没有意义）。
- `null` 视为未出现；`false` / `0` / 空串视为出现。

| 规则 | 请求 | 结果 |
| --- | --- | --- |
| `card_last4` required_if `payment_method == "card"` | `payment_method=card`，无 `card_last4` | `400 missing-parameter`，`field=body.card_last4` |
| 同上 | `payment_method=cash`，无 `card_last4` | 放行 |
| 同上 | 无 `payment_method`，无 `card_last4` | 放行（条件不成立） |
| `coupon` required_if `"use_coupon"` | 无 `use_coupon`，无 `coupon` | 放行 |
| 同上 | `use_coupon=`，无 `coupon` | `400 missing-parameter`（空串也算出现） |

#### 5.0.2 字段正则 `regex`

```json
"coupon": { "type": "char", "regex": "^[A-Z0-9]{4,16}$" },
"slug":   { "regex": "^[a-z0-9-]+$" }
```

- 语法与 3.5 的 `path_regex` 相同：Rust `regex`，最长 256 字符，不支持反向引用/环视；编译失败 → 整份契约拒绝加载。
- **全匹配**语义：等价于 `^(?:<regex>)$`，不是“包含”；`^` / `$` 可写可不写。
- 只能用于字符串值：
  - 写了 `type` 时必须是 `char`；
  - 省略 `type` 时按字符串处理：JSON 里数字/布尔/对象/数组 → `400 invalid-parameter`；
  - 不能与 `enum` 同时使用（`values` 和 `regex` 二选一）。
- 校验顺序：`type` → `min_len` / `max_len` / `len`（若写）→ `regex`；任一失败立即返回。
- 作用在**该维度处理后的值**上：path 用原始未解码值、query/form 用百分号解码后的值、header 用去掉 OWS 的值、
  JSON 用解码后的字符串。不做隐式 trim、转义或二次解码。
- 空串是否合法由正则决定（`.*` 匹配空串，`^[0-9]+$` 不匹配）。
- 字段缺失时**不执行** `regex`；需要“必须有且符合格式”时同时写 `required: true`（或在条件成立时用 `required_if`）。
- 失败 → `400 invalid-parameter`，`X-Gateway-Field` 指向该字段，日志 `rule=regex`；响应与日志都不包含正则或值。
- `filters` 在 `regex` 之后执行（5.5）：正则不通过时直接返回 `400`，不会继续做 SQLi/XSS 检测。
- 正则只做格式约束，不替代后端的业务校验；数值范围请用 `min` / `max`，不要用正则硬凑。

补充约定：

- **`null` 等于未提供**：JSON body 里字段值为 `null` 时，`required: true` → `missing-parameter`；
  非必填 → 跳过。Query / Form / Header 没有 `null` 概念，空串是“存在但为空”。
- **未声明字段的 `null` 仍然算“出现”**：`{"is_admin": null}` 在 `unknown: "reject"` 下同样返回
  `400 invalid-parameter`（`X-Gateway-Field: body.is_admin`）。否则 Mass Assignment 可以用 `null`
  绕过字段白名单；“`null` 等于未提供”只适用于**契约声明过的字段**。
- **不做隐式转换**：`"123"` 对 `int` 合法（Query/Form/Header 只能是字符串），但 `"1.0"`、`" 1"` 不合法；
  JSON 数字 `1.0`、`1e3` 对 `int` 不合法，JSON 字符串 `"1"` 合法。
- **不做归一化**：不额外 trim（Header 维度的 OWS 去除除外）、不转小写、不做 Unicode NFC/NFD、不做 URL 解码以外的处理。
- **不校验未声明字段**，除非该参数块显式 `"unknown": "reject"`（见 5.1 / 5.2）。Header 永不拒绝未声明字段。
- **规则数量上限**：单条路由的参数规则合计不超过 256 条；单个参数块不超过 128 条；超过 → 契约拒绝加载。
  分组继承后按**展开后的叶子路由**计算（父级规则也计入该路由的合计）。
- 参数名规则：
  - `query` / `body`：1–128 个 UTF-8 字符，不能为空、不能含控制字符；区分大小写。
  - `headers`：必须是合法的 HTTP field-name（token），1–128 字符；大小写不敏感，契约里建议全小写。
  - `path_params`：必须匹配 `[A-Za-z_][A-Za-z0-9_]*`，最长 64 字符。
  - 同一参数块内名字重复（header 按大小写不敏感比较）→ 契约拒绝加载。
- 类型与约束不匹配（如 `int` 写 `max_len`、`char` 写 `values`、非 `enum` 写 `ignore_case`）→ 契约拒绝加载。

### 5.1 Query（✅ 已实现）

```json
"query": {
  "params": {
    "page": { "type": "int", "required": false, "min": 1, "max": 1000 },
    "kw":   { "type": "char", "max_len": 32 },
    "sort": { "type": "enum", "values": ["asc", "desc"], "required": false },
    "flag": { "type": "bool" }
  },
  "unknown": "ignore"
}
```

| 选项 | 值 | 默认 | 说明 |
| --- | --- | --- | --- |
| `unknown` | `"ignore"` / `"reject"` | `"ignore"` | 未声明 query 参数：忽略，或直接 400 |

解析规则：

- Query 取自**原始 request target** 的第一个 `?` 之后，与路径判定同源；不使用 nginx 归一化后的 `$args`。
- 以 `&` 分隔；空段（`&&`、开头/结尾的 `&`）忽略；`a` 等价于 `a=`；空 key（`=x`）→ `400 invalid-parameter`。
- key 与 value 都做一次百分号解码：`%XX`，`+` 转空格；解码后必须是合法 UTF-8。
  非法 `%` 序列或非法 UTF-8 → `400 invalid-parameter`。只解码一次，不递归解码。
- 键名**区分大小写**。
- **同名参数出现多次（解码后同名）→ `400 invalid-parameter`**，不做“取第一个/最后一个”的猜测。
  `%61` 与 `a` 视为同名。本版不支持数组参数；`tags[]` 是字面量键名。
- 解码后的参数对总数上限 256；超过 → `400 invalid-parameter`（无 `X-Gateway-Field`）。
- `required` 的语义是“键出现”，空值也算出现；空值是否合法由 `char` 的 `min_len` / `len` 或 `int` 的语法决定。
- `required_if`（5.0.1）可用于条件必填：条件成立而键缺失 → `400 missing-parameter`，`field` 指向本字段。
- 声明字段按契约顺序校验；`unknown: "reject"` 时，未声明字段在声明字段全部通过后再报错。
- `query` 块省略时，query 完全不解析；畸形 query 也原样交给后端。
- 分组上的 `query` 块由后代继承；`params` 同名规则不能跨层重复，`unknown` 取更严格者（见 3.4.2）。

### 5.2 Body（✅ 已实现）

```json
"body": {
  "params": {
    "sku":  { "type": "char", "required": true, "max_len": 64 },
    "qty":  { "type": "int", "required": true, "min": 1, "max": 99 },
    "note": { "type": "char", "max_len": 200 }
  },
  "unknown": "reject",
  "content_types": ["application/json", "application/x-www-form-urlencoded"]
}
```

| 选项 | 值 | 默认 | 说明 |
| --- | --- | --- | --- |
| `unknown` | `"ignore"` / `"reject"` | `"reject"` | 未声明 body 字段：拒绝（防 Mass Assignment）或忽略 |
| `content_types` | string[] | 两种都接受 | 允许的媒体类型；元素只能是 `application/json` 与 `application/x-www-form-urlencoded`，去重、大小写不敏感 |
| `max_bytes` | u64 | 不限制 | 原始 body 字节上限；`0` 表示关闭本子树从祖先继承的上限 |
| `max_depth` | u64 | `32` | JSON 嵌套深度上限（1..=128）；`0` 关闭 |
| `max_string_length` | u64 | `8192` | 单个 JSON 字符串的原始字节上限；`0` 关闭 |
| `max_array_entries` | u64 | `500` | 单个 JSON 数组的元素个数上限；`0` 关闭 |

> **默认值为什么与 Query 不同**：Query 常带统计、追踪、旧客户端附加参数，默认忽略以兼容；
> Body 是 Mass Assignment 的入口，默认拒绝未声明字段，避免后端“只取自己认识的字段”造成提权。
> 需要透传额外字段时，显式写 `"unknown": "ignore"`。

请求体读取与媒体类型：

- **只有写了 `body` 块的路由才读取请求体**；没写的路由不读、不解析、原样转发（保持流式转发能力）。
- `max_bytes` 统计的是**原始 HTTP body 字节**（Content-Length；chunked 时按解码后的实体字节），
  包含空格/换行/转义；不是字段字符数，也不是解压后的长度。
- 继承：分组上的 `max_bytes` 传给所有后代；子级可写更小值，最终取**更严格者**（min）；
  子级写 `0` 关闭该子树的上限（之后的后代仍可重新开启）。多个网关绑定同一 location 时也取 min。
- 执行时机：`Content-Length` 已知时在**读 body 之前**直接 `413 body-too-large`（可应用
  `responses.body_too_large`）；chunked 没有长度，读完后再判。nginx 的 `client_max_body_size`
  仍是最后兜底，插件不能替代它。
- 没有 `body` 块的路由不会因为 `max_bytes` 去读 body；要限制这类请求请用 nginx 配置。

JSON 结构扫描（`max_depth` / `max_string_length` / `max_array_entries`）：

- 只对 `application/json` 生效；form body 不扫描。
- 在 `serde_json` 解析之前做一次单遍字节扫描（O(N)、固定 128 层栈、无堆分配），
  先挡掉深层嵌套/超长字符串/超大数组，避免递归与内存放大；它**不是** JSON 校验器，
  非法 JSON 仍由 serde 拒绝。
- 深度只数 `{` / `[`；字符串内的括号、转义引号不参与。顶层 `{` 算第 1 层。
- 字符串长度按**原始字节**统计（含键名、反斜杠和转义字符，不含两侧引号）；UTF-8 多字节按字节数。
- 数组元素数按每个数组层各自统计；对象成员的逗号不计。
- 三者都按 3.4.2 继承：组传给后代，子级可写更小值（取 min）；`0` 关闭本子树的上限，
  后代仍可重新开启。`max_depth` 超过 128 的契约直接拒绝加载；即使写 `0`，扫描器仍有
  128 层硬上限（与 `serde_json` 默认递归上限一致），防止栈溢出。
- 命中 → `400 invalid-parameter`，`X-Gateway-Reason: invalid-parameter`，无 `X-Gateway-Field`；
  日志 `rule=json-depth` / `json-string-length` / `json-array-entries`。
- 默认值面向普通 API；包含 base64/大文本字段的接口应显式调大 `max_string_length` 或写 `0`
  关闭，文件上传优先用 `client_max_body_size` 控制。


建议值（原始字节，留出 JSON 空白/转义余量）：

| 场景 | 建议 `max_bytes` |
| --- | --- |
| 登录 / 注册 JSON | 1–4 KiB |
| 简单表单 | 4–16 KiB |
| 普通 CRUD JSON | 16–64 KiB |
| 批量 / 富文本 | 64–256 KiB |
| 文件上传 | 不设，交给 nginx `client_max_body_size` |

- 空 body（`Content-Length: 0` 且无 `Transfer-Encoding`）按空对象处理，不要求 `Content-Type`。
- 非空 body 必须有 `Content-Type`；类型与子类型大小写不敏感；参数（如 `; charset=utf-8`）忽略，
  但 `charset` 只接受 `utf-8` / `utf8`，其他 → `400 unsupported-media-type`。
- 不在 `content_types` 中的类型 → `400 unsupported-media-type`。
- `Content-Encoding` 只能是缺省或 `identity`；`gzip` 等 → `400 unsupported-media-type`（网关不解压）。
- 媒体类型通过后才解析字段；解析失败（坏 JSON、坏 form 编码、非法 UTF-8）→ `400 invalid-parameter`。
- 请求体上限与读取方式见 5.7；超过上限不做部分解析，直接 `413 body-too-large`。

`application/json`：

- 顶层必须是 JSON **object**；数组、字符串、数字、`null` → `400 invalid-parameter`。
- 字段名区分大小写；**重复键 → `400 invalid-parameter`**（不采用“最后一个生效”）。
- 声明的字段必须是**标量**：`char` 要字符串、`int` 要整数、`bool` 要布尔/`1`/`0`、`enum` 要字符串。
  对象或数组 → `400 invalid-parameter`；不做嵌套字段声明。
- `null` 按 5.0 视为未提供（仅限声明字段；未声明字段的 `null` 仍会触发 `unknown: "reject"`）。
- `required_if`（5.0.1）的 `equals` 只匹配 JSON **字符串**值；数字/布尔不会转成字符串。
- `unknown: "reject"` 时，任何未声明的顶层键 → `400 invalid-parameter`；`"ignore"` 时忽略。

`application/x-www-form-urlencoded`：

- 解析规则与 5.1 Query 相同（`&` 分隔、`+` 转空格、一次百分号解码、非法编码 400）。
- 同名键重复 → `400 invalid-parameter`；`a[b]`、`tags[]` 都是字面量键名，不做嵌套解析。
- `unknown: "reject"` 时，未声明键 → `400 invalid-parameter`。

其他：

- `body: { "params": {}, "unknown": "reject" }` 的语义是“只允许空 body”；`unknown: "ignore"` 则是
  “不校验字段，但仍校验媒体类型与大小”。
- `body` 规则与 HTTP 方法无关：GET/HEAD 带 body 时也按同一规则校验（是否能到后端由 nginx/后端决定）。
- 网关只拒绝，不改写 body；通过后上游收到的字节与客户端发送的一致（nginx 缓冲/解 chunk 除外）。
- 分组上的 `body` 块由后代继承：`content_types` 取各层交集，`unknown` 取更严格者，`params` 同名规则不能跨层重复（见 3.4.2）。

### 5.3 Header（存在性、值规则、`required_if` 已实现；过滤已实现）

```json
"headers": {
  "params": {
    "authorization": true,
    "x-api-version": { "type": "enum", "values": ["1", "2"], "required": true },
    "x-request-id": { "regex": "^[A-Za-z0-9-]{8,64}$" },
    "x-payment-method": { "type": "enum", "values": ["card", "cash"] },
    "x-card-last4": { "required_if": { "field": "x-payment-method", "equals": "card" }, "regex": "[0-9]{4}" }
  }
}
```

> 上例的规则当前都能加载：`authorization: true` 只查存在性；`x-api-version` 走 `enum`；
> `x-request-id` 走 `regex`；`x-card-last4` 在 `x-payment-method: card` 时必填。
> 分组继承与维度 `filters` 均已实现。

已实现（存在性）：

- **只校验显式声明的 header**；未声明的 header 永远不看、不拒绝（避免影响 `User-Agent`、`Cookie`、
  `Accept-*` 等正常业务头）。因此 Header 块没有 `unknown` 选项。
- 名字大小写不敏感；契约里建议全小写，解析时统一按小写匹配。同一契约内仅大小写不同的名字视为重复 → 拒绝加载；
  JSON 里完全相同的键（重复键）也在解析阶段拒绝，不采用“最后一个生效”。
- 名字必须是合法的 HTTP field-name（token），1–128 字符；报错字段统一写 `header.<小写名>`。
- `true` 等价于 `{ "required": true }`：头缺失 → `400 missing-parameter`，`X-Gateway-Field: header.<name>`。
- `{ "required": false }`：显式可选，运行时不做检查；`required` 缺省也按 `false`。
- **同一个声明的 header 出现多行 → `400 invalid-parameter`**（本版不做多值合并）；逗号分隔的值不拆分。
  注意：`Host` / `Content-Length` 等被 nginx 标记为“唯一”的头，重复行通常由 nginx 在进入插件前直接拒绝，
  此时可能没有 `X-Gateway-*`；插件只能检查 `headers_in` 中实际保留下来的重复行。
- 同一维度内**先检查无条件必填缺失，再检查条件必填，再检查重复行，最后才校验值**：一个请求只报第一个错误，
  顺序稳定；多个必填头都缺失时，按**契约书写顺序**报第一个（不是字母序）。
- `required_if`（见 5.0.1）：条件成立且目标头缺失 → `400 missing-parameter`，日志 `rule=required_if`；
  条件不成立时目标头可选，但出现后仍执行值规则。字符串形式只看被引用头是否出现（空值、非 UTF-8 也算）；
  对象形式只与去掉 OWS 后的 UTF-8 值做**区分大小写**的完全相等比较。
- `"params": {}` 合法，表示“没有 Header 规则”（生成器可能输出空块）；不产生任何检查。
- 请求头总大小由 nginx `large_client_header_buffers` 控制，超限通常由 nginx 在进入插件前返回 400/414，
  不会有 `X-Gateway-*`。
- 该切片已在真实 nginx 下验证：`tests/phase1.sh` 的 `/api/header-check` 用例覆盖缺失、放行、
  头名大小写、显式可选、未声明头、重复行，以及 `X-Gateway-Field` / `rule=` 日志。

已实现（值规则第一批，见 5.0）：

- 值的处理：只去掉首尾 OWS（SP / HTAB），不做其他 trim、不做 Unicode 归一化、不解码。
- 值必须是合法 UTF-8；**声明了值规则时**非 UTF-8 值 → `400 invalid-parameter`，
  `rule` 按类型写 `char` / `int` / `bool` / `values`。仅有存在性规则（`true` / 纯 `required`）时仍只判断“头是否出现”，
  不读取值，因此非 UTF-8 值也算存在。
- 支持的规则：
  - `type: "char"`：`min_len` / `max_len` / `len`，按 **Unicode 码点**计数；值中出现 NUL → 拒绝（`rule=char`）。
  - `type: "int"`：十进制 i64 语法 `-?(0|[1-9][0-9]*)`，`min` / `max` 闭区间；`01` / `+1` / `1.0` / `1e3` 拒绝。
  - `type: "bool"`：`true` / `false` / `1` / `0`，ASCII 大小写不敏感。
  - `type: "enum"`：`values` 1–64 个、不重复；`ignore_case: true` 时比较忽略 ASCII 大小写，
    且仅大小写不同的 `values` 也视为重复（拒绝加载）。
  - `regex`：Rust `regex`，加载时编译，最长 256 字符；按 `^(?:pattern)$` **全匹配**；
    只能配 `char` 或省略 `type`，不能与 `enum` / `values` 同用。
- 值规则可以省略：省略时只按 `required` 判断存在性；可选值规则在头未出现时直接跳过。
- 校验顺序：`type` → `min` / `max` 或 `min_len` / `max_len` / `len` → `regex`；任一失败立即返回。
- 契约静态校验：`min` / `max` 只能配 `int`，长度约束只能配 `char`，`values` / `ignore_case` 只能配 `enum`；
  `len` 与 `min_len` / `max_len` 互斥，`min <= max`、`min_len <= max_len`；`type: "enum"` 必须带 `values`；
  任一不满足 → 整份契约拒绝。
- 错误响应统一为 `400 invalid-parameter` + `X-Gateway-Field: header.<name>`，日志 `rule=` 写
  `char` / `int` / `bool` / `values` / `min` / `max` / `min_len` / `max_len` / `len` / `regex` / `duplicate`。
- 集成测试见 `tests/phase1.sh` 的 `/api/header-value-check`：合法放行、各类非法值 400、`X-Gateway-*`、
  `rule=` 日志、请求未到后端、未声明头忽略。
- `required_if` 的集成测试见 `tests/phase1.sh` 的 `/api/header-required-if`：对象/字符串两种形式、
  条件不成立放行、条件成立缺失 400、目标存在后仍做值校验、`rule=required_if` 日志。

尚未覆盖（🧪）：

- 按维度的 `filters.scope` 之外没有更细的过滤粒度。

其他约定：

- 空值算“存在”；`required: true` 只保证出现，是否允许空由 `char` 的 `min_len` / `len` 决定。
- `required_if` 比较时用去掉 OWS 后的值，`equals` 区分大小写；被引用 header 的名字大小写不敏感。
- 值比较默认区分大小写；`enum` 可用 `ignore_case: true`。
- 校验对象是**客户端发来的请求头**。如果 nginx 用 `proxy_set_header` 改写了某个头，后端看到的值可能不同；
  这类头仍可按客户端值校验，但契约作者应确保两边语义一致。

### 5.4 路径参数（✅ 已实现）

```json
"path": "/api/user/{id}",
"methods": ["GET"],
"path_params": {
  "params": {
    "id": { "type": "int", "min": 1 }
  }
}
```

- 值来自 3.3 的提取结果：**原始、未解码**的路径段；`{*rest}` 的值不含开头的 `/`，可以含 `/`。
- 路径参数总是存在，规则里**禁止**写 `required`、`required_if` 和 `true` / `false` 简写（写了 → 契约拒绝加载）。
- `params` 里出现路由中不存在的参数名 → 契约拒绝加载；路由里有参数但没写规则 → 合法，不校验。
- 值按 5.0 校验。`char` 的长度按原始字符数（`%31` 算 3 个字符）；`int` 只接受原始数字。
- `regex`（5.0.2）作用于**原始、未解码**的路径值：`/api/user/%34%32` 对 `^[0-9]+$` 不通过，
  不会先解码成 `42` 再匹配。`required_if` 对路径参数没有意义，所以禁止。
- 分组上的 `path_params.params` 由后代共用（例如统一约束 `{tenant}`）；规则名必须出现在每个后代叶子路由的
  完整路径里，否则拒绝加载（见 3.4.2）。
- 例：

  | 契约 | 请求 | 结果 |
  | --- | --- | --- |
  | `{id}: int` | `/api/user/42` | 放行 |
  | `{id}: int` | `/api/user/%34%32` | `400 invalid-parameter`（值是 `%34%32`，不是 `42`） |
  | `{id}: int` | `/api/user/abc` | `400 invalid-parameter` |
  | `{id}: char, regex: ^[0-9]+$` | `/api/user/42` | 放行 |
  | 同上 | `/api/user/%34%32` | `400 invalid-parameter`（正则匹配原始值，不解码） |
  | `{*rest}: char, max_len: 64` | `/files/a/b.txt` | 放行，`rest = "a/b.txt"` |

### 5.5 校验顺序与短路（Header / Query / Body / 路径参数 / filters 已实现）

单个网关内，固定按下列顺序，**第一个失败立即返回**：

| # | 阶段 | 失败结果 |
| --- | --- | --- |
| 1 | 路径匹配（`path` → 继承的 `path_regex` → `unmatched` 作用域） | 未命中 → `404` / `forward`（3.4.3、3.5） |
| 2 | 方法（有效方法 = 各层 `methods` 交集） | `405` + `Allow` |
| 3 | 路径参数（`path_params`）→ path 维度过滤 | `400` |
| 4 | Query（`query`）→ query 维度过滤 | `400` |
| 5 | Header（`headers`）→ header 维度过滤 | `400` |
| 6 | Body 媒体类型与 `Content-Encoding` | `400 unsupported-media-type`；✅ |
| 7 | Body 大小 | `413 body-too-large`；nginx 读取时处理 |
| 8 | Body 解析成字段（JSON / form） | `400 invalid-parameter`；✅ |
| 9 | Body 字段值校验（`body.params`）→ body 维度过滤 | `400 invalid-parameter` |

关于顺序：

- 媒体类型和大小必须在解析之前判定，避免为不支持的类型或超大 body 做无用解析。
- `filters`（5.9）按**维度**执行：某个维度自己的 `params` 规则全部通过后，紧接着过滤该维度的字符串值，
  再进入下一个维度。这样“哪个维度先失败”只由上面的阶段顺序决定，与字段是否命中过滤规则无关。
- 过滤针对的是**解析后的字符串值**：路径参数用 3.3 的原始值、query/form 用百分号解码后的值、
  header 用去掉 OWS 后的值、JSON body 用 JSON 解码后的字符串。不重新扫描原始字节。
- 同一个维度里，`filters` 排在 `unknown: "reject"` 检查之后：先保证字段集合合法，再检查值。

每个维度内部固定为五步（该维度没有对应参数块时跳过 1–4）：

1. 声明字段的**无条件** `required: true` 缺失检查（按契约顺序）。
2. 声明字段的 `required_if` 条件必填检查（按契约顺序；只对被引用字段和本字段做判定，见 5.0.1）。
3. 声明字段的值校验（按契约顺序）：`type` → `min` / `max` 或 `min_len` / `max_len` / `len` → `regex`。
4. 未声明字段检查（仅 `query` / `body` 的 `unknown: "reject"`；按请求出现顺序）。
5. 该维度的 `filters` 检查（开启了对应开关时；见 5.9）。

这样“必填缺失”（无论无条件还是条件必填）优先于“值非法”，错误稳定、可复现，不受请求参数顺序影响。
`required_if` 的条件只看请求本身，不会因为被引用字段自身校验失败而改变判定结果。

> **当前实现范围**：Path / Header / Query / Body 四个维度都已执行第 1–4 步：无条件必填、`required_if`、
> 重复/重复键、值校验、`unknown: "reject"`（Header 没有 `unknown`）。Path 没有 `required` / `required_if`；
> Header 额外有“同一头多行”检查（排在必填之后）。
> 第 5 步 `filters`、路由级 `path_regex`、`priority`、`responses` 均已实现；
> `responses` 的 `contract_not_ready` 仍只做解析；`body_too_large` 只在插件读到 413 时生效。

绑定多个网关时：

- 网关按配置里的 `enable_gateway_by` 顺序逐个判定；**任一网关拒绝即拒绝**，`X-Gateway-Name` 是第一个拒绝的网关。
- 不合并多个网关的规则：每个网关按自己的契约独立校验，全部通过才放行。
- **Body 只读一次**：先对所有网关做“不依赖 body”的判定（1–5，含 path/query/header 的过滤）；全部通过且
  至少一个网关声明了 `body` 时，再读取一次请求体，按网关顺序做第 6–9 步。这样不会为了一个会被 405 拒绝的
  请求去读 body，也不会重复读。
- 未声明路径（`forward`）不进入参数校验；但同一 location 里其他网关若声明了该路径，仍按它们的规则校验。
- 分组展开发生在**加载时**：access 阶段每条叶子路由看到的都是已经合并好的方法、正则、参数规则和过滤开关。

### 5.6 错误与日志（Header / Query / Body / 路径参数 / filters 已实现）

| 场景 | 状态码 | `X-Gateway-Reason` | `X-Gateway-Field` |
| --- | --- | --- | --- |
| 必填字段缺失（`required: true`） | `400` | `missing-parameter` | 有 |
| 条件必填字段缺失（`required_if` 成立） | `400` | `missing-parameter` | 有（指向**本字段**） |
| 类型/范围/长度/枚举/正则不符 | `400` | `invalid-parameter` | 有 |
| 同名参数重复 | `400` | `invalid-parameter` | 有 |
| 未声明字段且 `unknown: "reject"` | `400` | `invalid-parameter` | 有（请求中的字段名） |
| SQLi / XSS 过滤命中 | `400` | `invalid-parameter` | 有（命中所在的 dimension.name） |
| 百分号编码 / UTF-8 / JSON / Form 解析失败 | `400` | `invalid-parameter` | 能定位时才有 |
| `Content-Type` 不支持、`charset` 不支持、`Content-Encoding` 非 identity | `400` | `unsupported-media-type` | 无 |
| Body 超过上限 | `413` | `body-too-large` | 无 |

> 当前真正实现并写日志的是 Header / Query / Body 三个维度：必填缺失（`rule=required`）、条件必填缺失
> （`rule=required_if`）、重复（`rule=duplicate`）、值规则失败
> （`rule=char|int|bool|values|min|max|min_len|max_len|len|regex`）、未声明字段（`rule=unknown`），
> 以及 query/form 解码失败（`rule=encoding`）、JSON 解析失败（`rule=json`）、
> 媒体类型/charset/Content-Encoding（`rule=content-type|charset|content-encoding`）。
> 第 5 行 SQLi/XSS 过滤已实现。

日志（`error_log` notice）只写判定所需信息，不写字段值：

```text
gateway: 400 GET /api/orders reason=invalid-parameter field=body.qty rule=int gateway=myrustwebdemo1
gateway: 400 POST /api/login reason=missing-parameter field=header.x-api-version rule=required gateway=myrustwebdemo1
gateway: 400 GET /api/orders reason=invalid-parameter field=header.authorization rule=duplicate gateway=myrustwebdemo1
gateway: 400 GET /api/orders reason=missing-parameter field=header.x-card-last4 rule=required_if gateway=myrustwebdemo1
gateway: 400 POST /api/orders reason=missing-parameter field=body.card_last4 rule=required_if gateway=myrustwebdemo1
gateway: 400 GET /api/coupon reason=invalid-parameter field=query.coupon rule=regex gateway=myrustwebdemo1
gateway: 400 GET /api/user/1 reason=invalid-parameter field=query.kw rule=filter filter=sqli gateway=myrustwebdemo1
gateway: 413 POST /api/echo reason=body-too-large limit=1048576 gateway=myrustwebdemo1
```

- `field` 的格式与 `X-Gateway-Field` 相同；`rule` 只写类型或约束名（如 `required` / `required_if` / `int` / `max_len` / `regex` / `duplicate` / `filter`），不写具体阈值，也不写正则内容。
- 过滤命中的日志额外带 `filter=sqli` 或 `filter=xss`（多个规则命中时只记第一个，`sqli` 先于 `xss`）。
- 不记录 body 内容、header 值、query 值（可能含密码、token、个人信息），**也不记录命中的片段**；
  需要复现时用请求 ID / access log 定位。
- 契约加载失败时，日志要包含路由序号、字段路径（如 `routes[3].body.params.qty.min`）与原因。

### 5.7 请求体读取与 nginx 配置（✅ 已实现）

协议只要求“有上限、不部分解析”，具体上限由 nginx 配置控制。推荐：

```nginx
http {
    # 请求体硬上限：超过时 nginx 先返回 413（早于插件，通常没有 X-Gateway-*）
    client_max_body_size 1m;

    # 只影响 body 是否落临时文件，不影响协议语义
    client_body_buffer_size 16k;
    client_body_temp_path   logs/tmp/client_body;

    # 请求行 + 请求头的大小上限；超限由 nginx 返回 400/414
    large_client_header_buffers 4 8k;

    # 默认即可：网关在 access 阶段读过的 body 由 nginx 缓冲，proxy 阶段复用
    # proxy_request_buffering on;
}
```

约定：

- 只在路由声明了 `body` 时才读请求体；读取发生在 access 阶段，读完后再交给 `proxy_pass`。
- 请求体上限取 `client_max_body_size`；`Content-Length` 超过上限直接 `413`。
  `Transfer-Encoding: chunked` 时读到“上限 + 1 字节”即判定超限，不缓存完整 body。
- 超过上限**不做部分解析**，不会出现“前半段字段校验通过”的情况。
- nginx 可能在插件之前就因 `client_max_body_size` 返回 `413`，此时没有 `X-Gateway-Reason` / `X-Gateway-Name`；
  客户端不能假设 413 一定来自插件。
- `client_body_timeout`、客户端中途断开由 nginx 处理；插件不把“读失败”当成“空 body 放行”。
- `Content-Encoding: gzip` 等不自动解压；要支持压缩请求体，必须在协议层新增字段，不能默认假设。
- `proxy_request_buffering off` 与 access 阶段读 body 的组合未验证；启用 body 校验的 location 建议保持默认 `on`。
- **当前实现**：body 在 access 阶段由插件调用 `ngx_http_read_client_request_body` 读取，支持内存中的 buffer chain；
  请求体超过 `client_body_buffer_size` 落临时文件时，插件按 `temp_file->offset` 从临时文件读回；
  读取失败才返回 `500`，不会误放行。
  demo 配置把 `client_body_buffer_size` 设为 `1m`，与 `client_max_body_size` 一致。
- body 读完后由插件把控制权交回 phase engine，`proxy_pass` 复用已缓冲的 body，不会重复读取；大 body 的分片读取
  （异步 post handler）已由 `tests/phase1.sh` 的 `--limit-rate` 用例覆盖。

### 5.8 完整示例（Query / Body / path_params / filters 已实现；path_regex / priority / responses 已实现）

契约：

```json
{
  "protocol": 1,
  "version": "params-demo",
  "routes": [
    {
      "path": "/api/orders/{order_id}",
      "methods": ["POST"],
      "path_params": {
        "params": {
          "order_id": { "type": "int", "min": 1 }
        }
      },
      "query": {
        "params": {
          "dry_run": { "type": "bool" },
          "trace":   { "type": "char", "max_len": 32 }
        },
        "unknown": "ignore"
      },
      "headers": {
        "params": {
          "authorization": true,
          "x-api-version": { "type": "enum", "values": ["1", "2"], "required": true }
        }
      },
      "body": {
        "params": {
          "sku":  { "type": "char", "required": true, "min_len": 1, "max_len": 64 },
          "qty":  { "type": "int", "required": true, "min": 1, "max": 99 },
          "note": { "type": "char", "max_len": 200 },
          "payment_method": { "type": "enum", "values": ["card", "cash"] },
          "card_last4": {
            "required_if": { "field": "payment_method", "equals": "card" },
            "regex": "^[0-9]{4}$"
          },
          "coupon": { "regex": "^[A-Z0-9]{4,16}$" }
        },
        "unknown": "reject",
        "content_types": ["application/json"]
      }
    }
  ]
}
```

请求与结果（基线请求默认带 `Authorization: Bearer demo` 与 `X-Api-Version: 2`，
body 为 `{"sku":"kb-1","qty":2,"payment_method":"cash"}`）：

| 请求 | 结果 |
| --- | --- |
| `POST /api/orders/42?dry_run=1`，基线头与 body | 放行 |
| 同上但缺少 `Authorization` | `400 missing-parameter`，`X-Gateway-Field: header.authorization`（`authorization: true` 只要求存在） |
| 同上但缺少 `X-Api-Version` | `400 missing-parameter`，`X-Gateway-Field: header.x-api-version` |
| 同上但 `X-Api-Version: 3` | `400 invalid-parameter` |
| 同上但 `?dry_run=yes` | `400 invalid-parameter`，`X-Gateway-Field: query.dry_run` |
| 同上但 `?extra=1` | 放行（`query.unknown` 默认 `ignore`） |
| 同上但 body 多一个 `"price": 1` | `400 invalid-parameter`，`X-Gateway-Field: body.price`（Mass Assignment 拦截） |
| 同上但 `Content-Type: text/plain` | `400 unsupported-media-type` |
| 同上但 `qty: 0` | `400 invalid-parameter`，`X-Gateway-Field: body.qty` |
| 同上但 body 改为 `{"sku":"kb-1","qty":2,"payment_method":"card"}` | `400 missing-parameter`，`X-Gateway-Field: body.card_last4`（`required_if` 成立） |
| body 为 `{"sku":"kb-1","qty":2,"payment_method":"card","card_last4":"12a4"}` | `400 invalid-parameter`，`X-Gateway-Field: body.card_last4`（`regex` 不通过） |
| 基线 body 追加 `"coupon":"abc"` | `400 invalid-parameter`，`X-Gateway-Field: body.coupon`（`regex` 要求 4–16 位大写字母/数字） |
| `POST /api/orders/0` | `400 invalid-parameter`，`X-Gateway-Field: path.order_id` |
| `POST /api/orders/%34%32` | `400 invalid-parameter`（路径参数不解码） |

### 5.9 字段过滤：SQLi / XSS（✅ 已实现）

> **实现说明**：插件使用第三方 `libinjectionrs`（libinjection 的纯 Rust port）的
> `detect_sqli` / `detect_xss`，不在网关内自维护正则规则集。

`filters` 是**可分级传递的开关**，用于在网关侧拦掉明显的注入/脚本载荷。它不替代参数的类型/范围校验，
也不替代后端的转义与参数化查询；定位是“提前挡掉明显攻击 + 留下可观测日志”。

```json
{
  "filters": { "sqli": true, "xss": true },
  "routes": [
    { "path": "/api/login", "methods": ["POST"], "filters": { "sqli": true } }
  ]
}
```

| 键 | 类型 | 默认 | 说明 |
| --- | --- | --- | --- |
| `sqli` | bool | `false` | 开启 SQL 注入特征检测 |
| `xss` | bool | `false` | 开启 XSS 特征检测 |
| `scope` | string[] | `["path","query","header","body"]` | 只检查列出的维度；仅在本块开启 `sqli`/`xss` 时生效 |

- 可写在**顶层**、**分组**、**路由**；继承采用**或**语义：任一层写 `true` 即对该叶子路由生效。
  子级写 `false` 不能关闭父级的开关（显式写出只为表达意图，不改变结果）。
- `scope` 可限制过滤维度，取值 `path` / `query` / `header` / `body`，省略时四个维度全开：
  ```json
  { "filters": { "sqli": true, "xss": true, "scope": ["query", "body"] } }
  ```
- `scope` 是**共享维度集合**，同时作用于 `sqli` 与 `xss`；各层取**并集**，子层不能收窄父层
  已开启的维度。要排除某个维度，请在更小的分组/路由上开启过滤，并只写需要的 `scope`。
- 同一块写了 `scope` 但没有把 `sqli` / `xss` 设为 `true` → 拒绝；空数组、重复维度、未知维度
  （例如 `cookie`）→ 拒绝。这样避免“以为收窄了，其实没有生效”。
- 只接受 JSON 布尔；`"true"`、`1`、`null` → 契约拒绝加载；未知键（如 `"csrf": true`）→ 契约拒绝加载。
- `scope` 必须是数组且元素只能是 `path` / `query` / `header` / `body`；`"query"` 字符串写法 → 拒绝。
- 分组展开时按 3.4.2 合并；access 阶段只读合并后的结果，不再回溯分组。
- 顶层 `filters` 对**所有叶子路由**生效；对“未命中 → `forward`”的请求不生效（没有叶子路由就没有规则）。
- 被网关 `forward` 的路径等于完全没有保护，需要覆盖时请为该前缀补一条 `{*rest}` 路由 + 分组规则。

#### 5.9.1 检查哪些值

过滤器只检查**网关已经解析出来的字符串值**，不重新扫描原始字节：

| 维度 | 前提 | 被检查的值 |
| --- | --- | --- |
| `path` | 路由含 `{name}` / `{*rest}` | 3.3 提取出的**原始、未解码**参数值；静态段不检查 |
| `query` | 路由（或分组）有 `query` 块 | 所有出现值的**一次百分号解码**结果，含未声明参数（`unknown: "ignore"` 时它们仍会转发） |
| `header` | `headers.params` 里显式声明的 header | 去掉首尾 OWS 后的值；未声明的 header 不检查 |
| `body` | 路由（或分组）有 `body` 块且解析成功 | JSON 的字符串值 / form 的字符串值；数字与布尔不检查，也不转成字符串 |

推论：

- **没有参数块 = 不解析 = 不扫描**。只想过滤但不想加类型规则时，写空规则块即可，例如
  `"query": { "params": {} }`、`"body": { "params": {}, "unknown": "ignore" }`。
- 路径维度不需要 `path_params` 块：路径参数本来就要提取。
- 字段名（key）不检查，只检查值：避免把 `order_by` 这类正常键名误判。
- 不检查 JSON 的嵌套对象/数组内部（本版不支持嵌套声明，也不会递归扫描）。
- 不解码路径值，不做二次解码、Unicode 归一化或 HTML 实体解码；`%2527` 这类双重编码不会在这里还原，
  必须由后端继续防御。这与 3.3 的“不猜测后端解码行为”一致。

#### 5.9.2 检测规则

实现使用 `libinjectionrs` 的检测器；下表是协议要求覆盖的最低特征集合，作为验收参考。
升级库版本时应在 `version` 或发布说明里体现：

`sqli`：

| 类别 | 至少覆盖 |
| --- | --- |
| 联合/子查询 | `union\s+select`、`select\s+.+\s+from` |
| 写操作 | `insert\s+into`、`update\s+.+\s+set`、`delete\s+from`、`drop\s+(table\|database)`、`alter\s+table` |
| 元数据/命令 | `information_schema`、`xp_cmdshell`、`exec\s*\(` |
| 时间盲注 | `sleep\s*\(`、`benchmark\s*\(`、`pg_sleep\s*\(` |
| 布尔/注释 | `'\s*(or\|and)\s+'?\w`、`"\s*(or\|and)\s+"?\w`、`--\s`、`/\*`、`\*/`、`;\s*(select\|insert\|update\|delete\|drop)\b` |

`xss`：

| 类别 | 至少覆盖 |
| --- | --- |
| 标签 | `<script`、`</script`、`<iframe`、`<object`、`<embed`、`<svg`、`<img` |
| 事件属性 | `on[a-z]+\s*=`（如 `onerror=`、`onload=`） |
| 协议 | `javascript:`、`vbscript:`、`data:text/html` |
| 执行/表达式 | `document\.cookie`、`alert\s*\(`、`expression\s*\(` |

约定：

- 匹配语义是“值中**包含**任一模式”，不是全匹配；匹配统一按 ASCII 大小写不敏感处理。
- 值长度受 query 参数对上限与 body 大小上限约束；匹配必须是线性时间（Rust `regex` 默认即线性）。
- `sqli` 与 `xss` 同时命中时只报 `sqli`（固定顺序 `sqli` → `xss`），只返回第一个错误。
- 规则集变化属于**内容修订**，应在 `version` 或发布说明里体现；语义变化（例如改成解码后扫描）需要改协议。
- 误报处理：过滤器只应作为额外防线；业务确实需要提交 SQL 片段/HTML 时，在该路由上不要开启对应开关。
  由于子级 `false` 不能关闭祖先的开关，排除某条路由时要把开关放在**更小的分组**或只写在需要的路由上，
  不要把 `sqli` / `xss` 放在覆盖全站的顶层。
- 该检测**不保证**覆盖所有注入变体，也不对响应做输出编码；XSS 的最终防线仍是后端的上下文转义与 CSP。

#### 5.9.3 执行与错误

- 执行位置见 5.5：每个维度在自己的参数规则通过后立即过滤，`body` 在字段校验之后。
- 命中 → `400`，`X-Gateway-Reason: invalid-parameter`，`X-Gateway-Field: <dimension>.<name>`；
  日志带 `rule=filter filter=sqli|xss`，不记录命中的值或片段（见 5.6）。
- 同一维度内的顺序：先按契约顺序检查已声明字段，再按请求出现顺序检查其余值（query/body）；
  header 只按契约顺序；path 按参数在路径中的出现顺序。保证同一请求每次得到同一个错误字段。
- 过滤失败**不会**回退到 `forward`，也不会因为 `unmatched: "forward"` 而放行。

```json
{
  "prefix": "/api",
  "filters": { "sqli": true },
  "headers": {
    "params": {
      "authorization": { "type": "char", "required": true, "min_len": 8 }
    }
  },
  "router": [
    {
      "path": "/search",
      "methods": ["GET"],
      "filters": { "xss": true },
      "query": { "params": { "kw": { "type": "char", "max_len": 64 } } }
    }
  ]
}
```

上例的 `/api/search` 同时开启 `sqli`（继承）与 `xss`（本路由）；`kw` 值含 `<script` 或 `union select`
都会 `400 invalid-parameter`，`X-Gateway-Field: query.kw`。

---

## 6. 响应覆盖（✅ 当前 core 拒绝类响应）

默认值见第 4 节。后端可选择覆盖“网关拒绝时”的状态码与响应体（只允许拒绝类响应）：

```json
{
  "protocol": 1,
  "responses": {
    "route_not_declared":    { "status": 404, "body": "{\"error\":\"not_found\"}" },
    "method_not_allowed":    { "status": 405 },
    "contract_not_ready":    { "status": 503, "retry_after": 5 },
    "missing_parameter":     { "status": 400 },
    "invalid_parameter":     { "status": 400 },
    "unsupported_media_type":{ "status": 400 },
    "body_too_large":        { "status": 413 }
  }
}
```

> **实现范围**：`route_not_declared` / `method_not_allowed` / `missing_parameter` / `invalid_parameter` /
> `unsupported_media_type` 已接入运行时（状态码、body、`Retry-After`）。
> `contract_not_ready` / `body_too_large` 仍只做契约严格校验；nginx 预检 413 也可能早于插件。

约束：

- `status` 只能是 `4xx` / `5xx`，禁止把拒绝改成放行（`2xx` / `3xx` 一律视为非法契约）。
- `body` 最长 1 KiB，默认 `Content-Type: application/json`；诊断头是否保留取决于 `gateway_diagnostics`
  （见 4.2），`Allow` 与 `X-Request-Id` 不受影响。
- 缺少的键沿用第 4 节默认值；`Allow` 仍由方法校验自动生成。
- 未实现前，顶层出现 `responses` 会被严格模式拒绝。

---

## 7. 后端生成契约的检查清单

- [ ] `protocol` 明确写出（缺省按 1），当前只有 `1`。
- [ ] `routes` 覆盖**所有**真实对外路由；漏写 = 线上 404（默认拒绝）。
- [ ] 确实需要“未声明也转发”时才用 `"unmatched": "forward"`，并确保后端自己兜底 404。
- [ ] 每个 `path` 以 `/` 开头、无尾斜杠、无查询串；参数路由用 `{name}` / `{*rest}`。
- [ ] `methods` 与后端实际注册一致；不限方法才用 `[]`（慎用）；需要 `HEAD` 就显式写；
       方法名只能是 RFC 9110 token（不要写 `"GET, POST"`，那会被整份拒绝）。
- [ ] 不输出本文未定义（或尚未支持）的字段。
- [ ] **分组 / `path_regex` / `filters` 实现前不要输出**：`prefix` / `router` / `path_regex` / `filters`
      目前都会触发严格模式拒绝加载。
- [ ] **Header 支持存在性、第一批值规则与 `required_if`**：可以输出 `authorization: true`、
      `{ "required": true|false }`，以及 `type`（`char` / `int` / `bool` / `enum`）、`min` / `max`、
      `min_len` / `max_len` / `len`、`values` / `ignore_case` / `regex`、`required_if`；
      分组继承、`filters`、路由级 `path_regex`、`priority` 与 `responses` 均已实现。
- [ ] **其他参数校验字段实现前不要输出** `path_params` / `query` / `body`（会被整份拒绝）；
      Header 的上述规则除外。
- [ ] 路径参数规则的名字与 `{name}` 完全一致；路径参数是原始值，通常用 `int` 时要求客户端不要编码。
- [ ] 只给需要白名单的维度写参数块；不写 = 完全不校验（保持旧行为）。
- [ ] 公共检查（JWT header、路径正则、Query 规则、`filters`）优先放进**分组**，避免每条路由重复；
      同名参数规则不要在分组和子路由里重复定义（会拒绝加载）。
- [ ] 分组 `prefix` 只能是字面量，不能写 `{name}` / `{*rest}`；子路由 `path` 仍以 `/` 开头。
- [ ] 分组 `methods` 与子路由 `methods` 取交集；交集为空会让整份契约拒绝加载。
- [ ] 分组 `unmatched: "forward"` 只覆盖“该前缀下未命中”的路径，不覆盖方法/参数失败；迁移结束后改回 `deny`。
- [ ] `path_regex` 作用于完整原始路径且是**全匹配**；分组正则也要写出 `prefix` 部分（不是相对路径），
      需要前缀匹配时显式写 `.*`，并使用 Rust `regex` 语法。
- [ ] 开启 `filters` 的维度必须真的被解析：query/body 要有对应参数块，header 要声明具体头，否则扫不到；
      过滤在参数校验之后执行，命中返回 `400 invalid-parameter`，日志 `rule=filter filter=sqli|xss`。
- [ ] `filters` 只是额外防线；需要提交 SQL/HTML 的路由不要开对应开关，后端仍必须参数化查询与输出转义。
- [ ] `body.unknown` 默认 `reject`；确实要透传额外字段时显式写 `"ignore"`，并确认后端自己做了权限校验。
- [ ] `body.content_types` 与后端实际接受的一致；不要让网关放行后端不解析的类型。
- [ ] 参数值类型只用 `char` / `int` / `bool` / `enum`；不要写数组、嵌套对象，也不要用 `regex` 代替数值范围。
- [ ] 只判断 key 是否存在时用 `true` 简写（等价 `{ "required": true }`），不要为了“看起来完整”强行写 `type`；
       规则不能是空对象 `{}`，`false` / `null` / 字符串 / 数字都会被拒绝加载。
- [ ] 字段格式约束用 `regex`（Rust regex、最长 256、**全匹配**、只用于字符串，不能和 `enum` 同时用）；
       需要“包含某模式”时自己写 `.*pattern.*`，不要假设是子串匹配。
- [ ] `required_if` 只引用同一参数块里的字段，不能引用自己；`required_if` 与 `required: true` 不能同时写；
       路径参数规则禁止 `required` / `required_if` / `true` 简写。
- [ ] 需要限制请求体大小时，在 nginx 配 `client_max_body_size`，不要依赖后端自己截断。
- [ ] `/info` 自身也在 `routes` 里。
- [ ] 契约损坏、超时、非 200 时网关会保留旧契约；后端需监控 `/info` 的可用性。

---

## 8. 插件解析契约的检查清单

- [ ] 只接受 `protocol == 1`；其他值整份拒绝并记录日志。
- [ ] 顶层/路由/分组/参数规则出现未定义字段 → 整份拒绝（严格模式）。
- [ ] 任一路由非法 → 整份拒绝（不使用半份契约）。
- [ ] `methods` 元素是 RFC 9110 token（1–32 字符）：空串/空白、CR/LF、空格、逗号 → 整份拒绝；
      路由与分组都要校验（`Allow` 与日志会原样输出该方法名）。
- [ ] 路径参数名必须是 `[A-Za-z_][A-Za-z0-9_]{0,63}` 且同一路由内不重复——无论是否写了
      `path_params`；否则 matchit 会把它当成任意段通配，而规则名又无法引用它。
- [ ] 单条路由（含分组继承）的规则合计 ≤256、单块 ≤128 → 否则整份拒绝。
- [ ] 参数规则做完整的静态校验：类型与约束匹配、`min <= max`、`min_len <= max_len`、
      `len` 与 `min_len`/`max_len` 互斥、枚举非空且不重复、header 名合法、路径参数名存在于路由、
      路径参数规则不得写 `required` / `required_if` / `true` 简写。
- [ ] **Header 切片**：接受 `true` / `{ "required": true|false }` / 第一批值规则
      （`type` / `min` / `max` / `min_len` / `max_len` / `len` / `values` / `ignore_case` / `regex`）
      / `required_if`；分组继承与 `filters` 等其他字段整份拒绝。`headers.params` 最多 128 条；
      名字必须是 1–128 字符的 HTTP token，同一契约内按小写去重；完全相同的 JSON 重复键也拒绝
      （不取最后一个值）。运行时先报无条件必填缺失、再报条件必填缺失（`missing-parameter`），
      再报同一头多行（`invalid-parameter`），最后按契约顺序校验值（`invalid-parameter`）。
- [ ] 值规则静态校验：约束必须与 `type` 匹配、`min <= max`、`min_len <= max_len`、`len` 与长度区间互斥、
      `enum` 的 `values` 非空且（按 `ignore_case` 归一后）不重复、`regex` 可编译且不与 `enum` 同用。
- [ ] **路径参数切片**：`path_params.params` 最多 128 条；规则名必须出现在该路由的 `{name}` / `{*rest}` 中；
      名字匹配 `[A-Za-z_][A-Za-z0-9_]{0,63}`；禁止 `required` / `required_if` / `true` / `false`；
      校验值是**原始未解码**路径段，`{*rest}` 不含开头 `/`。
- [ ] **分组切片（当前范围）**：`prefix` 必须是字面量、无尾斜杠、≤512；展开后完整路径/前缀不能重复；
      嵌套深度 ≤8、叶子路由 ≤4096；`methods` 取交集且 `Allow` 用有效方法；
      `headers.params` / `query.params` / `body.params` / `path_params.params` 按结构父子合并，同名跨层重复拒绝；
      `query.unknown` / `body.unknown` 取更严格者，`body.content_types` 取交集且不能为空；
      `unmatched` 按最长分组前缀 + 结构父链选择，`forward` 日志写提供策略的 `scope`；
      分组 `path_regex` 对后代 AND，全匹配完整路径，失败按“未命中”处理。
      分组 `filters` 按或语义继承。
- [ ] **Query 切片**：`query.params` 最多 128 条，名字 1–128 个 UTF-8 字符且不含控制字符；
      原始 query 一次解码（`%XX`、`+`）、解码后 UTF-8、空 key / 同名 / 超过 256 对 → `400`；
      `unknown` 缺省 `ignore`，`reject` 时在声明字段校验完后报未声明 key。
- [ ] **Body 切片**：`body.params` 最多 128 条；`unknown` 缺省 `reject`；`content_types` 只能包含
      `application/json` / `application/x-www-form-urlencoded`，大小写不敏感去重；
      非空 body 必须有 `Content-Type`，`charset` 只接受 UTF-8，`Content-Encoding` 只接受 identity；
      JSON 顶层必须是对象、重复键拒绝、声明字段必须是标量；form 与 Query 同一套解码规则；
      空 body 按空对象处理且不要求 `Content-Type`。
- [ ] 只有声明了 `body` 的路由才读 body；读一次后 proxy 复用；body 落临时文件时从文件读回；读取失败不能误放行。
- [ ] 规则形态校验：`true` 等价 `{ "required": true }`；`false` / `null` / 字符串 / 数字 / 空对象 `{}` → 整份拒绝；
      省略 `type` 时不做类型/范围/长度校验，存在性只看 `required` / `required_if`，出现值再按 `regex`（若写）校验；
      写了 `type` 才校验范围/长度，约束必须与类型匹配。
- [ ] `required_if` 校验（Header / Query / Body）：引用同一参数块内已声明的字段、不得自引用、
      不得与 `required: true` 同时出现；字符串形式非空；对象形式只允许 `field` / `equals` 两个键，
      `field` 非空、`equals` 可为空串；`equals` 只与字符串值比较；条件不成立时本字段仍按自身规则校验。
- [ ] `regex` 校验：Rust `regex`、最长 256、编译失败整份拒绝、全匹配语义；只能配 `char` 或省略 `type`，
      不得与 `enum` / `values` 同时出现；不通过返回 `400 invalid-parameter`，日志 `rule=regex`。
- [ ] 成功才替换快照；失败保留 last-known-good。
- [ ] 从未成功加载过 → 受保护请求 `503 contract-not-ready`。
- [ ] access 阶段不做网络 I/O，只读内存快照。
- [ ] 路径匹配使用原始 request target 的 path；参数提取不解码，校验顺序按 5.5。
- [ ] 只有声明了 `body` 的路由才读请求体；多网关时只读一次；超限 413，不部分解析。
- [ ] 参数错误只返回**第一个**、状态码与 reason token 按 5.6；日志不写字段值。
- [ ] 参数块缺省时，行为与本协议实现前完全一致（回归测试守这条）。
- [ ] 分组在**加载时**展开为叶子路由：拼接完整路径、校验前缀/深度/数量上限、合并方法/正则/参数/过滤，
      同名参数规则跨层重复、方法交集为空、`content_types` 交集为空都要整份拒绝。
- [ ] 分组展开后与顶层直接写的路由一起建树；完整路径重复或形状冲突 → 整份拒绝。
- [ ] `path_regex` 编译失败或超长 → 整份拒绝；不匹配 → 按“路径未命中”处理并选 `unmatched` 作用域。
- [ ] `filters` 合并为“任一层开启即开启”；只按 5.9 的规则集检查已解析的字符串值，命中返回
      `400 invalid-parameter` + `X-Gateway-Field`，日志 `rule=filter filter=sqli|xss`。
- [ ] `filters` 不会因为 `unmatched: "forward"` 而生效；`forward` 的路径没有过滤保护。

---

## 9. 示例

### 9.1 最小契约

```json
{
  "protocol": 1,
  "routes": [
    { "path": "/api/health", "methods": ["GET"] }
  ]
}
```

### 9.2 当前 demo 后端实际返回（`/info`，默认 `deny`）

```json
{
  "protocol": 1,
  "version": "phase1-fixed",
  "meta": { "owner": "demo_server", "generated_by": "handwritten" },
  "routes": [
    { "path": "/info",                  "methods": ["GET"] },
    { "path": "/api/health",            "methods": ["GET"] },
    { "path": "/api/login",             "methods": ["POST"] },
    { "path": "/api/user/{id}",         "methods": ["GET"] },
    { "path": "/api/user/{id}/profile", "methods": ["GET", "PUT"] },
    { "path": "/api/products",          "methods": ["GET"] },
    { "path": "/api/orders",            "methods": ["POST"] },
    { "path": "/api/echo",              "methods": ["POST"] }
  ]
}
```

### 9.2.1 当前可用的 Header 存在性 + 值规则示例（✅）

```json
{
  "protocol": 1,
  "routes": [
    {
      "path": "/api/admin",
      "methods": ["GET"],
      "headers": {
        "params": {
          "authorization": true,
          "x-request-id": { "required": true },
          "x-trace-id": { "required": false },
          "x-api-version": { "type": "enum", "values": ["1", "2"], "required": true },
          "x-page-size": { "type": "int", "min": 1, "max": 100 },
          "x-code": { "regex": "[A-Z]{2}[0-9]{2}" },
          "x-payment-method": { "type": "enum", "values": ["card", "cash"] },
          "x-card-last4": { "required_if": { "field": "x-payment-method", "equals": "card" }, "regex": "[0-9]{4}" }
        }
      }
    }
  ]
}
```

行为：`authorization` / `x-request-id` / `x-api-version` 缺失 → `400 missing-parameter`；
`x-trace-id` 可有可无；任一声明头出现多行 → `400 invalid-parameter`；
`x-api-version` 不在 `["1","2"]`、`x-page-size` 不是合法 int 或越界、`x-code` 不满足全匹配正则
→ `400 invalid-parameter` + `X-Gateway-Field`。
`x-payment-method: card` 时缺 `x-card-last4` → `400 missing-parameter`（`rule=required_if`）；
`x-payment-method: cash` 时 `x-card-last4` 可选。分组继承、`filters`、路由级 `path_regex` 与 `responses` 均已实现。

### 9.3 带参数校验的契约（Query / Body / 路径参数 / filters 已实现）

见 5.8。Query / Header / Body / `path_params` 部分可以加载并有上面的行为；
其中的 `filters` 等未实现字段仍会让整份契约被拒绝。

### 9.4 分组 + 继承 + 过滤（✅ 当前范围）

```json
{
  "protocol": 1,
  "version": "grouped-demo",
  "unmatched": "deny",
  "routes": [
    {
      "prefix": "/api",
      "methods": ["GET", "POST"],
      "unmatched": "forward",
      "filters": { "sqli": true },
      "headers": {
        "params": {
          "authorization": { "type": "char", "required": true, "min_len": 8, "max_len": 4096 }
        }
      },
      "router": [
        { "path": "/health", "methods": ["GET"] },
        {
          "prefix": "/v1",
          "filters": { "xss": true },
          "router": [
            {
              "path": "/user/{id}",
              "methods": ["GET"],
              "path_params": { "params": { "id": { "type": "int", "min": 1 } } },
              "query": { "params": {}, "unknown": "ignore" }
            },
            {
              "prefix": "/admin",
              "methods": ["GET"],
              "unmatched": "deny",
              "router": [
                { "path": "/audit", "methods": ["GET"] }
              ]
            }
          ]
        }
      ]
    }
  ]
}
```

展开与效果：

| 请求 | 结果 |
| --- | --- |
| `GET /api/health`，`Authorization: Bearer x` | 放行；受 `sqli` 过滤（没有 query 块，query 不解析） |
| `GET /api/health`，无 `Authorization` | `400 missing-parameter`，`field=header.authorization` |
| `GET /api/v1/user/42`，合法 Authorization | 放行；`sqli` + `xss` 均生效 |
| `GET /api/v1/user/abc` | `400 invalid-parameter`，`field=path.id` |
| `GET /api/v1/user/42?kw=union%20select` | `400 invalid-parameter`，`field=query.kw`（空 `query` 块开启了解析与扫描） |
| `GET /api/v1/unknown` | 转发给后端（继承 `/api` 的 `unmatched: "forward"`），日志记录作用域 `/api` |
| `GET /api/v1/admin/unknown` | `404`（`/admin` 显式 `deny`，不继承 `/api` 的 forward） |
| `POST /api/health` | `405`（组 `GET/POST` ∩ 路由 `GET`） |

注意：`/user/{id}` 上的 `"query": { "params": {}, "unknown": "ignore" }` 不限制字段，只是为了**开启解析**，
让 `filters` 能扫到 query 值。省略这个块时 query 完全不解析，`?kw=union%20select` 会原样转发给后端。

给 `/user/{id}` 再加一条 `"path_regex": "^/api/v1/user/[0-9]+$"`，那么 `/api/v1/user/42abc` 虽然能匹配
`{id}` 的形状，但会在正则处失败，按 3.5 视为**路径未命中**：这里继承 `/api` 的 `forward`，请求交给后端；
如果 `/api` 没开 forward，就是 `404`，**不会**变成 `400 invalid-parameter`。

---

## 10. 变更记录

| 日期 | 版本 | 变更 |
| --- | --- | --- |
| 2026-10-26 | v1 | 落地 `protocol` / `version` / `ttl` / `meta` / `routes[]`；严格路径、方法校验、`404/405/503` 与 `X-Gateway-Reason`；严格模式拒绝未知字段 |
| 2026-10-26 | v1 | 新增顶层可选 `unmatched`（`deny` 默认 / `forward` 交给后端）——开发期仍在 v1，不升版本号 |
| 2026-10-26 | v1 | **定稿**路径匹配条款：原始路径、大小写敏感、严格尾斜杠、不解码/不合并、静态 > 参数 > 通配、冲突整份拒绝；明确 `HEAD` 不隐含于 `GET`；新增 3.3 路径参数提取 |
| 2026-10-26 | v1 | **定稿**参数校验协议（待实现）：`path_params` / `query` / `body` / `headers` 四个可选块，统一值规则（`char` / `int` / `bool` / `enum`）、`unknown` 策略、`content_types`、校验顺序、`400/413` 与 `X-Gateway-Field`、body 读取上限与 nginx 配置要求 |
| 2026-10-26 | v1 | 新增**分组路由**（`prefix` + `router`）：字面量前缀、加载时展开、方法与参数规则继承、同名规则跨层重复拒绝、分组 `unmatched` 作用域；新增 **`path_regex`**（Rust regex、全匹配、不通过按未命中处理）；新增 **`filters.sqli/xss`** 分级开关与检测规则、执行顺序、日志字段 |
| 2026-10-26 | v1 | 参数规则新增 **`true` 简写**（等价 `{ "required": true }`）、**`type` 可省略**（只做存在性检查）、**`required_if` 条件必填**、**字段级 `regex`**（Rust regex、全匹配、仅字符串）；空规则 `{}` 与 `false`/`null` 拒绝加载；同步校验顺序、错误/日志、检查清单与示例 |
| 2026-10-26 | v1 | **实现 Header 存在性切片**：`headers.params.<name>` 支持 `true` / `{ "required": true\|false }`；缺失 `400 missing-parameter`、同一头多行 `400 invalid-parameter`，回写 `X-Gateway-Field` 与 `rule=required\|duplicate`；Header 名大小写不敏感并按 token 校验。值规则仍未实现 |
| 2026-10-26 | v1 | **实现 Header 值规则第一批**：`type`（`char` / `int` / `bool` / `enum`）、`min` / `max`、`min_len` / `max_len` / `len`、`values` / `ignore_case`、`regex`（全匹配）；Header 值去首尾 OWS，非 UTF-8 在声明值规则时 `400 invalid-parameter`；顺序为必填 → 重复 → 值校验；日志 `rule=` 写类型/约束名。`required_if` / `filters` / Query / Body / 路径参数仍未实现 |
| 2026-10-26 | v1 | **JSON 结构扫描**：`body.max_depth`（默认 32，上限 128）、`max_string_length`（默认 8192 字节）、`max_array_entries`（默认 500）；解析前单遍扫描、组继承、`0` 关闭；命中返回 400 与 `rule=json-*`。 |
| 2026-10-26 | v1 | **Body 增加 `max_bytes`**：原始 body 字节上限，分组继承、子级取更小值、`0` 关闭；Content-Length 超限时在读 body 前 `413 body-too-large`，chunked 读后检查；不替代 nginx `client_max_body_size`。 |
| 2026-10-26 | v1 | **filters 增加 `scope`**：可限制只检查 `path` / `query` / `header` / `body` 中的若干维度；默认四个全开，跨层取并集，子层不能收窄；空/重复/未知维度、未同时开启过滤器时整份拒绝。 |
| 2026-10-26 | v1 | **`gateway_info_url` 支持本地自签 HTTPS**：可选第三参数 `allow-localhost-ca`，仅限 `https://localhost` / `127.0.0.1` / `[::1]`，关闭该网关的证书校验并输出 WARN。 |
| 2026-10-26 | v1 | **契约拉取不跟随重定向**：`max_redirects(0)`，只接受 `2xx`；`30x` 等一律按 `bad-status` 失败并保留旧快照。 |
| 2026-10-26 | v1 | **`responses.blocked` 接入探针**：探针命中时按契约覆盖状态码/body/`Retry-After`，`X-Gateway-Blocked` 保留 |
| 2026-10-26 | v1 | **Body 临时文件支持**：请求体超过 `client_body_buffer_size` 落临时文件时，按 `temp_file->offset` 读回再校验；读取失败返回 500。nginx 预检 413 仍可能早于插件。 |
| 2026-10-26 | v1 | **实现路由级 `path_regex` / `priority` / `responses`**：路由级正则与分组正则 AND；按 priority 分组路由树、高优先级先匹配；`responses` 覆盖 core 拒绝状态码/body/`Retry-After`。`contract_not_ready` / `blocked` / `body_too_large` 覆盖仅解析未生效 |
| 2026-10-26 | v1 | **实现 filters（SQLi / XSS）**：顶层/分组/路由 `filters`、或继承、按维度在参数校验后扫描；SQLi/XSS 使用第三方 `libinjectionrs`；命中 `400 invalid-parameter` + `rule=filter filter=sqli|xss`。路由级 `path_regex` / `priority` / `responses` 仍未实现 |
| 2026-10-26 | v1 | **实现分组剩余继承**：`methods` 交集与 `Allow` 顺序、`query`/`body`/`path_params` 合并、`unknown` 严格化、`body.content_types` 交集；同名跨层重复拒绝。`filters` 继承仍未实现 |
| 2026-10-26 | v1 | **实现路径参数与分组切片**：`path_params`（原始未解码值、规则名必须存在于路由、禁止 required/required_if/true）；分组 `prefix`/`router` 展开、`headers` 继承、`unmatched` 作用域、分组 `path_regex`（AND、全匹配、失败按未命中）。分组 `methods` / `query` / `body` / `path_params` / `filters` 与路由级 `path_regex`、`priority` 仍未实现 |
| 2026-10-26 | v1 | **实现 Query / Body 参数校验**：Query 原始 query 解析（一次解码、UTF-8、空 key / 同名 / 256 对上限）、`unknown` 默认 `ignore`；Body 支持 JSON 与 form、`unknown` 默认 `reject`、`content_types`、charset/Content-Encoding 校验、重复键与标量类型；三个维度复用同一套 `required` / `required_if` / 值规则实现。`filters` / 分组继承 / 路径参数仍未实现 |
| 2026-10-26 | v1 | **实现 Header `required_if`**：字符串形式按被引用头是否出现（空值/非 UTF-8 也算出现）；对象形式 `{ field, equals }` 按去 OWS 后区分大小写的完全相等；条件成立且目标缺失 → `400 missing-parameter`、`rule=required_if`；契约拒绝自引用、引用未声明头、与 `required: true` 同用及非法对象形态。当前仅 Header 维度；`filters` / 分组继承 / Query / Body / 路径参数仍未实现 |
| 2026-10-26 | v1 | 修正文档内部矛盾：`routes` 改为可选（默认 `[]`）；`{ "required": false }` 明确为合法的显式可选声明；补充 Header 缺失/重复的优先级（按契约书写顺序报第一个）、空 `headers.params` 语义、非 UTF-8 值暂不校验、完全相同的 JSON 重复键拒绝；`required_if.equals` 改为“`field` 非空、`equals` 可为空” |
| 2026-10-26 | v1 | **严格模式补强**：`methods` 元素必须是 RFC 9110 token（1–32 字符，拒绝 CR/LF/空格/逗号，防止 `Allow` 响应头与日志注入）；路径参数名按 `[A-Za-z_][A-Za-z0-9_]{0,63}` 校验、重复名无条件拒绝；单路由（含继承）规则合计 ≤256；JSON body 未声明字段为 `null` 仍触发 `unknown: reject`（声明字段的 `null` 仍等于未提供）；移除演示钩子 `/__gateway/deny`；URL 契约响应体上限 1 MiB |
| 2026-10-26 | v1 | **诊断响应头上线开关**：新增 http 级 `gateway_diagnostics on\|off`（默认 `off`），`X-Gateway-Name` / `-Reason` / `-Field` / `-Blocked` 只在测试环境返回；打开时 `nginx -t` 打 WARN，非法值/重复声明拒绝加载；诊断关闭后拒绝响应统一带 `X-Request-Id`（= nginx `$request_id`），日志以 `rid=` 结尾，排障不再依赖响应头 |
| 2026-10-27 | v1 | **移除 TTL**：删除契约顶层 `ttl` 字段与 nginx `gateway_url_ttl` 指令，不再有周期轮询；URL 契约由 leader 启动/接管时拉一次，首次失败每 2s 重试，更新通过reload。开发期直接改 v1，不考虑旧契约兼容 |
| 2026-10-27 | v1 | **移除内置探针**：删除硬编码 `.php` / `.env` / `.git` / `/wp-*` 规则、`X-Gateway-Blocked` 响应头与 `responses.blocked` 覆盖；这些路径现在按契约的 `route-not-declared` 处理（404 或 `forward`），不再有契约之外的拦截层 |
| 计划 | 未排期 | `responses` 覆盖拒绝时的状态码/响应体 |
