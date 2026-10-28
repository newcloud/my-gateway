#![recursion_limit = "512"]

//! demo_server：my_gateway 阶段 1 的被保护后端 + 固定契约接口。
//!
//! 目的有两个：
//! 1. 提供一组常用接口（登录、用户、下单等），供 curl 与网关联调；
//! 2. `GET /info` / `GET /info_v2` 返回**写死的**路由契约（协议 v1，见 `docs/PROTOCOL.md`），
//!    两份内容不同，供 nginx 里的多个网关分别指向；后续改为真实路由生成。
//!
//! 每个请求都会在 stdout 打印一行 `[demo_server] METHOD URI`。
//! 被网关拦掉的请求不会出现在这里——这是验证"插件确实先于后端执行"的直接证据。

use axum::{
    body::Bytes,
    extract::{Path, RawQuery, Request},
    http::{header::CONTENT_TYPE, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

#[tokio::main]
async fn main() {
    let port = std::env::var("DEMO_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(9000);

    let app = Router::new()
        // 契约接口（阶段 1 固定内容）：v1 / v2 内容不同，供多网关分别指向
        .route("/info", get(info))
        .route("/info_v2", get(info_v2))
        // 契约来源加固测试：302 → /info；插件不跟随重定向
        .route("/info-redirect", get(info_redirect))
        // 业务测试接口
        .route("/api/health", get(health))
        .route("/api/metrics", get(metrics))
        .route("/api/audit", get(audit))
        .route("/api/reports/{year}", get(report))
        .route("/api/login", post(login))
        .route("/api/user/{id}", get(get_user))
        .route("/api/user/{id}/profile", get(get_profile).put(put_profile))
        .route("/api/products", get(products))
        .route("/api/orders", post(create_order))
        .route("/api/echo", post(echo))
        // 阶段 5：Header 存在性校验的放行目标（契约在 /info 中声明）
        .route("/api/header-check", get(header_check))
        // 阶段 5：Header 值规则（type / 范围 / 长度 / 枚举 / regex）的放行目标
        .route("/api/header-value-check", get(header_value_check))
        // 阶段 5：Header 条件必填（required_if）的放行目标
        .route("/api/header-required-if", get(header_required_if))
        // 阶段 5：Query 参数校验的放行目标
        .route("/api/query-check", get(query_check))
        .route("/api/query-strict", get(query_strict))
        // 阶段 5：Body 参数校验的放行目标
        .route("/api/body-check", post(body_check))
        // 阶段 5：路径参数 / 分组路由的放行目标
        .route("/api/group/items/{id}", get(group_item))
        .route("/api/group/admin/audit", get(group_audit))
        // 示例：/api/v1 分组要求登录 header；/api/public 表单无需登录
        .route("/api/v1/me", get(v1_me))
        .route("/api/v1/orders/{id}", get(v1_order))
        .route("/api/v1/feedback", post(v1_feedback))
        .route("/api/v1/admin/audit", get(v1_admin_audit))
        .route("/api/public/survey", post(public_survey))
        .route("/api/org/{org}/profile", get(org_profile))
        // 示例：filters（SQLi / XSS）
        .route("/api/filter/search", get(filter_search))
        .route("/api/filter/comment", post(filter_comment))
        .route("/api/filter/item/{name}", get(filter_item))
        .route("/api/filter/header", get(filter_header))
        // filters scope 示例：只扫描 query
        .route("/api/filter/scope/query", get(filter_search))
        .route("/api/filter/scope/body", post(filter_comment))
        .route("/api/filter/scope/header", get(filter_header))
        .route("/api/filter/scope/item/{name}", get(filter_item))
        // body.max_bytes 示例：组级 2K，子级可关闭/收紧
        .route("/api/limit/inherit", post(echo))
        .route("/api/limit/disabled", post(echo))
        .route("/api/limit/small", post(echo))
        // 示例：路由级 path_regex 与 priority
        .route("/api/route-regex/{id}", get(route_regex))
        .route("/api/priority/me", get(priority_static))
        .route("/api/priority/{id}", get(priority_param))
        // 故意"存在但不进契约"的接口，供阶段 4 验证未声明路由
        .route("/api/admin/users", get(admin_users))
        // 故意做成可访问的假漏洞文件，证明网关能在到达后端前拦下
        .route("/phpinfo.php", get(phpinfo_stub))
        .fallback(fallback)
        .layer(middleware::from_fn(log_requests));

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap_or_else(|e| panic!("bind 127.0.0.1:{port} failed: {e}"));

    println!("[demo_server] listening on http://127.0.0.1:{port}");
    println!("[demo_server] contract v1: http://127.0.0.1:{port}/info");
    println!("[demo_server] contract v2: http://127.0.0.1:{port}/info_v2");

    axum::serve(listener, app).await.expect("server error");
}

/// 请求日志 + 标记响应来源，便于 curl 判断是后端还是网关返回的。
async fn log_requests(req: Request, next: Next) -> Response {
    println!("[demo_server] {} {}", req.method(), req.uri());
    let mut res = next.run(req).await;
    res.headers_mut()
        .insert("x-demo-backend", HeaderValue::from_static("1"));
    res
}

// ---------------------------------------------------------------------------
// 契约接口
// ---------------------------------------------------------------------------

/// 协议 v1（见 docs/PROTOCOL.md）：契约目前写死，后续改成由真实路由生成。
/// `protocol` 决定解析规则，`version` 只是内容修订号。
async fn info() -> Json<Value> {
    Json(json!({
        "protocol": 1,
        "version": "phase1-fixed",
        "meta": { "owner": "demo_server", "generated_by": "handwritten" },
        "responses": {
            "route_not_declared": { "status": 404, "body": "{\"error\":\"route-denied\"}" },
            "method_not_allowed": { "status": 405, "body": "{\"error\":\"method-denied\"}" },
            "missing_parameter": { "status": 400, "body": "{\"error\":\"missing-parameter\"}" },
            "invalid_parameter": { "status": 400, "body": "{\"error\":\"invalid-parameter\"}" },
            "unsupported_media_type": { "status": 415, "body": "{\"error\":\"unsupported-media-type\"}" },
            "body_too_large": { "status": 413, "body": "{\"error\":\"body-too-large\"}" }
        },
        "routes": [
            { "path": "/info",                     "methods": ["GET"] },
            { "path": "/api/health",               "methods": ["GET"] },
            { "path": "/api/login",                "methods": ["POST"] },
            {
                "path": "/api/user/{id}",
                "methods": ["GET"],
                "path_params": { "params": { "id": { "type": "int", "min": 1 } } }
            },
            { "path": "/api/user/{id}/profile",    "methods": ["GET", "PUT"] },
            { "path": "/api/products",             "methods": ["GET"] },
            { "path": "/api/orders",               "methods": ["POST"] },
            { "path": "/api/echo",                 "methods": ["POST"] },
            // 阶段 5：Header 存在性校验。只声明 required / 显式可选；
            // 值规则放在下面的 /api/header-value-check，便于分别验证两个切片。
            {
                "path": "/api/header-check",
                "methods": ["GET"],
                "headers": {
                    "params": {
                        "x-demo-token": true,
                        "x-demo-note": { "required": false }
                    }
                }
            },
            // 阶段 5：Header 值规则（第一批：type / 长度 / 范围 / 枚举 / regex）。
            // 规则都声明为可选（x-demo-token 除外），便于分别验证“缺失跳过”与“值非法拒绝”。
            {
                "path": "/api/header-value-check",
                "methods": ["GET"],
                "headers": {
                    "params": {
                        "x-demo-token": { "type": "char", "required": true, "min_len": 2, "max_len": 8 },
                        "x-demo-count": { "type": "int", "min": 1, "max": 99 },
                        "x-demo-mode":  { "type": "enum", "values": ["alpha", "beta"], "ignore_case": true },
                        "x-demo-code":  { "regex": "[A-Z]{2}[0-9]{2}" }
                    }
                }
            },
            // 阶段 5：Header 条件必填（required_if），同时覆盖对象形式与字符串形式。
            {
                "path": "/api/header-required-if",
                "methods": ["GET"],
                "headers": {
                    "params": {
                        "x-demo-mode":   { "type": "enum", "values": ["secure", "open"] },
                        "x-demo-auth":   {
                            "required_if": { "field": "x-demo-mode", "equals": "secure" },
                            "type": "char", "min_len": 2
                        },
                        "x-demo-note":   { "required": false },
                        "x-demo-legacy": { "required_if": "x-demo-note", "type": "int" }
                    }
                }
            },
            // 阶段 5：Query 参数（required / int 范围 / enum / char 长度 / bool / regex / required_if）。
            {
                "path": "/api/query-check",
                "methods": ["GET"],
                "query": {
                    "params": {
                        "page":  { "type": "int", "required": true, "min": 1, "max": 99 },
                        "mode":  { "type": "enum", "values": ["secure", "open"] },
                        "token": {
                            "required_if": { "field": "mode", "equals": "secure" },
                            "type": "char", "min_len": 2
                        },
                        "kw":    { "type": "char", "max_len": 8 },
                        "flag":  { "type": "bool" },
                        "code":  { "regex": "[A-Z]{2}[0-9]{2}" }
                    },
                    "unknown": "ignore"
                }
            },
            // 阶段 5：Query 未声明字段拒绝。
            {
                "path": "/api/query-strict",
                "methods": ["GET"],
                "query": {
                    "params": { "page": { "type": "int", "min": 1 } },
                    "unknown": "reject"
                }
            },
            // 阶段 5：Body 参数（默认 unknown=reject，防 Mass Assignment）。
            {
                "path": "/api/body-check",
                "methods": ["POST"],
                "body": {
                    "max_bytes": 65536,
                    "max_depth": 4,
                    "max_string_length": 32,
                    "max_array_entries": 5,
                    "params": {
                        "sku":  { "type": "char", "required": true, "max_len": 8 },
                        "qty":  { "type": "int", "required": true, "min": 1, "max": 99 },
                        "note": { "type": "char", "max_len": 16 }
                    }
                }
            },
            // 阶段 5：分组路由（当前只启用 headers / unmatched / path_regex）。
            {
                "prefix": "/api/group",
                "unmatched": "forward",
                "path_regex": "^/api/group/(items/[0-9]+|admin/audit)$",
                "headers": { "params": { "x-group-token": true } },
                "router": [
                    {
                        "path": "/items/{id}",
                        "methods": ["GET"],
                        "path_params": { "params": { "id": { "type": "int", "min": 1 } } }
                    },
                    {
                        "prefix": "/admin",
                        "unmatched": "deny",
                        "headers": { "params": { "x-admin-token": true } },
                        "router": [ { "path": "/audit", "methods": ["GET"] } ]
                    }
                ]
            }
            ,
            // 示例：登录后才能访问的 /api/v1 分组。
            // - 组级 authorization 对所有后代生效
            // - path_regex 只允许下面声明的资源形状（数字订单号）
            // - 嵌套 /admin 再要求 x-admin-token
            {
                "prefix": "/api/v1",
                "methods": ["GET", "POST"],
                "path_regex": "^/api/v1/(me|orders/[0-9]+|feedback|admin/audit)$",
                "query": {
                    "params": { "trace": { "type": "char", "max_len": 8 } },
                    "unknown": "ignore"
                },
                "body": {
                    "params": { "locale": { "type": "char", "max_len": 4 } }
                },
                "headers": {
                    "params": {
                        "authorization": { "type": "char", "required": true, "min_len": 8, "max_len": 128 }
                    }
                },
                "router": [
                    { "path": "/me", "methods": ["GET"] },
                    {
                        "path": "/orders/{id}",
                        "methods": ["GET"],
                        "path_params": { "params": { "id": { "type": "int", "min": 1 } } }
                    },
                    {
                        "path": "/feedback",
                        "methods": ["POST"],
                        "body": {
                            "params": {
                                "rating":  { "type": "int", "required": true, "min": 1, "max": 5 },
                                "topic":   { "type": "enum", "values": ["bug", "feature"] },
                                "comment": {
                                    "required_if": { "field": "topic", "equals": "bug" },
                                    "type": "char", "max_len": 200
                                }
                            },
                            "unknown": "reject",
                            "content_types": ["application/json", "application/x-www-form-urlencoded"]
                        }
                    },
                    {
                        "prefix": "/admin",
                        "methods": ["GET"],
                        "unmatched": "deny",
                        "path_regex": "^/api/v1/admin/audit$",
                        "headers": { "params": { "x-admin-token": true } },
                        "router": [ { "path": "/audit", "methods": ["GET"] } ]
                    }
                ]
            },
            // 示例：公开表单分组，不需要登录 header。
            {
                "prefix": "/api/public",
                "unmatched": "deny",
                "path_regex": "^/api/public/survey$",
                "router": [{
                    "path": "/survey",
                    "methods": ["POST"],
                    "body": {
                        "params": {
                            "email":   { "type": "char", "required": true, "max_len": 64, "regex": "[^@]+@[^@]+" },
                            "score":   { "type": "int", "required": true, "min": 1, "max": 5 },
                            "consent": { "type": "bool", "required": true },
                            "comment": { "type": "char", "max_len": 200 }
                        },
                        "unknown": "reject",
                        "content_types": ["application/json", "application/x-www-form-urlencoded"]
                    }
                }]
            }
            ,
            // 示例：分组级 path_params 对后代生效（org 必须 > 0）。
            {
                "prefix": "/api/org",
                "path_params": { "params": { "org": { "type": "int", "min": 1 } } },
                "router": [ { "path": "/{org}/profile", "methods": ["GET"] } ]
            }
            ,
            // 示例：路由级 path_regex（只允许 4 位数字）。
            {
                "path": "/api/route-regex/{id}",
                "methods": ["GET"],
                "path_regex": "^/api/route-regex/[0-9]{4}$"
            },
            // 示例：priority 高的参数路由先于低优先级静态路由。
            { "path": "/api/priority/me", "methods": ["GET"], "priority": 0 },
            {
                "path": "/api/priority/{id}",
                "methods": ["GET"],
                "priority": 10,
                "path_params": { "params": { "id": { "type": "char", "max_len": 1 } } }
            },
            // 示例：SQLi / XSS 过滤（第三方 libinjectionrs）。
            // 组级 filters 对后代生效；route 的 sqli:false 不能关闭组级开关。
            {
                "prefix": "/api/filter",
                "filters": { "sqli": true, "xss": true },
                "path_regex": "^/api/filter/(search|comment|item/[^/]+|header)$",
                "query": { "params": { "kw": { "type": "char", "max_len": 64 } } },
                "headers": { "params": { "x-filter-note": { "required": false } } },
                "router": [
                    { "path": "/search", "methods": ["GET"] },
                    {
                        "path": "/comment",
                        "methods": ["POST"],
                        "filters": { "sqli": false },
                        "body": { "params": {
                            "comment": { "type": "char", "required": true, "max_len": 200 },
                            "rating":  { "type": "int", "min": 1, "max": 5 }
                        } }
                    },
                    {
                        "path": "/item/{name}",
                        "methods": ["GET"],
                        "path_params": { "params": { "name": { "type": "char", "max_len": 32 } } }
                    },
                    { "path": "/header", "methods": ["GET"] }
                ]
            },
            // filters scope：只扫描 query；body/header/path 即使带注入特征也放行。
            {
                "prefix": "/api/filter/scope",
                "filters": { "sqli": true, "xss": true, "scope": ["query"] },
                "path_regex": "^/api/filter/scope/(query|body|header|item/[^/]+)$",
                "query": { "params": { "kw": { "type": "char", "max_len": 64 } } },
                "body": { "params": { "comment": { "type": "char", "max_len": 200 } } },
                "headers": { "params": { "x-filter-note": { "required": false } } },
                "router": [
                    { "path": "/query", "methods": ["GET"] },
                    { "path": "/body", "methods": ["POST"] },
                    { "path": "/header", "methods": ["GET"] },
                    {
                        "path": "/item/{name}",
                        "methods": ["GET"],
                        "path_params": { "params": { "name": { "type": "char", "max_len": 32 } } }
                    }
                ]
            },
            // body.max_bytes：组级 2K 继承给子级；子级可收紧或写 0 关闭。
            {
                "prefix": "/api/limit",
                "body": {
                    "max_bytes": 2048,
                    "unknown": "reject",
                    "params": {
                        "data": { "type": "char", "required": true, "max_len": 8192 }
                    }
                },
                "router": [
                    { "path": "/inherit", "methods": ["POST"] },
                    {
                        "path": "/disabled",
                        "methods": ["POST"],
                        "body": { "max_bytes": 0 }
                    },
                    {
                        "path": "/small",
                        "methods": ["POST"],
                        "body": { "max_bytes": 512 }
                    }
                ]
            }
        ]
    }))
}

/// 第二份契约：同样是 protocol 1，指向不同地址，用来验证多网关独立快照与未声明路由策略。
///
/// 与 `/info` 的差异：
/// - **只在 `/info_v2`**：`/info_v2`、`/api/metrics`、`/api/reports/{year}`
/// - **只在 `/info`**：`/api/login`、`/api/user/{id}`、`/api/user/{id}/profile`
/// - **两边都有**：`/info`、`/api/health`、`/api/products`、`/api/orders`、`/api/echo`
/// - **策略不同**：这份写了 `"unmatched": "forward"`，契约没声明的路径交给后端处理（`/info` 是默认 deny）
/// 契约来源加固测试：`gateway_info_url` 指向这里时应拒绝 30x，而不是跟到 /info。
async fn info_redirect() -> impl IntoResponse {
    (
        StatusCode::FOUND,
        [
            (CONTENT_TYPE, "text/plain"),
            (axum::http::header::LOCATION, "/info"),
        ],
    )
}

async fn info_v2() -> Json<Value> {
    Json(json!({
        "protocol": 1,
        "unmatched": "forward",
        "version": "phase1-forward",
        "meta": { "owner": "demo_server", "generated_by": "handwritten", "contract": "forward-demo" },
        "routes": [
            { "path": "/info_v2",                  "methods": ["GET"] },
            { "path": "/info",                     "methods": ["GET"] },
            { "path": "/api/health",               "methods": ["GET"] },
            { "path": "/api/products",             "methods": ["GET"] },
            { "path": "/api/orders",               "methods": ["POST"] },
            { "path": "/api/echo",                 "methods": ["POST"] },
            { "path": "/api/metrics",              "methods": ["GET"] },
            { "path": "/api/reports/{year}",       "methods": ["GET"] }
        ]
    }))
}

// ---------------------------------------------------------------------------
// 业务接口
// ---------------------------------------------------------------------------

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "service": "demo_server" }))
}

