//! my_gateway —— nginx 适配层（阶段 1.5：契约真正生效）。
//!
//! 职责边界：
//! - 本文件只负责「取配置 → 组装 RequestCtx → 调 gateway-core → 写响应」；
//! - 路由/方法判定全部在 `gateway-core`，可用 `cargo test` 覆盖；
//! - 契约由**后台线程**按 TTL 拉取，access 阶段只读内存快照，不做网络 I/O。
//!
//! 配置形态（http 级定义多个具名网关，location 按名字绑定）：
//! ```nginx
//! http {
//!     gateway_name      myrustwebdemo1;
//!     gateway_info_url  myrustwebdemo1 http://127.0.0.1:9000/info;
//!     gateway_ttl       myrustwebdemo1 3;      # 可选，默认 60
//!
//!     server {
//!         location / {
//!             enable_gateway_by myrustwebdemo1; # 不写 = 不做判定（透传）
//!             proxy_pass http://backend;
//!         }
//!     }
//! }
//! ```
//!
//! 判定顺序：日志 → 内置探针（403/503）→ 每个绑定的网关依次判定（404/405/503）→ 放行。
//! 一个 location 绑定多个网关时是「与」语义：任一网关拒绝即拒绝。
//! 契约已配置但尚未加载成功时返回 503（fail-closed），不会误放行。

use core::ffi::{c_char, c_void};
use core::ptr;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gateway_core::{Decision, Gateway, RequestCtx};
use ngx::core::Status;
use ngx::ffi::{
    NGX_CONF_TAKE1, NGX_CONF_TAKE2, NGX_HTTP_LOC_CONF, NGX_HTTP_LOC_CONF_OFFSET,
    NGX_HTTP_MAIN_CONF, NGX_HTTP_MODULE, NGX_LOG_EMERG, NGX_LOG_NOTICE, ngx_command_t, ngx_conf_t,
    ngx_cycle_t, ngx_http_module_t, ngx_int_t, ngx_module_t, ngx_str_t,
};
use ngx::http::{
    self, HTTPStatus, HttpModule, HttpModuleLocationConf, HttpRequestHandler, MergeConfigError,
};
use ngx::{ngx_conf_log_error, ngx_log_error, ngx_string};

// ---------------------------------------------------------------------------
// 全局状态（每个 worker 进程一份，fork 后互不影响）
// ---------------------------------------------------------------------------

/// 一条网关定义，对应 http 块里的一组 `gateway_name` / `gateway_info_url` / `gateway_ttl`。
#[derive(Clone, Debug)]
struct GatewayDef {
    name: String,
    info_url: Option<String>,
    /// 未写 `gateway_ttl` 时为 None，启动刷新线程时按默认 60s。
    ttl: Option<u64>,
}

/// http 块解析期间收集的定义；`preconfiguration` 每个配置周期重置一次。
static DEFS: Mutex<Vec<GatewayDef>> = Mutex::new(Vec::new());
/// 被至少一个 location 绑定的网关名。未被引用的定义不拉契约，避免无谓流量。
static USED_GATEWAYS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
/// 各网关当前生效的契约快照。key 不存在 = 该网关尚未加载成功（fail-closed）。
/// 读多写少，用 RwLock 足够；阶段 3 若压测有争用再换 ArcSwap。
static SNAPSHOTS: RwLock<BTreeMap<String, Arc<Gateway>>> = RwLock::new(BTreeMap::new());
/// 后台线程不能直接调 nginx logger，把消息排队，由请求线程写进 error_log。
static PENDING_LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// worker 退出时通知刷新线程收工，避免进程 teardown 与 detached 线程并发（关闭期偶发 SIGSEGV）。
static STOP_REFRESH: AtomicBool = AtomicBool::new(false);
static REFRESH_THREADS: Mutex<Vec<JoinHandle<()>>> = Mutex::new(Vec::new());

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn read<T>(l: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    l.read().unwrap_or_else(|e| e.into_inner())
}

