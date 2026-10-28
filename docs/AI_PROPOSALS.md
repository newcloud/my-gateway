# AI 提议清单

> 本文档只记录 **AI 主动提出的功能/方案建议**，不是已确认需求，也没有实现。
> 确认后移入 `docs/PROTOCOL.md` / `PLAN.md` 并进入开发；拒绝或暂缓的留在本文档标记状态。

| 项目 | 内容 |
| --- | --- |
| 生成者 | **DeepSeek V4.1 Flash 1017** |
| Provider / Model | `deepseek` / `deepseek-flash` |
| 生成时间 | **2026-10-27 22:47 CST (+0800)** |
| 状态 | 全部为提议，未确认、未实现 |
| 约束 | 确认前不写入协议、不落代码；临时实现必须登记到 `docs/ISSUES.md` |

## 0. 当前基线

已实现：静态路由、`{id}`、`{*rest}`、分组/继承、`priority`、`path_regex`、方法校验、
Header/Query/Body/路径参数、`filters`、顶层 `responses`、`unmatched`、body 限制与 JSON 结构扫描、
leader worker + 共享内存分发、首次失败重试。

## 1. 总览

| ID | 提议 | 优先级 | 工作量 | 状态 |
| --- | --- | --- | --- | --- |
| P-01 | 路由等级 `tier` + 策略预设 | 中 | M | 待确认 |
| P-02 | 路由生命周期（deprecated / sunset / replacement） | 中 | S/M | 待确认 |
| P-03 | 路由 `tags` 元数据 | 低 | S | 待确认 |
| P-04 | `POST /_gateway_refresh` 主动刷新接口 | **高** | M | **已确认需要**，设计见 `REFRESH_ENDPOINT_DESIGN.md` |
| P-05 | 契约版本 / 灰度 / 回滚 | 中 | L | 待确认 |
| P-06 | 不做“运行时任意增删路由” | — | — | 建议明确 |
| P-07 | 路径参数类型 / 约束 | 中 | M | 待确认 |
| P-08 | host / base path 作用域 | 低 | M | 待确认 |
| P-09 | HEAD / OPTIONS / CORS 策略 | 中 | S/M | 待确认 |
| P-10 | 尾斜杠 / 编码路径策略 | 低 | S/M | 待确认 |
| P-11 | 多级数组 / 嵌套 JSON / query 数组 | **高** | L | 待确认（见 ISSUES #2） |
| P-12 | `multipart/form-data` 与文件上传校验 | 中 | M/L | 待确认 |
| P-13 | 跨字段比较规则 | 中 | M | 待确认 |
| P-14 | Cookie 维度 | 低 | M | 待确认 |
| P-15 | 不做响应体 schema 校验 | — | — | 建议明确 |
| P-16 | 契约体积 / 共享内存配置化 | **高** | S/M | 待确认（见 ISSUES #1） |
| P-17 | 分组 / 路由级 `responses` | 中 | S/M | 待确认 |
| P-18 | 判定可观测性（route / tier / version） | 中 | S/M | 待确认 |
| P-19 | 契约模块化 / include | 低 | L | 待确认 |
| P-20 | 契约版本 / diff / 审批 / 回滚 | 低 | L | 待确认 |

## 2. 路由分级与元数据

### P-01 路由等级 `tier` + 策略预设

```json
"tiers": {
  "public":  { "filters": { "sqli": true }, "body": { "max_bytes": 65536 } },
  "internal":{ "headers": { "params": { "x-api-key": { "required": true } } } },
  "admin":   { "headers": { "params": { "authorization": { "required": true } } },
               "body": { "max_bytes": 1048576 } }
},
"routes": [
  { "path": "/api/admin/users", "tier": "admin", "methods": ["GET"] }
]
```

- **解决**：大量路由重复写 `filters`、body 上限、必填头。
- **继承**：全局 → tier → group → route；安全类配置只允许收紧，不允许子级放宽。
- **风险**：隐式预设容易让人忘记实际生效规则；日志/诊断头必须能看出生效的 tier。
- **工作量**：M。