/// 阶段 5：只有通过了网关 Header 存在性校验，请求才会走到这里。
/// 只回显“头是否存在”，不读取值——与 core 当前只做存在性的实现保持一致。
async fn header_check(headers: HeaderMap) -> Json<Value> {
    Json(json!({
        "ok": true,
        "has_token": headers.contains_key("x-demo-token"),
        "has_note": headers.contains_key("x-demo-note"),
        "source": "demo_server"
    }))
}

/// 阶段 5：只有通过了网关 Header 值规则校验，请求才会走到这里。
/// 回显收到的值，用来证明 OWS 处理与放行的是同一个请求。
async fn header_value_check(headers: HeaderMap) -> Json<Value> {
    let value = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };

    Json(json!({
        "ok": true,
        "token": value("x-demo-token"),
        "count": value("x-demo-count"),
        "mode": value("x-demo-mode"),
        "code": value("x-demo-code"),
        "source": "demo_server"
    }))
}

/// 阶段 5：只有通过了网关 Header 条件必填（required_if）校验，请求才会走到这里。
async fn header_required_if(headers: HeaderMap) -> Json<Value> {
    let value = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };

    Json(json!({
        "ok": true,
        "mode": value("x-demo-mode"),
        "auth": value("x-demo-auth"),
        "note": value("x-demo-note"),
        "legacy": value("x-demo-legacy"),
        "source": "demo_server"
    }))
}