fn write<T>(l: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    l.write().unwrap_or_else(|e| e.into_inner())
}

fn push_log(msg: String) {
    let mut pending = lock(&PENDING_LOG);
    if pending.len() < 16 {
        pending.push(msg);
    }
}

fn drain_logs(request: &http::Request) {
    let messages: Vec<String> = {
        let mut pending = lock(&PENDING_LOG);
        core::mem::take(&mut *pending)
    };
    for msg in messages {
        ngx_log_error!(NGX_LOG_NOTICE, request.log(), "gateway: {}", msg);
    }
}

/// 网关名会出现在配置里和响应头 `X-Gateway-Name` 上，限制成安全字符。
fn valid_gateway_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

// ---------------------------------------------------------------------------
// 契约刷新（每个网关一个独立线程，按各自 TTL）
// ---------------------------------------------------------------------------

fn refresh_loop(name: String, url: String, ttl: u64) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(3)))
        .build()
        .into();

    let mut last_message: Option<String> = None;

    while !STOP_REFRESH.load(Ordering::Relaxed) {
        let message = match fetch_contract(&agent, &url) {
            Ok(gateway) => {
                let msg = format!(
                    "[{name}] contract loaded protocol={} version={} routes={}",
                    gateway.protocol(),
                    gateway.version().unwrap_or("-"),
                    gateway.route_count()
                );
                write(&SNAPSHOTS).insert(name.clone(), Arc::new(gateway));
                msg
            }
            Err(err) => format!("[{name}] contract refresh failed: {err}"),
        };

        // 只在状态变化时记一条，避免 TTL 很小时刷屏
        if last_message.as_deref() != Some(message.as_str()) {
            push_log(message.clone());
            last_message = Some(message);
        }

        sleep_interruptible(ttl);
    }
}

/// 可被 worker 退出打断的 sleep：避免 `exit_process` 里 join 卡满一个 TTL。
fn sleep_interruptible(ttl: u64) {
    let deadline = Instant::now() + Duration::from_secs(ttl);
    while !STOP_REFRESH.load(Ordering::Relaxed) {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        std::thread::sleep((deadline - now).min(Duration::from_millis(50)));
    }
}

fn fetch_contract(agent: &ureq::Agent, url: &str) -> Result<Gateway, String> {
    let mut response = agent.get(url).call().map_err(|e| e.to_string())?;
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    Gateway::parse(&body).map_err(|e| e.to_string())
}

/// 每个 worker 进程启动时调用一次（master 不调用）：为每个被引用的网关起刷新线程。
unsafe extern "C" fn ngx_http_my_gateway_init_process(_cycle: *mut ngx_cycle_t) -> ngx_int_t {
    let used = lock(&USED_GATEWAYS).clone();
    let defs: Vec<GatewayDef> = lock(&DEFS)
        .iter()
        .filter(|d| used.contains(&d.name))
        .cloned()
        .collect();

    STOP_REFRESH.store(false, Ordering::SeqCst);
    let mut handles = Vec::with_capacity(defs.len());

    for def in defs {
        let Some(url) = def.info_url.clone() else {
            // postconfiguration 已拒绝这种配置，这里只是防御。
            push_log(format!("[{}] no gateway_info_url, skip", def.name));
            continue;
        };
        let name = def.name.clone();
        let ttl = def.ttl.unwrap_or(60).max(1);
        let thread_name = name.clone();
        let spawn = std::thread::Builder::new()
            .name(format!("my-gateway-{name}"))
            .spawn(move || refresh_loop(thread_name, url, ttl));

        match spawn {
            Ok(handle) => handles.push(handle),
            Err(err) => push_log(format!("[{name}] refresh thread spawn failed: {err}")),
        }
    }

    if !handles.is_empty() {
        push_log(format!("refresh started for {} gateway(s)", handles.len()));
    }
    *lock(&REFRESH_THREADS) = handles;

    Status::NGX_OK.into()
}

