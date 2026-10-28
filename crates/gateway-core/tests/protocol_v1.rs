//! 协议 v1 验收（纯逻辑，不加载 nginx）。
//!
//! 规则来源：`docs/PROTOCOL.md`；用例清单：`docs/PROTOCOL_TEST_PLAN.md`。
//!
//! 写法约定：
//! - 每个 test 自带契约 JSON，不使用共享夹具/常量；
//! - 直接 `Gateway::parse` + `gw.check`，不封装 helper；
//! - 期望值写死，不在测试里推算；不做表驱动循环；
//! - 分区编号对应协议章节，新增规则时在对应分区补用例。

use gateway_core::{BodyInput, ContractError, Decision, Gateway, RequestCtx};

// ---------------------------------------------------------------------------
// §2 顶层结构
// ---------------------------------------------------------------------------

#[test]
fn s2_protocol_omitted_defaults_to_1() {
    let gw = Gateway::parse(r#"{"routes":[]}"#).expect("minimal contract should load");

    assert_eq!(gw.protocol(), 1);
    assert_eq!(gw.route_count(), 0);
}

#[test]
fn s2_protocol_1_accepted() {
    let gw = Gateway::parse(r#"{"protocol":1,"routes":[]}"#).expect("protocol 1 should load");

    assert_eq!(gw.protocol(), 1);
}

#[test]
fn s2_protocol_2_rejected() {
    let err = Gateway::parse(r#"{"protocol":2,"routes":[]}"#)
        .expect_err("protocol 2 must be rejected by a v1 plugin");

    match err {
        ContractError::UnsupportedProtocol {
            protocol,
            supported,
        } => {
            assert_eq!(protocol, 2);
            assert_eq!(supported, 1);
        }
        other => panic!("expected UnsupportedProtocol, got {other:?}"),
    }
}

#[test]
fn s2_version_exposed() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "version": "2026-10-26T12:00:00Z",
            "routes": [{ "path": "/api/health", "methods": ["GET"] }]
        }"#,
    )
    .expect("contract should load");

    assert_eq!(gw.version(), Some("2026-10-26T12:00:00Z"));
    assert_eq!(gw.route_count(), 1);
}

#[test]
fn s2_meta_is_ignored() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "meta": { "owner": "x", "anything": [1, 2, 3], "nested": { "a": true } },
            "routes": []
        }"#,
    )
    .expect("meta is free-form and must be ignored");

    assert_eq!(gw.route_count(), 0);
}

#[test]
fn s2_unknown_top_level_field_rejected() {
    let err = Gateway::parse(r#"{"protocol":1,"unknown_thing":{},"routes":[]}"#)
        .expect_err("unknown top-level field must reject the whole contract");

    match err {
        ContractError::UnknownField { scope, name } => {
            assert_eq!(scope, "contract");
            assert_eq!(name, "unknown_thing");
        }
        other => panic!("expected UnknownField, got {other:?}"),
    }
}

#[test]
fn s2_routes_omitted_denies_all() {
    let gw = Gateway::parse(r#"{"protocol":1}"#).expect("routes may be omitted");
    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/health",
    });

    assert_eq!(out.decision.status_code(), Some(404));
    assert_eq!(out.allow, None);
    assert!(!out.forwarded);
}

#[test]
fn s2_invalid_json_rejected() {
    let err = Gateway::parse("{not json").expect_err("invalid json must be rejected");

    assert!(matches!(err, ContractError::Json(_)), "got {err:?}");
}

// ---------------------------------------------------------------------------
// §2.1 未声明路由策略
// ---------------------------------------------------------------------------

#[test]
fn s21_unmatched_defaults_to_deny() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/urlabc", "methods": ["GET"] }]
        }"#,
    )
    .expect("contract should load");

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/urlnotexist",
    });

    assert_eq!(out.decision.status_code(), Some(404));
    assert_eq!(out.allow, None);
    assert!(!out.forwarded);
}

#[test]
fn s21_unmatched_deny_explicit() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "unmatched": "deny",
            "routes": [{ "path": "/urlabc", "methods": ["GET"] }]
        }"#,
    )
    .expect("explicit deny should load");

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/urlnotexist",
    });

    assert_eq!(out.decision.status_code(), Some(404));
    assert!(!out.forwarded);
}

#[test]
fn s21_unmatched_forward_allows_unknown() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "unmatched": "forward",
            "routes": [{ "path": "/urlabc", "methods": ["GET"] }]
        }"#,
    )
    .expect("forward should load");

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/urlnotexist",
    });

    assert_eq!(out.decision.status_code(), None);
    assert_eq!(out.allow, None);
    assert!(out.forwarded, "undeclared route must be forwarded");
}

#[test]
fn s21_unmatched_forward_keeps_declared() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "unmatched": "forward",
            "routes": [{ "path": "/urlabc", "methods": ["GET"] }]
        }"#,
    )
    .expect("forward should load");

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/urlabc",
    });

    assert_eq!(out.decision.status_code(), None);
    assert!(
        !out.forwarded,
        "declared route must not be marked as forwarded"
    );
}

#[test]
fn s21_unmatched_forward_keeps_method_check() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "unmatched": "forward",
            "routes": [{ "path": "/urlabc", "methods": ["GET"] }]
        }"#,
    )
    .expect("forward should load");

    let out = gw.check(&RequestCtx {
        method: "POST",
        path: "/urlabc",
    });

    assert_eq!(out.decision.status_code(), Some(405));
    assert_eq!(out.allow, Some("GET"));
    assert!(!out.forwarded, "declared route must not be forwarded");
}

#[test]
fn s21_unmatched_invalid_value_rejected() {
    let err = Gateway::parse(r#"{"protocol":1,"unmatched":"allow","routes":[]}"#)
        .expect_err("unknown unmatched value must be rejected");

    assert!(matches!(err, ContractError::Json(_)), "got {err:?}");
}

#[test]
fn s21_unmatched_is_case_sensitive() {
    let err = Gateway::parse(r#"{"protocol":1,"unmatched":"FORWARD","routes":[]}"#)
        .expect_err("unmatched value must be lowercase");

    assert!(matches!(err, ContractError::Json(_)), "got {err:?}");
}

// ---------------------------------------------------------------------------
// §3.1 路径匹配
// ---------------------------------------------------------------------------

#[test]
fn s31_static_path_matches() {
    let gw =
        Gateway::parse(r#"{"protocol":1,"routes":[{"path":"/api/health","methods":["GET"]}]}"#)
            .expect("contract should load");

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/health",
    });

    assert_eq!(out.decision.status_code(), None);
    assert!(!out.forwarded);
}

#[test]
fn s31_path_is_case_sensitive() {
    let gw =
        Gateway::parse(r#"{"protocol":1,"routes":[{"path":"/api/health","methods":["GET"]}]}"#)
            .expect("contract should load");

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/Health",
    });

    assert_eq!(out.decision.status_code(), Some(404));
}

#[test]
fn s31_trailing_slash_not_matched() {
    let gw =
        Gateway::parse(r#"{"protocol":1,"routes":[{"path":"/api/products","methods":["GET"]}]}"#)
            .expect("contract should load");

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/products/",
    });

    assert_eq!(out.decision.status_code(), Some(404));
}

#[test]
fn s31_contract_trailing_slash_rejected() {
    let err = Gateway::parse(r#"{"routes":[{"path":"/api/x/","methods":["GET"]}]}"#)
        .expect_err("trailing slash in contract must be rejected");

    match err {
        ContractError::InvalidRoute { index, path, .. } => {
            assert_eq!(index, 0);
            assert_eq!(path, "/api/x/");
        }
        other => panic!("expected InvalidRoute, got {other:?}"),
    }
}

#[test]
fn s31_relative_path_rejected() {
    let err = Gateway::parse(r#"{"routes":[{"path":"api/x","methods":["GET"]}]}"#)
        .expect_err("path must start with '/'");

    match err {
        ContractError::InvalidRoute { index, path, .. } => {
            assert_eq!(index, 0);
            assert_eq!(path, "api/x");
        }
        other => panic!("expected InvalidRoute, got {other:?}"),
    }
}

#[test]
fn s31_duplicate_path_rejected() {
    let err = Gateway::parse(
        r#"{"routes":[
            {"path":"/api/x","methods":["GET"]},
            {"path":"/api/x","methods":["POST"]}
        ]}"#,
    )
    .expect_err("duplicate path must be rejected");

    match err {
        ContractError::InvalidRoute { index, path, .. } => {
            assert_eq!(index, 1);
            assert_eq!(path, "/api/x");
        }
        other => panic!("expected InvalidRoute, got {other:?}"),
    }
}

#[test]
fn s31_ambiguous_param_routes_rejected() {
    let err = Gateway::parse(
        r#"{"routes":[
            {"path":"/u/{id}","methods":["GET"]},
            {"path":"/u/{name}","methods":["GET"]}
        ]}"#,
    )
    .expect_err("same-shape param routes must be rejected");

    match err {
        ContractError::InvalidRoute { index, .. } => assert_eq!(index, 1),
        other => panic!("expected InvalidRoute, got {other:?}"),
    }
}

#[test]
fn s31_path_param_name_must_be_identifier() {
    // 参数名语法：`[A-Za-z_][A-Za-z0-9_]*`，最长 64。非法名字必须整份拒绝，
    // 不能静默变成一个“能被匹配、但规则永远引用不到”的裸通配段。
    for pattern in ["/a/{1id}", "/a/{a b}", "/a/{a-b}", "/a/{}"] {
        let json = format!(r#"{{"routes":[{{"path":"{pattern}","methods":["GET"]}}]}}"#);
        let err = Gateway::parse(&json).expect_err("illegal path parameter name must be rejected");

        assert!(
            matches!(err, ContractError::InvalidRoute { .. }),
            "{pattern} -> {err:?}"
        );
    }
}

#[test]
fn s31_duplicate_path_param_name_rejected_without_rules() {
    // 参数名重复与“有没有写 path_params”无关；不能等到规则引用时才检查。
    let err = Gateway::parse(r#"{"routes":[{"path":"/d/{id}/{id}","methods":["GET"]}]}"#)
        .expect_err("duplicate path parameter names must be rejected");

    assert!(
        matches!(err, ContractError::InvalidRoute { .. }),
        "got {err:?}"
    );
}

#[test]
fn s31_escaped_literal_braces_accepted() {
    // matchit 用 `{{` / `}}` 表示字面量花括号；校验参数名时不能把它们当成参数。
    let gw = Gateway::parse(r#"{"routes":[{"path":"/lit/{{x}}","methods":["GET"]}]}"#)
        .expect("literal brace escape should still load");

    assert_eq!(gw.route_count(), 1);
}

#[test]
fn s31_param_route_single_segment() {
    let gw =
        Gateway::parse(r#"{"protocol":1,"routes":[{"path":"/api/user/{id}","methods":["GET"]}]}"#)
            .expect("param route should compile");

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/user/42",
    });
    assert_eq!(out.decision.status_code(), None);

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/user/42/x",
    });
    assert_eq!(out.decision.status_code(), Some(404));
}

#[test]
fn s31_wildcard_matches_rest() {
    let gw =
        Gateway::parse(r#"{"protocol":1,"routes":[{"path":"/static/{*rest}","methods":["GET"]}]}"#)
            .expect("catch-all route should compile");

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/static/a/b/c.txt",
    });

    assert_eq!(out.decision.status_code(), None);
}

#[test]
fn s31_wildcard_must_be_last() {
    let err = Gateway::parse(r#"{"routes":[{"path":"/static/{*rest}/x","methods":["GET"]}]}"#)
        .expect_err("catch-all in the middle must be rejected");

    assert!(
        matches!(err, ContractError::InvalidRoute { .. }),
        "got {err:?}"
    );
}

#[test]
fn s31_static_wins_over_param() {
    let gw = Gateway::parse(
        r#"{"protocol":1,"routes":[
            {"path":"/a/{x}","methods":["POST"]},
            {"path":"/a/b","methods":["GET"]}
        ]}"#,
    )
    .expect("overlapping static/param should compile");

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/a/b",
    });
    assert_eq!(out.decision.status_code(), None);

    let out = gw.check(&RequestCtx {
        method: "POST",
        path: "/a/b",
    });
    assert_eq!(out.decision.status_code(), Some(405));
    assert_eq!(out.allow, Some("GET"));

    let out = gw.check(&RequestCtx {
        method: "POST",
        path: "/a/c",
    });
    assert_eq!(out.decision.status_code(), None);
}

#[test]
fn s31_percent_encoding_not_decoded() {
    let gw =
        Gateway::parse(r#"{"protocol":1,"routes":[{"path":"/api/health","methods":["GET"]}]}"#)
            .expect("contract should load");

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/%68ealth",
    });

    assert_eq!(out.decision.status_code(), Some(404));
}

#[test]
fn s31_double_slash_is_distinct() {
    let gw =
        Gateway::parse(r#"{"protocol":1,"routes":[{"path":"/api/health","methods":["GET"]}]}"#)
            .expect("contract should load");

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "//api/health",
    });

    assert_eq!(out.decision.status_code(), Some(404));
}

#[test]
fn s31_root_path_works() {
    let gw = Gateway::parse(r#"{"protocol":1,"routes":[{"path":"/","methods":["GET"]}]}"#)
        .expect("root path should compile");

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/",
    });

    assert_eq!(out.decision.status_code(), None);
}

// ---------------------------------------------------------------------------
// §3.2 方法匹配
// ---------------------------------------------------------------------------

