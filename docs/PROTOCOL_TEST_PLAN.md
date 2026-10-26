# 协议测试开发计划（纯 `gateway-core`，不加载 nginx）

> 状态：**已实现**（`crates/gateway-core/tests/protocol_v1.rs`，42 项全绿）。第 6 节清单仍是后续补用例的对照表。
> 协议本身以 [PROTOCOL.md](PROTOCOL.md) 为准；本文只描述怎么测、测哪些、不测哪些。

## 1. 目标

在 `crates/gateway-core/tests/` 里用 `cargo test` 覆盖协议 v1 的每一条可本地判定的规则：

- 不启动 nginx、不加载 `.so`、不访问网络、不依赖执行顺序；
- 每个用例自带契约、自己发请求、断言硬编码期望值；
- 能直接断言**状态码**（`404` / `405`）、`Allow` 头内容和"是否转发"；
- 用例名与协议章节对应，新增协议规则时能一眼看出该补哪条测试。

## 2. 写法约束（"解耦、直白"的具体含义）

1. **一个 test = 一份契约 + 一到多个请求。** 同一份契约下的"通过 / 方法不符 / 未声明"可以写在同一个 test 里，按顺序断言。
2. **契约 JSON 内联在 test 里**，不使用 `REPO_CONTRACT` 之类的共享常量。改一个用例不会影响别的用例。
3. **不用共享 helper**（现有 `gw()` / `check()` 删掉）。每个 test 直接：

   ```rust
   let gw = Gateway::parse(/* 契约 */).expect("contract should load");
   let out = gw.check(&RequestCtx { method, path });
   ```

4. **测试里不写判定逻辑**：不做表驱动循环、不用宏生成用例、不按条件推算期望值。期望一律写死（`Some(405)`、`Some("GET")`）。
5. **一个行为一条断言组**，失败信息里要能看出是"哪个请求、期望什么"。
6. 只依赖 `gateway-core` 的公开 API：`Gateway::parse` / `Gateway::check` / `RequestCtx` / `Outcome` / `ContractError` / `Decision`。

### 样例（实现时照这个形状写）

```rust
#[test]
fn s4_declared_route_matrix() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [
                { "path": "/urlabc", "methods": ["GET"] }
            ]
        }"#,
    )
    .expect("contract should load");

    // 1) 声明过的路径 + 声明过的方法：放行，不产生响应
    let out = gw.check(&RequestCtx { method: "GET", path: "/urlabc" });
    assert_eq!(out.decision.status_code(), None);
    assert_eq!(out.allow, None);
    assert!(!out.forwarded);

    // 2) 声明过的路径 + 未声明的方法：405 + Allow
    let out = gw.check(&RequestCtx { method: "POST", path: "/urlabc" });
    assert_eq!(out.decision.status_code(), Some(405));
    assert_eq!(out.allow, Some("GET"));
    assert!(!out.forwarded);

    // 3) 配置里没有的路径：默认 deny → 404
    let out = gw.check(&RequestCtx { method: "GET", path: "/urlnotexist" });
    assert_eq!(out.decision.status_code(), Some(404));
    assert_eq!(out.allow, None);
    assert!(!out.forwarded);
}
```

反例（不要这样写）：

```rust
// ❌ 共享夹具：改 REPO_CONTRACT 会连带改掉一批用例的语义
const REPO_CONTRACT: &str = r#"..."#;
fn gw() -> Gateway { Gateway::parse(REPO_CONTRACT).unwrap() }

// ❌ 封装掉了 forwarded / 状态码，测试看不到完整结果
fn check(gw: &Gateway, m: &str, p: &str) -> (Decision, Option<&str>) { ... }

// ❌ 表驱动 + 循环：失败时不知道是哪个契约
for contract in [...] { ... }
```

## 3. 改造前现状（25 项，原 `tests/contract_cases.rs`）

### 3.1 结构问题

| 问题 | 现状 | 影响 |
| --- | --- | --- |
| 共享夹具 | 9 个用例依赖 `REPO_CONTRACT` + `gw()` | 改夹具会静默改变多个用例的前提 |
| 封装丢信息 | 21 处走 `check()`，只回传 `decision` / `allow` | 测不到 `forwarded`，也测不到状态码 |
| 没有状态码 API | `404/405` 写死在 `crates/ngx-gateway/src/lib.rs` | 不加载 nginx 就无法断言"返回状态码" |
| 表驱动 | `unmatched_defaults_to_deny` 用 `for` 循环 | 失败时只能看到整组失败 |
| 无章节映射 | 测试名是描述性的，但不带协议章节 | 无法核对"协议条款是否都有用例" |