/// 阶段 5：只有通过 Query 校验后才会到这里；回显原始 query 供测试断言。
async fn query_check(RawQuery(query): RawQuery) -> Json<Value> {
    Json(json!({ "ok": true, "query": query, "source": "demo_server" }))
}

async fn query_strict(RawQuery(query): RawQuery) -> Json<Value> {
    Json(json!({ "ok": true, "query": query, "source": "demo_server" }))
}

/// 阶段 5：只有通过 Body 校验后才会到这里；回显长度与原始 body。
async fn body_check(headers: HeaderMap, body: Bytes) -> Json<Value> {
    let len = body.len();
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("-");
    let body = String::from_utf8_lossy(&body);

    Json(json!({
        "ok": true,
        "len": len,
        "content_type": content_type,
        "body": body,
        "source": "demo_server"
    }))
}

/// 阶段 5：分组 `/api/group` 的路径参数放行目标。
async fn group_item(Path(id): Path<String>) -> Json<Value> {
    Json(json!({ "ok": true, "id": id, "source": "demo_server" }))
}

/// 阶段 5：嵌套分组 `/api/group/admin` 的放行目标。
async fn group_audit() -> Json<Value> {
    Json(json!({ "ok": true, "audit": true, "source": "demo_server" }))
}

/// 登录分组示例：只有通过分组 header 校验才会到这里。
async fn v1_me(headers: HeaderMap) -> Json<Value> {
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("-");

    Json(json!({
        "ok": true,
        "user": "demo",
        "authorization": authorization,
        "source": "demo_server"
    }))
}