#[test]
fn s32_method_is_case_insensitive() {
    let gw =
        Gateway::parse(r#"{"protocol":1,"routes":[{"path":"/api/health","methods":["GET"]}]}"#)
            .expect("contract should load");

    let out = gw.check(&RequestCtx {
        method: "get",
        path: "/api/health",
    });

    assert_eq!(out.decision.status_code(), None);
}

#[test]
fn s32_methods_omitted_allows_any() {
    let gw = Gateway::parse(r#"{"protocol":1,"routes":[{"path":"/api/echo"}]}"#)
        .expect("methods may be omitted");

    let out = gw.check(&RequestCtx {
        method: "PATCH",
        path: "/api/echo",
    });

    assert_eq!(out.decision.status_code(), None);
}

#[test]
fn s32_empty_methods_allows_any() {
    let gw = Gateway::parse(r#"{"protocol":1,"routes":[{"path":"/api/echo","methods":[]}]}"#)
        .expect("empty methods should load");

    let out = gw.check(&RequestCtx {
        method: "DELETE",
        path: "/api/echo",
    });

    assert_eq!(out.decision.status_code(), None);
}

#[test]
fn s32_empty_method_rejected() {
    let err = Gateway::parse(r#"{"routes":[{"path":"/api/x","methods":[""]}]}"#)
        .expect_err("empty method string must be rejected");

    match err {
        ContractError::EmptyMethod { index, path } => {
            assert_eq!(index, 0);
            assert_eq!(path, "/api/x");
        }
        other => panic!("expected EmptyMethod, got {other:?}"),
    }
}

#[test]
fn s32_blank_method_rejected() {
    let err = Gateway::parse(r#"{"routes":[{"path":"/api/x","methods":["   "]}]}"#)
        .expect_err("blank method string must be rejected");

    match err {
        ContractError::EmptyMethod { index, path } => {
            assert_eq!(index, 0);
            assert_eq!(path, "/api/x");
        }
        other => panic!("expected EmptyMethod, got {other:?}"),
    }
}

#[test]
fn s32_method_must_be_http_token() {
    // 方法名会原样进 `Allow` 响应头与日志：带 CR/LF 等于响应头/日志注入，
    // 带逗号会让 `["GET, POST"]` 被拆成两个方法。因此必须是 RFC 9110 token。
    for method in ["GET\\r\\nX-Injected: 1", "GE T", "GET, POST", "G\\u0001T"] {
        let json = format!(r#"{{"routes":[{{"path":"/m","methods":["{method}"]}}]}}"#);
        let err = Gateway::parse(&json).expect_err("non-token method must be rejected");

        assert!(
            matches!(err, ContractError::InvalidMethod { .. }),
            "{method:?} -> {err:?}"
        );
    }

    // 合法但少见的方法（自定义 token）仍要能加载，并且大小写不敏感地匹配。
    let gw = Gateway::parse(r#"{"routes":[{"path":"/m","methods":["PURGE"]}]}"#)
        .expect("custom method token should load");
    let out = gw.check(&RequestCtx {
        method: "POST",
        path: "/m",
    });
    assert_eq!(out.allow, Some("PURGE"));
}

#[test]
fn s32_allow_keeps_contract_order() {
    let gw = Gateway::parse(r#"{"protocol":1,"routes":[{"path":"/a","methods":["PUT","GET"]}]}"#)
        .expect("contract should load");

    let out = gw.check(&RequestCtx {
        method: "POST",
        path: "/a",
    });

    assert_eq!(out.decision.status_code(), Some(405));
    assert_eq!(out.allow, Some("PUT, GET"));
}

#[test]
fn s32_dynamic_route_wrong_method() {
    let gw =
        Gateway::parse(r#"{"protocol":1,"routes":[{"path":"/api/user/{id}","methods":["GET"]}]}"#)
            .expect("param route should compile");

    let out = gw.check(&RequestCtx {
        method: "DELETE",
        path: "/api/user/42",
    });

    assert_eq!(out.decision.status_code(), Some(405));
    assert_eq!(out.allow, Some("GET"));
}

// ---------------------------------------------------------------------------
// §4 判定顺序与状态码
// ---------------------------------------------------------------------------

#[test]
fn s4_declared_route_matrix() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/urlabc", "methods": ["GET"] }]
        }"#,
    )
    .expect("contract should load");

    // 声明的路径 + 声明的方法：放行
    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/urlabc",
    });
    assert_eq!(out.decision.status_code(), None);
    assert_eq!(out.allow, None);
    assert!(!out.forwarded);

    // 声明的路径 + 未声明的方法：405 + Allow
    let out = gw.check(&RequestCtx {
        method: "POST",
        path: "/urlabc",
    });
    assert_eq!(out.decision.status_code(), Some(405));
    assert_eq!(out.allow, Some("GET"));
    assert!(!out.forwarded);

    // 配置里没有的路径：默认 deny → 404
    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/urlnotexist",
    });
    assert_eq!(out.decision.status_code(), Some(404));
    assert_eq!(out.allow, None);
    assert!(!out.forwarded);
}

#[test]
fn s4_forward_matrix() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "unmatched": "forward",
            "routes": [{ "path": "/urlabc", "methods": ["GET"] }]
        }"#,
    )
    .expect("contract should load");

    // 配置里没有的路径：交给后端
    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/urlnotexist",
    });
    assert_eq!(out.decision.status_code(), None);
    assert!(out.forwarded);

    // 声明过的路径：正常判定，不算转发
    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/urlabc",
    });
    assert_eq!(out.decision.status_code(), None);
    assert!(!out.forwarded);

    // 声明过的路径、方法不符：仍然 405，不转发
    let out = gw.check(&RequestCtx {
        method: "POST",
        path: "/urlabc",
    });
    assert_eq!(out.decision.status_code(), Some(405));
    assert_eq!(out.allow, Some("GET"));
    assert!(!out.forwarded);
}

#[test]
fn s4_allow_has_no_status() {
    let gw = Gateway::parse(r#"{"protocol":1,"routes":[{"path":"/urlabc","methods":["GET"]}]}"#)
        .expect("contract should load");

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/urlabc",
    });

    assert_eq!(out.decision, Decision::Allow);
    assert_eq!(out.decision.status_code(), None);
}

#[test]
fn s4_route_not_declared_is_404() {
    let gw = Gateway::parse(r#"{"protocol":1,"routes":[]}"#).expect("contract should load");

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/x",
    });

    assert_eq!(out.decision, Decision::RouteNotDeclared);
    assert_eq!(out.decision.status_code(), Some(404));
}

#[test]
fn s4_method_not_allowed_is_405() {
    let gw = Gateway::parse(r#"{"protocol":1,"routes":[{"path":"/urlabc","methods":["GET"]}]}"#)
        .expect("contract should load");

    let out = gw.check(&RequestCtx {
        method: "POST",
        path: "/urlabc",
    });

    assert_eq!(out.decision, Decision::MethodNotAllowed);
    assert_eq!(out.decision.status_code(), Some(405));
}

#[test]
fn s4_reason_tokens() {
    assert_eq!(Decision::Allow.reason(), "allow");
    assert_eq!(Decision::RouteNotDeclared.reason(), "route-not-declared");
    assert_eq!(Decision::MethodNotAllowed.reason(), "method-not-allowed");
    assert_eq!(Decision::MissingParameter.reason(), "missing-parameter");
    assert_eq!(Decision::InvalidParameter.reason(), "invalid-parameter");
}

// ---------------------------------------------------------------------------
// §5.3 Header：当前只实现“字段是否存在”（required / true）
// ---------------------------------------------------------------------------

#[test]
fn s53_true_shorthand_accepts_present_header() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": { "authorization": true } }
            }]
        }"#,
    )
    .expect("header rule with true shorthand should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[("Authorization", Some("Bearer demo"))],
    );

    assert_eq!(out.decision, Decision::Allow);
    assert_eq!(out.decision.status_code(), None);
    assert_eq!(out.field, None);
}

#[test]
fn s53_required_header_missing_is_400() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": { "authorization": { "required": true } } }
            }]
        }"#,
    )
    .expect("explicit required header should load");

    let ctx = RequestCtx {
        method: "GET",
        path: "/api/private",
    };
    let out = gw.check_with_headers(&ctx, &[]);

    assert_eq!(out.decision, Decision::MissingParameter);
    assert_eq!(out.decision.status_code(), Some(400));
    assert_eq!(out.decision.reason(), "missing-parameter");
    assert_eq!(out.field.as_deref(), Some("header.authorization"));
    assert_eq!(out.rule, Some("required"));

    // 不带头信息的入口也必须 fail-closed，不能把“没传 headers”当成“头存在”。
    let out = gw.check(&ctx);
    assert_eq!(out.decision, Decision::MissingParameter);
}

#[test]
fn s53_optional_header_may_be_absent() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": { "x-trace-id": { "required": false } } }
            }]
        }"#,
    )
    .expect("optional header rule should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[],
    );

    assert_eq!(out.decision, Decision::Allow);
    assert_eq!(out.field, None);
}

#[test]
fn s53_empty_header_value_counts_as_present() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": { "authorization": true } }
            }]
        }"#,
    )
    .expect("contract should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[("authorization", Some(""))],
    );

    assert_eq!(out.decision, Decision::Allow);
}

#[test]
fn s53_header_name_is_case_insensitive() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": { "X-Api-Version": true } }
            }]
        }"#,
    )
    .expect("contract should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[("x-api-version", Some("2"))],
    );
    assert_eq!(out.decision, Decision::Allow);

    // 报错时统一小写，便于客户端稳定匹配。
    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[],
    );
    assert_eq!(out.decision, Decision::MissingParameter);
    assert_eq!(out.field.as_deref(), Some("header.x-api-version"));
}

#[test]
fn s53_missing_header_reports_contract_order() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": {
                    "z-last": true,
                    "a-first": true
                } }
            }]
        }"#,
    )
    .expect("contract should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[],
    );

    assert_eq!(out.decision, Decision::MissingParameter);
    assert_eq!(out.field.as_deref(), Some("header.z-last"));
}

#[test]
fn s53_duplicate_request_header_lines_are_invalid() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": { "authorization": true } }
            }]
        }"#,
    )
    .expect("contract should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[("authorization", Some("a")), ("Authorization", Some("b"))],
    );

    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.decision.status_code(), Some(400));
    assert_eq!(out.field.as_deref(), Some("header.authorization"));
    assert_eq!(out.rule, Some("duplicate"));
}

#[test]
fn s53_undeclared_headers_are_ignored() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": { "authorization": true } }
            }]
        }"#,
    )
    .expect("contract should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[
            ("authorization", Some("Bearer demo")),
            ("user-agent", Some("curl")),
            ("cookie", Some("a=b")),
        ],
    );

    assert_eq!(out.decision, Decision::Allow);
}

#[test]
fn s53_required_missing_wins_over_duplicate_check() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": {
                    "authorization": true,
                    "x-trace-id": { "required": false }
                } }
            }]
        }"#,
    )
    .expect("contract should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[("x-trace-id", Some("a")), ("x-trace-id", Some("b"))],
    );

    assert_eq!(out.decision, Decision::MissingParameter);
    assert_eq!(out.field.as_deref(), Some("header.authorization"));
}

#[test]
fn s53_method_error_precedes_header_check() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": { "authorization": true } }
            }]
        }"#,
    )
    .expect("contract should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "POST",
            path: "/api/private",
        },
        &[],
    );

    assert_eq!(out.decision, Decision::MethodNotAllowed);
    assert_eq!(out.decision.status_code(), Some(405));
    assert_eq!(out.allow, Some("GET"));
    assert_eq!(out.field, None);
    assert_eq!(out.rule, None);
}

#[test]
fn s53_header_rules_are_per_route() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [
                {
                    "path": "/api/private",
                    "methods": ["GET"],
                    "headers": { "params": { "authorization": true } }
                },
                { "path": "/api/public", "methods": ["GET"] }
            ]
        }"#,
    )
    .expect("contract should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[],
    );
    assert_eq!(out.decision, Decision::MissingParameter);

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/public",
        },
        &[],
    );
    assert_eq!(out.decision, Decision::Allow);
}

#[test]
fn s53_exact_duplicate_contract_header_key_rejected() {
    let err = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": {
                    "authorization": true,
                    "authorization": { "required": false }
                } }
            }]
        }"#,
    )
    .expect_err("duplicate JSON keys must not silently take the last value");

    assert!(matches!(err, ContractError::Json(_)), "got {err:?}");
}

#[test]
fn s53_duplicate_contract_header_names_rejected() {
    let err = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": {
                    "Authorization": true,
                    "authorization": true
                } }
            }]
        }"#,
    )
    .expect_err("header names are case-insensitive and must not be declared twice");

    assert!(
        matches!(err, ContractError::InvalidRoute { index: 0, .. }),
        "got {err:?}"
    );
}

#[test]
fn s53_invalid_header_name_rejected() {
    let err = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": { "bad header": true } }
            }]
        }"#,
    )
    .expect_err("header name with a space is not a valid field-name");

    assert!(
        matches!(err, ContractError::InvalidRoute { index: 0, .. }),
        "got {err:?}"
    );
}

#[test]
fn s53_false_rule_rejected() {
    let err = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": { "authorization": false } }
            }]
        }"#,
    )
    .expect_err("rule value false is ambiguous and must be rejected");

    assert!(
        matches!(err, ContractError::InvalidRoute { index: 0, .. }),
        "got {err:?}"
    );
}

#[test]
fn s53_empty_rule_rejected() {
    let err = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": { "authorization": {} } }
            }]
        }"#,
    )
    .expect_err("empty rule has no effect and must be rejected");

    assert!(
        matches!(err, ContractError::InvalidRoute { index: 0, .. }),
        "got {err:?}"
    );
}

#[test]
fn s53_unimplemented_rule_field_rejected() {
    let err = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": { "authorization": {
                    "required": true,
                    "filters": { "sqli": true }
                } } }
            }]
        }"#,
    )
    .expect_err("unimplemented header rule fields must not be silently ignored");

    match err {
        ContractError::UnknownField { scope, name } => {
            assert_eq!(scope, "route #0 headers.params.authorization");
            assert_eq!(name, "filters");
        }
        other => panic!("expected UnknownField, got {other:?}"),
    }
}

#[test]
fn s53_headers_block_unknown_option_rejected() {
    let err = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": {
                    "params": { "authorization": true },
                    "unknown": "reject"
                }
            }]
        }"#,
    )
    .expect_err("headers block has no unknown option; reject the whole contract");

    match err {
        ContractError::UnknownField { scope, name } => {
            assert_eq!(scope, "route #0 headers");
            assert_eq!(name, "unknown");
        }
        other => panic!("expected UnknownField, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// §5.3 Header 值规则（type / 范围 / 长度 / 枚举 / regex）
// ---------------------------------------------------------------------------

#[test]
fn s53_char_length_counts_unicode_code_points() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": { "x-name": {
                    "type": "char", "required": true, "len": 2
                } } }
            }]
        }"#,
    )
    .expect("char length rule should load");

    let ctx = RequestCtx {
        method: "GET",
        path: "/api/private",
    };

    // 2 个 Unicode 码点、6 个字节：按码点算应通过。
    let out = gw.check_with_headers(&ctx, &[("x-name", Some("你好"))]);
    assert_eq!(out.decision, Decision::Allow);

    let out = gw.check_with_headers(&ctx, &[("x-name", Some("你好吗"))]);
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("header.x-name"));
    assert_eq!(out.rule, Some("len"));
}

#[test]
fn s53_char_min_and_max_len() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": { "x-name": {
                    "type": "char", "min_len": 2, "max_len": 4
                } } }
            }]
        }"#,
    )
    .expect("char min_len/max_len rule should load");

    let ctx = RequestCtx {
        method: "GET",
        path: "/api/private",
    };

    let out = gw.check_with_headers(&ctx, &[("x-name", Some("a"))]);
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.rule, Some("min_len"));

    let out = gw.check_with_headers(&ctx, &[("x-name", Some("abcde"))]);
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.rule, Some("max_len"));

    let out = gw.check_with_headers(&ctx, &[("x-name", Some(""))]);
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.rule, Some("min_len"));

    let out = gw.check_with_headers(&ctx, &[("x-name", Some("abc"))]);
    assert_eq!(out.decision, Decision::Allow);
}

#[test]
fn s53_char_rejects_nul() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": { "x-name": { "type": "char" } } }
            }]
        }"#,
    )
    .expect("char rule should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[("x-name", Some("ab\0cd"))],
    );

    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("header.x-name"));
    assert_eq!(out.rule, Some("char"));
}

#[test]
fn s53_header_value_ows_is_trimmed() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": { "x-name": {
                    "type": "char", "len": 2
                } } }
            }]
        }"#,
    )
    .expect("char rule should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[("x-name", Some("  ab\t"))],
    );

    assert_eq!(out.decision, Decision::Allow);
}

#[test]
fn s53_int_syntax_is_strict() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": { "x-count": { "type": "int" } } }
            }]
        }"#,
    )
    .expect("int rule should load");

    let ctx = RequestCtx {
        method: "GET",
        path: "/api/private",
    };

    let out = gw.check_with_headers(&ctx, &[("x-count", Some("0"))]);
    assert_eq!(out.decision, Decision::Allow);

    let out = gw.check_with_headers(&ctx, &[("x-count", Some("-12"))]);
    assert_eq!(out.decision, Decision::Allow);

    // OWS 可以去掉，但其他空白/前导零/显式正号/小数都不接受。
    let out = gw.check_with_headers(&ctx, &[("x-count", Some(" 42 "))]);
    assert_eq!(out.decision, Decision::Allow);

    for bad in ["01", "+1", "1.0", "1e3", " 4 2 "] {
        let out = gw.check_with_headers(&ctx, &[("x-count", Some(bad))]);
        assert_eq!(out.decision, Decision::InvalidParameter, "value={bad:?}");
        assert_eq!(out.rule, Some("int"), "value={bad:?}");
    }
}

#[test]
fn s53_int_min_and_max() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": { "x-count": {
                    "type": "int", "min": 1, "max": 99
                } } }
            }]
        }"#,
    )
    .expect("int range rule should load");

    let ctx = RequestCtx {
        method: "GET",
        path: "/api/private",
    };

    let out = gw.check_with_headers(&ctx, &[("x-count", Some("0"))]);
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.rule, Some("min"));

    let out = gw.check_with_headers(&ctx, &[("x-count", Some("100"))]);
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.rule, Some("max"));

    let out = gw.check_with_headers(&ctx, &[("x-count", Some("99"))]);
    assert_eq!(out.decision, Decision::Allow);
}