### 3.2 覆盖缺口

| 协议条款 | 规则 | 现状 |
| --- | --- | --- |
| §3.1 | 路径**区分大小写** | ❌ 无 |
| §3.1 | 百分号编码**不解码**（`/api/%68ealth` 与 `/api/health` 不同） | ❌ 无 |
| §3.1 | `//api/health` 与 `/api/health` 是两条路径 | ❌ 无（只在 `tests/phase1.sh` 端到端测过） |
| §3.1 | 根路径 `/` 可用 | ❌ 无 |
| §3.1 | 通配符必须出现在最后一段 | ❌ 无 |
| §3.2 | `Allow` 按**契约书写顺序**输出（不只是 `GET, PUT`） | ⚠️ 只有一条顺序巧合的断言 |
| §3.2 | 全空白方法串（`"   "`）拒绝 | ❌ 无 |
| §2.1 | `forward` 下已声明路由的方法校验仍然生效（独立用例） | ⚠️ 混在 forward 用例里 |
| §2.1 | `"unmatched"` 值大小写敏感（`"FORWARD"` 拒绝） | ❌ 无 |
| §4 | `Decision` → 状态码 / reason token 的映射 | ❌ 无 |
| §2 | `protocol: 1` 显式写出可接受 | ⚠️ 只在夹具里隐式覆盖 |

### 3.3 结论

- 现有用例覆盖了主干（命中、方法、尾斜杠、参数路由、通配、重复路由、严格字段），**可以作为重写的基础**，但需要拆掉共享夹具、补上状态码与上表缺口。
- 目标：约 **42 个 test**，预计 400~500 行，全部位于 `crates/gateway-core/tests/protocol_v1.rs`。

## 4. 实现前需要的一处生产代码改动

`gateway-core` 目前只有 `Decision`，没有状态码。为了让"不加载 nginx 也能断言 404/405"，增加：

```rust
impl Decision {
    /// `Allow` 不产生响应（继续走 proxy），返回 `None`。
    pub fn status_code(self) -> Option<u16> {
        match self {
            Decision::Allow => None,
            Decision::RouteNotDeclared => Some(404),
            Decision::MethodNotAllowed => Some(405),
        }
    }
}
```

并让 `crates/ngx-gateway/src/lib.rs` 的 404 / 405 分支改用这个返回值，保证**测试与线上是同一份映射**（现在适配层用的是 `HTTPStatus::NOT_FOUND` / `NOT_ALLOWED`，属于两套常量）。

- 不新增依赖；
- 不改变线上行为；
- `503 contract-not-ready` 属于适配层语义，不放进 `Decision`。

## 5. 文件与命名

| 项 | 约定 |
| --- | --- |
| 文件 | `crates/gateway-core/tests/protocol_v1.rs`（现 `contract_cases.rs` 的内容并入后删除原文件） |
| 分区 | 文件内用注释分成 §2 / §2.1 / §3.1 / §3.2 / §4 五段 |
| 命名 | `s2_*` / `s21_*` / `s31_*` / `s32_*` / `s4_*`，例如 `s4_undeclared_route_returns_404` |
| 断言顺序 | 先 `status_code`，再 `allow`，最后 `forwarded`，每个用例保持一致 |
| 单跑 | `cargo test -p gateway-core --test protocol_v1 s31_` |

## 6. 用例清单

图例：**新增** = 改造前没有；**保留** = 从旧文件迁移（改为自包含写法）；**拆分** = 原来混在一个 test 里。
下表的标记保留改造时的分类，供以后对照；当前 42 项均已实现。

### 6.1 §2 顶层结构

