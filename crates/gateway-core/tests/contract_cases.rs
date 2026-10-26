use gateway_core::{ContractError, Decision, Gateway, RequestCtx};

const REPO_CONTRACT: &str = r#"{
    "protocol": 1,
    "version": "phase1-fixed",
    "ttl": 3,
    "meta": { "owner": "test" },
    "routes": [
        { "path": "/info", "methods": ["GET"] },
        { "path": "/api/health", "methods": ["GET"] },
        { "path": "/api/login", "methods": ["POST"] },
        { "path": "/api/user/{id}", "methods": ["GET"] },
        { "path": "/api/user/{id}/profile", "methods": ["GET", "PUT"] },
        { "path": "/api/products", "methods": ["GET"] },
        { "path": "/api/orders", "methods": ["POST"] },
        { "path": "/api/echo", "methods": [] }
    ]
}"#;

fn gw() -> Gateway {
    Gateway::parse(REPO_CONTRACT).expect("contract should parse")
}

fn check<'a>(gw: &'a Gateway, method: &str, path: &'a str) -> (Decision, Option<&'a str>) {
    let out = gw.check(&RequestCtx { method, path });
    (out.decision, out.allow)
}

#[test]
fn allows_declared_static_route() {
    assert_eq!(check(&gw(), "GET", "/api/health").0, Decision::Allow);
}

#[test]
fn allows_declared_dynamic_route() {
    let g = gw();
    assert_eq!(check(&g, "GET", "/api/user/42").0, Decision::Allow);
    // {id} 只匹配单段，多出来的一段应落到未声明
    assert_eq!(
        check(&g, "GET", "/api/user/abc/def").0,
        Decision::RouteNotDeclared
    );
    assert_eq!(check(&g, "PUT", "/api/user/42/profile").0, Decision::Allow);
}

#[test]
fn rejects_undeclared_route() {
    assert_eq!(
        check(&gw(), "GET", "/api/admin/users").0,
        Decision::RouteNotDeclared
    );
    assert_eq!(check(&gw(), "GET", "/phpinfo.php").0, Decision::RouteNotDeclared);
}

#[test]
fn forwards_undeclared_route_when_configured() {
    let g = Gateway::parse(
        r#"{"protocol":1,"unmatched":"forward","routes":[
            {"path":"/api/health","methods":["GET"]}
        ]}"#,
    )
    .expect("forward contract should parse");

    let out = g.check(&RequestCtx {
        method: "GET",
        path: "/api/admin/users",
    });
    assert_eq!(out.decision, Decision::Allow);
    assert!(out.forwarded, "undeclared route should be forwarded");

    // 已声明的路由照常判定：方法不符仍然是 405，不转发。
    let out = g.check(&RequestCtx {
        method: "POST",
        path: "/api/health",
    });
    assert_eq!(out.decision, Decision::MethodNotAllowed);
    assert!(!out.forwarded);
}

#[test]
fn unmatched_defaults_to_deny() {
    for contract in [
        r#"{"routes":[]}"#,
        r#"{"unmatched":"deny","routes":[]}"#,
    ] {
        let g = Gateway::parse(contract).expect("contract should parse");
        let out = g.check(&RequestCtx {
            method: "GET",
            path: "/api/admin/users",
        });
        assert_eq!(out.decision, Decision::RouteNotDeclared);
        assert!(!out.forwarded);
    }
}

