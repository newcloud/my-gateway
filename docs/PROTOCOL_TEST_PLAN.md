# 协议测试开发计划（纯 `gateway-core`，不加载 nginx）

> 状态：**已实现**（`crates/gateway-core/tests/protocol_v1.rs`，196 项全绿）。第 6 节清单仍是后续补用例的对照表。
> 开发顺序也由本文约定：**先写 `gateway-core` 测试，再接入 nginx 并补 `phase1.sh` 集成测试**（见 §1.1）。
> 协议本身以 [PROTOCOL.md](PROTOCOL.md) 为准；本文只描述怎么测、测哪些、不测哪些。

## 1. 目标

在 `crates/gateway-core/tests/` 里用 `cargo test` 覆盖协议 v1 的每一条可本地判定的规则：

- 不启动 nginx、不加载 `.so`、不访问网络、不依赖执行顺序；
- 每个用例自带契约、自己发请求、断言硬编码期望值；
- 能直接断言**状态码**（`404` / `405`）、`Allow` 头内容和"是否转发"；
- 用例名与协议章节对应，新增协议规则时能一眼看出该补哪条测试。

### 1.1 开发顺序（强制：先 core 测试，再 nginx 集成）

后续每个能力切片都按下面的顺序推进，参考已完成的 Header 存在性：

1. **协议先行**：先改 `docs/PROTOCOL.md`，明确字段、状态码、`reason` / `field` / `rule` 和失败优先级；
   未实现部分继续标 🧪。
2. **先写 core 测试**：在本文第 6 节对应章节补 `s<章节>_*` 用例，覆盖正常、缺失/非法、优先级、大小写、
   重复、契约拒绝等边界。此时代码还没改，用例应当是红的。
3. **实现 core 到全绿**：`cargo test -p gateway-core`、`cargo clippy -p gateway-core --all-targets`、
   `cargo fmt --all -- --check` 全部通过。core 测试**不加载 nginx**。
4. **再接入 nginx**：适配层收集请求数据并映射状态码/响应头/日志；demo 契约和 `deploy/nginx.conf` 只打开这个切片。
5. **补集成测试**：在 `tests/phase1.sh` 增加真实 nginx 断言（状态码、`X-Gateway-*`、日志、未到后端、
   未实现能力仍被拒绝）。**只有 core 单测、没有集成测试的切片不算完成。**
6. **全量验收并回写文档**：`build-module.sh` → `dev.sh restart` → `tests/phase1.sh`，然后更新
   `PROGRESS.md` 的验证结果、本文的用例数、以及 `PLAN.md` / `PHASE1_PLAN.md` 里的过期计数。

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
- 目标：约 **42 个 test**（后续 Header 存在性补 18 个、Header 值规则补 20 个、Header `required_if` 补 17 个、Query 补 10 个、Body 补 12 个、路径参数补 8 个、分组补 13 个、分组剩余继承补 9 个、filters 补 11 个、path_regex/priority/responses 补 11 个、filters scope 补 8 个、严格模式补强补 6 个、body max_bytes 补 5 个、JSON 结构扫描补 6 个，当前共 196 个），全部位于 `crates/gateway-core/tests/protocol_v1.rs`。

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
            Decision::MissingParameter | Decision::InvalidParameter => Some(400),
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
| 分区 | 文件内用注释分成 §2 / §2.1 / §3.1 / §3.2 / §4 / §5.0.1–5.3（Header、Header 值规则、`required_if`、Query、Body） |
| 命名 | `s2_*` / `s21_*` / `s31_*` / `s32_*` / `s4_*` / `s53_*`，例如 `s53_required_header_missing_is_400` |
| 断言顺序 | 先 `status_code`，再 `allow` / `forwarded`，最后 `field` / `rule`，每个用例保持一致 |
| 单跑 | `cargo test -p gateway-core --test protocol_v1 s31_` |

## 6. 用例清单

图例：**新增** = 改造前没有；**保留** = 从旧文件迁移（改为自包含写法）；**拆分** = 原来混在一个 test 里。
下表的标记保留改造时的分类，供以后对照；原 42 项均已实现，Header 存在性另补 18 项、Header 值规则另补 20 项、Header `required_if` 另补 17 项、Query 补 10 项、Body 补 12 项、路径参数补 8 项、分组补 13 项、分组剩余继承补 9 项、filters 补 11 项、路由级 path_regex / priority / responses 补 11 项。

### 6.1 §2 顶层结构

| 用例 | 契约要点 | 请求 | 期望 |
| --- | --- | --- | --- |
| `s2_protocol_omitted_defaults_to_1`（保留） | 只有 `routes: []` | — | 解析成功，`protocol() == 1` |
| `s2_protocol_1_accepted`（新增） | 显式 `"protocol": 1` | — | 解析成功 |
| `s2_protocol_2_rejected`（保留） | `"protocol": 2` | — | `UnsupportedProtocol { protocol: 2, supported: 1 }` |
| `s2_version_exposed` | `version` 写入 | — | `version() == Some(...)` |
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
| `s4_reason_tokens`（新增） | — | — | `allow` / `route-not-declared` / `method-not-allowed` / `missing-parameter` / `invalid-parameter` |