| 用例 | 契约要点 | 请求 | 期望 |
| --- | --- | --- | --- |
| `s2_protocol_omitted_defaults_to_1`（保留） | 只有 `routes: []` | — | 解析成功，`protocol() == 1` |
| `s2_protocol_1_accepted`（新增） | 显式 `"protocol": 1` | — | 解析成功 |
| `s2_protocol_2_rejected`（保留） | `"protocol": 2` | — | `UnsupportedProtocol { protocol: 2, supported: 1 }` |
| `s2_version_and_ttl_exposed`（保留） | `version` + `ttl` 都写 | — | `version() == Some(...)`、`ttl() == Some(3)` |
| `s2_meta_is_ignored`（保留） | `meta` 为任意 JSON / 省略 | — | 解析成功 |
| `s2_unknown_top_level_field_rejected`（保留） | 顶层多一个 `responses` | — | `UnknownField { scope: "contract", name: "responses" }` |
| `s2_routes_omitted_denies_all`（拆分） | 不写 `routes` | `GET /api/health` | 404 |
| `s2_invalid_json_rejected`（保留） | `{not json` | — | `ContractError::Json` |

### 6.2 §2.1 未声明路由策略

| 用例 | 契约要点 | 请求 | 期望 |
| --- | --- | --- | --- |
| `s21_unmatched_defaults_to_deny`（拆分） | 不写 `unmatched` | `GET /urlnotexist` | 404，`forwarded == false` |
| `s21_unmatched_deny_explicit`（新增） | `"unmatched": "deny"` | `GET /urlnotexist` | 404 |
| `s21_unmatched_forward_allows_unknown`（拆分） | `"unmatched": "forward"` | `GET /urlnotexist` | `status_code() == None`，`forwarded == true` |
| `s21_unmatched_forward_keeps_declared`（新增） | forward + `/urlabc GET` | `GET /urlabc` | 放行，`forwarded == false` |
| `s21_unmatched_forward_keeps_method_check`（拆分） | forward + `/urlabc GET` | `POST /urlabc` | 405，`Allow: GET`，`forwarded == false` |
| `s21_unmatched_invalid_value_rejected`（保留） | `"unmatched": "allow"` | — | `ContractError::Json` |
| `s21_unmatched_is_case_sensitive`（新增） | `"unmatched": "FORWARD"` | — | `ContractError::Json` |

### 6.3 §3.1 路径匹配

| 用例 | 契约要点 | 请求 | 期望 |
| --- | --- | --- | --- |
| `s31_static_path_matches`（保留） | `/api/health GET` | `GET /api/health` | 放行 |
| `s31_path_is_case_sensitive`（新增） | `/api/health GET` | `GET /api/Health` | 404 |
| `s31_trailing_slash_not_matched`（保留） | `/api/products GET` | `GET /api/products/` | 404 |
| `s31_contract_trailing_slash_rejected`（保留） | 契约写 `/api/x/` | — | `InvalidRoute` |
| `s31_relative_path_rejected`（保留） | 契约写 `api/x` | — | `InvalidRoute` |
| `s31_duplicate_path_rejected`（保留） | 同一 `path` 写两次 | — | `InvalidRoute { index: 1 }` |
| `s31_ambiguous_param_routes_rejected`（保留） | `/u/{id}` 与 `/u/{name}` | — | `InvalidRoute { index: 1 }` |
| `s31_param_route_single_segment`（拆分） | `/api/user/{id} GET` | `GET /api/user/42`；`GET /api/user/42/x` | 放行；404 |
| `s31_wildcard_matches_rest`（保留） | `/static/{*rest} GET` | `GET /static/a/b/c.txt` | 放行 |
| `s31_wildcard_must_be_last`（新增，待验证） | `/static/{*rest}/x` | — | `InvalidRoute`（依赖 matchit 行为，实现时确认） |
| `s31_static_wins_over_param`（保留） | `/a/{x} POST` + `/a/b GET` | `GET /a/b`；`POST /a/b`；`POST /a/c` | 放行；405；放行 |
| `s31_percent_encoding_not_decoded`（新增） | `/api/health GET` | `GET /api/%68ealth` | 404 |
| `s31_double_slash_is_distinct`（新增） | `/api/health GET` | `GET //api/health` | 404 |
| `s31_root_path_works`（新增） | `/ GET` | `GET /` | 放行 |

### 6.4 §3.2 方法匹配