/// worker 退出时调用：先让所有刷新线程收工再返回，避免进程 teardown 与线程并发。
/// 注意 nginx 的 `exit_process` 钩子没有返回值（与 `init_process` 不同）。
unsafe extern "C" fn ngx_http_my_gateway_exit_process(_cycle: *mut ngx_cycle_t) {
    STOP_REFRESH.store(true, Ordering::SeqCst);
    for handle in lock(&REFRESH_THREADS).drain(..) {
        // 线程 sleep 是 50ms 粒度；最坏情况等一次 HTTP 超时（3s）。退出阶段不再写日志队列，
        // 因为没有请求线程来 drain 了。
        let _ = handle.join();
    }
}

// ---------------------------------------------------------------------------
// 模块定义
// ---------------------------------------------------------------------------

struct Module;

impl http::HttpModule for Module {
    fn module() -> &'static ngx_module_t {
        unsafe { &*ptr::addr_of!(ngx_http_my_gateway_module) }
    }

    /// 每个配置周期开始时重置全局状态（reload 后新 worker 必须重新拉契约）。
    unsafe extern "C" fn preconfiguration(_cf: *mut ngx_conf_t) -> ngx_int_t {
        lock(&DEFS).clear();
        lock(&USED_GATEWAYS).clear();
        write(&SNAPSHOTS).clear();
        lock(&PENDING_LOG).clear();
        Status::NGX_OK.into()
    }

    unsafe extern "C" fn postconfiguration(cf: *mut ngx_conf_t) -> ngx_int_t {
        // 此时 http 块已解析完：每个网关都必须有契约地址，否则拒绝启动。
        let defs = lock(&DEFS);
        for def in defs.iter() {
            if def.info_url.is_none() {
                ngx_conf_log_error!(
                    NGX_LOG_EMERG,
                    cf,
                    "gateway \"{}\" has no `gateway_info_url`",
                    def.name
                );
                return Status::NGX_ERROR.into();
            }
        }
        drop(defs);

        // SAFETY: nginx 调用本函数时 cf 一定非空
        let cf = unsafe { &mut *cf };
        http::add_phase_handler::<GatewayHandler>(cf)
            .map_or(Status::NGX_ERROR, |_| Status::NGX_OK)
            .into()
    }
}

/// location 级配置：绑定了哪些网关。一个 location 可绑定多个（与语义）。
#[derive(Debug, Default)]
struct ModuleConfig {
    gateways: Vec<String>,
}

unsafe impl HttpModuleLocationConf for Module {
    type LocationConf = ModuleConfig;
}

impl http::Merge for ModuleConfig {
    fn merge(&mut self, prev: &ModuleConfig) -> Result<(), MergeConfigError> {
        // 子 location 没写 enable_gateway_by 时继承父级的绑定。
        if self.gateways.is_empty() {
            self.gateways.clone_from(&prev.gateways);
        }
        Ok(())
    }
}

static mut NGX_HTTP_MY_GATEWAY_COMMANDS: [ngx_command_t; 5] = [
    ngx_command_t {
        name: ngx_string!("gateway_name"),
        type_: NGX_HTTP_MAIN_CONF | NGX_CONF_TAKE1,
        set: Some(ngx_http_my_gateway_set_name),
        conf: 0,
        offset: 0,
        post: ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("gateway_info_url"),
        type_: NGX_HTTP_MAIN_CONF | NGX_CONF_TAKE2,
        set: Some(ngx_http_my_gateway_set_info_url),
        conf: 0,
        offset: 0,
        post: ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("gateway_ttl"),
        type_: NGX_HTTP_MAIN_CONF | NGX_CONF_TAKE2,
        set: Some(ngx_http_my_gateway_set_ttl),
        conf: 0,
        offset: 0,
        post: ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("enable_gateway_by"),
        type_: NGX_HTTP_LOC_CONF | NGX_CONF_TAKE1,
        set: Some(ngx_http_my_gateway_set_enable_by),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: ptr::null_mut(),
    },
    ngx_command_t::empty(),
];