#[test]
fn rejects_unknown_unmatched_value() {
    let err = Gateway::parse(r#"{"unmatched":"allow","routes":[]}"#)
        .expect_err("unknown unmatched value must be rejected");
    assert!(matches!(err, ContractError::Json(_)));
}

#[test]
fn rejects_wrong_method_and_reports_allow() {
    let g = gw();
    let (decision, allow) = check(&g, "POST", "/api/health");
    assert_eq!(decision, Decision::MethodNotAllowed);
    assert_eq!(allow, Some("GET"));

    let (decision, allow) = check(&g, "POST", "/api/user/42/profile");
    assert_eq!(decision, Decision::MethodNotAllowed);
    assert_eq!(allow, Some("GET, PUT"));
}

#[test]
fn method_match_is_case_insensitive() {
    assert_eq!(check(&gw(), "get", "/api/health").0, Decision::Allow);
}

#[test]
fn empty_methods_means_any_method() {
    let g = gw();
    assert_eq!(check(&g, "GET", "/api/echo").0, Decision::Allow);
    assert_eq!(check(&g, "PATCH", "/api/echo").0, Decision::Allow);
}

#[test]
fn trailing_slash_is_not_matched() {
    assert_eq!(
        check(&gw(), "GET", "/api/products/").0,
        Decision::RouteNotDeclared
    );
}

#[test]
fn rejects_contract_with_trailing_slash() {
    let err = Gateway::parse(r#"{"routes":[{"path":"/api/x/","methods":["GET"]}]}"#)
        .expect_err("should reject");
    assert!(matches!(err, ContractError::InvalidRoute { index: 0, .. }));
}

#[test]
fn rejects_contract_with_relative_path() {
    assert!(Gateway::parse(r#"{"routes":[{"path":"api/x","methods":["GET"]}]}"#).is_err());
}

#[test]
fn rejects_duplicate_route() {
    let json = r#"{"routes":[
        {"path":"/api/x","methods":["GET"]},
        {"path":"/api/x","methods":["POST"]}
    ]}"#;
    assert!(matches!(
        Gateway::parse(json).expect_err("duplicate should fail"),
        ContractError::InvalidRoute { index: 1, .. }
    ));
}

#[test]
fn rejects_empty_method_string() {
    assert!(matches!(
        Gateway::parse(r#"{"routes":[{"path":"/api/x","methods":[""]}]}"#)
            .expect_err("empty method should fail"),
        ContractError::EmptyMethod { index: 0, .. }
    ));
}

#[test]
fn rejects_invalid_json() {
    assert!(matches!(
        Gateway::parse("{not json").expect_err("bad json"),
        ContractError::Json(_)
    ));
}

#[test]
fn exposes_contract_metadata() {
    let g = gw();
    assert_eq!(g.protocol(), 1);
    assert_eq!(g.version(), Some("phase1-fixed"));
    assert_eq!(g.ttl(), Some(3));
    assert_eq!(g.route_count(), 8);
}

#[test]
fn protocol_defaults_to_1_for_minimal_contract() {
    let g = Gateway::parse(r#"{"routes":[]}"#).expect("minimal contract should parse");
    assert_eq!(g.protocol(), 1);
}

#[test]
fn rejects_unsupported_protocol() {
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
fn rejects_unknown_top_level_field() {
    let err = Gateway::parse(r#"{"routes":[],"responses":{}}"#)
        .expect_err("unknown top-level field must fail");
    assert!(matches!(
        err,
        ContractError::UnknownField { ref scope, ref name }
            if scope == "contract" && name == "responses"
    ));
}

#[test]
fn rejects_unimplemented_route_field() {
    let err = Gateway::parse(
        r#"{"routes":[{"path":"/api/x","methods":["GET"],"priority":10}]}"#,
    )
    .expect_err("priority is not implemented yet");
    assert!(matches!(
        err,
        ContractError::UnknownField { ref scope, ref name }
            if scope == "route #0" && name == "priority"
    ));
}

#[test]
fn static_segment_wins_over_param_segment() {
    // 协议第 3.1 节：静态段 > 参数段；契约顺序不影响结果。
    let g = Gateway::parse(
        r#"{"routes":[
            {"path":"/a/{x}","methods":["POST"]},
            {"path":"/a/b","methods":["GET"]}
        ]}"#,
    )
    .expect("overlapping static/param should compile");

    assert_eq!(check(&g, "GET", "/a/b").0, Decision::Allow);
    assert_eq!(check(&g, "POST", "/a/b").0, Decision::MethodNotAllowed);
    assert_eq!(check(&g, "POST", "/a/c").0, Decision::Allow);
}

#[test]
fn rejects_ambiguous_param_routes() {
    // 形状相同、参数名不同：无法确定优先级，整份契约拒绝（协议第 3.1 节）。
    let err = Gateway::parse(
        r#"{"routes":[
            {"path":"/u/{id}","methods":["GET"]},
            {"path":"/u/{name}","methods":["GET"]}
        ]}"#,
    )
    .expect_err("ambiguous param routes must be rejected");
    assert!(matches!(err, ContractError::InvalidRoute { index: 1, .. }));
}

#[test]
fn meta_is_ignored() {
    let g = Gateway::parse(
        r#"{"protocol":1,"meta":{"owner":"x","anything":[1,2,3]},"routes":[]}"#,
    )
    .expect("meta is free-form");
    assert_eq!(g.route_count(), 0);
}

#[test]
fn catch_all_route_works() {
    let g = Gateway::parse(r#"{"routes":[{"path":"/static/{*rest}","methods":["GET"]}]}"#)
        .expect("catch-all should compile");
    assert_eq!(check(&g, "GET", "/static/a/b/c.txt").0, Decision::Allow);
}

#[test]
fn empty_contract_denies_everything() {
    let g = Gateway::parse(r#"{"version":"empty","routes":[]}"#).expect("empty ok");
    assert_eq!(check(&g, "GET", "/api/health").0, Decision::RouteNotDeclared);
    assert_eq!(g.route_count(), 0);
}