#[test]
fn s53_bool_forms_are_case_insensitive() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": { "x-flag": { "type": "bool" } } }
            }]
        }"#,
    )
    .expect("bool rule should load");

    let ctx = RequestCtx {
        method: "GET",
        path: "/api/private",
    };

    for good in ["true", "TRUE", "false", "False", "1", "0"] {
        let out = gw.check_with_headers(&ctx, &[("x-flag", Some(good))]);
        assert_eq!(out.decision, Decision::Allow, "value={good:?}");
    }

    let out = gw.check_with_headers(&ctx, &[("x-flag", Some("yes"))]);
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.rule, Some("bool"));
}

#[test]
fn s53_enum_values_and_ignore_case() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": {
                    "x-mode": { "type": "enum", "values": ["alpha", "beta"], "ignore_case": true },
                    "x-strict": { "type": "enum", "values": ["alpha"] }
                } }
            }]
        }"#,
    )
    .expect("enum rules should load");

    let ctx = RequestCtx {
        method: "GET",
        path: "/api/private",
    };

    let out = gw.check_with_headers(&ctx, &[("x-mode", Some("ALPHA"))]);
    assert_eq!(out.decision, Decision::Allow);

    let out = gw.check_with_headers(&ctx, &[("x-mode", Some("gamma"))]);
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("header.x-mode"));
    assert_eq!(out.rule, Some("values"));

    let out = gw.check_with_headers(&ctx, &[("x-strict", Some("ALPHA"))]);
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.rule, Some("values"));
}

#[test]
fn s53_regex_is_full_match() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": { "x-code": {
                    "regex": "[A-Z]{2}[0-9]{2}"
                } } }
            }]
        }"#,
    )
    .expect("regex rule should load");

    let ctx = RequestCtx {
        method: "GET",
        path: "/api/private",
    };

    let out = gw.check_with_headers(&ctx, &[("x-code", Some("AB12"))]);
    assert_eq!(out.decision, Decision::Allow);

    let out = gw.check_with_headers(&ctx, &[("x-code", Some("xAB12"))]);
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("header.x-code"));
    assert_eq!(out.rule, Some("regex"));

    let out = gw.check_with_headers(&ctx, &[("x-code", Some("AB12x"))]);
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.rule, Some("regex"));
}

#[test]
fn s53_optional_value_rule_skipped_when_absent() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": { "x-name": {
                    "type": "char", "min_len": 2
                } } }
            }]
        }"#,
    )
    .expect("optional value rule should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[],
    );

    assert_eq!(out.decision, Decision::Allow);
}

#[test]
fn s53_required_with_value_rule_accepts_valid_value() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": { "x-name": {
                    "type": "char", "required": true, "min_len": 2
                } } }
            }]
        }"#,
    )
    .expect("required + value rule should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[("x-name", Some("ab"))],
    );

    assert_eq!(out.decision, Decision::Allow);
}

#[test]
fn s53_required_missing_wins_over_value_error() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": {
                    "x-must": { "type": "char", "required": true, "min_len": 1 },
                    "x-count": { "type": "int", "min": 1 }
                } }
            }]
        }"#,
    )
    .expect("contract should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[("x-count", Some("0"))],
    );

    assert_eq!(out.decision, Decision::MissingParameter);
    assert_eq!(out.field.as_deref(), Some("header.x-must"));
    assert_eq!(out.rule, Some("required"));
}

#[test]
fn s53_duplicate_wins_over_value_error() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": { "x-count": {
                    "type": "int", "min": 1
                } } }
            }]
        }"#,
    )
    .expect("contract should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[("x-count", Some("0")), ("X-Count", Some("0"))],
    );

    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("header.x-count"));
    assert_eq!(out.rule, Some("duplicate"));
}

#[test]
fn s53_value_errors_report_contract_order() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": {
                    "z-count": { "type": "int", "min": 1 },
                    "a-count": { "type": "int", "min": 1 }
                } }
            }]
        }"#,
    )
    .expect("contract should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[("a-count", Some("0")), ("z-count", Some("0"))],
    );

    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("header.z-count"));
    assert_eq!(out.rule, Some("min"));
}

#[test]
fn s53_non_utf8_value_is_invalid_when_value_rule_exists() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": { "x-name": {
                    "type": "char", "min_len": 1
                } } }
            }]
        }"#,
    )
    .expect("contract should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[("x-name", None)],
    );

    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("header.x-name"));
    assert_eq!(out.rule, Some("char"));
}

#[test]
fn s53_non_utf8_value_is_ok_for_existence_only_rule() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": { "authorization": true } }
            }]
        }"#,
    )
    .expect("contract should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[("authorization", None)],
    );

    assert_eq!(out.decision, Decision::Allow);
}

#[test]
fn s53_value_constraints_must_match_type() {
    let min_on_char = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/x", "methods": ["GET"],
                "headers": { "params": { "x-name": {
                    "type": "char", "min": 1
                } } } }]
        }"#,
    )
    .expect_err("min only applies to int");
    assert!(matches!(
        min_on_char,
        ContractError::InvalidRoute { index: 0, .. }
    ));

    let max_len_on_int = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/x", "methods": ["GET"],
                "headers": { "params": { "x-count": {
                    "type": "int", "max_len": 2
                } } } }]
        }"#,
    )
    .expect_err("max_len only applies to char");
    assert!(matches!(
        max_len_on_int,
        ContractError::InvalidRoute { index: 0, .. }
    ));

    let ignore_case_without_enum = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/x", "methods": ["GET"],
                "headers": { "params": { "x-name": {
                    "type": "char", "ignore_case": true
                } } } }]
        }"#,
    )
    .expect_err("ignore_case only applies to enum");
    assert!(matches!(
        ignore_case_without_enum,
        ContractError::InvalidRoute { index: 0, .. }
    ));
}

#[test]
fn s53_invalid_length_and_range_combinations_rejected() {
    let len_with_min = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/x", "methods": ["GET"],
                "headers": { "params": { "x-name": {
                    "type": "char", "len": 2, "min_len": 1
                } } } }]
        }"#,
    )
    .expect_err("len cannot be combined with min_len/max_len");
    assert!(matches!(
        len_with_min,
        ContractError::InvalidRoute { index: 0, .. }
    ));

    let min_gt_max = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/x", "methods": ["GET"],
                "headers": { "params": { "x-count": {
                    "type": "int", "min": 5, "max": 1
                } } } }]
        }"#,
    )
    .expect_err("min must be <= max");
    assert!(matches!(
        min_gt_max,
        ContractError::InvalidRoute { index: 0, .. }
    ));

    let min_len_gt_max_len = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/x", "methods": ["GET"],
                "headers": { "params": { "x-name": {
                    "type": "char", "min_len": 5, "max_len": 1
                } } } }]
        }"#,
    )
    .expect_err("min_len must be <= max_len");
    assert!(matches!(
        min_len_gt_max_len,
        ContractError::InvalidRoute { index: 0, .. }
    ));
}

#[test]
fn s53_invalid_enum_values_rejected() {
    let empty = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/x", "methods": ["GET"],
                "headers": { "params": { "x-mode": {
                    "type": "enum", "values": []
                } } } }]
        }"#,
    )
    .expect_err("enum values must not be empty");
    assert!(matches!(
        empty,
        ContractError::InvalidRoute { index: 0, .. }
    ));

    let duplicate = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/x", "methods": ["GET"],
                "headers": { "params": { "x-mode": {
                    "type": "enum", "values": ["A", "a"], "ignore_case": true
                } } } }]
        }"#,
    )
    .expect_err("ignore_case makes A/a duplicate enum values");
    assert!(matches!(
        duplicate,
        ContractError::InvalidRoute { index: 0, .. }
    ));
}

#[test]
fn s53_invalid_regex_rules_rejected() {
    let with_enum = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/x", "methods": ["GET"],
                "headers": { "params": { "x-mode": {
                    "type": "enum", "values": ["a"], "regex": "a"
                } } } }]
        }"#,
    )
    .expect_err("regex cannot be combined with enum");
    assert!(matches!(
        with_enum,
        ContractError::InvalidRoute { index: 0, .. }
    ));

    let invalid = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/x", "methods": ["GET"],
                "headers": { "params": { "x-code": {
                    "regex": "("
                } } } }]
        }"#,
    )
    .expect_err("invalid regex must reject the contract");
    assert!(matches!(
        invalid,
        ContractError::InvalidRoute { index: 0, .. }
    ));
}

// ---------------------------------------------------------------------------
// §5.0.1 / §5.3 Header `required_if`（条件必填）
// ---------------------------------------------------------------------------

#[test]
fn s53_required_if_string_presence_triggers() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": {
                    "x-mode": { "required": false },
                    "x-token": { "required_if": "x-mode" }
                } }
            }]
        }"#,
    )
    .expect("required_if string form should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[("x-mode", Some("on"))],
    );

    assert_eq!(out.decision, Decision::MissingParameter);
    assert_eq!(out.field.as_deref(), Some("header.x-token"));
    assert_eq!(out.rule, Some("required_if"));
}

#[test]
fn s53_required_if_string_absent_condition_false() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": {
                    "x-mode": { "required": false },
                    "x-token": { "required_if": "x-mode" }
                } }
            }]
        }"#,
    )
    .expect("required_if string form should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[],
    );

    assert_eq!(out.decision, Decision::Allow);
}

#[test]
fn s53_required_if_string_empty_value_counts_as_present() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": {
                    "x-mode": { "required": false },
                    "x-token": { "required_if": "x-mode" }
                } }
            }]
        }"#,
    )
    .expect("required_if string form should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[("x-mode", Some(""))],
    );

    assert_eq!(out.decision, Decision::MissingParameter);
    assert_eq!(out.rule, Some("required_if"));
}

#[test]
fn s53_required_if_object_equals_matches() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": {
                    "x-mode": { "required": false },
                    "x-token": { "required_if": { "field": "x-mode", "equals": "secure" } }
                } }
            }]
        }"#,
    )
    .expect("required_if object form should load");

    let ctx = RequestCtx {
        method: "GET",
        path: "/api/private",
    };

    let out = gw.check_with_headers(&ctx, &[("x-mode", Some("secure"))]);
    assert_eq!(out.decision, Decision::MissingParameter);
    assert_eq!(out.field.as_deref(), Some("header.x-token"));
    assert_eq!(out.rule, Some("required_if"));

    let out = gw.check_with_headers(&ctx, &[("x-mode", Some("open"))]);
    assert_eq!(out.decision, Decision::Allow);
}

#[test]
fn s53_required_if_object_equals_is_case_sensitive() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": {
                    "x-mode": { "required": false },
                    "x-token": { "required_if": { "field": "x-mode", "equals": "secure" } }
                } }
            }]
        }"#,
    )
    .expect("required_if object form should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[("x-mode", Some("SECURE"))],
    );

    assert_eq!(out.decision, Decision::Allow);
}

#[test]
fn s53_required_if_object_trims_ows() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": {
                    "x-mode": { "required": false },
                    "x-token": { "required_if": { "field": "x-mode", "equals": "secure" } }
                } }
            }]
        }"#,
    )
    .expect("required_if object form should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[("x-mode", Some("  secure\t"))],
    );

    assert_eq!(out.decision, Decision::MissingParameter);
    assert_eq!(out.rule, Some("required_if"));
}

#[test]
fn s53_required_if_non_utf8_counts_as_present_for_string_form() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": {
                    "x-mode": { "required": false },
                    "x-token": { "required_if": "x-mode" }
                } }
            }]
        }"#,
    )
    .expect("required_if string form should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[("x-mode", None)],
    );

    assert_eq!(out.decision, Decision::MissingParameter);
    assert_eq!(out.rule, Some("required_if"));
}

#[test]
fn s53_required_if_non_utf8_never_equals_for_object_form() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": {
                    "x-mode": { "required": false },
                    "x-token": { "required_if": { "field": "x-mode", "equals": "secure" } }
                } }
            }]
        }"#,
    )
    .expect("required_if object form should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[("x-mode", None)],
    );

    assert_eq!(out.decision, Decision::Allow);
}

#[test]
fn s53_required_if_target_present_still_validates_value() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": {
                    "x-mode": { "required": false },
                    "x-token": {
                        "required_if": { "field": "x-mode", "equals": "secure" },
                        "type": "char", "min_len": 2
                    }
                } }
            }]
        }"#,
    )
    .expect("required_if + value rule should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[("x-mode", Some("secure")), ("x-token", Some("a"))],
    );

    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("header.x-token"));
    assert_eq!(out.rule, Some("min_len"));
}

#[test]
fn s53_required_if_condition_false_still_validates_present_value() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": {
                    "x-mode": { "required": false },
                    "x-token": {
                        "required_if": { "field": "x-mode", "equals": "secure" },
                        "type": "char", "min_len": 2
                    }
                } }
            }]
        }"#,
    )
    .expect("required_if + value rule should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[("x-mode", Some("open")), ("x-token", Some("a"))],
    );

    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("header.x-token"));
    assert_eq!(out.rule, Some("min_len"));
}

#[test]
fn s53_required_if_missing_wins_over_duplicate() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": {
                    "x-mode": { "required": false },
                    "x-token": { "required_if": "x-mode" },
                    "x-other": { "required": false }
                } }
            }]
        }"#,
    )
    .expect("contract should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[
            ("x-mode", Some("on")),
            ("x-other", Some("a")),
            ("x-other", Some("b")),
        ],
    );

    assert_eq!(out.decision, Decision::MissingParameter);
    assert_eq!(out.field.as_deref(), Some("header.x-token"));
    assert_eq!(out.rule, Some("required_if"));
}

#[test]
fn s53_unconditional_required_wins_over_required_if() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": {
                    "x-cond": { "required_if": "x-mode" },
                    "x-must": { "required": true },
                    "x-mode": { "required": false }
                } }
            }]
        }"#,
    )
    .expect("contract should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[("x-mode", Some("on"))],
    );

    assert_eq!(out.decision, Decision::MissingParameter);
    assert_eq!(out.field.as_deref(), Some("header.x-must"));
    assert_eq!(out.rule, Some("required"));
}

#[test]
fn s53_required_if_missing_reports_contract_order() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/private",
                "methods": ["GET"],
                "headers": { "params": {
                    "z-cond": { "required_if": "x-mode" },
                    "a-cond": { "required_if": "x-mode" },
                    "x-mode": { "required": false }
                } }
            }]
        }"#,
    )
    .expect("contract should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/private",
        },
        &[("x-mode", Some("on"))],
    );

    assert_eq!(out.decision, Decision::MissingParameter);
    assert_eq!(out.field.as_deref(), Some("header.z-cond"));
    assert_eq!(out.rule, Some("required_if"));
}

#[test]
fn s53_required_if_self_reference_rejected() {
    let err = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/x", "methods": ["GET"],
                "headers": { "params": {
                    "x-mode": { "required_if": "x-mode" }
                } } }]
        }"#,
    )
    .expect_err("required_if must not reference itself");

    assert!(matches!(err, ContractError::InvalidRoute { index: 0, .. }));
}

#[test]
fn s53_required_if_unknown_reference_rejected() {
    let err = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/x", "methods": ["GET"],
                "headers": { "params": {
                    "x-token": { "required_if": "x-missing" }
                } } }]
        }"#,
    )
    .expect_err("required_if must reference a declared header");

    assert!(matches!(err, ContractError::InvalidRoute { index: 0, .. }));
}

#[test]
fn s53_required_if_with_required_true_rejected() {
    let err = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/x", "methods": ["GET"],
                "headers": { "params": {
                    "x-mode": { "required": false },
                    "x-token": { "required": true, "required_if": "x-mode" }
                } } }]
        }"#,
    )
    .expect_err("required and required_if cannot be combined");

    assert!(matches!(err, ContractError::InvalidRoute { index: 0, .. }));
}