### P-02 路由生命周期

```json
{ "path": "/api/v1/users", "deprecated": true,
  "sunset": "2026-12-31", "replacement": "/api/v2/users" }
```

- **解决**：API 版本迁移、旧接口下线没有网关侧提示。
- **行为建议**：自动加 `Deprecation` / `Sunset` / `Link` 响应头；过期后可选 `410 Gone`。
- **工作量**：S/M。

### P-03 路由 `tags`

`tags: ["billing", "admin"]`，只用于日志、指标、审计，不参与判定。
- **解决**：后续 metrics 按业务域聚合，也方便和 OpenAPI 对齐。
- **工作量**：S。

## 3. 动态路由 / 契约更新

### P-04 `POST /_gateway_refresh` 主动刷新接口（**已确认需要**）

- 任意 worker 收请求，不拉 `/info`；写共享内存后由 leader 按名单刷新；
- `gateway_refresh name1,name2;` 在 location 里指定名单；
- `?mode=auto`（默认，成功上线/失败保留旧快照）、`?mode=check`（只校验不上线）、
  `?mode=force`（失败也清空旧快照，按加载失败处理）；
- 同步返回 JSON，不用 HTTP 状态码表达刷新成功/失败；不返回 version；
- 并发、专用超时不做；安全由 nginx ACL + 文档/demo 负责。
- **设计稿**：`docs/REFRESH_ENDPOINT_DESIGN.md`
- **工作量**：M。

### P-05 契约版本 / 灰度 / 回滚

- 契约带 `version`，共享内存保留当前版本和上一版；按 header / 百分比选择版本。
- 支持“新契约只对 `X-Contract-Canary: 1` 生效”，验证后再全量。
- **解决**：大契约直接全量生效风险高；出问题只能 reload 回滚。
- **工作量**：L；若只做“reload 切换版本”，不依赖 P-04。

### P-06 不做“运行时任意增删路由”

契约仍然是唯一事实来源；动态能力限定为“重新加载契约 / 选择版本”，
不做管理 API 直接改内存路由。否则审计、回滚、多 worker 一致性都会失控。
- **状态**：建议明确为不做。

## 4. 路由匹配能力

### P-07 路径参数类型 / 约束

```json
{ "path": "/api/users/{id:int}", "methods": ["GET"] }
{ "path": "/api/files/{name:regex}", "methods": ["GET"] }
```

- **解决**：现在只能先 `{id}` 再写 `path_params` 规则；类型写进 path 更直观，也能减少无效匹配。
- **工作量**：M，涉及路由树与优先级冲突规则。

### P-08 host / base path 作用域

- 契约目前只描述 path；同一 path 不同 host 需要拆多个 gateway/location。
- 可评估契约级 `hosts` 或 location 级 `base_path`。
- **风险**：没有明确多租户场景时容易过度设计。
- **工作量**：M。

### P-09 HEAD / OPTIONS / CORS 策略

- 当前 `HEAD` 不隐含于 `GET`，`OPTIONS` 没有自动处理。
- 提议显式策略：`head: "auto" | "deny"`、`OPTIONS` 预检响应、CORS 白名单。
- **工作量**：S/M，需要产品确认语义。

### P-10 尾斜杠 / 编码路径策略

- 现在严格匹配、不解码。
- 可提议契约级 `trailing_slash: "strict" | "redirect" | "ignore"`，
  以及 `%2e%2e` 等编码路径的明确拒绝规则。
- **工作量**：S/M。

## 5. 判定能力

### P-11 多级数组 / 嵌套 JSON / query 数组

- `type: "array"`、`somekey.*.id`、`abc.efd.hh.id`；
- query 的 `a[]=1&a[]=2`、`a[0][id]=1`；
- 需要同时定义：错误 `field` 路径（如 `body.items.0.id`）、未知键、数组长度/深度上限。
- **解决**：当前只能校验顶层平铺字段，是“大量表单后端”最影响可用性的缺口。
- **工作量**：L（详见 `ISSUES.md` #2）。

