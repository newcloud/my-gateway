//! 协议 v1 验收（纯逻辑，不加载 nginx）。
//!
//! 规则来源：`docs/PROTOCOL.md`；用例清单：`docs/PROTOCOL_TEST_PLAN.md`。
//!
//! 写法约定：
//! - 每个 test 自带契约 JSON，不使用共享夹具/常量；
//! - 直接 `Gateway::parse` + `gw.check`，不封装 helper；
//! - 期望值写死，不在测试里推算；不做表驱动循环；
//! - 分区编号对应协议章节，新增规则时在对应分区补用例。

use gateway_core::{ContractError, Decision, Gateway, RequestCtx};

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
fn s2_version_and_ttl_exposed() {
    let gw = Gateway::parse(
        r#"{
            "protocol": 1,
            "version": "2026-10-26T12:00:00Z",
            "ttl": 3,
            "routes": [{ "path": "/api/health", "methods": ["GET"] }]
        }"#,
    )
    .expect("contract should load");

    assert_eq!(gw.version(), Some("2026-10-26T12:00:00Z"));
    assert_eq!(gw.ttl(), Some(3));
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
    let err = Gateway::parse(r#"{"protocol":1,"responses":{},"routes":[]}"#)
        .expect_err("unknown top-level field must reject the whole contract");

    match err {
        ContractError::UnknownField { scope, name } => {
            assert_eq!(scope, "contract");
            assert_eq!(name, "responses");
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
}