#[test]
fn s53_required_if_invalid_forms_rejected() {
    let empty_string = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/x", "methods": ["GET"],
                "headers": { "params": { "x-token": { "required_if": "" } } } }]
        }"#,
    )
    .expect_err("empty required_if string must be rejected");
    assert!(matches!(
        empty_string,
        ContractError::InvalidRoute { index: 0, .. }
    ));

    let empty_field = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/x", "methods": ["GET"],
                "headers": { "params": { "x-token": {
                    "required_if": { "field": "", "equals": "on" }
                } } } }]
        }"#,
    )
    .expect_err("empty required_if.field must be rejected");
    assert!(matches!(
        empty_field,
        ContractError::InvalidRoute { index: 0, .. }
    ));

    let missing_equals = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/x", "methods": ["GET"],
                "headers": { "params": { "x-token": {
                    "required_if": { "field": "x-mode" }
                } } } }]
        }"#,
    )
    .expect_err("object form requires equals");
    assert!(matches!(
        missing_equals,
        ContractError::InvalidRoute { index: 0, .. }
    ));

    let unknown_key = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/x", "methods": ["GET"],
                "headers": { "params": {
                    "x-mode": { "required": false },
                    "x-token": {
                        "required_if": { "field": "x-mode", "equals": "on", "extra": 1 }
                    }
                } } }]
        }"#,
    )
    .expect_err("unknown required_if key must be rejected");
    match unknown_key {
        ContractError::UnknownField { scope, name } => {
            assert_eq!(scope, "route #0 headers.params.x-token.required_if");
            assert_eq!(name, "extra");
        }
        other => panic!("expected UnknownField, got {other:?}"),
    }

    let non_string_equals = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/x", "methods": ["GET"],
                "headers": { "params": {
                    "x-mode": { "required": false },
                    "x-token": {
                        "required_if": { "field": "x-mode", "equals": 1 }
                    }
                } } }]
        }"#,
    )
    .expect_err("equals must be a string");
    assert!(matches!(non_string_equals, ContractError::Json(_)));
}

// ---------------------------------------------------------------------------
// §5.0 参数规则数量上限
// ---------------------------------------------------------------------------

#[test]
fn s50_route_rule_total_limit() {
    // 单条路由（含分组继承）四个维度合计 ≤256；单块 ≤128。
    // 三个块各 85~86 条，单块都在 128 以内，只有“合计”能拦住 257。
    let build = |count: usize| {
        let mut headers = Vec::new();
        let mut query = Vec::new();
        let mut body = Vec::new();
        for i in 0..count {
            match i % 3 {
                0 => headers.push(format!("\"h{i}\":{{ \"type\":\"char\" }}")),
                1 => query.push(format!("\"q{i}\":{{ \"type\":\"char\" }}")),
                _ => body.push(format!("\"b{i}\":{{ \"type\":\"char\" }}")),
            }
        }
        format!(
            r#"{{"protocol":1,"routes":[{{"path":"/a","methods":["GET"],
                "headers":{{"params":{{{}}}}},
                "query":{{"params":{{{}}}}},
                "body":{{"params":{{{}}},"unknown":"ignore"}}}}]}}"#,
            headers.join(","),
            query.join(","),
            body.join(",")
        )
    };

    Gateway::parse(&build(256)).expect("256 rules should load");

    let err = Gateway::parse(&build(257)).expect_err("257 rules must be rejected");
    assert!(
        matches!(err, ContractError::InvalidRoute { .. }),
        "got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// §5.1 Query 参数
// ---------------------------------------------------------------------------

#[test]
fn s51_query_required_and_range() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/q",
                "methods": ["GET"],
                "query": { "params": { "page": { "type": "int", "required": true, "min": 1, "max": 10 } } }
            }]
        }"#,
    )
    .expect("query rule should load");

    let ctx = RequestCtx {
        method: "GET",
        path: "/api/q",
    };

    let out = gw.check_without_body(&ctx, &[], None);
    assert_eq!(out.decision, Decision::MissingParameter);
    assert_eq!(out.field.as_deref(), Some("query.page"));
    assert_eq!(out.rule, Some("required"));

    let out = gw.check_without_body(&ctx, &[], Some("page=0"));
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.rule, Some("min"));

    let out = gw.check_without_body(&ctx, &[], Some("page=abc"));
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.rule, Some("int"));

    let out = gw.check_without_body(&ctx, &[], Some("page=5"));
    assert_eq!(out.decision, Decision::Allow);
}

#[test]
fn s51_query_percent_decoding_and_plus() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/q",
                "methods": ["GET"],
                "query": { "params": { "kw": { "type": "char", "len": 3 } } }
            }]
        }"#,
    )
    .expect("query rule should load");

    let out = gw.check_without_body(
        &RequestCtx {
            method: "GET",
            path: "/api/q",
        },
        &[],
        Some("kw=%E4%BD%A0+%E5%A5%BD"),
    );

    assert_eq!(out.decision, Decision::Allow);
}

#[test]
fn s51_query_duplicate_after_decoding_is_rejected() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/q",
                "methods": ["GET"],
                "query": { "params": { "a": { "required": false } } }
            }]
        }"#,
    )
    .expect("query rule should load");

    let out = gw.check_without_body(
        &RequestCtx {
            method: "GET",
            path: "/api/q",
        },
        &[],
        Some("%61=1&a=2"),
    );

    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("query.a"));
    assert_eq!(out.rule, Some("duplicate"));
}

#[test]
fn s51_query_malformed_encoding_and_utf8() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/q",
                "methods": ["GET"],
                "query": { "params": { "a": { "required": false } } }
            }]
        }"#,
    )
    .expect("query rule should load");

    let ctx = RequestCtx {
        method: "GET",
        path: "/api/q",
    };

    for raw in ["a=%ZZ", "a=%FF", "a=%"] {
        let out = gw.check_without_body(&ctx, &[], Some(raw));
        assert_eq!(out.decision, Decision::InvalidParameter, "raw={raw}");
        assert_eq!(out.rule, Some("encoding"), "raw={raw}");
        assert_eq!(out.field, None, "raw={raw}");
    }
}

#[test]
fn s51_query_empty_key_and_too_many_pairs() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/q",
                "methods": ["GET"],
                "query": { "params": { "a": { "required": false } } }
            }]
        }"#,
    )
    .expect("query rule should load");

    let ctx = RequestCtx {
        method: "GET",
        path: "/api/q",
    };

    let out = gw.check_without_body(&ctx, &[], Some("=x"));
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.rule, Some("empty-key"));

    let raw = (0..257)
        .map(|i| format!("k{i}=v"))
        .collect::<Vec<_>>()
        .join("&");
    let out = gw.check_without_body(&ctx, &[], Some(&raw));
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.rule, Some("limit"));
}

#[test]
fn s51_query_unknown_ignore_and_reject() {
    let ignore = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/q",
                "methods": ["GET"],
                "query": { "params": { "page": { "type": "int" } }, "unknown": "ignore" }
            }]
        }"#,
    )
    .expect("query block should load");

    let ctx = RequestCtx {
        method: "GET",
        path: "/api/q",
    };

    let out = ignore.check_without_body(&ctx, &[], Some("extra=1"));
    assert_eq!(out.decision, Decision::Allow);

    let reject = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/q",
                "methods": ["GET"],
                "query": { "params": { "page": { "type": "int", "min": 1 } }, "unknown": "reject" }
            }]
        }"#,
    )
    .expect("query block should load");

    let out = reject.check_without_body(&ctx, &[], Some("extra=1"));
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("query.extra"));
    assert_eq!(out.rule, Some("unknown"));

    // 声明字段先校验：page 越界优先于未声明字段。
    let out = reject.check_without_body(&ctx, &[], Some("extra=1&page=0"));
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("query.page"));
    assert_eq!(out.rule, Some("min"));
}

#[test]
fn s51_query_required_if_and_empty_value() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/q",
                "methods": ["GET"],
                "query": { "params": {
                    "mode": { "required": false },
                    "token": { "required_if": { "field": "mode", "equals": "secure" } },
                    "legacy": { "required_if": "note", "type": "int" },
                    "note": { "required": false },
                    "page": { "type": "int" }
                } }
            }]
        }"#,
    )
    .expect("query rules should load");

    let ctx = RequestCtx {
        method: "GET",
        path: "/api/q",
    };

    let out = gw.check_without_body(&ctx, &[], Some("mode=secure"));
    assert_eq!(out.decision, Decision::MissingParameter);
    assert_eq!(out.field.as_deref(), Some("query.token"));
    assert_eq!(out.rule, Some("required_if"));

    let out = gw.check_without_body(&ctx, &[], Some("mode=open"));
    assert_eq!(out.decision, Decision::Allow);

    let out = gw.check_without_body(&ctx, &[], Some("note="));
    assert_eq!(out.decision, Decision::MissingParameter);
    assert_eq!(out.field.as_deref(), Some("query.legacy"));
    assert_eq!(out.rule, Some("required_if"));

    let out = gw.check_without_body(&ctx, &[], Some("page="));
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.rule, Some("int"));
}

#[test]
fn s51_query_enum_bool_regex_and_ignore_case() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/q",
                "methods": ["GET"],
                "query": { "params": {
                    "mode": { "type": "enum", "values": ["alpha", "beta"], "ignore_case": true },
                    "flag": { "type": "bool" },
                    "code": { "regex": "[A-Z]{2}[0-9]{2}" }
                } }
            }]
        }"#,
    )
    .expect("query rules should load");

    let ctx = RequestCtx {
        method: "GET",
        path: "/api/q",
    };

    let out = gw.check_without_body(&ctx, &[], Some("mode=ALPHA&flag=TRUE&code=AB12"));
    assert_eq!(out.decision, Decision::Allow);

    let out = gw.check_without_body(&ctx, &[], Some("mode=gamma"));
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.rule, Some("values"));

    let out = gw.check_without_body(&ctx, &[], Some("flag=yes"));
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.rule, Some("bool"));

    let out = gw.check_without_body(&ctx, &[], Some("code=AB12x"));
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.rule, Some("regex"));
}

#[test]
fn s51_query_block_omitted_ignores_malformed_query() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/api/q", "methods": ["GET"] }]
        }"#,
    )
    .expect("contract should load");

    let out = gw.check_without_body(
        &RequestCtx {
            method: "GET",
            path: "/api/q",
        },
        &[],
        Some("a=%ZZ"),
    );

    assert_eq!(out.decision, Decision::Allow);
}

#[test]
fn s51_query_contract_shape_rejected() {
    let unknown_option = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/api/q", "methods": ["GET"],
                "query": { "params": { "a": true }, "unknown": "strict" } }]
        }"#,
    )
    .expect_err("unknown query option must be rejected");
    assert!(matches!(
        unknown_option,
        ContractError::Json(_) | ContractError::UnknownField { .. }
    ));

    let bad_rule = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/api/q", "methods": ["GET"],
                "query": { "params": { "a": false } } }]
        }"#,
    )
    .expect_err("false rule must be rejected");
    assert!(matches!(
        bad_rule,
        ContractError::InvalidRoute { index: 0, .. }
    ));

    let bad_reference = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/api/q", "methods": ["GET"],
                "query": { "params": { "a": { "required_if": "missing" } } } }]
        }"#,
    )
    .expect_err("required_if must reference a declared query field");
    assert!(matches!(
        bad_reference,
        ContractError::InvalidRoute { index: 0, .. }
    ));
}

// ---------------------------------------------------------------------------
// §5.2 Body 参数
// ---------------------------------------------------------------------------

#[test]
fn s52_body_json_required_and_range() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/b",
                "methods": ["POST"],
                "body": { "params": {
                    "sku": { "type": "char", "required": true, "max_len": 8 },
                    "qty": { "type": "int", "required": true, "min": 1, "max": 99 }
                } }
            }]
        }"#,
    )
    .expect("body rule should load");

    let ctx = RequestCtx {
        method: "POST",
        path: "/api/b",
    };

    let body = BodyInput {
        content_type: Some("application/json; charset=utf-8"),
        content_encoding: None,
        bytes: br#"{"sku":"A-1","qty":2}"#,
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.decision, Decision::Allow);

    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{"sku":"A-1"}"#,
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.decision, Decision::MissingParameter);
    assert_eq!(out.field.as_deref(), Some("body.qty"));
    assert_eq!(out.rule, Some("required"));

    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{"sku":"A-1","qty":0}"#,
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("body.qty"));
    assert_eq!(out.rule, Some("min"));
}

#[test]
fn s52_body_json_int_types() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/api/b", "methods": ["POST"],
                "body": { "params": { "qty": { "type": "int" } } } }]
        }"#,
    )
    .expect("body rule should load");

    let ctx = RequestCtx {
        method: "POST",
        path: "/api/b",
    };

    for bytes in [
        br#"{"qty":2}"#.as_slice(),
        br#"{"qty":"2"}"#.as_slice(),
        br#"{"qty":-3}"#.as_slice(),
    ] {
        let body = BodyInput {
            content_type: Some("application/json"),
            content_encoding: None,
            bytes,
        };
        let out = gw.check_with_body(&ctx, &[], None, &body);
        assert_eq!(out.decision, Decision::Allow, "bytes={bytes:?}");
    }

    for bytes in [
        br#"{"qty":2.0}"#.as_slice(),
        br#"{"qty":1e3}"#.as_slice(),
        br#"{"qty":true}"#.as_slice(),
        br#"{"qty":"02"}"#.as_slice(),
    ] {
        let body = BodyInput {
            content_type: Some("application/json"),
            content_encoding: None,
            bytes,
        };
        let out = gw.check_with_body(&ctx, &[], None, &body);
        assert_eq!(out.decision, Decision::InvalidParameter, "bytes={bytes:?}");
        assert_eq!(out.rule, Some("int"), "bytes={bytes:?}");
    }
}

#[test]
fn s52_body_json_char_enum_regex_require_string() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/api/b", "methods": ["POST"],
                "body": { "params": {
                    "sku": { "type": "char" },
                    "mode": { "type": "enum", "values": ["a", "b"] },
                    "code": { "regex": "[A-Z]{2}" }
                } } }]
        }"#,
    )
    .expect("body rules should load");

    let ctx = RequestCtx {
        method: "POST",
        path: "/api/b",
    };

    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{"sku":"A","mode":"a","code":"AB"}"#,
    };
    assert_eq!(
        gw.check_with_body(&ctx, &[], None, &body).decision,
        Decision::Allow
    );

    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{"sku":123}"#,
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.rule, Some("char"));

    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{"sku":{"nested":true}}"#,
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.rule, Some("type"));

    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{"mode":1}"#,
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.rule, Some("values"));

    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{"code":123}"#,
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.rule, Some("regex"));
}

#[test]
fn s52_body_json_bool_forms() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/api/b", "methods": ["POST"],
                "body": { "params": { "flag": { "type": "bool" } } } }]
        }"#,
    )
    .expect("body rule should load");

    let ctx = RequestCtx {
        method: "POST",
        path: "/api/b",
    };

    for bytes in [
        br#"{"flag":true}"#.as_slice(),
        br#"{"flag":false}"#.as_slice(),
        br#"{"flag":1}"#.as_slice(),
        br#"{"flag":0}"#.as_slice(),
        br#"{"flag":"TRUE"}"#.as_slice(),
    ] {
        let body = BodyInput {
            content_type: Some("application/json"),
            content_encoding: None,
            bytes,
        };
        assert_eq!(
            gw.check_with_body(&ctx, &[], None, &body).decision,
            Decision::Allow,
            "bytes={bytes:?}"
        );
    }

    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{"flag":2}"#,
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.rule, Some("bool"));
}