### 6.6 §5.3 Header 存在性（新增，18 项）

| 用例 | 契约 / 请求 | 期望 |
| --- | --- | --- |
| `s53_true_shorthand_accepts_present_header` | `authorization: true`，请求带 `Authorization` | `Allow`，`field=None` |
| `s53_required_header_missing_is_400` | `authorization: {required:true}`，不带头 | `400 missing-parameter`，`field=header.authorization`，`rule=required` |
| `s53_optional_header_may_be_absent` | `x-trace-id: {required:false}`，不带头 | `Allow` |
| `s53_empty_header_value_counts_as_present` | `authorization: true`，值 `""` | `Allow`（存在性只看是否出现） |
| `s53_header_name_is_case_insensitive` | 契约 `X-Api-Version: true`，请求 `x-api-version` / 缺失 | `Allow` / `field=header.x-api-version` |
| `s53_duplicate_request_header_lines_are_invalid` | 同一头两行（大小写不同） | `400 invalid-parameter`，`rule=duplicate` |
| `s53_undeclared_headers_are_ignored` | 请求带 `user-agent` / `cookie` | `Allow` |
| `s53_required_missing_wins_over_duplicate_check` | 缺必填头，同时另一个头重复 | 先报 `missing-parameter` |
| `s53_missing_header_reports_contract_order` | 契约先写 `z-last` 再写 `a-first`，都缺 | 报 `header.z-last`（按契约顺序，不是字母序） |
| `s53_method_error_precedes_header_check` | 方法不符 + 缺头 | `405`，不检查 Header |
| `s53_header_rules_are_per_route` | 两条路由，只有一条有 Header 规则 | 各自生效 |
| `s53_duplicate_contract_header_names_rejected` | 契约同时写 `Authorization` / `authorization` | 加载失败 |
| `s53_exact_duplicate_contract_header_key_rejected` | 契约里同一个键写两次（JSON 重复键） | 解析失败，不采用“最后一个生效” |
| `s53_invalid_header_name_rejected` | 名字含空格 | 加载失败 |
| `s53_false_rule_rejected` | 规则值 `false` | 加载失败 |
| `s53_empty_rule_rejected` | 规则值 `{}` | 加载失败 |
| `s53_unimplemented_rule_field_rejected` | 规则里写 `filters` | `UnknownField`，整份拒绝 |
| `s53_headers_block_unknown_option_rejected` | `headers` 块写 `unknown` | `UnknownField`，整份拒绝 |

### 6.7 §5.3 Header 值规则（新增，20 项）

| 用例 | 契约 / 请求 | 期望 |
| --- | --- | --- |
| `s53_char_length_counts_unicode_code_points` | `char, len: 2`，值 `你好` / `你好吗` | `Allow` / `400 rule=len` |
| `s53_char_min_and_max_len` | `char, min_len: 2, max_len: 4` | 空串/1 字符 `min_len`；5 字符 `max_len`；3 字符通过 |
| `s53_char_rejects_nul` | `char`，值含 `\0` | `400 rule=char` |
| `s53_header_value_ows_is_trimmed` | `char, len: 2`，值 `"  ab\t"` | `Allow` |
| `s53_int_syntax_is_strict` | `int`，值 `0` / `-12` / `" 42 "` 通过；`01` / `+1` / `1.0` / `1e3` / 中间空格拒绝 | `Allow` / `400 rule=int` |
| `s53_int_min_and_max` | `int, min: 1, max: 99`，值 `0` / `100` / `99` | `min` / `max` / `Allow` |
| `s53_bool_forms_are_case_insensitive` | `bool`，`true/TRUE/false/False/1/0` 通过；`yes` 拒绝 | `Allow` / `400 rule=bool` |
| `s53_enum_values_and_ignore_case` | `enum` 开/关 `ignore_case` | 大小写不敏感通过；`gamma` / 严格模式 `ALPHA` → `400 rule=values` |
| `s53_regex_is_full_match` | `regex: [A-Z]{2}[0-9]{2}`，值 `AB12` / `xAB12` / `AB12x` | `Allow` / 两个 `400 rule=regex` |
| `s53_optional_value_rule_skipped_when_absent` | 可选值规则，不带头 | `Allow` |
| `s53_required_with_value_rule_accepts_valid_value` | `required + char, min_len: 2`，值 `ab` | `Allow` |
| `s53_required_missing_wins_over_value_error` | 缺必填头，同时另一头值非法 | `missing-parameter`（`header.x-must`，`rule=required`） |
| `s53_duplicate_wins_over_value_error` | 同一头两行且值非法 | `invalid-parameter`，`rule=duplicate` |
| `s53_value_errors_report_contract_order` | 契约先 `z-count` 后 `a-count`，两个都非法 | 报 `header.z-count`（契约顺序） |
| `s53_non_utf8_value_is_invalid_when_value_rule_exists` | 值传 `None` + `char` 规则 | `400`，`rule=char` |
| `s53_non_utf8_value_is_ok_for_existence_only_rule` | 值传 `None` + `true` 规则 | `Allow` |
| `s53_value_constraints_must_match_type` | `char+min` / `int+max_len` / `char+ignore_case` | 加载失败 |
| `s53_invalid_length_and_range_combinations_rejected` | `len+min_len` / `min>max` / `min_len>max_len` | 加载失败 |
| `s53_invalid_enum_values_rejected` | 空 `values` / `ignore_case` 下 `A` 与 `a` 重复 | 加载失败 |
| `s53_invalid_regex_rules_rejected` | `enum+regex` / 非法正则 `(` | 加载失败 |