async fn v1_order(Path(id): Path<String>) -> Json<Value> {
    Json(json!({ "ok": true, "order_id": id, "status": "paid", "source": "demo_server" }))
}

#[derive(Debug, Deserialize)]
struct FeedbackReq {
    rating: i64,
    #[serde(default)]
    topic: Option<String>,
    #[serde(default)]
    comment: Option<String>,
}

/// 表单示例：JSON 与 form 都接受；topic=bug 时 comment 由网关条件必填。
async fn v1_feedback(headers: HeaderMap, body: Bytes) -> Response {
    let req: FeedbackReq = match parse_body(&headers, &body) {
        Ok(req) => req,
        Err(msg) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "bad_request", "detail": msg })),
            )
                .into_response()
        }
    };

    Json(json!({
        "ok": true,
        "rating": req.rating,
        "topic": req.topic,
        "comment": req.comment,
        "source": "demo_server"
    }))
    .into_response()
}

/// 嵌套 /admin 分组：除 authorization 外还需要 x-admin-token。
async fn v1_admin_audit(headers: HeaderMap) -> Json<Value> {
    Json(json!({
        "ok": true,
        "audit": true,
        "has_admin_token": headers.contains_key("x-admin-token"),
        "source": "demo_server"
    }))
}

#[derive(Debug, Deserialize)]
struct SurveyReq {
    email: String,
    score: i64,
    #[serde(default)]
    consent: bool,
    #[serde(default)]
    comment: Option<String>,
}