#[test]
fn s52_body_unknown_default_reject_and_ignore() {
    let reject = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/api/b", "methods": ["POST"],
                "body": { "params": { "qty": { "type": "int" } } } }]
        }"#,
    )
    .expect("body block should load");

    let ctx = RequestCtx {
        method: "POST",
        path: "/api/b",
    };

    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{"qty":1,"is_admin":true}"#,
    };
    let out = reject.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("body.is_admin"));
    assert_eq!(out.rule, Some("unknown"));

    let ignore = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/api/b", "methods": ["POST"],
                "body": { "params": { "qty": { "type": "int" } }, "unknown": "ignore" } }]
        }"#,
    )
    .expect("body block should load");

    let out = ignore.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.decision, Decision::Allow);
}

#[test]
fn s52_body_unknown_null_rejected() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/api/b", "methods": ["POST"],
                "body": { "params": { "qty": { "type": "int" } } } }]
        }"#,
    )
    .expect("body block should load");

    let ctx = RequestCtx {
        method: "POST",
        path: "/api/b",
    };
    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{"qty":1,"is_admin":null}"#,
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);

    // `null` 对“声明字段”等于未提供，但对“未声明字段”仍然是出现：
    // 否则 unknown=reject 会被 `{"is_admin":null}` 绕过（Mass Assignment）。
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("body.is_admin"));
    assert_eq!(out.rule, Some("unknown"));

    let ignore = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/api/b", "methods": ["POST"],
                "body": { "params": { "qty": { "type": "int" } }, "unknown": "ignore" } }]
        }"#,
    )
    .expect("body block should load");
    assert_eq!(
        ignore.check_with_body(&ctx, &[], None, &body).decision,
        Decision::Allow
    );

    // 声明字段为 null 仍按“未提供”处理：required 缺失 -> missing-parameter。
    let required = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/api/b", "methods": ["POST"],
                "body": { "params": { "qty": { "type": "int", "required": true } } } }]
        }"#,
    )
    .expect("body block should load");
    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{"qty":null}"#,
    };
    let out = required.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.decision, Decision::MissingParameter);
    assert_eq!(out.field.as_deref(), Some("body.qty"));
}

#[test]
fn s52_body_json_duplicate_key_and_shape() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/api/b", "methods": ["POST"],
                "body": { "params": { "qty": { "type": "int" } }, "unknown": "ignore" } }]
        }"#,
    )
    .expect("body block should load");

    let ctx = RequestCtx {
        method: "POST",
        path: "/api/b",
    };

    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{"qty":1,"qty":2}"#,
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("body.qty"));
    assert_eq!(out.rule, Some("duplicate"));

    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"[{"qty":1}]"#,
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.rule, Some("json"));

    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{"qty":"#,
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.rule, Some("json"));
}

#[test]
fn s52_body_form_parsing_and_duplicates() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/api/b", "methods": ["POST"],
                "body": { "params": {
                    "qty": { "type": "int", "required": true },
                    "kw": { "type": "char", "len": 3 }
                } } }]
        }"#,
    )
    .expect("body block should load");

    let ctx = RequestCtx {
        method: "POST",
        path: "/api/b",
    };

    let body = BodyInput {
        content_type: Some("application/x-www-form-urlencoded"),
        content_encoding: None,
        bytes: b"qty=2&kw=%E4%BD%A0+%E5%A5%BD",
    };
    assert_eq!(
        gw.check_with_body(&ctx, &[], None, &body).decision,
        Decision::Allow
    );

    let body = BodyInput {
        content_type: Some("application/x-www-form-urlencoded"),
        content_encoding: None,
        bytes: b"qty=2&qty=3",
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.field.as_deref(), Some("body.qty"));
    assert_eq!(out.rule, Some("duplicate"));

    let body = BodyInput {
        content_type: Some("application/x-www-form-urlencoded"),
        content_encoding: None,
        bytes: b"qty=%ZZ",
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.rule, Some("form"));
}

#[test]
fn s52_body_media_type_checks() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/api/b", "methods": ["POST"],
                "body": { "params": {} } }]
        }"#,
    )
    .expect("empty body block should load");

    let ctx = RequestCtx {
        method: "POST",
        path: "/api/b",
    };

    let body = BodyInput {
        content_type: None,
        content_encoding: None,
        bytes: br#"{"a":1}"#,
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.decision, Decision::UnsupportedMediaType);
    assert_eq!(out.rule, Some("content-type"));

    let body = BodyInput {
        content_type: Some("text/plain"),
        content_encoding: None,
        bytes: b"a=1",
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.decision, Decision::UnsupportedMediaType);
    assert_eq!(out.rule, Some("content-type"));

    let body = BodyInput {
        content_type: Some("application/json; charset=iso-8859-1"),
        content_encoding: None,
        bytes: br#"{"a":1}"#,
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.decision, Decision::UnsupportedMediaType);
    assert_eq!(out.rule, Some("charset"));

    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: Some("gzip"),
        bytes: br#"{"a":1}"#,
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.decision, Decision::UnsupportedMediaType);
    assert_eq!(out.rule, Some("content-encoding"));

    // 空 body 不要求 Content-Type。
    let body = BodyInput {
        content_type: None,
        content_encoding: None,
        bytes: b"",
    };
    assert_eq!(
        gw.check_with_body(&ctx, &[], None, &body).decision,
        Decision::Allow
    );
}

#[test]
fn s52_body_content_types_restriction() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/api/b", "methods": ["POST"],
                "body": {
                    "params": {},
                    "content_types": ["application/json"]
                } }]
        }"#,
    )
    .expect("body block should load");

    let ctx = RequestCtx {
        method: "POST",
        path: "/api/b",
    };

    let body = BodyInput {
        content_type: Some("application/x-www-form-urlencoded"),
        content_encoding: None,
        bytes: b"a=1",
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.decision, Decision::UnsupportedMediaType);

    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{}"#,
    };
    assert_eq!(
        gw.check_with_body(&ctx, &[], None, &body).decision,
        Decision::Allow
    );
}

#[test]
fn s52_body_empty_null_and_required_if() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/api/b", "methods": ["POST"],
                "body": { "params": {
                    "qty": { "type": "int", "required": true },
                    "mode": { "type": "enum", "values": ["card", "cash"] },
                    "card": { "required_if": { "field": "mode", "equals": "card" } }
                } } }]
        }"#,
    )
    .expect("body block should load");

    let ctx = RequestCtx {
        method: "POST",
        path: "/api/b",
    };

    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{}"#,
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.decision, Decision::MissingParameter);
    assert_eq!(out.field.as_deref(), Some("body.qty"));

    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{"qty":1,"mode":"cash","card":null}"#,
    };
    assert_eq!(
        gw.check_with_body(&ctx, &[], None, &body).decision,
        Decision::Allow
    );

    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{"qty":1,"mode":"card"}"#,
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.decision, Decision::MissingParameter);
    assert_eq!(out.field.as_deref(), Some("body.card"));
    assert_eq!(out.rule, Some("required_if"));

    // required_if.equals 只匹配 JSON 字符串：数字 1 不等于 "1"。
    let number_equals = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/api/b", "methods": ["POST"],
                "body": { "params": {
                    "mode": { "type": "int" },
                    "card": { "required_if": { "field": "mode", "equals": "1" } }
                } } }]
        }"#,
    )
    .expect("body block should load");

    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{"mode":1}"#,
    };
    assert_eq!(
        number_equals
            .check_with_body(&ctx, &[], None, &body)
            .decision,
        Decision::Allow
    );
}

#[test]
fn s52_body_empty_object_unknown_reject() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/api/b", "methods": ["POST"],
                "body": { "params": {} } }]
        }"#,
    )
    .expect("body block should load");

    let ctx = RequestCtx {
        method: "POST",
        path: "/api/b",
    };

    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{"is_admin":true}"#,
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("body.is_admin"));
    assert_eq!(out.rule, Some("unknown"));
}

#[test]
fn s52_body_contract_shape_rejected() {
    let bad_content_type = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/api/b", "methods": ["POST"],
                "body": { "params": {}, "content_types": ["text/plain"] } }]
        }"#,
    )
    .expect_err("unsupported content_types entry must be rejected");
    assert!(matches!(
        bad_content_type,
        ContractError::InvalidRoute { index: 0, .. }
    ));

    let unknown_option = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/api/b", "methods": ["POST"],
                "body": { "params": {}, "unknown": "strict" } }]
        }"#,
    )
    .expect_err("unknown body option must be rejected");
    assert!(matches!(
        unknown_option,
        ContractError::Json(_) | ContractError::UnknownField { .. }
    ));

    let bad_reference = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/api/b", "methods": ["POST"],
                "body": { "params": { "a": { "required_if": "missing" } } } }]
        }"#,
    )
    .expect_err("required_if must reference a declared body field");
    assert!(matches!(
        bad_reference,
        ContractError::InvalidRoute { index: 0, .. }
    ));
}

#[test]
fn s52_body_max_bytes_exposed_per_route() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [
                { "path": "/api/body", "methods": ["POST"],
                  "body": { "max_bytes": 1024,
                            "params": { "note": { "required": false } } } },
                { "path": "/api/plain", "methods": ["POST"] }
            ]
        }"#,
    )
    .expect("body max_bytes should load");

    let ctx = RequestCtx {
        method: "POST",
        path: "/api/body",
    };
    assert!(gw.needs_body(&ctx));
    assert_eq!(gw.body_max_bytes(&ctx), Some(1024));

    let ctx = RequestCtx {
        method: "POST",
        path: "/api/plain",
    };
    assert!(!gw.needs_body(&ctx));
    assert_eq!(gw.body_max_bytes(&ctx), None);
}

#[test]
fn s52_body_max_bytes_group_inherits_stricter_value() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api",
                "body": { "max_bytes": 2048,
                          "params": { "data": { "type": "char", "required": true } } },
                "router": [
                    { "path": "/a", "methods": ["POST"] },
                    { "path": "/b", "methods": ["POST"], "body": { "max_bytes": 1024 } },
                    { "path": "/c", "methods": ["POST"], "body": { "max_bytes": 4096 } }
                ]
            }]
        }"#,
    )
    .expect("group body max_bytes should load");

    for (path, expected) in [("/api/a", 2048), ("/api/b", 1024), ("/api/c", 2048)] {
        let ctx = RequestCtx {
            method: "POST",
            path,
        };
        assert_eq!(gw.body_max_bytes(&ctx), Some(expected));
    }
    // /c 子级更大时仍取更严格者。
}

#[test]
fn s52_body_max_bytes_zero_disables_and_can_reenable() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api",
                "body": { "max_bytes": 2048,
                          "params": { "data": { "type": "char", "required": true } } },
                "router": [
                    { "path": "/off", "methods": ["POST"], "body": { "max_bytes": 0 } },
                    { "prefix": "/nested",
                      "body": { "max_bytes": 0 },
                      "router": [
                          { "path": "/on", "methods": ["POST"], "body": { "max_bytes": 512 } }
                      ] }
                ]
            }]
        }"#,
    )
    .expect("body max_bytes disable should load");

    let off = RequestCtx {
        method: "POST",
        path: "/api/off",
    };
    assert!(gw.needs_body(&off));
    assert_eq!(gw.body_max_bytes(&off), None);

    let on = RequestCtx {
        method: "POST",
        path: "/api/nested/on",
    };
    assert_eq!(gw.body_max_bytes(&on), Some(512));
}

#[test]
fn s52_body_max_bytes_without_body_block_is_none() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{ "path": "/api/plain", "methods": ["POST"] }]
        }"#,
    )
    .expect("contract should load");

    let ctx = RequestCtx {
        method: "POST",
        path: "/api/plain",
    };
    assert_eq!(gw.body_max_bytes(&ctx), None);
}

#[test]
fn s52_body_max_bytes_invalid_values_rejected() {
    for value in ["-1", "\"1024\"", "1.5"] {
        let contract = format!(
            r#"{{ "protocol": 1, "routes": [{{ "path": "/api/b", "methods": ["POST"],
                "body": {{ "max_bytes": {value}, "params": {{}} }} }}] }}"#
        );
        let err = Gateway::parse(&contract).expect_err("invalid max_bytes must be rejected");
        assert!(matches!(err, ContractError::Json(_)));
    }
}

#[test]
fn s52_json_limits_depth() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/json",
                "methods": ["POST"],
                "body": {
                    "max_depth": 3,
                    "params": {},
                    "unknown": "ignore"
                }
            }]
        }"#,
    )
    .expect("json limits should load");

    let ctx = RequestCtx {
        method: "POST",
        path: "/api/json",
    };
    fn body_of(text: &str) -> BodyInput<'_> {
        BodyInput {
            content_type: Some("application/json"),
            content_encoding: None,
            bytes: text.as_bytes(),
        }
    }

    // 顶层 { 算第 1 层；3 层允许，4 层拒绝。
    let ok = r#"{"a":{"b":{}}}"#;
    assert_eq!(
        gw.check_with_body(&ctx, &[], None, &body_of(ok)).decision,
        Decision::Allow
    );

    let too_deep = r#"{"a":{"b":{"c":{}}}}"#;
    let out = gw.check_with_body(&ctx, &[], None, &body_of(too_deep));
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.rule, Some("json-depth"));
}