static NGX_HTTP_MY_GATEWAY_MODULE_CTX: ngx_http_module_t = ngx_http_module_t {
    preconfiguration: Some(Module::preconfiguration),
    postconfiguration: Some(Module::postconfiguration),
    create_main_conf: None,
    init_main_conf: None,
    create_srv_conf: None,
    merge_srv_conf: None,
    create_loc_conf: Some(Module::create_loc_conf),
    merge_loc_conf: Some(Module::merge_loc_conf),
};

#[cfg(feature = "export-modules")]
ngx::ngx_modules!(ngx_http_my_gateway_module);

#[used]
#[allow(non_upper_case_globals)]
#[cfg_attr(not(feature = "export-modules"), unsafe(no_mangle))]
pub static mut ngx_http_my_gateway_module: ngx_module_t = ngx_module_t {
    ctx: &raw const NGX_HTTP_MY_GATEWAY_MODULE_CTX as _,
    commands: unsafe { &raw mut NGX_HTTP_MY_GATEWAY_COMMANDS[0] },
    type_: NGX_HTTP_MODULE,
    init_process: Some(ngx_http_my_gateway_init_process),
    exit_process: Some(ngx_http_my_gateway_exit_process),
    ..ngx_module_t::default()
};

// ---------------------------------------------------------------------------
// access 处理器
// ---------------------------------------------------------------------------

struct GatewayHandler;

impl HttpRequestHandler for GatewayHandler {
    const PHASE: http::HttpPhase = http::HttpPhase::Access;
    type Output = Status;

    fn handler(request: &mut http::Request) -> Self::Output {
        let Some(co) = Module::location_conf(request) else {
            return Status::NGX_DECLINED;
        };
        if co.gateways.is_empty() {
            return Status::NGX_DECLINED;
        }

        drain_logs(request);

        let method = request.method();
        let uri = request.unparsed_uri().to_str().unwrap_or("<non-utf8>");
        let path = request.path().to_str().unwrap_or("/");

        ngx_log_error!(
            NGX_LOG_NOTICE,
            request.log(),
            "gateway: {} {} (path={}, via={})",
            method,
            uri,
            path,
            co.gateways.join(",")
        );

        // 第一层：硬编码探针（演示用，阶段 4 之后会被契约完全取代）
        if let Some(reason) = probe_block_reason(path) {
            ngx_log_error!(
                NGX_LOG_NOTICE,
                request.log(),
                "gateway: blocked {} {} reason={}",
                method,
                uri,
                reason
            );
            request.add_header_out("X-Gateway-Blocked", reason);
            return HTTPStatus::FORBIDDEN.into();
        }

        if path == "/__gateway/deny" {
            request.add_header_out("X-Gateway-Blocked", "demo-deny");
            return HTTPStatus::SERVICE_UNAVAILABLE.into();
        }

        // 第二层：契约。绑定的每个网关都要放行（与语义），先命中先返回。
        let ctx = RequestCtx {
            method: method.as_ref(),
            path,
        };

        for name in &co.gateways {
            let snapshot = read(&SNAPSHOTS).get(name).cloned();
            let Some(gateway) = snapshot else {
                ngx_log_error!(
                    NGX_LOG_NOTICE,
                    request.log(),
                    "gateway: contract not ready gateway={}, rejecting {} {}",
                    name,
                    method,
                    uri
                );
                request.add_header_out("X-Gateway-Name", name);
                request.add_header_out("X-Gateway-Reason", "contract-not-ready");
                return HTTPStatus::SERVICE_UNAVAILABLE.into();
            };

            let outcome = gateway.check(&ctx);
            match outcome.decision {
                Decision::Allow => {}
                Decision::RouteNotDeclared => {
                    ngx_log_error!(
                        NGX_LOG_NOTICE,
                        request.log(),
                        "gateway: 404 {} {} reason=route-not-declared gateway={}",
                        method,
                        uri,
                        name
                    );
                    request.add_header_out("X-Gateway-Name", name);
                    request.add_header_out("X-Gateway-Reason", outcome.decision.reason());
                    return HTTPStatus::NOT_FOUND.into();
                }
                Decision::MethodNotAllowed => {
                    ngx_log_error!(
                        NGX_LOG_NOTICE,
                        request.log(),
                        "gateway: 405 {} {} allow={} gateway={}",
                        method,
                        uri,
                        outcome.allow.unwrap_or("-"),
                        name
                    );
                    if let Some(allow) = outcome.allow {
                        request.add_header_out("Allow", allow);
                    }
                    request.add_header_out("X-Gateway-Name", name);
                    request.add_header_out("X-Gateway-Reason", outcome.decision.reason());
                    return HTTPStatus::NOT_ALLOWED.into();
                }
            }
        }

        Status::NGX_DECLINED
    }
}