### 6.8 §5.0.1 / §5.3 Header `required_if`（新增，17 项）

| 用例 | 契约 / 请求 | 期望 |
| --- | --- | --- |
| `s53_required_if_string_presence_triggers` | `x-token: required_if "x-mode"`，`x-mode` 出现、无 token | `400 missing-parameter`，`field=header.x-token`，`rule=required_if` |
| `s53_required_if_string_absent_condition_false` | 同上，`x-mode` 缺失 | `Allow` |
| `s53_required_if_string_empty_value_counts_as_present` | 同上，`x-mode: ""` | `400`，`rule=required_if` |
| `s53_required_if_object_equals_matches` | `{field: x-mode, equals: secure}`，`secure` / `open` | `400` / `Allow` |
| `s53_required_if_object_equals_is_case_sensitive` | 同上，`SECURE` | `Allow`（不触发） |
| `s53_required_if_object_trims_ows` | 同上，`"  secure\t"` | `400`，`rule=required_if` |
| `s53_required_if_non_utf8_counts_as_present_for_string_form` | 字符串形式，被引用值传 `None` | `400`，`rule=required_if` |
| `s53_required_if_non_utf8_never_equals_for_object_form` | 对象形式，被引用值传 `None` | `Allow` |
| `s53_required_if_target_present_still_validates_value` | 条件成立、目标值 `a`（`min_len: 2`） | `400`，`rule=min_len` |
| `s53_required_if_condition_false_still_validates_present_value` | 条件不成立、目标值 `a` | `400`，`rule=min_len` |
| `s53_required_if_missing_wins_over_duplicate` | 条件必填缺失 + 另一头重复 | `missing-parameter`，`rule=required_if` |
| `s53_unconditional_required_wins_over_required_if` | 无条件必填和条件必填都缺失，条件规则写在前面 | 报无条件字段，`rule=required` |
| `s53_required_if_missing_reports_contract_order` | 两个条件都成立且都缺失 | 报契约中靠前的字段 |
| `s53_required_if_self_reference_rejected` | `required_if` 引用自己 | 加载失败 |
| `s53_required_if_unknown_reference_rejected` | 引用未声明的头 | 加载失败 |
| `s53_required_if_with_required_true_rejected` | `required: true` + `required_if` | 加载失败 |
| `s53_required_if_invalid_forms_rejected` | 空字符串 / 空 `field` / 缺 `equals` / 未知键 / 非字符串 `equals` | 全部拒绝（未知键为 `UnknownField`） |

### 6.9 §5.1 Query（新增，10 项）

| 用例 | 契约 / 请求 | 期望 |
| --- | --- | --- |
| `s51_query_required_and_range` | `page int required 1..10` | 缺失 `missing`；`0` → `min`；`abc` → `int`；`5` → `Allow` |
| `s51_query_percent_decoding_and_plus` | `kw char len 3`，`%E4%BD%A0+%E5%A5%BD` | `Allow`（解码 + `+` 转空格） |
| `s51_query_duplicate_after_decoding_is_rejected` | `%61=1&a=2` | `400`，`field=query.a`，`rule=duplicate` |
| `s51_query_malformed_encoding_and_utf8` | `%ZZ` / `%FF` / 单独 `%` | `400`，`rule=encoding`，无 `field` |
| `s51_query_empty_key_and_too_many_pairs` | `=x` / 257 对 | `empty-key` / `limit` |
| `s51_query_unknown_ignore_and_reject` | `unknown` 两种策略 | ignore 放行；reject 报 `query.extra`，声明字段错误优先 |
| `s51_query_required_if_and_empty_value` | 对象/字符串形式条件必填、空 `page` | 条件成立缺失 `required_if`；空值算出现；空 int 报 `int` |
| `s51_query_enum_bool_regex_and_ignore_case` | `enum` / `bool` / `regex` | 合法放行；`values` / `bool` / `regex` 分别报错 |
| `s51_query_block_omitted_ignores_malformed_query` | 无 `query` 块，`a=%ZZ` | `Allow`（完全不解析） |
| `s51_query_contract_shape_rejected` | 非法 `unknown` / `false` / 未声明引用 | 整份契约拒绝 |