#[test]
fn s52_json_limits_string_length_and_escapes() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/json",
                "methods": ["POST"],
                "body": {
                    "max_depth": 4,
                    "max_string_length": 8,
                    "params": {},
                    "unknown": "ignore"
                }
            }]
        }"#,
    )
    .expect("json limits should load");

    let ctx = RequestCtx {
        method: "POST",
        path: "/api/json",
    };
    fn body_of(text: &str) -> BodyInput<'_> {
        BodyInput {
            content_type: Some("application/json"),
            content_encoding: None,
            bytes: text.as_bytes(),
        }
    }

    let out = gw.check_with_body(&ctx, &[], None, &body_of(r#"{"a":"123456789"}"#));
    assert_eq!(out.rule, Some("json-string-length"));

    // 恰好 8 字节允许。
    assert_eq!(
        gw.check_with_body(&ctx, &[], None, &body_of(r#"{"a":"12345678"}"#))
            .decision,
        Decision::Allow
    );

    // 字符串里的括号和转义引号不参与深度/结构判断。
    let escaped = r#"{"a":"[{}\"]"}"#;
    assert_eq!(
        gw.check_with_body(&ctx, &[], None, &body_of(escaped))
            .decision,
        Decision::Allow
    );
}

#[test]
fn s52_json_limits_array_entries() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/json",
                "methods": ["POST"],
                "body": {
                    "max_array_entries": 3,
                    "params": {},
                    "unknown": "ignore"
                }
            }]
        }"#,
    )
    .expect("json limits should load");

    let ctx = RequestCtx {
        method: "POST",
        path: "/api/json",
    };
    fn body_of(text: &str) -> BodyInput<'_> {
        BodyInput {
            content_type: Some("application/json"),
            content_encoding: None,
            bytes: text.as_bytes(),
        }
    }

    assert_eq!(
        gw.check_with_body(&ctx, &[], None, &body_of(r#"{"a":[1,2,3]}"#))
            .decision,
        Decision::Allow
    );

    let out = gw.check_with_body(&ctx, &[], None, &body_of(r#"{"a":[1,2,3,4]}"#));
    assert_eq!(out.rule, Some("json-array-entries"));

    // 嵌套数组按各自的层级计数。
    assert_eq!(
        gw.check_with_body(&ctx, &[], None, &body_of(r#"{"a":[[1,2,3],[1,2,3]]}"#))
            .decision,
        Decision::Allow
    );
}

#[test]
fn s52_json_limits_inherit_and_zero_disables() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api",
                "body": {
                    "max_depth": 4,
                    "max_string_length": 16,
                    "max_array_entries": 10,
                    "params": {},
                    "unknown": "ignore"
                },
                "router": [
                    { "path": "/inherit", "methods": ["POST"] },
                    { "path": "/small", "methods": ["POST"],
                      "body": { "max_depth": 2, "unknown": "ignore" } },
                    { "path": "/off", "methods": ["POST"],
                      "body": { "max_depth": 0, "unknown": "ignore" } }
                ]
            }]
        }"#,
    )
    .expect("group json limits should load");

    fn body_of(text: &str) -> BodyInput<'_> {
        BodyInput {
            content_type: Some("application/json"),
            content_encoding: None,
            bytes: text.as_bytes(),
        }
    }
    let ctx = RequestCtx {
        method: "POST",
        path: "/api/inherit",
    };

    assert_eq!(
        gw.check_with_body(&ctx, &[], None, &body_of(r#"{"a":{"b":{"c":{}}}}"#))
            .decision,
        Decision::Allow
    );
    assert_eq!(
        gw.check_with_body(&ctx, &[], None, &body_of(r#"{"a":{"b":{"c":{"d":{}}}}}"#))
            .rule,
        Some("json-depth")
    );
    assert_eq!(
        gw.check_with_body(&ctx, &[], None, &body_of(r#"{"a":"12345678901234567"}"#))
            .rule,
        Some("json-string-length")
    );
    assert_eq!(
        gw.check_with_body(
            &ctx,
            &[],
            None,
            &body_of(r#"{"a":[1,2,3,4,5,6,7,8,9,10,11]}"#)
        )
        .rule,
        Some("json-array-entries")
    );

    // 子级收紧到 2 层。
    let small = RequestCtx {
        method: "POST",
        path: "/api/small",
    };
    assert_eq!(
        gw.check_with_body(&small, &[], None, &body_of(r#"{"a":{"b":{}}}"#))
            .rule,
        Some("json-depth")
    );
    assert_eq!(
        gw.check_with_body(&small, &[], None, &body_of(r#"{"a":{}}"#))
            .decision,
        Decision::Allow
    );

    // 子级 max_depth=0 关闭继承的上限（仍受扫描器 128 层硬上限约束）。
    let off = RequestCtx {
        method: "POST",
        path: "/api/off",
    };
    let mut deep = String::new();
    for _ in 0..10 {
        deep.push_str("{\"a\":");
    }
    deep.push('1');
    for _ in 0..10 {
        deep.push('}');
    }
    assert_eq!(
        gw.check_with_body(&off, &[], None, &body_of(&deep))
            .decision,
        Decision::Allow
    );
}

#[test]
fn s52_json_limits_defaults() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/json",
                "methods": ["POST"],
                "body": { "params": {}, "unknown": "ignore" }
            }]
        }"#,
    )
    .expect("default limits should load");

    let ctx = RequestCtx {
        method: "POST",
        path: "/api/json",
    };
    fn body_of(text: &str) -> BodyInput<'_> {
        BodyInput {
            content_type: Some("application/json"),
            content_encoding: None,
            bytes: text.as_bytes(),
        }
    }

    let nested = |depth: usize| {
        let mut text = String::new();
        for _ in 0..depth {
            text.push_str("{\"a\":");
        }
        text.push('1');
        for _ in 0..depth {
            text.push('}');
        }
        text
    };

    assert_eq!(
        gw.check_with_body(&ctx, &[], None, &body_of(&nested(32)))
            .decision,
        Decision::Allow
    );
    assert_eq!(
        gw.check_with_body(&ctx, &[], None, &body_of(&nested(33)))
            .rule,
        Some("json-depth")
    );

    let long_string = format!(r#"{{"a":"{}"}}"#, "x".repeat(8193));
    assert_eq!(
        gw.check_with_body(&ctx, &[], None, &body_of(&long_string))
            .rule,
        Some("json-string-length")
    );

    let mut array = String::from(r#"{"a":["#);
    for index in 0..501 {
        if index > 0 {
            array.push(',');
        }
        array.push('1');
    }
    array.push_str("]}");
    assert_eq!(
        gw.check_with_body(&ctx, &[], None, &body_of(&array)).rule,
        Some("json-array-entries")
    );
}

#[test]
fn s52_json_limits_invalid_depth_rejected() {
    let err = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/json",
                "methods": ["POST"],
                "body": { "max_depth": 129, "params": {} }
            }]
        }"#,
    )
    .expect_err("max_depth above scanner hard limit must be rejected");
    assert!(matches!(err, ContractError::InvalidRoute { index: 0, .. }));
}

// ---------------------------------------------------------------------------
// §3.3 / §5.4 路径参数
// ---------------------------------------------------------------------------

#[test]
fn s54_path_param_int_range_and_raw_value() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/user/{id}",
                "methods": ["GET"],
                "path_params": { "params": { "id": { "type": "int", "min": 1 } } }
            }]
        }"#,
    )
    .expect("path_params should load");

    let ctx = RequestCtx {
        method: "GET",
        path: "/api/user/42",
    };
    assert_eq!(gw.check(&ctx).decision, Decision::Allow);

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/user/0",
    });
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("path.id"));
    assert_eq!(out.rule, Some("min"));

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/user/abc",
    });
    assert_eq!(out.rule, Some("int"));

    // 路径参数用原始未解码值：%34%32 不是 42。
    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/user/%34%32",
    });
    assert_eq!(out.rule, Some("int"));
}

#[test]
fn s54_path_param_char_and_rest_raw_length() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [
                { "path": "/api/code/{code}", "methods": ["GET"],
                  "path_params": { "params": { "code": { "type": "char", "len": 3 } } } },
                { "path": "/files/{*rest}", "methods": ["GET"],
                  "path_params": { "params": { "rest": { "type": "char", "max_len": 3 } } } }
            ]
        }"#,
    )
    .expect("path_params should load");

    assert_eq!(
        gw.check(&RequestCtx {
            method: "GET",
            path: "/api/code/%31",
        })
        .decision,
        Decision::Allow
    );

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/code/42",
    });
    assert_eq!(out.rule, Some("len"));

    assert_eq!(
        gw.check(&RequestCtx {
            method: "GET",
            path: "/files/a/b",
        })
        .decision,
        Decision::Allow
    );

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/files/a/bcd",
    });
    assert_eq!(out.rule, Some("max_len"));
}

#[test]
fn s54_path_param_bool_enum_regex() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/{flag}/{mode}/{code}",
                "methods": ["GET"],
                "path_params": { "params": {
                    "flag": { "type": "bool" },
                    "mode": { "type": "enum", "values": ["alpha", "beta"], "ignore_case": true },
                    "code": { "regex": "[A-Z]{2}[0-9]{2}" }
                } }
            }]
        }"#,
    )
    .expect("path_params should load");

    assert_eq!(
        gw.check(&RequestCtx {
            method: "GET",
            path: "/api/TRUE/ALPHA/AB12",
        })
        .decision,
        Decision::Allow
    );

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/yes/ALPHA/AB12",
    });
    assert_eq!(out.field.as_deref(), Some("path.flag"));
    assert_eq!(out.rule, Some("bool"));

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/true/gamma/AB12",
    });
    assert_eq!(out.rule, Some("values"));

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/true/alpha/AB12x",
    });
    assert_eq!(out.rule, Some("regex"));
}

#[test]
fn s54_path_param_checked_before_query() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/user/{id}",
                "methods": ["GET"],
                "path_params": { "params": { "id": { "type": "int" } } },
                "query": { "params": { "page": { "required": true } } }
            }]
        }"#,
    )
    .expect("contract should load");

    let out = gw.check_without_body(
        &RequestCtx {
            method: "GET",
            path: "/api/user/abc",
        },
        &[],
        None,
    );

    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("path.id"));
    assert_eq!(out.rule, Some("int"));
}

#[test]
fn s54_path_param_route_rule_coverage() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [
                { "path": "/api/user/{id}", "methods": ["GET"] },
                { "path": "/api/item/{id}", "methods": ["GET"],
                  "path_params": { "params": { "id": { "type": "int" } } } }
            ]
        }"#,
    )
    .expect("route without path_params should load");

    assert_eq!(
        gw.check(&RequestCtx {
            method: "GET",
            path: "/api/user/abc",
        })
        .decision,
        Decision::Allow
    );

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/item/abc",
    });
    assert_eq!(out.rule, Some("int"));
}

#[test]
fn s54_path_param_missing_route_name_rejected() {
    let err = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/user/{id}",
                "methods": ["GET"],
                "path_params": { "params": { "other": { "type": "int" } } }
            }]
        }"#,
    )
    .expect_err("path param rule must exist in route path");

    assert!(matches!(err, ContractError::InvalidRoute { index: 0, .. }));
}

#[test]
fn s54_path_param_required_forms_rejected() {
    for rule in [
        r#"true"#,
        r#"false"#,
        r#"{ "required": false }"#,
        r#"{ "required_if": "other" }"#,
    ] {
        let json = format!(
            r#"{{
                "protocol": 1,
                "routes": [{{
                    "path": "/api/{{id}}",
                    "methods": ["GET"],
                    "path_params": {{ "params": {{ "id": {rule} }} }}
                }}]
            }}"#
        );
        let err = Gateway::parse(&json).expect_err("path params must not declare required forms");
        assert!(
            matches!(err, ContractError::InvalidRoute { index: 0, .. }),
            "rule={rule}, got {err:?}"
        );
    }
}

#[test]
fn s54_path_param_invalid_name_and_option_rejected() {
    let bad_name = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/{bad-name}",
                "methods": ["GET"],
                "path_params": { "params": { "bad-name": { "type": "int" } } }
            }]
        }"#,
    )
    .expect_err("path param names must match [A-Za-z_][A-Za-z0-9_]*");
    assert!(matches!(
        bad_name,
        ContractError::InvalidRoute { index: 0, .. }
    ));

    let bad_option = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/api/{id}",
                "methods": ["GET"],
                "path_params": { "params": { "id": { "type": "int" } }, "unknown": "reject" }
            }]
        }"#,
    )
    .expect_err("path_params has no unknown option");
    assert!(matches!(bad_option, ContractError::UnknownField { .. }));
}

// ---------------------------------------------------------------------------
// §3.4 / §3.5 分组路由（当前只实现 headers / unmatched / path_regex）
// ---------------------------------------------------------------------------

#[test]
fn s34_group_prefix_join() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api",
                "router": [
                    { "path": "/user", "methods": ["GET"] },
                    {
                        "prefix": "/v1",
                        "router": [ { "path": "/users", "methods": ["GET"] } ]
                    }
                ]
            }]
        }"#,
    )
    .expect("group should load");

    assert_eq!(
        gw.check(&RequestCtx {
            method: "GET",
            path: "/api/user",
        })
        .decision,
        Decision::Allow
    );
    assert_eq!(
        gw.check(&RequestCtx {
            method: "GET",
            path: "/api/v1/users",
        })
        .decision,
        Decision::Allow
    );
    assert_eq!(
        gw.check(&RequestCtx {
            method: "GET",
            path: "/api/v1",
        })
        .decision,
        Decision::RouteNotDeclared
    );
}

#[test]
fn s34_group_root_and_route_root_join() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [
                { "prefix": "/", "router": [ { "path": "/health", "methods": ["GET"] } ] },
                { "prefix": "/api", "router": [ { "path": "/", "methods": ["GET"] } ] }
            ]
        }"#,
    )
    .expect("groups should load");

    assert_eq!(
        gw.check(&RequestCtx {
            method: "GET",
            path: "/health",
        })
        .decision,
        Decision::Allow
    );
    assert_eq!(
        gw.check(&RequestCtx {
            method: "GET",
            path: "/api",
        })
        .decision,
        Decision::Allow
    );
}

#[test]
fn s34_group_headers_inherited() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api",
                "headers": { "params": { "authorization": true } },
                "router": [
                    { "path": "/user", "methods": ["GET"] },
                    {
                        "prefix": "/admin",
                        "headers": { "params": { "x-admin": true } },
                        "router": [ { "path": "/audit", "methods": ["GET"] } ]
                    }
                ]
            }]
        }"#,
    )
    .expect("group headers should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/user",
        },
        &[],
    );
    assert_eq!(out.decision, Decision::MissingParameter);
    assert_eq!(out.field.as_deref(), Some("header.authorization"));

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/admin/audit",
        },
        &[("authorization", Some("ok"))],
    );
    assert_eq!(out.decision, Decision::MissingParameter);
    assert_eq!(out.field.as_deref(), Some("header.x-admin"));

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/admin/audit",
        },
        &[("authorization", Some("ok")), ("x-admin", Some("1"))],
    );
    assert_eq!(out.decision, Decision::Allow);
}

#[test]
fn s34_group_headers_can_reference_route_header() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api",
                "headers": { "params": {
                    "x-auth": { "required_if": { "field": "x-mode", "equals": "secure" } }
                } },
                "router": [{
                    "path": "/user",
                    "methods": ["GET"],
                    "headers": { "params": { "x-mode": { "required": false } } }
                }]
            }]
        }"#,
    )
    .expect("merged required_if should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/user",
        },
        &[("x-mode", Some("secure"))],
    );
    assert_eq!(out.decision, Decision::MissingParameter);
    assert_eq!(out.field.as_deref(), Some("header.x-auth"));
    assert_eq!(out.rule, Some("required_if"));
}

#[test]
fn s34_group_duplicate_header_rejected() {
    let err = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api",
                "headers": { "params": { "authorization": true } },
                "router": [{
                    "path": "/user",
                    "methods": ["GET"],
                    "headers": { "params": { "authorization": true } }
                }]
            }]
        }"#,
    )
    .expect_err("same header must not be defined in group and route");

    assert!(matches!(err, ContractError::InvalidRoute { index: 0, .. }));
}

#[test]
fn s34_group_unmatched_forward_and_scope() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api",
                "unmatched": "forward",
                "router": [ { "path": "/health", "methods": ["GET"] } ]
            }]
        }"#,
    )
    .expect("group should load");

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/unknown",
    });
    assert_eq!(out.decision, Decision::Allow);
    assert!(out.forwarded);
    assert_eq!(out.scope, Some("/api"));

    // 已命中路由的方法错误不受分组 unmatched 影响。
    let out = gw.check(&RequestCtx {
        method: "POST",
        path: "/api/health",
    });
    assert_eq!(out.decision, Decision::MethodNotAllowed);
}

#[test]
fn s34_group_unmatched_nested_override_and_inherit() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api",
                "unmatched": "forward",
                "router": [{
                    "prefix": "/admin",
                    "unmatched": "deny",
                    "router": [ { "path": "/audit", "methods": ["GET"] } ]
                }, {
                    "prefix": "/v1",
                    "router": [ { "path": "/users", "methods": ["GET"] } ]
                }]
            }]
        }"#,
    )
    .expect("group should load");

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/admin/unknown",
    });
    assert_eq!(out.decision, Decision::RouteNotDeclared);

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/v1/unknown",
    });
    assert_eq!(out.decision, Decision::Allow);
    assert_eq!(out.scope, Some("/api"));
}

#[test]
fn s34_group_path_regex_and() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api",
                "path_regex": "^/api/.*$",
                "router": [{
                    "prefix": "/v1",
                    "path_regex": "^/api/v1/(user|order)s/[0-9]+$",
                    "router": [ { "path": "/users/{id}", "methods": ["GET"] } ]
                }]
            }]
        }"#,
    )
    .expect("group path_regex should load");

    assert_eq!(
        gw.check(&RequestCtx {
            method: "GET",
            path: "/api/v1/users/42",
        })
        .decision,
        Decision::Allow
    );

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/v1/users/abc",
    });
    assert_eq!(out.decision, Decision::RouteNotDeclared);
}

#[test]
fn s34_group_path_regex_failure_uses_unmatched() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api",
                "unmatched": "forward",
                "path_regex": "^/api/(user|order)s/[0-9]+$",
                "router": [ { "path": "/users/{id}", "methods": ["GET"] } ]
            }]
        }"#,
    )
    .expect("group path_regex should load");

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/users/42",
    });
    assert_eq!(out.decision, Decision::Allow);
    assert!(!out.forwarded);

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/users/abc",
    });
    assert_eq!(out.decision, Decision::Allow);
    assert!(out.forwarded);
    assert_eq!(out.scope, Some("/api"));
}

#[test]
fn s34_group_empty_router_unmatched_scope() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/legacy",
                "unmatched": "forward",
                "router": []
            }]
        }"#,
    )
    .expect("empty group should load");

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/legacy/anything",
    });
    assert_eq!(out.decision, Decision::Allow);
    assert!(out.forwarded);
    assert_eq!(out.scope, Some("/legacy"));
}