/// 阶段 1 遗留的内置探针规则：只用于演示"未到后端就被拦"。
fn probe_block_reason(path: &str) -> Option<&'static str> {
    let lower = path.to_ascii_lowercase();

    if lower.ends_with(".php") {
        return Some("php-suffix");
    }
    if lower == "/.env" || lower.starts_with("/.git") {
        return Some("vcs-leak");
    }
    if lower.starts_with("/wp-admin") || lower.starts_with("/wp-login") {
        return Some("wordpress-scan");
    }
    None
}

// ---------------------------------------------------------------------------
// 指令解析
// ---------------------------------------------------------------------------

/// `gateway_name <name>;` —— 声明一个具名网关，必须在 http 块、且先于其他 gateway_* 指令。
extern "C" fn ngx_http_my_gateway_set_name(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    _conf: *mut c_void,
) -> *mut c_char {
    unsafe {
        let args: &[ngx_str_t] = (*(*cf).args).as_slice();

        let Ok(name) = args[1].to_str() else {
            ngx_conf_log_error!(
                NGX_LOG_EMERG,
                cf,
                "`gateway_name` value is not valid UTF-8"
            );
            return ngx::core::NGX_CONF_ERROR;
        };

        if !valid_gateway_name(name) {
            ngx_conf_log_error!(
                NGX_LOG_EMERG,
                cf,
                "invalid gateway name \"{}\" (allowed: A-Z a-z 0-9 _ - .; max 64 chars)",
                name
            );
            return ngx::core::NGX_CONF_ERROR;
        }

        let mut defs = lock(&DEFS);
        if defs.iter().any(|d| d.name == name) {
            ngx_conf_log_error!(NGX_LOG_EMERG, cf, "duplicate `gateway_name {}`", name);
            return ngx::core::NGX_CONF_ERROR;
        }
        defs.push(GatewayDef {
            name: name.to_string(),
            info_url: None,
            ttl: None,
        });
    }

    ngx::core::NGX_CONF_OK
}

/// `gateway_info_url <name> <url>;` —— 给已声明的网关指定契约地址。
extern "C" fn ngx_http_my_gateway_set_info_url(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    _conf: *mut c_void,
) -> *mut c_char {
    unsafe {
        let args: &[ngx_str_t] = (*(*cf).args).as_slice();

        let (Ok(name), Ok(url)) = (args[1].to_str(), args[2].to_str()) else {
            ngx_conf_log_error!(
                NGX_LOG_EMERG,
                cf,
                "`gateway_info_url` arguments are not valid UTF-8"
            );
            return ngx::core::NGX_CONF_ERROR;
        };

        if !(url.starts_with("http://") || url.starts_with("https://")) {
            ngx_conf_log_error!(
                NGX_LOG_EMERG,
                cf,
                "`gateway_info_url {}` must start with http:// or https://, got \"{}\"",
                name,
                url
            );
            return ngx::core::NGX_CONF_ERROR;
        }

        let mut defs = lock(&DEFS);
        let Some(def) = defs.iter_mut().find(|d| d.name == name) else {
            ngx_conf_log_error!(
                NGX_LOG_EMERG,
                cf,
                "unknown gateway \"{}\"; declare `gateway_name {}` before `gateway_info_url`",
                name,
                name
            );
            return ngx::core::NGX_CONF_ERROR;
        };

        if def.info_url.is_some() {
            ngx_conf_log_error!(
                NGX_LOG_EMERG,
                cf,
                "duplicate `gateway_info_url {}`",
                name
            );
            return ngx::core::NGX_CONF_ERROR;
        }

        def.info_url = Some(url.to_string());
    }

    ngx::core::NGX_CONF_OK
}