### 6.10 §5.2 Body（新增，12 项；body.max_bytes 另补 5 项）

| 用例 | 契约 / 请求 | 期望 |
| --- | --- | --- |
| `s52_body_json_required_and_range` | `sku char required`、`qty int required 1..99` | 合法放行；缺 `qty` / `qty=0` 分别 `required` / `min` |
| `s52_body_json_int_types` | JSON 整数 / 字符串 / 负数；浮点 / 指数 / 布尔 / 前导零 | 前三放行，后四 `rule=int` |
| `s52_body_json_char_enum_regex_require_string` | 数字 / 对象 / 数字枚举 / 数字正则 | `char` / `type` / `values` / `regex` |
| `s52_body_json_bool_forms` | `true/false/1/0/"TRUE"`；`2` | 放行；`2` → `bool` |
| `s52_body_unknown_default_reject_and_ignore` | 默认 `unknown` / 显式 `ignore` | 默认报 `body.is_admin`；ignore 放行 |
| `s52_body_json_duplicate_key_and_shape` | 重复键 / 顶层数组 / 坏 JSON | `duplicate` / `json` / `json` |
| `s52_body_form_parsing_and_duplicates` | form 合法 / 重复键 / 坏 `%` | 放行 / `duplicate` / `form` |
| `s52_body_media_type_checks` | 缺 `Content-Type` / `text/plain` / 非 UTF-8 charset / gzip / 空 body | 前四 `unsupported-media-type`（不同 `rule`）；空 body 放行 |
| `s52_body_content_types_restriction` | `content_types: ["application/json"]` + form / JSON | form `unsupported`；JSON 放行 |
| `s52_body_empty_null_and_required_if` | 空对象 / `null` / 条件必填 / JSON 数字不等于 `"1"` | `required` / `null` 视为缺失 / `required_if` / 不触发 |
| `s52_body_empty_object_unknown_reject` | `params: {}` + `{"is_admin":true}` | `400`，`field=body.is_admin`，`rule=unknown` |
| `s52_body_contract_shape_rejected` | 非法 `content_types` / 非法 `unknown` / 未声明引用 | 整份契约拒绝 |

### 6.10.1 §5.2 body.max_bytes（新增，5 项）

| 用例 | 契约 / 请求 | 期望 |
| --- | --- | --- |
| `s52_body_max_bytes_exposed_per_route` | 路由 `max_bytes: 1024` / 无 body 块 | `BodyRules` 暴露 1024 / `None` |
| `s52_body_max_bytes_group_inherits_stricter_value` | 组 2048，子级 1024 / 4096 | 1024 / 2048（取更严格者） |
| `s52_body_max_bytes_zero_disables_and_can_reenable` | 组 2048，子级 0；嵌套子级再开 512 | 关闭为 `None`，再开启为 512 |
| `s52_body_max_bytes_without_body_block_is_none` | 路由无 body 块 | `None` |
| `s52_body_max_bytes_invalid_values_rejected` | `-1` / 字符串 / 浮点 | 契约拒绝 |

### 6.10.1 §5.2 body.max_bytes（新增，5 项）

| 用例 | 契约 / 请求 | 期望 |
| --- | --- | --- |
| `s52_body_max_bytes_exposed_per_route` | 路由 `max_bytes: 1024` / 无 body 块 | 暴露 1024 / `None` |
| `s52_body_max_bytes_group_inherits_stricter_value` | 组 2048，子级 1024 / 4096 | 1024 / 2048（取更严格者） |
| `s52_body_max_bytes_zero_disables_and_can_reenable` | 组 2048，子级 0；嵌套再开 512 | `None` / 512 |
| `s52_body_max_bytes_without_body_block_is_none` | 路由无 body 块 | `None` |
| `s52_body_max_bytes_invalid_values_rejected` | `-1` / 字符串 / 浮点 | 契约拒绝 |

### 6.10.2 §5.2 JSON 结构扫描（新增，6 项）

| 用例 | 契约 / 请求 | 期望 |
| --- | --- | --- |
| `s52_json_limits_depth` | `max_depth: 3`，3 / 4 层嵌套 | 3 层放行；4 层 `rule=json-depth` |
| `s52_json_limits_string_length_and_escapes` | `max_string_length: 8`，9 字节字符串、`[{}\"]` | 9 字节拒绝；8 字节与字符串内括号/转义引号放行 |
| `s52_json_limits_array_entries` | `max_array_entries: 3`，3 / 4 元素、嵌套数组 | 3 元素放行；4 元素 `json-array-entries` |
| `s52_json_limits_inherit_and_zero_disables` | 组 4 层/16 字节/10 元素；子级 2 层；子级 0 | 继承/收紧/关闭都生效 |
| `s52_json_limits_defaults` | 默认 32 层 / 8192 字节 / 500 元素 | 32/8192/500 放行；33/8193/501 拒绝 |
| `s52_json_limits_invalid_depth_rejected` | `max_depth: 129` | 契约拒绝 |

