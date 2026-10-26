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
    extract::{Path, Request},
    http::{header::CONTENT_TYPE, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
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
/// `protocol` 决定解析规则，`version` 只是内容修订号，`ttl` 是建议值（被 nginx `gateway_ttl` 覆盖）。
async fn info() -> Json<Value> {
    Json(json!({
        "protocol": 1,
        "version": "phase1-fixed",
        "ttl": 60,
        "meta": { "owner": "demo_server", "generated_by": "handwritten" },
        "routes": [
            { "path": "/info",                     "methods": ["GET"] },
            { "path": "/api/health",               "methods": ["GET"] },
            { "path": "/api/login",                "methods": ["POST"] },
            { "path": "/api/user/{id}",            "methods": ["GET"] },
            { "path": "/api/user/{id}/profile",    "methods": ["GET", "PUT"] },
            { "path": "/api/products",             "methods": ["GET"] },
            { "path": "/api/orders",               "methods": ["POST"] },
            { "path": "/api/echo",                 "methods": ["POST"] }
        ]
    }))
}

/// 第二份契约：**protocol 2**，指向不同地址，用来验证多网关独立快照与未声明路由策略。
///
/// 与 `/info`（protocol 1，默认 `deny`）的差异：
/// - **只在 v2**：`/info_v2`、`/api/metrics`、`/api/reports/{year}`
/// - **只在 v1**：`/api/login`、`/api/user/{id}`、`/api/user/{id}/profile`
/// - **两边都有**：`/info`、`/api/health`、`/api/products`、`/api/orders`、`/api/echo`
/// - **策略不同**：v2 写了 `"unmatched": "forward"`，契约没声明的路径交给后端处理（v1 是 404）
async fn info_v2() -> Json<Value> {
    Json(json!({
        "protocol": 2,
        "unmatched": "forward",
        "version": "phase1-v2",
        "ttl": 30,
        "meta": { "owner": "demo_server", "generated_by": "handwritten", "contract": "v2" },
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
    let ct = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let parsed: Result<LoginReq, String> = if ct.starts_with("application/json") {
        serde_json::from_slice(&body).map_err(|e| format!("invalid json: {e}"))
    } else if ct.starts_with("application/x-www-form-urlencoded") {
        serde_urlencoded::from_bytes(&body).map_err(|e| format!("invalid form: {e}"))
    } else {
        Err(format!("unsupported content-type: {ct:?}"))
    };

    let req = match parsed {
        Ok(req) => req,
        Err(msg) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "bad_request", "detail": msg })),
            )
                .into_response()
        }
    };

    println!("[demo_server] login attempt user={:?} remember={:?}", req.username, req.remember);

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
