# 网关协议 v1（契约接口）

> 本文是**唯一约定**：后端生成 `GET /info` 的返回值、插件解析契约、以及拒绝请求时的行为，都以本文为准。
> 代码对应：`crates/gateway-core/src/contract.rs`（结构）、`crates/gateway-core/src/gateway.rs`（校验与判定）、
> `crates/ngx-gateway/src/lib.rs`（拉取与响应）、`demo_server/src/main.rs`（参考实现）、
> `crates/gateway-core/tests/protocol_v1.rs`（协议用例，`cargo test` 不加载 nginx）。
>
> 状态：**v1 已实现（含未声明路由策略 `unmatched`）**。项目仍在开发中，协议只有 v1：
> 加字段直接改本版本，不递增版本号。文中标注 `🧪 草案` 的字段/语义插件**尚未支持**，
> 提前出现在契约里会被拒绝加载（fail-closed）。

## 0. 版本与演进

| 概念 | 字段 | 含义 |
| --- | --- | --- |
| 协议版本 | 顶层 `protocol` | 契约**结构**的版本，整数。插件只接受自己支持的版本 |
| 内容版本 | 顶层 `version` | 后端契约的**内容修订号**，仅用于日志/排查，不参与解析 |
| 刷新周期 | 顶层 `ttl` | 建议值（秒）。当前插件以该网关的 `gateway_ttl` 为准 |

当前插件只支持 `protocol = 1`。演进规则：

1. **开发期不递增版本**：契约还没正式对外使用，加字段、改语义都直接改 v1；不存在需要兼容的旧契约。
2. **正式上线后**，只有需要同时兼容旧契约（新旧插件/后端混跑）时才递增 `protocol`，
   并明确旧版本在新插件里的行为（通常整份拒绝或按旧语义解析）。
3. 插件看到不支持的 `protocol` 会**拒绝整份契约**（不会"读一半"）。
4. **纯信息字段**放进顶层 `meta`（自由 JSON 对象），插件必须忽略 `meta`。
5. 解析是**严格模式**：顶层与 `routes[]` 内出现未定义的字段，整份契约拒绝加载，日志写明字段名。
   这样后端误发字段时会立刻暴露，而不是被静默忽略造成"以为生效了"。
6. 新增字段优先设计成**可选**：省略时行为保持合理默认值。
7. `routes: []` 合法，语义是"拒绝所有未在契约中声明的路由"（即全站 404，`/passthrough/` 之类关掉网关的 location 除外）。

## 1. 契约来源与传输

每个网关**必须且只能**有一个契约来源：`gateway_info_url`（HTTP 拉取）或 `gateway_info_file`
（本地文件）。两者取哪个是**信任模型的选择**，不只是部署方便与否，见 1.1。

| 项 | 约定 |
| --- | --- |
| 来源（URL） | `gateway_info_url <网关名> <url>`，必须是 `http://` 或 `https://`（当前构建只启用明文 HTTP，写 `https://` 会持续拉取失败 → 503） |
| 来源（文件） | `gateway_info_file <网关名> <path>`，绝对路径或相对 nginx prefix（`-p`）的路径；`nginx -t` 检查来源唯一，但文件读不到只 warning |
| 方法 | URL 方式为 `GET`；文件方式直接读本地文件 |
| 成功响应 | `200` + JSON 对象；建议 `Content-Type: application/json`。文件方式为 UTF-8 JSON |
| 超时 | 插件单次 HTTP 请求 3s（`timeout_global`）；文件方式无网络 |
| 刷新 | 每个被 location 引用的网关、每个 worker 一个后台线程，按该网关的 `gateway_ttl` 秒循环：URL 重新拉取；文件只 `stat`，mtime/长度/inode 变化才重读。**成功才替换**自己的快照 |
| 失败处理 | 保留该网关上一份成功契约（last-known-good）；从未成功过则该网关的绑定请求 `503 contract-not-ready` |
| 日志去重 | 成功/失败的同一消息只在状态变化时写一条 `error_log`（带网关名前缀） |

### 1.1 为什么要有文件模式（安全原因，别忽略）

URL 模式的隐含前提是「**后端说的就是规则**」。后端一旦被拿下（RCE、配置被改、依赖被投毒、
供应链污染），攻击者只要改自己的 `/info` 返回值，下一个 TTL 内就能：

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

代价：规则变更要走发布流程（写文件，等 TTL 或 `nginx -s reload`），不再能"后端一改就生效"。