### 6.11 §3.3 / §5.4 路径参数（新增，8 项）

| 用例 | 契约 / 请求 | 期望 |
| --- | --- | --- |
| `s54_path_param_int_range_and_raw_value` | `{id}: int min 1` | `42` 放行；`0` → `min`；`abc` / `%34%32` → `int` |
| `s54_path_param_char_and_rest_raw_length` | `{code}: char len 3`、`{*rest}: char max_len 3` | `%31` 放行；`42` → `len`；`a/b` 放行；`a/bcd` → `max_len` |
| `s54_path_param_bool_enum_regex` | `{flag}` / `{mode}` / `{code}` | 合法放行；`bool` / `values` / `regex` |
| `s54_path_param_checked_before_query` | path id 非法 + query 必填缺失 | 先报 `path.id` |
| `s54_path_param_route_rule_coverage` | 一条路由有规则、一条没有 | 无规则路由不校验 |
| `s54_path_param_missing_route_name_rejected` | 规则名不在路由里 | 加载失败 |
| `s54_path_param_required_forms_rejected` | `true` / `false` / `required` / `required_if` | 加载失败 |
| `s54_path_param_invalid_name_and_option_rejected` | 非法名字 / `unknown` 选项 | 加载失败 |

### 6.12 §3.4 / §3.5 分组（新增，13 项）

| 用例 | 契约 / 请求 | 期望 |
| --- | --- | --- |
| `s34_group_prefix_join` | `/api` + `/user`、嵌套 `/v1` + `/users` | 完整路径命中 |
| `s34_group_root_and_route_root_join` | 根前缀 / 路由 `/` | 拼接正确 |
| `s34_group_headers_inherited` | 组 header + 嵌套组 header | 缺失按契约顺序报；都存在放行 |
| `s34_group_headers_can_reference_route_header` | 组 `required_if` 引用路由 header | 合并后可加载并按条件缺报 |
| `s34_group_duplicate_header_rejected` | 组与路由同名 header | 加载失败 |
| `s34_group_unmatched_forward_and_scope` | 组 `forward` | 未命中放行且 `scope`；已命中方法错误仍 405 |
| `s34_group_unmatched_nested_override_and_inherit` | 嵌套 `deny` / 未写继承 `forward` | 404 / forward |
| `s34_group_path_regex_and` | 两级正则 AND | 全匹配放行；失败 404 |
| `s34_group_path_regex_failure_uses_unmatched` | 正则失败 + `forward` | 转后端，`scope=/api` |
| `s34_group_empty_router_unmatched_scope` | 空分组 + forward | 该前缀未命中转发 |
| `s34_group_prefix_boundary` | `/api` 不包含 `/apix` | 404 |
| `s34_group_unimplemented_fields_rejected` | 组 `methods` / `query` / `body` / `path_params` / `filters` | 全拒绝 |
| `s34_group_duplicate_prefix_and_invalid_prefix_rejected` | 重复/非法 prefix | 加载失败 |

### 6.13 §3.4.2 分组剩余继承（新增，9 项）

| 用例 | 契约 / 请求 | 期望 |
| --- | --- | --- |
| `s34_group_methods_intersection_and_allow_order` | 组 `POST,GET` + 路由 `GET` / 省略 | 405 `Allow` 取有效顺序；省略时继承组 |
| `s34_group_methods_empty_intersection_rejected` | 组 `GET` + 路由 `POST` | 加载失败 |
| `s34_group_query_inherited_and_merged` | 组 `trace` + 路由 `page` | 两字段都校验；缺失/超长各自报 |
| `s34_group_query_duplicate_rejected` | 组与路由同名 query | 加载失败 |
| `s34_group_query_unknown_takes_stricter` | 组 ignore + 路由 reject | 未声明参数拒绝 |
| `s34_group_body_inherited_and_unknown_stricter` | 组 `locale` + 路由 `qty`；content_types 交集；unknown 严格化 | 合并校验、form 被交集拒绝、未知字段拒绝 |
| `s34_group_body_content_types_empty_intersection_rejected` | 组 JSON + 路由 form | 加载失败 |
| `s34_group_path_params_inherited` | 组 `org: int min 1`，路由 `/{org}/profile` | 合法放行；`abc` → `path.org` |
| `s34_group_path_params_must_cover_all_descendants` | 组规则缺一个后代路径 | 加载失败 |

### 6.14 §5.9 filters（新增，11 项）