/// `gateway_ttl <name> <seconds>;` —— 可选，默认 60。
extern "C" fn ngx_http_my_gateway_set_ttl(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    _conf: *mut c_void,
) -> *mut c_char {
    unsafe {
        let args: &[ngx_str_t] = (*(*cf).args).as_slice();

        let (Ok(name), Ok(val)) = (args[1].to_str(), args[2].to_str()) else {
            ngx_conf_log_error!(
                NGX_LOG_EMERG,
                cf,
                "`gateway_ttl` arguments are not valid UTF-8"
            );
            return ngx::core::NGX_CONF_ERROR;
        };

        let ttl = match val.parse::<u64>() {
            Ok(ttl) if (1..=86_400).contains(&ttl) => ttl,
            _ => {
                ngx_conf_log_error!(
                    NGX_LOG_EMERG,
                    cf,
                    "`gateway_ttl {}` must be an integer in 1..=86400, got \"{}\"",
                    name,
                    val
                );
                return ngx::core::NGX_CONF_ERROR;
            }
        };

        let mut defs = lock(&DEFS);
        let Some(def) = defs.iter_mut().find(|d| d.name == name) else {
            ngx_conf_log_error!(
                NGX_LOG_EMERG,
                cf,
                "unknown gateway \"{}\"; declare `gateway_name {}` first",
                name,
                name
            );
            return ngx::core::NGX_CONF_ERROR;
        };

        if def.ttl.is_some() {
            ngx_conf_log_error!(NGX_LOG_EMERG, cf, "duplicate `gateway_ttl {}`", name);
            return ngx::core::NGX_CONF_ERROR;
        }

        def.ttl = Some(ttl);
    }

    ngx::core::NGX_CONF_OK
}

/// `enable_gateway_by <name>;` —— location 级，可写多条（与语义）。
extern "C" fn ngx_http_my_gateway_set_enable_by(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut c_void,
) -> *mut c_char {
    unsafe {
        // SAFETY: nginx 保证 conf 指向本模块的 ModuleConfig
        let conf = &mut *(conf as *mut ModuleConfig);
        let args: &[ngx_str_t] = (*(*cf).args).as_slice();

        let Ok(name) = args[1].to_str() else {
            ngx_conf_log_error!(
                NGX_LOG_EMERG,
                cf,
                "`enable_gateway_by` value is not valid UTF-8"
            );
            return ngx::core::NGX_CONF_ERROR;
        };

        // 定义必须写在 location 之前（通常放在 http 块开头），这样拼错能立刻被 nginx -t 拦住。
        if !lock(&DEFS).iter().any(|d| d.name == name) {
            ngx_conf_log_error!(
                NGX_LOG_EMERG,
                cf,
                "unknown gateway \"{}\"; declare `gateway_name {}` + `gateway_info_url {} <url>` \
                 in the http block before this location",
                name,
                name,
                name
            );
            return ngx::core::NGX_CONF_ERROR;
        }

        if conf.gateways.iter().any(|g| g == name) {
            ngx_conf_log_error!(
                NGX_LOG_EMERG,
                cf,
                "duplicate `enable_gateway_by {}` in the same location",
                name
            );
            return ngx::core::NGX_CONF_ERROR;
        }

        conf.gateways.push(name.to_string());
        lock(&USED_GATEWAYS).insert(name.to_string());
    }

    ngx::core::NGX_CONF_OK
}