#[test]
fn s34_group_prefix_boundary() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api",
                "unmatched": "forward",
                "router": []
            }]
        }"#,
    )
    .expect("group should load");

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/apix/unknown",
    });
    assert_eq!(out.decision, Decision::RouteNotDeclared);
}

#[test]
fn s34_group_unimplemented_fields_rejected() {
    for field in [r#""responses": {}"#, r#""priority": 1"#] {
        let json = format!(
            r#"{{
                "protocol": 1,
                "routes": [{{
                    "prefix": "/api",
                    {field},
                    "router": [ {{ "path": "/x", "methods": ["GET"] }} ]
                }}]
            }}"#
        );
        let err = Gateway::parse(&json).expect_err("unimplemented group fields must be rejected");
        assert!(
            matches!(err, ContractError::UnknownField { .. }),
            "field={field}, got {err:?}"
        );
    }
}

#[test]
fn s34_group_duplicate_prefix_and_invalid_prefix_rejected() {
    let duplicate = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api",
                "router": [
                    { "prefix": "/v1", "router": [] },
                    { "prefix": "/v1", "router": [] }
                ]
            }]
        }"#,
    )
    .expect_err("duplicate sibling prefixes must be rejected");
    assert!(matches!(
        duplicate,
        ContractError::InvalidRoute { index: 0, .. }
    ));

    for prefix in ["api", "/api/", "/api?x=1", "/api/{id}"] {
        let json = format!(
            r#"{{ "protocol": 1, "routes": [ {{ "prefix": "{prefix}", "router": [] }} ] }}"#
        );
        let err = Gateway::parse(&json).expect_err("invalid prefix must be rejected");
        assert!(
            matches!(err, ContractError::InvalidRoute { index: 0, .. }),
            "prefix={prefix}, got {err:?}"
        );
    }
}

#[test]
fn s34_group_methods_intersection_and_allow_order() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api",
                "methods": ["POST", "GET"],
                "router": [
                    { "path": "/both", "methods": ["GET", "POST"] },
                    { "path": "/get", "methods": ["GET"] },
                    { "path": "/any" }
                ]
            }]
        }"#,
    )
    .expect("group methods should load");

    let out = gw.check(&RequestCtx {
        method: "PUT",
        path: "/api/both",
    });
    assert_eq!(out.decision, Decision::MethodNotAllowed);
    assert_eq!(out.allow, Some("GET, POST"));

    let out = gw.check(&RequestCtx {
        method: "POST",
        path: "/api/get",
    });
    assert_eq!(out.decision, Decision::MethodNotAllowed);
    assert_eq!(out.allow, Some("GET"));

    let out = gw.check(&RequestCtx {
        method: "PUT",
        path: "/api/any",
    });
    assert_eq!(out.decision, Decision::MethodNotAllowed);
    assert_eq!(out.allow, Some("POST, GET"));
}

#[test]
fn s34_group_methods_empty_intersection_rejected() {
    let err = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api",
                "methods": ["GET"],
                "router": [ { "path": "/x", "methods": ["POST"] } ]
            }]
        }"#,
    )
    .expect_err("empty methods intersection must reject the contract");

    assert!(matches!(err, ContractError::InvalidRoute { index: 0, .. }));
}

#[test]
fn s34_group_query_inherited_and_merged() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api",
                "query": { "params": { "trace": { "type": "char", "max_len": 4 } } },
                "router": [{
                    "path": "/items",
                    "methods": ["GET"],
                    "query": { "params": { "page": { "type": "int", "required": true, "min": 1 } } }
                }]
            }]
        }"#,
    )
    .expect("group query should load");

    let ctx = RequestCtx {
        method: "GET",
        path: "/api/items",
    };

    let out = gw.check_without_body(&ctx, &[], Some("page=1&trace=abcd"));
    assert_eq!(out.decision, Decision::Allow);

    let out = gw.check_without_body(&ctx, &[], Some("page=1&trace=abcde"));
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("query.trace"));
    assert_eq!(out.rule, Some("max_len"));

    let out = gw.check_without_body(&ctx, &[], Some("trace=abcd"));
    assert_eq!(out.decision, Decision::MissingParameter);
    assert_eq!(out.field.as_deref(), Some("query.page"));
}

#[test]
fn s34_group_query_duplicate_rejected() {
    let err = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api",
                "query": { "params": { "trace": { "type": "char" } } },
                "router": [{
                    "path": "/items",
                    "methods": ["GET"],
                    "query": { "params": { "trace": { "type": "char" } } }
                }]
            }]
        }"#,
    )
    .expect_err("same query rule must not be defined in group and route");

    assert!(matches!(err, ContractError::InvalidRoute { index: 0, .. }));
}

#[test]
fn s34_group_query_unknown_takes_stricter() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api",
                "query": { "params": {}, "unknown": "ignore" },
                "router": [{
                    "path": "/items",
                    "methods": ["GET"],
                    "query": { "params": {}, "unknown": "reject" }
                }]
            }]
        }"#,
    )
    .expect("query blocks should load");

    let out = gw.check_without_body(
        &RequestCtx {
            method: "GET",
            path: "/api/items",
        },
        &[],
        Some("extra=1"),
    );

    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("query.extra"));
    assert_eq!(out.rule, Some("unknown"));
}

#[test]
fn s34_group_body_inherited_and_unknown_stricter() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api",
                "body": {
                    "params": { "locale": { "type": "char", "max_len": 4 } },
                    "unknown": "ignore",
                    "content_types": ["application/json", "application/x-www-form-urlencoded"]
                },
                "router": [{
                    "path": "/orders",
                    "methods": ["POST"],
                    "body": {
                        "params": { "qty": { "type": "int", "required": true, "min": 1 } },
                        "unknown": "reject",
                        "content_types": ["application/json"]
                    }
                }]
            }]
        }"#,
    )
    .expect("group body should load");

    let ctx = RequestCtx {
        method: "POST",
        path: "/api/orders",
    };

    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{"qty":1,"locale":"zh"}"#,
    };
    assert_eq!(
        gw.check_with_body(&ctx, &[], None, &body).decision,
        Decision::Allow
    );

    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{"qty":1,"locale":"zh-cn"}"#,
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.field.as_deref(), Some("body.locale"));
    assert_eq!(out.rule, Some("max_len"));

    // form 被组级 content_types 允许，但路由只允许 JSON → 交集只有 JSON。
    let body = BodyInput {
        content_type: Some("application/x-www-form-urlencoded"),
        content_encoding: None,
        bytes: b"qty=1",
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.decision, Decision::UnsupportedMediaType);

    // 组 unknown=ignore、路由 unknown=reject → 更严格的 reject。
    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{"qty":1,"is_admin":true}"#,
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.field.as_deref(), Some("body.is_admin"));
    assert_eq!(out.rule, Some("unknown"));
}

#[test]
fn s34_group_body_content_types_empty_intersection_rejected() {
    let err = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api",
                "body": { "params": {}, "content_types": ["application/json"] },
                "router": [{
                    "path": "/items",
                    "methods": ["POST"],
                    "body": { "params": {}, "content_types": ["application/x-www-form-urlencoded"] }
                }]
            }]
        }"#,
    )
    .expect_err("empty content_types intersection must reject the contract");

    assert!(matches!(err, ContractError::InvalidRoute { index: 0, .. }));
}

#[test]
fn s34_group_path_params_inherited() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api/org",
                "path_params": { "params": { "org": { "type": "int", "min": 1 } } },
                "router": [ { "path": "/{org}/profile", "methods": ["GET"] } ]
            }]
        }"#,
    )
    .expect("group path_params should load");

    assert_eq!(
        gw.check(&RequestCtx {
            method: "GET",
            path: "/api/org/42/profile",
        })
        .decision,
        Decision::Allow
    );

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/org/abc/profile",
    });
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("path.org"));
    assert_eq!(out.rule, Some("int"));
}

#[test]
fn s34_group_path_params_must_cover_all_descendants() {
    let err = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api/org",
                "path_params": { "params": { "org": { "type": "int" } } },
                "router": [
                    { "path": "/{org}/profile", "methods": ["GET"] },
                    { "path": "/health", "methods": ["GET"] }
                ]
            }]
        }"#,
    )
    .expect_err("group path_params must exist in every descendant path");

    assert!(matches!(err, ContractError::InvalidRoute { index: 1, .. }));
}

// ---------------------------------------------------------------------------
// §5.9 filters（SQLi / XSS，第三方 libinjectionrs）
// ---------------------------------------------------------------------------

#[test]
fn s59_sqli_query_filter() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "filters": { "sqli": true },
            "routes": [{
                "path": "/api/search",
                "methods": ["GET"],
                "query": { "params": { "kw": { "type": "char", "max_len": 64 } } }
            }]
        }"#,
    )
    .expect("filters should load");

    let out = gw.check_without_body(
        &RequestCtx {
            method: "GET",
            path: "/api/search",
        },
        &[],
        Some("kw=1%27+OR+%271%27%3D%271"),
    );

    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("query.kw"));
    assert_eq!(out.rule, Some("filter"));
    assert_eq!(out.filter, Some("sqli"));

    assert_eq!(
        gw.check_without_body(
            &RequestCtx {
                method: "GET",
                path: "/api/search",
            },
            &[],
            Some("kw=hello")
        )
        .decision,
        Decision::Allow
    );
}

#[test]
fn s59_xss_query_filter() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "filters": { "xss": true },
            "routes": [{
                "path": "/api/search",
                "methods": ["GET"],
                "query": { "params": { "kw": { "type": "char", "max_len": 128 } } }
            }]
        }"#,
    )
    .expect("filters should load");

    let out = gw.check_without_body(
        &RequestCtx {
            method: "GET",
            path: "/api/search",
        },
        &[],
        Some("kw=%3Cscript%3Ealert(1)%3C%2Fscript%3E"),
    );

    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("query.kw"));
    assert_eq!(out.filter, Some("xss"));
}

#[test]
fn s59_sqli_precedes_xss_across_values() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "filters": { "sqli": true, "xss": true },
            "routes": [{
                "path": "/api/search",
                "methods": ["GET"],
                "query": { "params": {
                    "a": { "required": false },
                    "b": { "required": false }
                } }
            }]
        }"#,
    )
    .expect("filters should load");

    let out = gw.check_without_body(
        &RequestCtx {
            method: "GET",
            path: "/api/search",
        },
        &[],
        Some("a=%3Cscript%3Ealert(1)%3C%2Fscript%3E&b=union+select+1"),
    );

    assert_eq!(out.field.as_deref(), Some("query.b"));
    assert_eq!(out.filter, Some("sqli"));
}

#[test]
fn s59_filter_runs_after_param_validation() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "filters": { "xss": true },
            "routes": [{
                "path": "/api/search",
                "methods": ["GET"],
                "query": { "params": {
                    "page": { "type": "int", "required": true },
                    "kw": { "type": "char", "max_len": 128 }
                } }
            }]
        }"#,
    )
    .expect("filters should load");

    let out = gw.check_without_body(
        &RequestCtx {
            method: "GET",
            path: "/api/search",
        },
        &[],
        Some("page=abc&kw=%3Cscript%3E"),
    );

    assert_eq!(out.rule, Some("int"));
    assert_eq!(out.filter, None);
}

#[test]
fn s59_header_filter_only_declared() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "filters": { "xss": true },
            "routes": [{
                "path": "/api/headers",
                "methods": ["GET"],
                "headers": { "params": { "x-note": { "required": false } } }
            }]
        }"#,
    )
    .expect("filters should load");

    let out = gw.check_with_headers(
        &RequestCtx {
            method: "GET",
            path: "/api/headers",
        },
        &[("x-note", Some("<script>alert(1)</script>"))],
    );
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("header.x-note"));
    assert_eq!(out.filter, Some("xss"));

    // 未声明的 header 不扫描。
    assert_eq!(
        gw.check_with_headers(
            &RequestCtx {
                method: "GET",
                path: "/api/headers",
            },
            &[("x-other", Some("<script>alert(1)</script>"))]
        )
        .decision,
        Decision::Allow
    );
}

#[test]
fn s59_body_filter_only_strings() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "filters": { "xss": true },
            "routes": [{
                "path": "/api/body",
                "methods": ["POST"],
                "body": { "params": {
                    "qty": { "type": "int" },
                    "note": { "required": false }
                }, "unknown": "ignore" }
            }]
        }"#,
    )
    .expect("filters should load");

    let ctx = RequestCtx {
        method: "POST",
        path: "/api/body",
    };

    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{"qty":1,"note":"<script>alert(1)</script>"}"#,
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.field.as_deref(), Some("body.note"));
    assert_eq!(out.filter, Some("xss"));

    // 数字不扫描。
    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{"qty":1,"note":123}"#,
    };
    assert_eq!(
        gw.check_with_body(&ctx, &[], None, &body).decision,
        Decision::Allow
    );

    // form 字符串扫描。
    let body = BodyInput {
        content_type: Some("application/x-www-form-urlencoded"),
        content_encoding: None,
        bytes: b"qty=1&note=%3Cscript%3Ealert(1)%3C%2Fscript%3E",
    };
    let out = gw.check_with_body(&ctx, &[], None, &body);
    assert_eq!(out.field.as_deref(), Some("body.note"));
    assert_eq!(out.filter, Some("xss"));
}

#[test]
fn s59_path_filter_uses_raw_value() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "filters": { "sqli": true },
            "routes": [{
                "path": "/api/user/{q}",
                "methods": ["GET"],
                "path_params": { "params": { "q": { "type": "char", "max_len": 64 } } }
            }]
        }"#,
    )
    .expect("filters should load");

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/user/1' OR '1'='1",
    });

    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("path.q"));
    assert_eq!(out.filter, Some("sqli"));
}

#[test]
fn s59_filters_inherit_with_or_semantics() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api",
                "filters": { "sqli": true },
                "query": { "params": {
                    "a": { "required": false },
                    "b": { "required": false }
                } },
                "router": [{
                    "path": "/search",
                    "methods": ["GET"],
                    "filters": { "sqli": false, "xss": true }
                }]
            }]
        }"#,
    )
    .expect("group filters should load");

    let out = gw.check_without_body(
        &RequestCtx {
            method: "GET",
            path: "/api/search",
        },
        &[],
        Some("a=%3Cscript%3Ealert(1)%3C%2Fscript%3E&b=union+select+1"),
    );

    // 组 sqli=true 不能被路由 false 关闭；sqli 优先于 xss。
    assert_eq!(out.field.as_deref(), Some("query.b"));
    assert_eq!(out.filter, Some("sqli"));
}

#[test]
fn s59_forward_path_is_not_filtered() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "unmatched": "forward",
            "filters": { "sqli": true },
            "routes": [{ "path": "/api/health", "methods": ["GET"] }]
        }"#,
    )
    .expect("filters should load");

    let out = gw.check_without_body(
        &RequestCtx {
            method: "GET",
            path: "/api/unknown",
        },
        &[],
        Some("q=union+select+1"),
    );

    assert_eq!(out.decision, Decision::Allow);
    assert!(out.forwarded);
}

#[test]
fn s59_no_query_block_is_not_filtered() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "filters": { "sqli": true },
            "routes": [{ "path": "/api/health", "methods": ["GET"] }]
        }"#,
    )
    .expect("filters should load");

    let out = gw.check_without_body(
        &RequestCtx {
            method: "GET",
            path: "/api/health",
        },
        &[],
        Some("q=union+select+1"),
    );

    assert_eq!(out.decision, Decision::Allow);
}