| 用例 | 契约 / 请求 | 期望 |
| --- | --- | --- |
| `s59_sqli_query_filter` | 顶层 `sqli` + query 块，`kw=1' OR '1'='1` | `400`，`field=query.kw`，`filter=sqli`；`hello` 放行 |
| `s59_xss_query_filter` | 顶层 `xss`，`kw=<script>alert(1)</script>` | `400`，`filter=xss` |
| `s59_sqli_precedes_xss_across_values` | 两个字段分别命中 xss / sqli | 报后者的 `sqli`（固定顺序） |
| `s59_filter_runs_after_param_validation` | `page=abc` + 恶意 `kw` | 先报 `int`，不报 filter |
| `s59_header_filter_only_declared` | 声明 `x-note` / 未声明 `x-other` | 声明头命中；未声明头放行 |
| `s59_body_filter_only_strings` | JSON 字符串 / JSON 数字 / form 字符串 | 字符串命中；数字不扫描；form 命中 |
| `s59_path_filter_uses_raw_value` | 原始 `1' OR '1'='1` | `400 path.q`，`filter=sqli` |
| `s59_filters_inherit_with_or_semantics` | 组 sqli + 路由 sqli:false/xss:true | 组 sqli 不能被关闭；sqli 优先 |
| `s59_forward_path_is_not_filtered` | forward 路径 + 恶意 query | 放行转发，不过滤 |
| `s59_no_query_block_is_not_filtered` | 无 query 块 + 恶意 query | 放行（完全不解析） |
| `s59_filters_contract_shape_rejected` | 未知键 / 非布尔 / 显式 false | 未知键和非布尔拒绝；false 合法 |

### 6.14.1 §5.9 filters scope（新增，8 项）

| 用例 | 契约 / 请求 | 期望 |
| --- | --- | --- |
| `s59_scope_defaults_to_all_dimensions` | 不写 `scope`，path/query/header/body 各带载荷 | 四个维度全部命中 |
| `s59_scope_query_only_skips_other_dimensions` | `scope=["query"]` | query 命中；path/header/body 放行 |
| `s59_scope_body_and_header_only` | `scope=["body","header"]` | body/header 命中；query 放行 |
| `s59_scope_merges_across_group_and_route` | 组 `sqli+query`、路由 `xss+body` | 维度并集，query/body 都生效 |
| `s59_scope_cannot_narrow_inherited_filters` | 组全维度 + 路由 `scope=["query"]` | header 仍被扫描（不能收窄） |
| `s59_scope_requires_enabled_filter_in_same_block` | 只有 `scope` / `sqli:false` | `InvalidFilters` |
| `s59_scope_empty_or_duplicate_rejected` | `scope=[]` / 重复维度 | `InvalidFilters` |
| `s59_scope_unknown_area_rejected` | `cookie` / 字符串而非数组 | `ContractError::Json` |

### 6.15 §3.5 路由级 path_regex（新增，3 项）

| 用例 | 契约 / 请求 | 期望 |
| --- | --- | --- |
| `s35_route_path_regex_and_group_and` | 组正则 + 路由正则 | 两者都匹配才放行；失败 404 |
| `s35_route_path_regex_failure_uses_unmatched` | 路由正则失败 + 顶层 forward | 转发，不报 400 |
| `s35_route_path_regex_invalid_rejected` | 非法/超长正则 | 加载失败 |

### 6.16 §3.1.3 路由 priority（新增，4 项）

| 用例 | 契约 / 请求 | 期望 |
| --- | --- | --- |
| `s32_priority_precedes_specificity` | 静态 priority 0 + 参数 priority 10 | 参数路由先匹配并触发路径规则 |
| `s32_priority_same_level_keeps_specificity` | 同 priority 静态/参数 | 静态优先 |
| `s32_priority_path_regex_failure_no_fallback` | 高优先级正则失败 + 低优先级静态 | 404，不 fallback |
| `s32_duplicate_path_across_priority_rejected` | 同路径不同 priority | 加载失败 |

### 6.17 §6 responses（新增，4 项）

| 用例 | 契约 / 请求 | 期望 |
| --- | --- | --- |
| `s6_responses_override_lookup` | 覆盖 route_not_declared / invalid_parameter | 返回配置的 status/body/retry_after |
| `s6_responses_reject_non_4xx_5xx` | 200/3xx/600 | 加载失败 |
| `s6_responses_reject_bad_shape` | 缺 status / 超长 body / 未知键/字段 | 加载失败 |
| `s6_responses_only_top_level` | 路由/分组写 responses | 加载失败 |

合计：**196 个 test**（§2 8 + §2.1 7 + §3.1 17 + §3.2/priority 12 + §4 6 + §5.0 1 + §5.3 55 + §5.1 Query 10 + §5.2 Body 24 + §5.4 Path 8 + §3.4/3.5 Group 22 + §3.5 route regex 3 + §5.9 filters/scope 19 + §6 responses 4，其中 `s4_*` 与前面的矩阵有少量重叠，属于"状态码视角"的显式锁定）。

## 7. 与 `tests/phase1.sh` 的分工