/// 公开表单示例：email 正则、score 范围、consent 布尔。
async fn public_survey(headers: HeaderMap, body: Bytes) -> Response {
    let req: SurveyReq = match parse_body(&headers, &body) {
        Ok(req) => req,
        Err(msg) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "bad_request", "detail": msg })),
            )
                .into_response()
        }
    };

    Json(json!({
        "ok": true,
        "email": req.email,
        "score": req.score,
        "consent": req.consent,
        "comment": req.comment,
        "source": "demo_server"
    }))
    .into_response()
}

/// 分组级 path_params 示例：{org} 由组规则校验。
async fn org_profile(Path(org): Path<String>) -> Json<Value> {
    Json(json!({ "ok": true, "org": org, "name": "org-demo", "source": "demo_server" }))
}

/// filters 示例：query 命中 SQLi/XSS 时网关直接 400。
async fn filter_search(RawQuery(query): RawQuery) -> Json<Value> {
    Json(json!({ "ok": true, "query": query, "source": "demo_server" }))
}

/// filters 示例：body 字符串命中 XSS/SQLi；数字不扫描。
async fn filter_comment(headers: HeaderMap, body: Bytes) -> Json<Value> {
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("-");
    Json(json!({
        "ok": true,
        "content_type": content_type,
        "body": String::from_utf8_lossy(&body),
        "source": "demo_server"
    }))
}

