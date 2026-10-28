# 问题与设计待确认清单

> 只记录问题 / 风险 / 未实现能力，不写实现方案；实现或确认后从这里移除，并同步 `PROGRESS.md`。
> 新问题直接追加编号，保持简短。

## 1. 契约体积上限 1 MiB，可能不够

- **现象**：单个 URL 契约响应体上限 1 MiB；所有网关的共享内存快照区也只有 1 MiB。大量表单、多网关时容易超限。
- **原因**：
  - `MAX_CONTRACT_BYTES` 与 `CONTRACT_SHM_SIZE` 是两个独立常量，只调大一个不解决问题；
  - 文件来源在配置加载时没有体积检查；
  - 共享内存放不下时 `publish` 返回 `false`，`publish_snapshot` 没有检查，表现为请求 503，但日志里可能已经打过 `contract loaded`，没有“契约太大”的明确原因。
- **相关代码**：
  - `crates/ngx-gateway/src/contract_source.rs`：`MAX_CONTRACT_BYTES`
  - `crates/ngx-gateway/src/lib.rs`：`CONTRACT_SHM_SIZE`、配置加载时的文件读取
  - `crates/ngx-gateway/src/contract_shm.rs`：`publish`
  - `crates/ngx-gateway/src/contract_manager.rs`：`publish_snapshot`
- **备注**：worker 各自 `Gateway::parse` 后的内存通常比原始 JSON 大几倍，不能只按 shm 大小估算。

## 2. body / query 不支持多级数组和嵌套 JSON 路径

- **现象**：当前只能校验顶层平铺字段，无法表达：
  - 数组字段本身：`somekey` / `type: "array"`；
  - 数组元素：`somekey.*.id` / `type: "int"` / `max: ...`；
  - 嵌套对象：`abc.efd.hh.id` / `type: "string"` / `max_len: 33`、`abc.efd.hh.name` 等。
- **原因**：
  - `ValueType` 只有 `char` / `int` / `bool` / `enum`，没有 `array` / `object`；
  - JSON body 解析只读取顶层 key，嵌套 object/array 整体变成 `Composite`，不会继续下钻；
  - query / form 解析成扁平的 `(name, value)`，不解析 `a[]` / `a[b]` 语义；
  - `unknown: "reject"` 也只按顶层字段名判断。
- **期望方向（Laravel 风格）**：
  - 字段声明 `type: "array"`；
  - `items.*.id` 这类通配路径逐元素校验；
  - `abc.efd.hh.id` 这类点路径校验嵌套对象；
  - 同时需要定义：通配符语法、错误 `field` 路径（如 `body.items.0.id`）、缺失/多余键、数组长度/嵌套深度上限。
- **相关代码**：
  - `crates/gateway-core/src/contract.rs`：`ValueType`
  - `crates/gateway-core/src/body.rs`：`parse_json`
  - `crates/gateway-core/src/query.rs`：`parse`
  - `crates/gateway-core/src/params.rs`：规则匹配与 `unknown` 检查
- **状态**：未实现。

## 3. etcd 契约来源未开发

- **现象**：目前只有 `gateway_info_file` / `gateway_info_url`，没有 etcd watch 来源。
- **原因**：etcd 需要长连接、revision、断线重连、全量 + 增量同步；当前 `ContractSource` 只有一次 `fetch`。
- **相关代码**：
  - `crates/ngx-gateway/src/contract_source.rs`
  - `crates/ngx-gateway/src/contract_manager.rs`
  - 设计见 `docs/CONTRACT_UNIFICATION_DESIGN.md` §4 / §8
- **状态**：未实现。

## 4. `/_gateway_refresh` 主动刷新接口未实现（已确认需要）

- **现象**：nginx 配置不变、只改后端 `/info` 时，网关不会自动重新拉取。
- **需求**：任意 worker 收请求，不拉 `/info`；由 leader 按 location 名单刷新；
  `mode=auto|check|force`；同步返回 JSON，不按刷新结果映射 HTTP 状态码。
- **相关代码**：`crates/ngx-gateway/src/contract_manager.rs`、`contract_shm.rs`、`lib.rs`
- **设计稿**：`docs/REFRESH_ENDPOINT_DESIGN.md`
- **状态**：设计已定，代码未实现。

## 5. nginx 预检 413 无法补网关头 / 响应体

- **现象**：请求体超过 `client_max_body_size` 时 nginx 在进入插件前就返回 413，`X-Gateway-*`、`X-Request-Id`、`responses.body_too_large` 都不生效。
- **原因**：预检发生在 access handler 之前。
- **相关代码**：`crates/ngx-gateway/src/lib.rs`：`reject_body_too_large`（只处理插件读 body 得到的 413）
- **状态**：已知限制。

## 7. 流程问题：AI 的提议没有标明“待确认”，且不主动清理/告知

- **现象**：
  - “内置探针”（`.php` / `.env` / `.git` / `/wp-*` 硬编码拦截）是 AI 主动提出的演示方案，但没有明确说
    “这是我加的提议 / 你没要求过”，而是直接实现并写进协议，看起来像既定需求；
  - 后续多次改架构（契约管理器、TTL 移除、响应覆盖）都没有主动指出和删除它，直到用户明确问
    `probe_block_reason` 为什么还在；
  - 类似地，`responses.blocked`、`X-Gateway-Blocked` 都是围绕这个未确认方案长出来的。
- **原因**：没有区分“AI 提议”与“用户确认的需求”；提议没有登记，后续需求变化时也没有审计同一功能域的旧实现；
  答复只报新增，不报删除/保留。
- **相关记录**：
  - 已删除：`crates/ngx-gateway/src/lib.rs` 的 `probe_block_reason`、`responses.blocked`、`X-Gateway-Blocked`
  - 约束已写入：`AGENTS.md`「区分提议与已确认需求 / 临时实现登记 / 需求变化同步清理 / 主动报告删除项」
- **状态**：本次已清理；后续允许主动提议，但必须标注“待确认”并登记，确认后才进入协议和实现。