| 层次 | 覆盖内容 | 工具 |
| --- | --- | --- |
| `gateway-core`（本文） | 契约解析、路径/方法判定、`unmatched`、状态码与 reason token、Header / Query / Body 的 required / required_if / 值规则 / unknown / body 解析 | `cargo test`，不需要 nginx |
| `tests/phase1.sh` | nginx 实际写头（`X-Gateway-Name` / `X-Gateway-Reason` / `X-Gateway-Field` / `Allow` / `X-Request-Id`）、诊断头开关的两种形态（默认 off / 测试 on）、Header/Query/Body 400 行为（缺失/重复/类型/长度/范围/枚举/正则/条件必填/未知字段/媒体类型）、多网关"与"语义、503 冷启动、原始 request target | curl + 运行中的 nginx |

`X-Gateway-*` 由适配层写，**不在** core 测试里断言；否则就得把响应头逻辑塞进 core。

**例外：纯适配层切片没有 core 用例。** 例如 `gateway_diagnostics` 只是“写不写响应头”的开关，
`gateway-core` 的 `Outcome` 里根本没有这个概念，无法先写 core 测试。这类切片必须：

1. 在本文登记「无 core 用例」的原因，不能假装测过；
2. 在 `phase1.sh` 用**两组实例**覆盖：诊断打开（沿用现有断言）与诊断关闭（§14，默认 `off`）；
3. 两组都验证状态码、`error_log` 的 `reason/field/rule`、「请求未到后端」一致，只有响应头不同。

**已登记：统一契约管理器 / 共享内存分发（2026-10-27，纯适配层，无新增 core 用例）。**
`gateway-core` 的输入是「已解析的契约 + 请求」，它看不到“契约从哪来、几个 worker 拉”。
因此 `leader worker` 选举、`ngx_shm_zone` seqlock、`config_epoch` 让位、心跳接管都无法在
core 里断言；本切片在 `tests/phase1.sh` 的覆盖方式是：

- 第 6 节：两个网关各自 `contract loaded`、`contract manager: 2 gateway(s) configured`；
- 第 12 节：**独立实例**把契约地址指向 302，断言 `refresh failed: unexpected HTTP status 302`、
  请求 503（fail-closed）、且请求未到后端；同实例声明一个未被 location 引用的网关，断言它不被加载；
- 第 14 节：另一份独立实例（诊断头关闭）里仍然 `契约未就绪 → 503`，证明 fail-closed 与诊断开关无关；
- 第 10 节：文件来源仍走「配置加载时读一次」，改文件不 reload 不生效。

判定本身（`check_with_body` / `response_override` / `body_too_large_override`）在 core 已有用例
（`s5_*`、`s6_responses_override_lookup`），适配层只负责取 body 与把 override 写出去。

> **日志断言的前提**：`phase1.sh` 会先把 `logs/error.log` 裁到最近一次启动之后（原文件备份为
> `logs/error.log.before-phase1`）。`dev.sh restart` 是追加写，不裁的话上一次运行留下的
> `filter=sqli` / `rule=min` 会让断言“假通过”；新写的日志断言应尽量带本次请求的
> `rid=` 或同时校验响应头，避免只依赖历史字符串。`httpbin.sh` / `tls-local.sh` 用独立 prefix，
> 天然没有这个问题。

当前 `phase1.sh` 的 Header 集成断言（§5.5，共 17 项，目标路由 `/api/header-check`）：

- 缺必填 `x-demo-token` → `400`，`X-Gateway-Reason: missing-parameter`、`X-Gateway-Field: header.x-demo-token`，且不到后端；
- 带 `X-Demo-Token`（大写）→ `200`，证明头名大小写不敏感；
- 显式可选 `x-demo-note: { "required": false }` 存在或缺失都 → `200`；
- 携带契约未声明的头 → `200`（未声明头被忽略）；
- 同一必填头 / 可选头两行 → `400`，`X-Gateway-Reason: invalid-parameter`、`X-Gateway-Field: header.<name>`；
- `error.log` 出现 `reason=missing-parameter ... rule=required` 与 `reason=invalid-parameter ... rule=duplicate`。

Header 值规则集成断言（§5.6，共 46 项，目标路由 `/api/header-value-check`）：

- 全部合法（含枚举 `ALPHA` 大小写不敏感）→ `200`，后端收到原值；
- 可选值规则在头缺失时跳过；
- 必填缺失优先于其他字段的值错误；
- `char` 的 `min_len` / `max_len`、`int` 的语法/前导零/`min` / `max`、`enum` 的 `values`、`regex` 的全匹配；
- 值规则字段多行 → `400 invalid-parameter`（`rule=duplicate`）；
- 未声明头忽略；
- 拒绝时 `X-Gateway-*` 正确且请求未到后端；
- `error.log` 出现 `rule=int|min|max|min_len|max_len|values|regex|duplicate` 对应字段。

Header `required_if` 集成断言（§5.7，共 30 项，目标路由 `/api/header-required-if`）：