### 1.2 使用文件模式时必须注意（开发/运维清单）

1. **权限才是边界。** 契约文件应对后端进程**只读**：
   `chown root:<nginx 运行用户> contract.json && chmod 0640`，所在目录也不能被后端用户写。
   如果后端和 nginx 跑在同一用户下（本仓库 dev 模式就是），文件模式**不提供任何隔离**，
   只是验证了"从文件读契约"这条路径。
2. **原子替换发布。** 先写临时文件，再 `mv` 到目标路径（同目录、同文件系统）。
   直接 `>` 截断写入会出现"半截 JSON"窗口；插件会拒绝坏文件并保留旧契约，但不要依赖这个兜底。
3. **路径规则**：绝对路径，或相对 **nginx prefix**（`-p`，默认编译 prefix）的路径，例如
   `conf/contracts/web.json`。注意不是相对 conf 文件所在目录（那是 `include` 的规则）。
4. **TTL 含义不同**：URL 模式是"拉取间隔"；文件模式是"检查变化的间隔"（每轮只 `stat`，
   变了才读取解析），所以文件模式可以把 `gateway_ttl` 设小一些，成本很低。
5. **启动时文件不存在不阻止 nginx 启动**，只写一条 warning，绑定的 location 返回
   `503 contract-not-ready`（fail-closed）。这样发布顺序（先起 nginx 后放契约）不会把站点配置卡死。
6. **一个网关不能同时配两种来源**，`nginx -t` 会拒绝（报 `already has a contract source`）。
7. **两者可以在同一份配置里混用**：A 网关走 URL、B 网关走文件，各自独立刷新、互不影响。
8. **文件内容是同一份协议 v1 JSON**，没有额外包装；`ttl` 字段在文件模式下同样被
   `gateway_ttl` 覆盖。

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
  "ttl": 60,
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
| `ttl` | int（秒） | 否 | — | 建议刷新周期，当前被该网关的 `gateway_ttl` 覆盖 |
| `meta` | object | 否 | — | 自由信息，插件忽略 |
| `unmatched` | string | 否 | `"deny"` | 未声明路由策略：`deny` / `forward`，见 2.1 |
| `routes` | array | 是 | `[]` | 路由声明，见第 3 节 |
| 其他字段 | — | — | — | **拒绝整份契约** |

### 2.1 未声明路由策略

有些后端希望"契约只写重点路由，其余路径仍交给后端自己处理"（迁移期、旧客户端兼容）。
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

- **只影响"路径未声明"**：已经声明的路由，方法不符仍然是 `405 + Allow`，不会转发。
- 其他值（如 `"allow"`）→ JSON 解析失败 → 整份契约拒绝。
- 安全提示：`forward` 会把扫描器路径、拼错路径也交给后端，后端必须自己兜底（返回 404 而不是泄漏）。
  它定位是**迁移期开关**，不是长期默认值；契约覆盖完整后建议改回 `deny`。

## 3. 路由对象

```json
{
  "path": "/api/user/{id}/profile",
  "methods": ["GET", "PUT"]
}
```

| 字段 | 类型 | 必填 | 默认 | 说明 |
| --- | --- | --- | --- | --- |
| `path` | string | 是 | — | 以 `/` 开头；不允许查询串、`#`、尾斜杠（`/` 本身除外） |
| `methods` | string[] | 否 | `[]` | 允许的方法；**空数组 = 不限方法**；元素不能是空串 |
| 其他字段 | — | — | — | **拒绝整份契约**（例如提前写 `priority` 会被拒绝） |

### 3.1 路径匹配

- 路径来自**原始 request target**（只去掉 `?query`；不合并 `//`、不做百分号解码），
  与上游实际收到的路径一致。因此 `/api/products`、`/api/products/`、`//api/products` 是三条不同路径；
  契约里写编码形式（`%2F`）没有意义（客户端发 `%2F` 时按字面比较，通常匹配不到）。
- **区分大小写**，**严格尾斜杠**：`/api/products` 与 `/api/products/` 是两条不同路径。
- 段语法：
  | 写法 | 含义 | 示例 |
  | --- | --- | --- |
  | `/api/products` | 字面量（含 `-` `_` `.` 等） | 精确匹配 |
  | `/api/user/{id}` | 单段参数，名字 `[A-Za-z_][A-Za-z0-9_]*` | `/api/user/42` ✓，`/api/user/42/x` ✗ |
  | `/static/{*rest}` | 通配剩余部分（可跨多段），只能出现在最后一段 | `/static/a/b/c.txt` ✓ |