/// filters 示例：路径参数使用原始未解码值扫描。
async fn filter_item(Path(name): Path<String>) -> Json<Value> {
    Json(json!({ "ok": true, "name": name, "source": "demo_server" }))
}

/// filters 示例：只扫描显式声明的 header。
async fn filter_header(headers: HeaderMap) -> Json<Value> {
    Json(json!({
        "ok": true,
        "note": headers.get("x-filter-note").and_then(|value| value.to_str().ok()),
        "has_other": headers.contains_key("x-other"),
        "source": "demo_server"
    }))
}

/// 路由级 path_regex 示例。
async fn route_regex(Path(id): Path<String>) -> Json<Value> {
    Json(json!({ "ok": true, "id": id, "regex": true, "source": "demo_server" }))
}

/// priority 示例：低优先级静态路由。
async fn priority_static() -> Json<Value> {
    Json(json!({ "ok": true, "matched": "static", "source": "demo_server" }))
}

/// priority 示例：高优先级参数路由。
async fn priority_param(Path(id): Path<String>) -> Json<Value> {
    Json(json!({ "ok": true, "matched": "param", "id": id, "source": "demo_server" }))
}

/// 只在 v2 契约里声明：绑定 v1 网关时应在网关层 404，而不是打到这里。
async fn metrics() -> Json<Value> {
    Json(json!({ "qps": 42, "p99_ms": 7, "source": "demo_server" }))
}

