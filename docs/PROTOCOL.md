# 网关协议 v1（契约接口）

> 本文是**唯一约定**：后端生成 `GET /info` 的返回值、插件解析契约、以及拒绝请求时的行为，都以本文为准。
> 代码对应：`crates/gateway-core/src/contract.rs`（结构）、`crates/gateway-core/src/gateway.rs`（校验与判定）、
> `crates/ngx-gateway/src/lib.rs`（拉取与响应）、`demo_server/src/main.rs`（参考实现）。
>
> 状态：**v1 已实现**。文中标注 `🧪 草案` 的字段/语义插件**尚未支持**，提前出现在契约里会被拒绝加载（fail-closed）。

## 0. 版本与演进

| 概念 | 字段 | 含义 |
| --- | --- | --- |
| 协议版本 | 顶层 `protocol` | 契约**结构**的版本，整数。插件只接受自己支持的版本 |
| 内容版本 | 顶层 `version` | 后端契约的**内容修订号**，仅用于日志/排查，不参与解析 |
| 刷新周期 | 顶层 `ttl` | 建议值（秒）。当前插件以该网关的 `gateway_ttl` 为准 |

当前插件支持 `protocol = 1`。演进规则：

1. **任何影响判定的变更都必须递增 `protocol`**：新增校验字段（`priority` / `query` / `body` / `headers` / `responses`）、
   改变匹配或状态码语义，都算。旧插件看到更高的 `protocol` 会**拒绝整份契约**（不会"读一半"）。
2. **纯信息字段**放进顶层 `meta`（自由 JSON 对象），不需要递增版本；插件必须忽略 `meta`。
3. v1 的解析是**严格模式**：顶层与 `routes[]` 内出现未定义的字段，整份契约拒绝加载，日志写明字段名。
   这样后端误发新字段时会在演示环境立刻暴露，而不是被静默忽略造成"以为生效了"。
4. 新增字段时优先设计成**可选**：新插件解析旧契约（没有新字段）时行为必须完全不变。
5. `routes: []` 合法，语义是"拒绝所有未在契约中声明的路由"（即全站 404，`/passthrough/` 之类关掉网关的 location 除外）。

## 1. 传输约定

| 项 | 约定 |
| --- | --- |
| 地址 | `gateway_info_url <网关名> <url>` 指定，必须是 `http://` 或 `https://`（当前构建只启用明文 HTTP，写 `https://` 会持续拉取失败 → 503） |
| 方法 | `GET` |
| 成功响应 | `200` + JSON 对象；建议 `Content-Type: application/json` |
| 超时 | 插件单次请求 3s（`timeout_global`） |
| 刷新 | 每个被 location 引用的网关、每个 worker 一个后台线程，按该网关的 `gateway_ttl` 秒循环拉取；**成功才替换**自己的快照 |
| 失败处理 | 保留该网关上一份成功契约（last-known-good）；从未成功过则该网关的绑定请求 `503 contract-not-ready` |
| 日志去重 | 成功/失败的同一消息只在状态变化时写一条 `error_log`（带网关名前缀） |

`/info` 本身也应出现在 `routes` 里（后端自查用），是否放行由契约决定。

demo_server 同时提供 `/info`（`version: phase1-fixed`）与 `/info_v2`（`version: phase1-v2`），
两份内容部分重叠，用来让 nginx 里的两个网关指向不同地址、验证各自独立的快照；两者协议版本都是 `1`。

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
| `routes` | array | 是 | `[]` | 路由声明，见第 3 节 |
| 其他字段 | — | — | — | **拒绝整份契约** |

## 3. 路由对象（v1）

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

### 3.1 路径匹配（v1 已实现）

- 路径来自 nginx 归一化后的 URI（已百分号解码、合并 `//`、解析 `.` / `..`），契约里写编码形式（`%2F`）无效。
- **区分大小写**，**严格尾斜杠**：`/api/products` 与 `/api/products/` 是两条不同路径。
- 段语法：
  | 写法 | 含义 | 示例 |
  | --- | --- | --- |
  | `/api/products` | 字面量（含 `-` `_` `.` 等） | 精确匹配 |
  | `/api/user/{id}` | 单段参数，名字 `[A-Za-z_][A-Za-z0-9_]*` | `/api/user/42` ✓，`/api/user/42/x` ✗ |
  | `/static/{*rest}` | 通配剩余部分（可跨多段），只能出现在最后一段 | `/static/a/b/c.txt` ✓ |