- 条件不成立（缺 `x-demo-mode` / `mode=open`）且目标头缺失 → `200`；
- 对象形式 `mode=secure` 缺 `x-demo-auth` → `400 missing-parameter`、`field=header.x-demo-auth`、`rule=required_if`；
- 条件成立且目标存在 → `200`；目标存在但值非法 → `400`，条件成立/不成立都一样校验值；
- 字符串形式 `x-demo-note` 出现（含空值）缺 `x-demo-legacy` → `400 missing-parameter`；
- `note` 缺失时 `legacy` 可选；`legacy` 出现但非 int → `400`；
- 两个条件都成立且都缺失 → 按契约顺序先报 `auth`；
- `error.log` 出现 `rule=required_if`（两种形式）以及条件字段的 `rule=min_len` / `rule=int`。

契约拉取重定向集成断言（§12，共 4 项，独立实例）：302 契约地址不被跟随；从未加载成功时 503 `contract-not-ready`；`error_log` 出现 `unexpected HTTP status 302`。

路由级 path_regex / priority 集成断言（§5.16，共 13 项）；responses 集成断言（§5.15，共 17 项：自定义 404/405/400/415 body、状态码、X-Gateway-* 保留）。

Body 集成断言（§5.9，共 39 项）：新增临时文件/异步 16KB body 放行与超过 `client_max_body_size` 的 413 覆盖。

Body 大小上限与 JSON 结构集成断言（§5.17，共 25 项）：`/api/body-check` 的 100KB Content-Length/chunked 413、深度/字符串/数组扫描；`/api/limit` 组级 2K 继承、子级收紧 512、子级 `0` 关闭。

filters 集成断言（§5.14，共 39 项）：新增 `/api/filter/scope`，验证 scope=query 时 query 命中、body/header/path 放行。：query/header/body 的 SQLi/XSS、未声明 query 扫描、声明 header 才扫描、参数错误优先、组级开关不能被 route false 关闭、`rule=filter filter=sqli|xss` 日志。

分组剩余继承集成断言（§5.13，共 18 项）：组级 query/body/methods 合并、路径参数组级校验、`rule=max_len|int` 日志。

分组业务示例集成断言（§5.12，共 48 项）：`/api/v1` 登录 header、订单路径参数、path_regex 失败 404、feedback form/JSON 与 `required_if`、嵌套 admin header、`/api/public/survey` 公开表单（email 正则 / score / consent）、`rule=required|required_if|regex` 日志。

路径参数集成断言（§5.10，共 11 项）：原始值 `int` / `min`，`X-Gateway-Field: path.id`，请求未到后端，`rule=int|min` 日志。

分组集成断言（§5.11，共 24 项）：headers 继承、嵌套 headers、unmatched deny/forward 与 scope、path_regex 失败转 forward、路径参数仍校验、日志 `rule=required` 与 `scope=/api/group`。

Query 集成断言（§5.8，共 37 项，路由 `/api/query-check`、`/api/query-strict`）：

- 合法 / 缺必填 / 范围 / int 语法 / 条件必填 / 值规则（char、bool、regex）/ 百分号解码；
- `unknown: ignore` 放行未声明参数；`unknown: reject` 报 `query.extra`；
- 同名参数重复、非法百分号编码；
- 拒绝时 `X-Gateway-*` 正确且未到后端；日志含 `rule=required|required_if|unknown`。

Body 集成断言（§5.9，共 38 项，路由 `/api/body-check`）：
- JSON / form 合法放行，后端收到原始 body；
- 缺必填、范围、int 语法、浮点、char 超长、未声明字段、重复键；
- 缺 `Content-Type` / `text/plain` / 非 UTF-8 charset / gzip → `unsupported-media-type`；
- 空 body 按空对象校验；`--limit-rate` 触发异步 body 读取仍放行；
- 拒绝时 `X-Gateway-*` 正确且未到后端；日志含 `rule=min|unknown|duplicate`。

诊断头上线形态集成断言（§14 共 33 项 + §9 新增 5 项）：

- 独立实例**不写** `gateway_diagnostics`（默认 `off`）：404 / 405 / 400 / 413 / 503 全部不返回
  `X-Gateway-Name` / `-Reason` / `-Field` / `-Blocked`，但状态码、`Allow`、`error_log` 的
  `reason/field/rule`、「请求未到后端」与测试配置完全一致；
- 每种拒绝都返回 `X-Request-Id`，且能在 `error_log` 用同一个 `rid=` 找到对应判定行；
- 放行路径不出现任何 `X-Gateway-*`；
- `nginx -t`：非法取值、重复声明被拒绝；`on` 产生 WARN；默认 `off` 不产生警告。

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

- `responses` 的 `contract_not_ready` 运行时覆盖、nginx 预检 413 的网关响应头：仍待实现；
- nginx 层行为：响应头、Header 存在性、值规则与 `required_if` 400（缺失/重复/类型/长度/范围/枚举/正则/条件必填）、多网关、503、`merge_slashes`（由 `phase1.sh` 覆盖）；
- 性能、并发、fuzz。