#[test]
fn s59_filters_contract_shape_rejected() {
    let unknown = Gateway::parse(
        r#"{
            "protocol": 1,
            "filters": { "sqli": true, "csrf": true },
            "routes": []
        }"#,
    )
    .expect_err("unknown filter key must be rejected");
    assert!(matches!(unknown, ContractError::UnknownField { .. }));

    let wrong_type = Gateway::parse(
        r#"{
            "protocol": 1,
            "filters": { "sqli": "true" },
            "routes": []
        }"#,
    )
    .expect_err("filter values must be booleans");
    assert!(matches!(wrong_type, ContractError::Json(_)));

    let false_ok = Gateway::parse(
        r#"{
            "protocol": 1,
            "filters": { "sqli": false, "xss": false },
            "routes": []
        }"#,
    )
    .expect("false is a valid explicit switch");
    assert_eq!(false_ok.route_count(), 0);
}

#[test]
fn s59_scope_defaults_to_all_dimensions() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "filters": { "sqli": true, "xss": true },
            "routes": [{
                "path": "/api/u/{q}",
                "methods": ["GET"],
                "path_params": { "params": { "q": { "type": "char", "max_len": 64 } } },
                "query": { "params": { "kw": { "required": false } } },
                "headers": { "params": { "x-note": { "required": false } } },
                "body": { "params": { "note": { "required": false } }, "unknown": "ignore" }
            }]
        }"#,
    )
    .expect("filters should load");

    let ctx = RequestCtx {
        method: "GET",
        path: "/api/u/1' OR '1'='1",
    };
    assert_eq!(gw.check(&ctx).filter, Some("sqli"));

    let ctx = RequestCtx {
        method: "GET",
        path: "/api/u/x",
    };
    assert_eq!(
        gw.check_without_body(&ctx, &[], Some("kw=union+select+1"))
            .filter,
        Some("sqli")
    );
    assert_eq!(
        gw.check_with_headers(&ctx, &[("x-note", Some("<script>alert(1)</script>"))])
            .filter,
        Some("xss")
    );

    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{"note":"<script>alert(1)</script>"}"#,
    };
    assert_eq!(
        gw.check_with_body(&ctx, &[], None, &body).filter,
        Some("xss")
    );
}

#[test]
fn s59_scope_query_only_skips_other_dimensions() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "filters": { "sqli": true, "xss": true, "scope": ["query"] },
            "routes": [{
                "path": "/api/u/{q}",
                "methods": ["GET"],
                "path_params": { "params": { "q": { "type": "char", "max_len": 64 } } },
                "query": { "params": { "kw": { "required": false } } },
                "headers": { "params": { "x-note": { "required": false } } },
                "body": { "params": { "note": { "required": false } }, "unknown": "ignore" }
            }]
        }"#,
    )
    .expect("scoped filters should load");

    let ctx = RequestCtx {
        method: "GET",
        path: "/api/u/x",
    };
    assert_eq!(
        gw.check_without_body(&ctx, &[], Some("kw=union+select+1"))
            .filter,
        Some("sqli")
    );

    // 路径 / Header / Body 不在 scope 内：载荷通过，交给后端继续防御。
    let ctx = RequestCtx {
        method: "GET",
        path: "/api/u/1' OR '1'='1",
    };
    assert_eq!(gw.check(&ctx).decision, Decision::Allow);

    let ctx = RequestCtx {
        method: "GET",
        path: "/api/u/x",
    };
    assert_eq!(
        gw.check_with_headers(&ctx, &[("x-note", Some("<script>alert(1)</script>"))])
            .decision,
        Decision::Allow
    );

    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{"note":"<script>alert(1)</script>"}"#,
    };
    assert_eq!(
        gw.check_with_body(&ctx, &[], None, &body).decision,
        Decision::Allow
    );
}

#[test]
fn s59_scope_body_and_header_only() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "filters": { "sqli": true, "xss": true, "scope": ["body", "header"] },
            "routes": [{
                "path": "/api/u/{q}",
                "methods": ["GET"],
                "path_params": { "params": { "q": { "type": "char", "max_len": 64 } } },
                "query": { "params": { "kw": { "required": false } } },
                "headers": { "params": { "x-note": { "required": false } } },
                "body": { "params": { "note": { "required": false } }, "unknown": "ignore" }
            }]
        }"#,
    )
    .expect("scoped filters should load");

    let ctx = RequestCtx {
        method: "GET",
        path: "/api/u/x",
    };
    assert_eq!(
        gw.check_without_body(&ctx, &[], Some("kw=%3Cscript%3Ealert(1)%3C%2Fscript%3E"))
            .decision,
        Decision::Allow
    );

    assert_eq!(
        gw.check_with_headers(&ctx, &[("x-note", Some("<script>alert(1)</script>"))])
            .filter,
        Some("xss")
    );

    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{"note":"<script>alert(1)</script>"}"#,
    };
    assert_eq!(
        gw.check_with_body(&ctx, &[], None, &body).filter,
        Some("xss")
    );
}

#[test]
fn s59_scope_merges_across_group_and_route() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api",
                "filters": { "sqli": true, "scope": ["query"] },
                "query": { "params": { "kw": { "required": false } } },
                "router": [{
                    "path": "/search",
                    "methods": ["POST"],
                    "filters": { "xss": true, "scope": ["body"] },
                    "body": { "params": { "note": { "required": false } }, "unknown": "ignore" }
                }]
            }]
        }"#,
    )
    .expect("group scoped filters should load");

    let ctx = RequestCtx {
        method: "POST",
        path: "/api/search",
    };

    // 组：sqli + query；路由：xss + body。合并后两个维度都生效。
    assert_eq!(
        gw.check_without_body(&ctx, &[], Some("kw=union+select+1"))
            .filter,
        Some("sqli")
    );

    let body = BodyInput {
        content_type: Some("application/json"),
        content_encoding: None,
        bytes: br#"{"note":"<script>alert(1)</script>"}"#,
    };
    assert_eq!(
        gw.check_with_body(&ctx, &[], None, &body).filter,
        Some("xss")
    );

    // query 也是 XSS 的 scope（scope 是合并后的共享维度集合）。
    assert_eq!(
        gw.check_without_body(&ctx, &[], Some("kw=%3Cscript%3Ealert(1)%3C%2Fscript%3E"))
            .filter,
        Some("xss")
    );
}

#[test]
fn s59_scope_cannot_narrow_inherited_filters() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api",
                "filters": { "sqli": true },
                "router": [{
                    "path": "/search",
                    "methods": ["GET"],
                    "filters": { "sqli": true, "scope": ["query"] },
                    "headers": { "params": { "x-note": { "required": false } } }
                }]
            }]
        }"#,
    )
    .expect("filters should load");

    // 祖先默认全维度；子级 scope 只做并集，不能收窄。
    assert_eq!(
        gw.check_with_headers(
            &RequestCtx {
                method: "GET",
                path: "/api/search",
            },
            &[("x-note", Some("union select 1"))],
        )
        .filter,
        Some("sqli")
    );
}

#[test]
fn s59_scope_requires_enabled_filter_in_same_block() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api",
                "filters": { "scope": ["query"] },
                "router": [{ "path": "/x", "methods": ["GET"] }]
            }]
        }"#,
    )
    .expect_err("scope without enabled filter must be rejected");
    assert!(matches!(gw, ContractError::InvalidFilters(_)));

    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "filters": { "sqli": false, "scope": ["query"] },
            "routes": []
        }"#,
    )
    .expect_err("scope with false filters must be rejected");
    assert!(matches!(gw, ContractError::InvalidFilters(_)));
}

#[test]
fn s59_scope_empty_or_duplicate_rejected() {
    let empty = Gateway::parse(
        r#"{
            "protocol": 1,
            "filters": { "sqli": true, "scope": [] },
            "routes": []
        }"#,
    )
    .expect_err("empty scope must be rejected");
    assert!(matches!(empty, ContractError::InvalidFilters(_)));

    let duplicate = Gateway::parse(
        r#"{
            "protocol": 1,
            "filters": { "sqli": true, "scope": ["query", "query"] },
            "routes": []
        }"#,
    )
    .expect_err("duplicate scope must be rejected");
    assert!(matches!(duplicate, ContractError::InvalidFilters(_)));
}

#[test]
fn s59_scope_unknown_area_rejected() {
    let unknown = Gateway::parse(
        r#"{
            "protocol": 1,
            "filters": { "sqli": true, "scope": ["cookie"] },
            "routes": []
        }"#,
    )
    .expect_err("unknown scope area must be rejected");
    assert!(matches!(unknown, ContractError::Json(_)));

    let wrong_type = Gateway::parse(
        r#"{
            "protocol": 1,
            "filters": { "sqli": true, "scope": "query" },
            "routes": []
        }"#,
    )
    .expect_err("scope must be an array");
    assert!(matches!(wrong_type, ContractError::Json(_)));
}

#[test]
fn s35_route_path_regex_and_group_and() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api",
                "path_regex": "^/api/.*$",
                "router": [{
                    "path": "/reports/{year}",
                    "methods": ["GET"],
                    "path_regex": "^/api/reports/[0-9]{4}$"
                }]
            }]
        }"#,
    )
    .expect("route path_regex should load");

    assert_eq!(
        gw.check(&RequestCtx {
            method: "GET",
            path: "/api/reports/2026",
        })
        .decision,
        Decision::Allow
    );

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/reports/26",
    });
    assert_eq!(out.decision, Decision::RouteNotDeclared);
}

#[test]
fn s35_route_path_regex_failure_uses_unmatched() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "unmatched": "forward",
            "routes": [{
                "path": "/api/reports/{year}",
                "methods": ["GET"],
                "path_regex": "^/api/reports/[0-9]{4}$"
            }]
        }"#,
    )
    .expect("route path_regex should load");

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/reports/26",
    });

    assert_eq!(out.decision, Decision::Allow);
    assert!(out.forwarded);
}

#[test]
fn s35_route_path_regex_invalid_rejected() {
    for pattern in ["(", "a".repeat(257).as_str()] {
        let json = format!(
            r#"{{
                "protocol": 1,
                "routes": [{{
                    "path": "/api/x",
                    "methods": ["GET"],
                    "path_regex": "{pattern}"
                }}]
            }}"#
        );
        let err = Gateway::parse(&json).expect_err("invalid route path_regex must be rejected");
        assert!(
            matches!(err, ContractError::InvalidRoute { index: 0, .. }),
            "pattern={pattern:?}, got {err:?}"
        );
    }
}

#[test]
fn s32_priority_precedes_specificity() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [
                { "path": "/api/priority/me", "methods": ["GET"], "priority": 0 },
                {
                    "path": "/api/priority/{id}",
                    "methods": ["GET"],
                    "priority": 10,
                    "path_params": { "params": { "id": { "type": "char", "max_len": 1 } } }
                }
            ]
        }"#,
    )
    .expect("priority routes should load");

    // 高优先级参数路由先于低优先级静态路由。
    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/priority/me",
    });
    assert_eq!(out.decision, Decision::InvalidParameter);
    assert_eq!(out.field.as_deref(), Some("path.id"));
    assert_eq!(out.rule, Some("max_len"));

    assert_eq!(
        gw.check(&RequestCtx {
            method: "GET",
            path: "/api/priority/x",
        })
        .decision,
        Decision::Allow
    );
}

#[test]
fn s32_priority_same_level_keeps_specificity() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [
                { "path": "/api/same/me", "methods": ["GET"] },
                {
                    "path": "/api/same/{id}",
                    "methods": ["GET"],
                    "path_params": { "params": { "id": { "type": "char", "max_len": 1 } } }
                }
            ]
        }"#,
    )
    .expect("same priority routes should load");

    // 同一优先级仍按 matchit 的具体度：静态 > 参数。
    assert_eq!(
        gw.check(&RequestCtx {
            method: "GET",
            path: "/api/same/me",
        })
        .decision,
        Decision::Allow
    );
}

#[test]
fn s32_priority_path_regex_failure_no_fallback() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [
                { "path": "/api/fallback/me", "methods": ["GET"], "priority": 0 },
                {
                    "path": "/api/fallback/{id}",
                    "methods": ["GET"],
                    "priority": 10,
                    "path_regex": "^/api/fallback/[0-9]+$"
                }
            ]
        }"#,
    )
    .expect("priority routes should load");

    let out = gw.check(&RequestCtx {
        method: "GET",
        path: "/api/fallback/me",
    });
    assert_eq!(out.decision, Decision::RouteNotDeclared);
}

#[test]
fn s32_duplicate_path_across_priority_rejected() {
    let err = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [
                { "path": "/api/dup", "methods": ["GET"], "priority": 0 },
                { "path": "/api/dup", "methods": ["POST"], "priority": 10 }
            ]
        }"#,
    )
    .expect_err("duplicate full path must be rejected even with different priorities");

    assert!(matches!(err, ContractError::InvalidRoute { index: 1, .. }));
}

// ---------------------------------------------------------------------------
// §6 responses
// ---------------------------------------------------------------------------

#[test]
fn s6_responses_override_lookup() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "responses": {
                "route_not_declared": { "status": 410, "body": "{\"error\":\"gone\"}" },
                "invalid_parameter": { "status": 422 },
                "contract_not_ready": { "status": 503, "retry_after": 5 }
            },
            "routes": []
        }"#,
    )
    .expect("responses should load");

    let route = gw
        .response_override(Decision::RouteNotDeclared)
        .expect("route override");
    assert_eq!(route.status, 410);
    assert_eq!(route.body.as_deref(), Some("{\"error\":\"gone\"}"));
    assert_eq!(route.retry_after, None);

    let invalid = gw
        .response_override(Decision::InvalidParameter)
        .expect("invalid override");
    assert_eq!(invalid.status, 422);
    assert_eq!(invalid.body, None);

    assert!(gw.response_override(Decision::MissingParameter).is_none());
    assert!(gw.response_override(Decision::Allow).is_none());
}

#[test]
fn s6_responses_reject_non_4xx_5xx() {
    for status in [200u16, 204, 301, 399, 600] {
        let json = format!(
            r#"{{ "protocol": 1, "responses": {{ "invalid_parameter": {{ "status": {status} }} }}, "routes": [] }}"#
        );
        let err = Gateway::parse(&json).expect_err("only 4xx/5xx overrides are allowed");
        assert!(
            matches!(err, ContractError::InvalidResponses(_)),
            "status={status}, got {err:?}"
        );
    }
}

#[test]
fn s6_responses_reject_bad_shape() {
    let missing_status = Gateway::parse(
        r#"{ "protocol": 1, "responses": { "invalid_parameter": {} }, "routes": [] }"#,
    )
    .expect_err("status is required");
    assert!(matches!(missing_status, ContractError::InvalidResponses(_)));

    let long_body = "a".repeat(1025);
    let json = format!(
        r#"{{ "protocol": 1, "responses": {{ "invalid_parameter": {{ "status": 400, "body": "{long_body}" }} }}, "routes": [] }}"#
    );
    let err = Gateway::parse(&json).expect_err("body must be <= 1024 bytes");
    assert!(matches!(err, ContractError::InvalidResponses(_)));

    let unknown_key = Gateway::parse(
        r#"{ "protocol": 1, "responses": { "not_a_reason": { "status": 400 } }, "routes": [] }"#,
    )
    .expect_err("unknown response key must be rejected");
    assert!(matches!(unknown_key, ContractError::UnknownField { .. }));

    let unknown_field = Gateway::parse(
        r#"{ "protocol": 1, "responses": { "invalid_parameter": { "status": 400, "headers": {} } }, "routes": [] }"#,
    )
    .expect_err("unknown response field must be rejected");
    assert!(matches!(unknown_field, ContractError::UnknownField { .. }));
}

#[test]
fn s6_responses_only_top_level() {
    let route_level = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "path": "/x",
                "methods": ["GET"],
                "responses": { "invalid_parameter": { "status": 400 } }
            }]
        }"#,
    )
    .expect_err("responses is top-level only");
    assert!(matches!(route_level, ContractError::UnknownField { .. }));

    let group_level = Gateway::parse(
        r#"{
            "protocol": 1,
            "routes": [{
                "prefix": "/api",
                "responses": { "invalid_parameter": { "status": 400 } },
                "router": []
            }]
        }"#,
    )
    .expect_err("responses is top-level only");
    assert!(matches!(group_level, ContractError::UnknownField { .. }));
}