- 同一份契约内 `path` 不能重复；参数名不同但形状相同的路由（如 `/u/{id}` 与 `/u/{name}`）视为冲突，拒绝加载。
- 匹配优先级由路由树决定：**静态段 > 参数段 > 通配**。契约顺序**不**影响结果。
- 🧪 草案（未排期）：新增可选 `priority: i32`（默认 0，越大越先匹配）。命中多条时按 `priority` →
  匹配具体度 → 契约顺序。未实现前，写了 `priority` 的契约会被整份拒绝（严格模式）。

### 3.2 方法匹配

- 方法比较**大小写不敏感**；契约里建议统一大写。
- `methods` 省略或 `[]` → 不限方法（放行所有方法，包括 `PATCH`、自定义方法）。
- 路径命中但方法不在列表 → `405`，响应头 `Allow` 按契约书写顺序输出（如 `Allow: GET, PUT`）。
- `methods` 中任意元素为空串或全空白 → 拒绝整份契约。

## 4. 判定顺序与响应

对绑定了网关的 location（`enable_gateway_by <name>`，可绑定多个），按下列顺序，**先命中先返回**，
被拒请求不会到达后端：

| # | 条件 | 状态码 | 响应头 | 说明 |
| --- | --- | --- | --- | --- |
| 1 | 内置探针命中（`.php` / `.env` / `.git*` / `/wp-*`） | `403` | `X-Gateway-Blocked: <reason>` | 演示用硬编码，先于契约；将来数据驱动化 |
| 2 | 该网关从未成功加载契约 | `503` | `X-Gateway-Reason: contract-not-ready` + `X-Gateway-Name` | fail-closed，冷启动不放行 |
| 3 | 路径未在该网关契约中声明，且 `unmatched` 为 `deny`（默认） | `404` | `X-Gateway-Reason: route-not-declared` + `X-Gateway-Name` | 含扫目录/扫漏洞路径 |
| 3b | 路径未声明，但该网关契约 `unmatched: "forward"` | — | — | 交给后端处理，日志 `gateway: forward ... unmatched=forward` |
| 4 | 路径存在但方法不符 | `405` | `Allow: ...` + `X-Gateway-Reason: method-not-allowed` + `X-Gateway-Name` | `Allow` 只含该路由声明的方法 |
| 5 | 绑定多个网关时，全部允许 | — | — | 交给 `proxy_pass` 上游；任一拒绝即拒绝（转发也算允许） |

补充：

- 路径判定使用**原始** request target 的 path（去掉 `?query`，不合并 `//`、不做百分号解码）。
  nginx 默认 `merge_slashes on` 只影响 location 匹配与 `$uri`，不影响判定；因此 `//api/health`
  与 `/api/health` 是两条不同路径，前者命中 `404 route-not-declared`（与上游实际收到的路径一致）。
- 未声明路由是 `deny` 还是 `forward` 由**各自网关的契约**决定；一个 location 绑多个网关时，
  任一网关拒绝就拒绝，全部允许（含转发）才放行。
- **未绑定** `enable_gateway_by` 的 location 完全不参与判定（探针也不跑），请求直接透传；`/passthrough/`
  就是这种对照。
- `X-Gateway-Name` 标明是哪个网关做出的拒绝（多网关排查用）；探针拦截不带这个头。
- 演示钩子 `/__gateway/deny` 固定返回 `503` + `X-Gateway-Blocked: demo-deny`，用于验证"插件能自定义状态码"，
  不属于协议的一部分，后续会移除。
- 响应体不做约定：当前返回 nginx 默认错误页；客户端应以**状态码 + `X-Gateway-Reason`** 为准。
- `X-Gateway-Reason` 是稳定的机器可读 token；以后新增原因只增不改。
- 🧪 草案：顶层可选 `responses` 覆盖默认状态码/响应体，见第 6 节。

## 5. 参数校验（🧪 草案）

> 设计意图写在前面，避免后端提前依赖未实现的字段：**本节的字段在实现前不允许出现在契约里**，
> 出现即整份拒绝（严格模式）。

### 5.1 Query

```json
{
  "path": "/api/products",
  "methods": ["GET"],
  "query": {
    "page": { "type": "int", "required": false, "min": 1, "max": 1000 },
    "kw":   { "type": "char", "max_len": 32 },
    "sort": { "type": "enum", "values": ["asc", "desc"], "required": false },
    "flag": { "type": "bool" }
  }
}
```