/// 只在 v2 契约里声明：用于验证「v1 先判 → 404 且 X-Gateway-Name 指向 v1」。
async fn audit() -> Json<Value> {
    Json(json!({ "events": [], "source": "demo_server" }))
}

/// v2 的动态路由 `/api/reports/{year}`：用于验证 v2 的路径参数匹配。
async fn report(Path(year): Path<String>) -> Json<Value> {
    Json(json!({ "year": year, "report": "annual", "source": "demo_server" }))
}

/// 同时解析 JSON 与 x-www-form-urlencoded，供登录/反馈/问卷等示例复用。
fn parse_body<T: DeserializeOwned>(headers: &HeaderMap, body: &Bytes) -> Result<T, String> {
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");

    if content_type.starts_with("application/json") {
        serde_json::from_slice(body).map_err(|error| format!("invalid json: {error}"))
    } else if content_type.starts_with("application/x-www-form-urlencoded") {
        serde_urlencoded::from_bytes(body).map_err(|error| format!("invalid form: {error}"))
    } else {
        Err(format!("unsupported content-type: {content_type:?}"))
    }
}

#[derive(Debug, Deserialize)]
struct LoginReq {
    username: String,
    password: String,
    // 可选参数：阶段 5 会把这些写进契约做白名单校验
    #[serde(default)]
    remember: Option<bool>,
    #[serde(default)]
    otp: Option<String>,
    #[serde(default)]
    redirect: Option<String>,
}

/// 同时支持 JSON 与 x-www-form-urlencoded，方便 curl 测两种 Body。
async fn login(headers: HeaderMap, body: Bytes) -> Response {
    let req = match parse_body::<LoginReq>(&headers, &body) {
        Ok(req) => req,
        Err(msg) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "bad_request", "detail": msg })),
            )
                .into_response()
        }
    };

    println!(
        "[demo_server] login attempt user={:?} remember={:?}",
        req.username, req.remember
    );

    if req.username == "demo" && req.password == "demo123" {
        (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "token": "demo-token-0001",
                "username": req.username,
                "remember": req.remember.unwrap_or(false),
                "redirect": req.redirect,
                "otp_checked": req.otp.is_some()
            })),
        )
            .into_response()
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "invalid_credentials" })),
        )
            .into_response()
    }
}

async fn get_user(Path(id): Path<String>) -> Json<Value> {
    Json(json!({ "id": id, "name": "user-demo", "role": "member" }))
}

async fn get_profile(Path(id): Path<String>) -> Json<Value> {
    Json(json!({ "id": id, "bio": "hello from demo_server", "lang": "zh-CN" }))
}

async fn put_profile(Path(id): Path<String>, body: Bytes) -> Json<Value> {
    Json(json!({ "id": id, "updated": true, "bytes": body.len() }))
}

async fn products() -> Json<Value> {
    Json(json!({
        "items": [
            { "id": 1, "name": "keyboard", "price": 199 },
            { "id": 2, "name": "mouse", "price": 99 }
        ]
    }))
}

async fn create_order(body: Bytes) -> Response {
    let payload: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (
        StatusCode::CREATED,
        Json(json!({ "order_id": "ord-1001", "received": payload })),
    )
        .into_response()
}

async fn echo(headers: HeaderMap, body: Bytes) -> Json<Value> {
    let ct = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    Json(json!({
        "content_type": ct,
        "bytes": body.len(),
        "body": String::from_utf8_lossy(&body)
    }))
}

/// 不在契约里的管理接口：阶段 4 应由网关直接 404。
async fn admin_users() -> Json<Value> {
    Json(json!({ "leak": "should not reach here after phase 4" }))
}

/// 模拟被扫描器探测的漏洞文件：后端确实会返回 200，靠网关拦。
async fn phpinfo_stub() -> &'static str {
    "phpinfo() stub: if you see this, the gateway did NOT block it"
}

async fn fallback(req: Request) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "not_found", "path": req.uri().path() })),
    )
        .into_response()
}