### P-12 `multipart/form-data` 与文件上传校验

- 当前只支持 JSON 和 urlencoded；表单后端常有文件上传。
- 建议支持：文件数量、单文件大小、总大小、MIME 白名单、字段名白名单。
- **风险**：不能把大文件整个读进内存。
- **工作量**：M/L。

### P-13 跨字段比较规则

```json
"password_confirm": { "equals_field": "password" },
"end_date": { "gte_field": "start_date" }
```

- **解决**：日期区间、确认密码、金额上下限等业务边界；现在只有 `required_if`。
- **工作量**：M，需要定义比较类型与错误消息。

### P-14 Cookie 维度

- 目前只能把 `Cookie` 当普通 header 做字符串/正则校验，不能按 cookie 名拆开。
- 提议增加 `cookies.params` 维度，和 Header/Query 共用规则。
- **工作量**：M。

### P-15 不做响应体 schema 校验

网关校验上游响应体会带来性能、流式响应、压缩、错误页兼容等问题，也和“请求边界”定位不符。
可以只做响应头/状态码策略，不做 body schema 校验。
- **状态**：建议明确为不做。

## 6. 契约工程与可观测性

### P-16 契约体积 / 共享内存配置化

- 增加 `gateway_max_contract_size` / `gateway_shm_size`。
- 共享内存放不下时必须打明确日志，不能只表现为 503。
- 文件来源也要有体积检查。
- **工作量**：S/M（详见 `ISSUES.md` #1）。

### P-17 分组 / 路由级 `responses`

- 现在 `responses` 只有顶层；提议支持分组/路由覆盖，和 `filters` 一样继承。
- **解决**：管理接口、公开接口需要不同的统一错误结构。
- **工作量**：S/M。

### P-18 判定可观测性

- 诊断头增加 `X-Gateway-Route`（实际匹配的路由模式）、`X-Gateway-Priority`、
  `X-Gateway-Contract-Version`；
- 日志增加 `route=` / `tier=`；
- 后续接 metrics：按 route / tier / reason 计数。
- **解决**：现在只能看到 `field/rule`，排查“为什么命中这条路由”很困难。
- **工作量**：S/M。

### P-19 契约模块化 / include

- 大契约拆成多个 JSON 再合并。
- **风险**：合并冲突、优先级、审计会变复杂；建议先做 P-16，确认确实需要再上。
- **工作量**：L。

### P-20 契约版本 / diff / 审批 / 回滚

- 产品化管理面能力，和 P-05 相关；单机插件无法独立完成。
- **工作量**：L。

## 7. 推荐顺序

1. **P-04**：主动刷新接口，按设计稿实现；随后做 P-16 契约体积/共享内存配置化。
2. **P-18 + P-17**：判定可观测性 + 分组/路由级 `responses`，小改动、排障收益大。
3. **P-11 + P-12**：多级数组/嵌套 JSON + multipart，面向大量表单后端。
4. **P-01 + P-02**：路由 `tier` 与生命周期，减少重复配置、支持 API 迁移。
5. **P-07 + P-09 + P-10**：路径参数类型、HEAD/OPTIONS/CORS、尾斜杠策略。
6. **P-13 + P-14**：跨字段比较、Cookie 维度。
7. **P-05 / P-19 / P-20**：契约灰度、模块化、管理面。
8. **认证体系 / IP 白名单 / 限流**：需要产品与安全边界确认，不在本清单直接展开。

## 8. 如何确认

对每条提议回复 `采纳 P-xx` / `拒绝 P-xx` / `暂缓 P-xx` 即可：

- 采纳：先更新 `docs/PROTOCOL.md`，再按 `AGENTS.md` 的顺序补 core 测试、实现、集成测试；
- 拒绝：从本文档移除，若已产生代码则同一次修改里删除；
- 暂缓：保留在本文档，标注原因。