| 用例 | 契约要点 | 请求 | 期望 |
| --- | --- | --- | --- |
| `s32_method_is_case_insensitive`（保留） | `methods: ["GET"]` | `get /api/health` | 放行 |
| `s32_methods_omitted_allows_any`（拆分） | 路由不写 `methods` | `PATCH /api/echo` | 放行 |
| `s32_empty_methods_allows_any`（拆分） | `methods: []` | `DELETE /api/echo` | 放行 |
| `s32_empty_method_rejected`（保留） | `methods: [""]` | — | `EmptyMethod { index: 0 }` |
| `s32_blank_method_rejected`（新增） | `methods: ["   "]` | — | `EmptyMethod { index: 0 }` |
| `s32_allow_keeps_contract_order`（新增） | `methods: ["PUT", "GET"]` | `POST /a` | 405，`Allow: "PUT, GET"` |
| `s32_dynamic_route_wrong_method`（新增） | `/api/user/{id} GET` | `DELETE /api/user/42` | 405，`Allow: "GET"` |

### 6.5 §4 判定顺序与状态码

| 用例 | 契约要点 | 请求 | 期望 |
| --- | --- | --- | --- |
| `s4_declared_route_matrix`（新增，用户给的场景） | `/urlabc GET` | `GET /urlabc`；`POST /urlabc`；`GET /urlnotexist` | `None`；`Some(405)` + `Allow: GET`；`Some(404)` |
| `s4_forward_matrix`（新增，用户给的场景） | 同上 + `"unmatched": "forward"` | `GET /urlnotexist`；`GET /urlabc`；`POST /urlabc` | 放行且 `forwarded`；放行且不转发；405 |
| `s4_allow_has_no_status`（新增） | `/urlabc GET` | `GET /urlabc` | `status_code() == None` |
| `s4_route_not_declared_is_404`（新增） | `routes: []` | `GET /x` | `status_code() == Some(404)` |
| `s4_method_not_allowed_is_405`（新增） | `/urlabc GET` | `POST /urlabc` | `status_code() == Some(405)` |
| `s4_reason_tokens`（新增） | — | — | `allow` / `route-not-declared` / `method-not-allowed` |

合计：**42 个 test**（§2 8 + §2.1 7 + §3.1 14 + §3.2 7 + §4 6，其中 `s4_*` 与前面的矩阵有少量重叠，属于"状态码视角"的显式锁定）。

## 7. 与 `tests/phase1.sh` 的分工

| 层次 | 覆盖内容 | 工具 |
| --- | --- | --- |
| `gateway-core`（本文） | 契约解析、路径/方法判定、`unmatched`、状态码与 reason token | `cargo test`，不需要 nginx |
| `tests/phase1.sh` | nginx 实际写头（`X-Gateway-Name` / `X-Gateway-Reason` / `Allow`）、多网关"与"语义、503 冷启动、原始 request target、探针 | curl + 运行中的 nginx |

`X-Gateway-*` 由适配层写，**不在** core 测试里断言；否则就得把响应头逻辑塞进 core。

## 8. 待确认（实现前定，避免返工）

1. **未声明路由的默认策略**：协议当前是 `deny`（404，fail-closed）。需求描述里出现过"默认没有指定时路由为转发"的说法；清单按"默认 deny / 显式 forward"两条都写了。若确认默认改为 `forward`，需同步改 `PROTOCOL.md`、`gateway-core` 和 `tests/phase1.sh`。
2. **HEAD 语义**：契约只写 `GET` 时，`HEAD` 是否放行？当前代码按字面判 → 405。先不写用例，等语义定下来再补，避免把临时行为固化成契约。
3. **`/` 作为契约路径**：协议允许（`/` 本身除外），用例按"允许且能匹配"写；如果实现上要禁止，先改文档。

## 9. 验收

```bash
cargo test -p gateway-core            # 全绿，不需要 nginx
cargo test -p gateway-core --test protocol_v1 s31_   # 单跑一组
```

- 清单第 6 节每行都有对应用例，没有"写了规则但没测"的条款；
- 任一用例单独运行都能通过（没有共享状态 / 顺序依赖）；
- 测试里没有 `for` / 表驱动 / 条件推算期望值；
- `contract_cases.rs` 删除后，总用例数不减少（旧断言全部有对应项）。

## 10. 本次不做

- `query` / `body` / `headers` / `responses`：协议字段还没实现，等实现时按同样格式补测试；
- nginx 层行为：响应头、多网关、503、探针、`merge_slashes`（由 `phase1.sh` 覆盖）；
- 性能、并发、fuzz。