- 同一份契约内 `path` 不能重复；参数名不同但形状相同的路由（如 `/u/{id}` 与 `/u/{name}`）视为冲突，拒绝加载。
- v1 的匹配优先级由路由树决定：**静态段 > 参数段 > 通配**。契约顺序**不**影响结果。
- 🧪 草案（protocol 2）：新增可选 `priority: i32`（默认 0，越大越先匹配）。命中多条时按 `priority` →
  匹配具体度 → 契约顺序。未实现前，写了 `priority` 的契约会被 v1 插件整份拒绝。

### 3.2 方法匹配（v1 已实现）

- 方法比较**大小写不敏感**；契约里建议统一大写。
- `methods` 省略或 `[]` → 不限方法（放行所有方法，包括 `PATCH`、自定义方法）。
- 路径命中但方法不在列表 → `405`，响应头 `Allow` 按契约书写顺序输出（如 `Allow: GET, PUT`）。
- `methods` 中任意元素为空串或全空白 → 拒绝整份契约。

## 4. 判定顺序与响应（v1 已实现）

对绑定了网关的 location（`enable_gateway_by <name>`，可绑定多个），按下列顺序，**先命中先返回**，
被拒请求不会到达后端：

| # | 条件 | 状态码 | 响应头 | 说明 |
| --- | --- | --- | --- | --- |
| 1 | 内置探针命中（`.php` / `.env` / `.git*` / `/wp-*`） | `403` | `X-Gateway-Blocked: <reason>` | 演示用硬编码，先于契约；将来数据驱动化 |
| 2 | 该网关从未成功加载契约 | `503` | `X-Gateway-Reason: contract-not-ready` + `X-Gateway-Name` | fail-closed，冷启动不放行 |
| 3 | 路径未在该网关契约中声明 | `404` | `X-Gateway-Reason: route-not-declared` + `X-Gateway-Name` | 含扫目录/扫漏洞路径 |
| 4 | 路径存在但方法不符 | `405` | `Allow: ...` + `X-Gateway-Reason: method-not-allowed` + `X-Gateway-Name` | `Allow` 只含该路由声明的方法 |
| 5 | 绑定多个网关时，全部允许 | — | — | 交给 `proxy_pass` 上游；任一拒绝即拒绝 |

补充：

- **未绑定** `enable_gateway_by` 的 location 完全不参与判定（探针也不跑），请求直接透传；`/passthrough/`
  就是这种对照。
- `X-Gateway-Name` 标明是哪个网关做出的拒绝（多网关排查用）；探针拦截不带这个头。
- 演示钩子 `/__gateway/deny` 固定返回 `503` + `X-Gateway-Blocked: demo-deny`，用于验证"插件能自定义状态码"，
  不属于协议的一部分，后续会移除。
- 响应体不做约定：v1 返回 nginx 默认错误页；客户端应以**状态码 + `X-Gateway-Reason`** 为准。
- `X-Gateway-Reason` 是稳定的机器可读 token；以后新增原因只增不改。
- 🧪 草案（protocol 4）：顶层可选 `responses` 覆盖默认状态码/响应体，见第 6 节。

## 5. 参数校验（🧪 草案，protocol 3）

> 设计意图写在前面，避免后端提前依赖未实现的字段：**本节的字段在 protocol 3 落地前不允许出现在契约里**，
> 出现即整份拒绝（v1 严格模式）。

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

## 6. 响应覆盖（🧪 草案，protocol 4）

默认值见第 4 节。后端可选择覆盖"网关拒绝时"的状态码与响应体（只允许拒绝类响应）：

```json
{
  "protocol": 4,
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

- [ ] `protocol` 明确写出（或在 v1 缺省为 1），与插件支持版本一致。
- [ ] `routes` 覆盖**所有**真实对外路由；漏写 = 线上 404（默认拒绝）。
- [ ] 每个 `path` 以 `/` 开头、无尾斜杠、无查询串；参数路由用 `{name}` / `{*rest}`。
- [ ] `methods` 与后端实际注册一致；不限方法才用 `[]`（慎用）。
- [ ] 不输出本文未定义（或当前版本未支持）的字段；新字段先推进 `protocol` 再上线。
- [ ] `/info` 自身也在 `routes` 里。
- [ ] 契约损坏、超时、非 200 时网关会保留旧契约；后端需监控 `/info` 的可用性。

## 8. 插件解析契约的检查清单

- [ ] 只接受 `protocol == 1`；更高版本整份拒绝并记录日志。
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

### 9.2 当前 demo 后端实际返回

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
| 计划 | v2 | 路由 `priority`（优先级匹配） |
| 计划 | v3 | `query` / `body` / `headers` 参数校验与 `400/413` |
| 计划 | v4 | `responses` 覆盖拒绝时的状态码/响应体 |