### 5.2 Body（防 Mass Assignment）

```json
{
  "path": "/api/orders",
  "methods": ["POST"],
  "body": {
    "sku":  { "type": "char", "required": true, "max_len": 64 },
    "qty":  { "type": "int", "required": true, "min": 1, "max": 99 },
    "note": { "type": "char", "max_len": 200 }
  }
}
```

- 支持 `application/x-www-form-urlencoded` 与 `application/json`；其他 `Content-Type` 按 400 拒绝。
- 默认忽略未声明字段；`"strict": true` 时出现未声明字段 → `400`（用于拦截 Mass Assignment）。
- 请求体读取上限由 nginx 配置控制；超过上限返回 `413`，不做部分解析。

### 5.3 Header

```json
{
  "path": "/api/orders",
  "methods": ["POST"],
  "headers": {
    "x-api-version": { "required": true, "enum": ["1", "2"] }
  }
}
```

- Header 名大小写不敏感；只校验显式声明的 header。
- 类型集合先做 `int` / `char` / `bool` / `enum` 四种，够用再扩。
- 校验失败统一 `400`，`X-Gateway-Reason` 细分为 `missing-parameter` / `invalid-parameter` /
  `body-too-large`（413）/ `unsupported-media-type`（400）。

## 6. 响应覆盖（🧪 草案）

默认值见第 4 节。后端可选择覆盖"网关拒绝时"的状态码与响应体（只允许拒绝类响应）：

```json
{
  "protocol": 1,
  "responses": {
    "route_not_declared": { "status": 404, "body": "{\"error\":\"not_found\"}" },
    "method_not_allowed": { "status": 405 },
    "contract_not_ready": { "status": 503, "retry_after": 5 },
    "blocked":            { "status": 403 },
    "missing_parameter":  { "status": 400 },
    "invalid_parameter":  { "status": 400 }
  }
}
```

约束：

- `status` 只能是 `4xx` / `5xx`，禁止把拒绝改成放行（`2xx` / `3xx` 一律视为非法契约）。
- `body` 最长 1 KiB，默认 `Content-Type: application/json`；`X-Gateway-Reason` 始终保留。
- 缺少的键沿用第 4 节默认值；`Allow` 仍由方法校验自动生成。

## 7. 后端生成契约的检查清单

- [ ] `protocol` 明确写出（缺省按 1），当前只有 `1`。
- [ ] `routes` 覆盖**所有**真实对外路由；漏写 = 线上 404（默认拒绝）。
- [ ] 确实需要"未声明也转发"时才用 `"unmatched": "forward"`，并确保后端自己兜底 404。
- [ ] 每个 `path` 以 `/` 开头、无尾斜杠、无查询串；参数路由用 `{name}` / `{*rest}`。
- [ ] `methods` 与后端实际注册一致；不限方法才用 `[]`（慎用）。
- [ ] 不输出本文未定义（或尚未支持）的字段。
- [ ] `/info` 自身也在 `routes` 里。
- [ ] 契约损坏、超时、非 200 时网关会保留旧契约；后端需监控 `/info` 的可用性。

## 8. 插件解析契约的检查清单

- [ ] 只接受 `protocol == 1`；其他值整份拒绝并记录日志。
- [ ] 顶层/路由出现未定义字段 → 整份拒绝（严格模式）。
- [ ] 任一路由非法 → 整份拒绝（不使用半份契约）。
- [ ] 成功才替换快照；失败保留 last-known-good。
- [ ] 从未成功加载过 → 受保护请求 `503 contract-not-ready`。
- [ ] access 阶段不做网络 I/O，只读内存快照。

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
  "ttl": 60,
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

## 10. 变更记录

| 日期 | 版本 | 变更 |
| --- | --- | --- |
| 2026-10-26 | v1 | 落地 `protocol` / `version` / `ttl` / `meta` / `routes[]`；严格路径、方法校验、`404/405/503` 与 `X-Gateway-Reason`；严格模式拒绝未知字段 |
| 2026-10-26 | v1 | 新增顶层可选 `unmatched`（`deny` 默认 / `forward` 交给后端）——开发期仍在 v1，不升版本号 |
| 计划 | 未排期 | 路由 `priority`（优先级匹配） |
| 计划 | 未排期 | `query` / `body` / `headers` 参数校验与 `400/413` |
| 计划 | 未排期 | `responses` 覆盖拒绝时的状态码/响应体 |
