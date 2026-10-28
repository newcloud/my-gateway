//! my_gateway —— nginx 适配层（阶段 1.5：契约真正生效）。
//!
//! 职责边界：
//! - 本文件只负责「取配置 → 组装 RequestCtx → 调 gateway-core → 写响应」；
//! - 路由/方法判定全部在 `gateway-core`，可用 `cargo test` 覆盖；
//! - 契约由**唯一的 leader worker**（`contract_manager`）在启动/接管时拉取或读文件，
//!   校验后写进共享内存（`contract_shm`）；其余 worker 只读；
//! - URL 契约**不按 TTL 轮询**：后续更新由主动通知触发（当前是 `nginx -s reload`，
//!   专用刷新接口尚未实现）；首次拉取失败会在后台按固定间隔重试，避免永久 503；
//! - 文件契约在**配置加载时**读一次（与 `nginx.conf` 同样的读取时机，改文件要 `nginx -s reload`）；
//! - access 阶段只读内存快照，不做网络/磁盘 I/O（请求体除外：只在契约声明了 body 规则时读）。
//!
//! 配置形态（http 级定义多个具名网关，location 按名字绑定）：
//! ```nginx
//! http {
//!     gateway_name      myrustwebdemo1;
//!     gateway_info_url  myrustwebdemo1 http://127.0.0.1:9000/info;
//!
//!     gateway_name      myrustwebdemo2;
//!     gateway_info_file myrustwebdemo2 conf/contracts/info_v2.json;  # 改完要 reload
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
//! 每个网关必须且只能有 **一个** 契约来源：`gateway_info_url`（HTTP 拉取）或
//! `gateway_info_file`（本地文件）。文件模式是为了"规则不能由被保护的后端改写"：
//! URL 模式下后端一旦被拿下，攻击者可以改自己的 `/info` 返回值，等于改网关规则；
//! 文件模式把规则放在 nginx 侧、由发布流程写入，后端进程不应有写权限。
//! 文件读不到或 JSON 非法时**拒绝本次配置**（`nginx -t`/reload 失败，旧 worker 继续跑），
//! 不会带着空规则上线。详见 `docs/PROTOCOL.md` 第 1.1 / 1.2 节。
//!
//! 判定顺序：日志 → 每个绑定的网关依次判定（404/405/503）→ 放行。
//! 一个 location 绑定多个网关时是「与」语义：任一网关拒绝即拒绝。
//! 契约已配置但尚未加载成功时返回 503（fail-closed），不会误放行。

use core::ffi::{c_char, c_void};
use core::ptr;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

use gateway_core::{BodyInput, Decision, Gateway, RequestCtx};
use ngx::core::Buffer;
use ngx::core::{Status, NGX_CONF_ERROR, NGX_CONF_OK};
use ngx::ffi::ngx_http_core_run_phases;
use ngx::ffi::ngx_http_finalize_request;
use ngx::ffi::{
    ngx_chain_t, ngx_command_t, ngx_conf_t, ngx_cycle_t, ngx_http_module_t,
    ngx_http_read_client_request_body, ngx_http_request_t, ngx_int_t, ngx_module_t,
    ngx_shared_memory_add, ngx_shm_zone_t, ngx_str_t, NGX_CONF_TAKE1, NGX_CONF_TAKE2,
    NGX_CONF_TAKE3, NGX_DECLINED, NGX_DONE, NGX_HTTP_LOC_CONF, NGX_HTTP_LOC_CONF_OFFSET,
    NGX_HTTP_MAIN_CONF, NGX_HTTP_MODULE, NGX_HTTP_SPECIAL_RESPONSE, NGX_LOG_EMERG, NGX_LOG_NOTICE,
    NGX_LOG_WARN,
};
use ngx::http::{
    self, HTTPStatus, HttpModule, HttpModuleLocationConf, HttpRequestHandler, MergeConfigError,
};
use ngx::{ngx_conf_log_error, ngx_log_error, ngx_string};

mod contract_manager;
mod contract_shm;
mod contract_source;
use contract_manager::{set_configs, worker_bootstrap, GatewaySourceConfig};

// ---------------------------------------------------------------------------
// 全局状态
//
// 配置解析期（master）用 DEFS / USED_GATEWAYS；运行期每个 worker 各有一份
// SNAPSHOTS，由共享内存的 generation 变化驱动更新（见 contract_shm.rs）。
// ---------------------------------------------------------------------------

/// 一条网关定义，对应 http 块里的一组 `gateway_name` / `gateway_info_url`|`gateway_info_file`。
#[derive(Clone, Debug)]
struct GatewayDef {
    name: String,
    /// 契约来源；`None` 表示配置还没给（postconfiguration 会拒绝）。
    source: Option<InfoSource>,
    /// 仅 URL 来源有效：`gateway_info_url <name> <url> allow-localhost-ca` 时关闭证书校验。
    /// 只允许 `https://localhost` / `127.0.0.1` / `[::1]`，用于本地自签测试。
    allow_localhost_ca: bool,
}

/// 契约来源。
///
/// - `Url`：插件主动拉取，适合契约由服务自己生成（内容随服务版本变化）。
/// - `File`：读本地文件，适合**不允许后端决定网关规则**的场景。后端被入侵时，
///   攻击者只能改自己进程能写的东西；契约文件由发布流程/运维写入，后端无写权限，
///   因此动态规则不会被远程改写。
#[derive(Clone, Debug)]
enum InfoSource {
    Url(String),
    File(String),
}

impl InfoSource {
    fn command(&self) -> &'static str {
        match self {
            InfoSource::Url(_) => "gateway_info_url",
            InfoSource::File(_) => "gateway_info_file",
        }
    }
}

/// http 块解析期间收集的定义；`preconfiguration` 每个配置周期重置一次。
static DEFS: Mutex<Vec<GatewayDef>> = Mutex::new(Vec::new());
/// 被至少一个 location 绑定的网关名。未被引用的定义不拉契约，避免无谓流量。
static USED_GATEWAYS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
/// 各网关当前生效的契约快照。key 不存在 = 该网关尚未加载成功（fail-closed）。
/// 配置期由文件来源填充一次（fork 给 worker），运行期由 leader 发布的 shm 快照覆盖。
/// 读多写少，用 RwLock 足够；后续压测有争用再评估更细粒度方案。
pub(crate) static SNAPSHOTS: RwLock<BTreeMap<String, Arc<Gateway>>> = RwLock::new(BTreeMap::new());

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn read<T>(l: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    l.read().unwrap_or_else(|e| e.into_inner())
}

fn write<T>(l: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    l.write().unwrap_or_else(|e| e.into_inner())
}

/// 网关名会出现在配置里和响应头 `X-Gateway-Name` 上，限制成安全字符。
fn valid_gateway_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// gateway_diagnostics 每个配置周期只允许声明一次
static DIAG_SEEN: AtomicBool = AtomicBool::new(false);
static DIAG_ENABLED: AtomicBool = AtomicBool::new(false);

/// preconfiguration：每个配置周期开始时重置全部全局状态。
///
/// **必须清 DEFS / USED_GATEWAYS / SNAPSHOTS**：reload 会在同一个 master 进程里重新解析
/// nginx.conf，不清就会把上一次的 `gateway_name` 当成重复声明而整份拒绝。
unsafe extern "C" fn ngx_http_my_gateway_preconfiguration(_cf: *mut ngx_conf_t) -> ngx_int_t {
    lock(&DEFS).clear();
    lock(&USED_GATEWAYS).clear();
    write(&SNAPSHOTS).clear();
    DIAG_SEEN.store(false, Ordering::SeqCst);
    DIAG_ENABLED.store(false, Ordering::SeqCst);
    Status::NGX_OK.into()
}

// ---------------------------------------------------------------------------
// 模块定义
// ---------------------------------------------------------------------------

struct Module;

impl http::HttpModule for Module {
    fn module() -> &'static ngx_module_t {
        unsafe { &*ptr::addr_of!(ngx_http_my_gateway_module) }
    }

    unsafe extern "C" fn postconfiguration(cf: *mut ngx_conf_t) -> ngx_int_t {
        // 此时 http 块已解析完：每个网关必须有且只有一个契约来源，否则拒绝启动。
        let defs = lock(&DEFS);
        // 只让被 location 引用的网关进入 ContractManager：声明了但没绑定的网关
        // 不发 HTTP 请求，避免无谓流量（文件来源仍会在配置期校验语法）。
        let used = lock(&USED_GATEWAYS).clone();
        let mut manager_cfgs = Vec::new();
        for def in defs.iter() {
            let referenced = used.contains(&def.name);
            match &def.source {
                None => {
                    ngx_conf_log_error!(
                        NGX_LOG_EMERG,
                        cf,
                        "gateway \"{}\" needs a contract source: add `gateway_info_url {} <url>` \
                         or `gateway_info_file {} <path>`",
                        def.name,
                        def.name,
                        def.name
                    );
                    return Status::NGX_ERROR.into();
                }
                Some(InfoSource::Url(url)) => {
                    if referenced {
                        manager_cfgs.push(GatewaySourceConfig::Url {
                            name: def.name.clone(),
                            url: url.clone(),
                            allow_localhost_ca: def.allow_localhost_ca,
                        });
                    }
                }
                Some(InfoSource::File(path)) => {
                    // 保持原有行为：文件来源在配置加载时也要能读到，否则拒绝启动
                    let body = match std::fs::read_to_string(path) {
                        Ok(body) => body,
                        Err(err) => {
                            ngx_conf_log_error!(
                                NGX_LOG_EMERG,
                                cf,
                                "gateway \"{}\" cannot read contract file \"{}\": {}",
                                def.name,
                                path,
                                err
                            );
                            return Status::NGX_ERROR.into();
                        }
                    };

                    match Gateway::parse(&body) {
                        Ok(gateway) => {
                            ngx_conf_log_error!(
                                NGX_LOG_NOTICE,
                                cf,
                                "gateway \"{}\" contract loaded from file protocol={} version={} \
                                 routes={} unmatched={}",
                                def.name,
                                gateway.protocol(),
                                gateway.version().unwrap_or("-"),
                                gateway.route_count(),
                                if gateway.forward_unmatched() {
                                    "forward"
                                } else {
                                    "deny"
                                }
                            );
                            if referenced {
                                write(&SNAPSHOTS).insert(def.name.clone(), Arc::new(gateway));
                            }
                        }
                        Err(err) => {
                            ngx_conf_log_error!(
                                NGX_LOG_EMERG,
                                cf,
                                "gateway \"{}\" contract file \"{}\" rejected: {}",
                                def.name,
                                path,
                                err
                            );
                            return Status::NGX_ERROR.into();
                        }
                    }

                    if referenced {
                        manager_cfgs.push(GatewaySourceConfig::File {
                            name: def.name.clone(),
                            path: path.clone(),
                        });
                    }
                }
            }
        }
        drop(defs);

        set_configs(manager_cfgs);

        // 共享内存快照区：leader worker 写、其余 worker 只读（见 contract_shm.rs）。
        // zone 在 fork 之前注册；真正的选主与首次加载在 worker 的 init_process 里执行。
        if unsafe { add_contract_shm_zone(cf) }.is_err() {
            return Status::NGX_ERROR.into();
        }

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

static mut NGX_HTTP_MY_GATEWAY_COMMANDS: [ngx_command_t; 6] = [
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
        type_: NGX_HTTP_MAIN_CONF | NGX_CONF_TAKE2 | NGX_CONF_TAKE3,
        set: Some(ngx_http_my_gateway_set_info_url),
        conf: 0,
        offset: 0,
        post: ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("gateway_info_file"),
        type_: NGX_HTTP_MAIN_CONF | NGX_CONF_TAKE2,
        set: Some(ngx_http_my_gateway_set_info_file),
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
    ngx_command_t {
        name: ngx_string!("gateway_diagnostics"),
        type_: NGX_HTTP_MAIN_CONF | NGX_CONF_TAKE1,
        set: Some(ngx_http_my_gateway_set_diagnostics),
        conf: 0,
        offset: 0,
        post: ptr::null_mut(),
    },
    ngx_command_t::empty(),
];

/// 契约快照共享内存区大小：足够放下若干份完整契约（每份几十 KB）。
const CONTRACT_SHM_SIZE: usize = 1024 * 1024;

/// 每个 worker 启动一次：抢到 leader 的 worker 负责首次加载（见 contract_manager）。
unsafe extern "C" fn ngx_http_my_gateway_init_process(_cycle: *mut ngx_cycle_t) -> ngx_int_t {
    worker_bootstrap();
    Status::NGX_OK.into()
}

/// 注册 `ngx_shm_zone`：单点写、其余 worker 只读。
unsafe fn add_contract_shm_zone(cf: *mut ngx_conf_t) -> Result<(), ()> {
    static ZONE_NAME: &[u8] = b"my_gateway_contracts";
    let mut name = ngx_str_t {
        len: ZONE_NAME.len(),
        data: ZONE_NAME.as_ptr() as *mut u8,
    };
    let tag = ptr::addr_of_mut!(ngx_http_my_gateway_module).cast();
    let zone = unsafe { ngx_shared_memory_add(cf, &mut name, CONTRACT_SHM_SIZE, tag) };
    if zone.is_null() {
        return Err(());
    }
    // SAFETY: zone 由 nginx 在本次配置周期内保持有效
    unsafe { (*zone).init = Some(contract_shm_init) };
    Ok(())
}

/// zone 初始化回调：在 master、fork 之前执行一次；只记录映射地址并推进 `config_epoch`，
/// 不在这里读契约（此时还没有 worker，见 contract_manager.rs 的 leader 选举）。
unsafe extern "C" fn contract_shm_init(zone: *mut ngx_shm_zone_t, _data: *mut c_void) -> ngx_int_t {
    let zone = unsafe { &*zone };
    unsafe { contract_shm::attach(zone.shm.addr as usize, zone.shm.size) };
    Status::NGX_OK.into()
}

static NGX_HTTP_MY_GATEWAY_MODULE_CTX: ngx_http_module_t = ngx_http_module_t {
    preconfiguration: Some(ngx_http_my_gateway_preconfiguration),
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

        // worker 只读：leader 写了新快照才重新解析（每个 generation 一次）。
        contract_shm::refresh_local();
        contract_manager::worker_tick();

        // 每个请求一个状态槽：rid 只生成/回写一次（读 body 会让 handler 重入），
        // 同时记录 body 读取进度。
        let state: *mut GatewayReqCtx =
            match request.get_module_ctx::<GatewayReqCtx>(Module::module()) {
                Some(state) => state as *const GatewayReqCtx as *mut GatewayReqCtx,
                None => {
                    let state = request.pool().allocate(GatewayReqCtx {
                        status: NGX_DECLINED,
                        header_added: false,
                        rid: make_request_id(),
                    });
                    request.set_module_ctx(state.cast(), Module::module());
                    state
                }
            };
        if unsafe { (*state).status } == NGX_DONE {
            return Status(NGX_DONE);
        }
        // X-Request-Id：即使诊断头全关也必须返回，用来把客户端看到的错误页和 error_log 对上。
        if !unsafe { (*state).header_added } {
            request.add_header_out("X-Request-Id", unsafe { &(*state).rid });
            unsafe { (*state).header_added = true };
        }
        let rid = unsafe { (*state).rid.clone() };

        let method = request.method();
        let uri_storage = request
            .unparsed_uri()
            .to_str()
            .unwrap_or("<non-utf8>")
            .to_string();
        let uri = uri_storage.as_str();
        // 判定用**原始** request target 的 path，而不是 nginx 归一化后的 `$uri`：
        // nginx 默认 `merge_slashes on`，会把 `//api/health` 归一成 `/api/health`，但
        // `proxy_pass` 不带 URI 时上游收到的仍是原始的 `//api/health`。若按归一化结果判定，
        // 就会出现"网关放行 /api/health、后端处理 //api/health"的不一致（路径混淆风险）。
        let path = raw_path(uri);

        ngx_log_error!(
            NGX_LOG_NOTICE,
            request.log(),
            "gateway: {} {} (path={}, via={})",
            method,
            uri,
            path,
            co.gateways.join(",")
        );

        // 契约判定：绑定的每个网关都要放行（与语义），先命中先返回。
        //
        // 请求头按 nginx 收到的**行**逐条传给 core（同名多行会传多次，重复检测靠它）；
        // query 传原始串（不解码、不合并），与后端实际收到的保持一致。
        let header_pairs: Vec<(String, Option<String>)> = request
            .headers_in_iterator()
            .map(|(name, value)| (name.to_string(), Some(value.to_string())))
            .collect();
        let headers: Vec<(&str, Option<&str>)> = header_pairs
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_deref()))
            .collect();
        let raw_query = uri.split_once('?').map(|(_, query)| query);

        let ctx = RequestCtx {
            method: method.as_ref(),
            path,
        };

        let r: *mut ngx_http_request_t = request.into();

        // 只有契约里真的声明了 body 规则的路由才读请求体（GET/静态资源不碰）。
        let needs_body = {
            let snapshots = read(&SNAPSHOTS);
            co.gateways
                .iter()
                .any(|name| snapshots.get(name).is_some_and(|gw| gw.needs_body(&ctx)))
        };

        if needs_body {
            // Content-Length 已经超限：不必读完整个 body，直接 413。
            {
                let declared = unsafe { (*r).headers_in.content_length_n };
                if declared > 0 {
                    for name in &co.gateways {
                        let gateway = read(&SNAPSHOTS).get(name).cloned();
                        let Some(gateway) = gateway else { continue };
                        if let Some(max) = gateway.body_max_bytes(&ctx) {
                            if declared as u64 > max {
                                return reject_body_too_large(
                                    request, name, &gateway, &rid, &method, uri, max,
                                );
                            }
                        }
                    }
                }
            }

            if unsafe { (*r).request_body.is_null() } {
                unsafe { (*state).status = NGX_DONE };
                let rc = unsafe { ngx_http_read_client_request_body(r, Some(after_body_read)) };
                if rc >= NGX_HTTP_SPECIAL_RESPONSE {
                    return Status(rc);
                }
                // `read_client_request_body` 会增加一次引用计数，这里释放掉但保持请求存活。
                unsafe { ngx_http_finalize_request(r, NGX_DONE) };
                return Status(NGX_DONE);
            }
        }

        // 读完之后再判一次尺寸：chunked 没有 Content-Length，只能读到这里才知道。
        let body_bytes = if needs_body {
            let bytes = unsafe { collect_body(r) }.unwrap_or_default();
            for name in &co.gateways {
                let gateway = read(&SNAPSHOTS).get(name).cloned();
                let Some(gateway) = gateway else { continue };
                if let Some(max) = gateway.body_max_bytes(&ctx) {
                    if bytes.len() as u64 > max {
                        return reject_body_too_large(
                            request, name, &gateway, &rid, &method, uri, max,
                        );
                    }
                }
            }
            bytes
        } else {
            Vec::new()
        };

        let body_input = BodyInput {
            content_type: header_value(&headers, "content-type"),
            content_encoding: header_value(&headers, "content-encoding"),
            bytes: &body_bytes,
        };

        for name in &co.gateways {
            let snapshot = read(&SNAPSHOTS).get(name).cloned();
            let Some(gateway) = snapshot else {
                ngx_log_error!(
                    NGX_LOG_NOTICE,
                    request.log(),
                    "gateway: contract not ready gateway={}, rejecting {} {} rid={}",
                    name,
                    method,
                    uri,
                    rid
                );
                if DIAG_ENABLED.load(Ordering::Acquire) {
                    request.add_header_out("X-Gateway-Name", name);
                    request.add_header_out("X-Gateway-Reason", "contract-not-ready");
                }
                return HTTPStatus::SERVICE_UNAVAILABLE.into();
            };

            let outcome = if needs_body && gateway.needs_body(&ctx) {
                gateway.check_with_body(&ctx, &headers, raw_query, &body_input)
            } else {
                gateway.check_without_body(&ctx, &headers, raw_query)
            };
            match outcome.decision {
                Decision::Allow => {
                    if outcome.forwarded {
                        // 分组提供 `unmatched: "forward"` 时带上作用域前缀，便于排障。
                        let scope_note = match outcome.scope {
                            Some(scope) => format!(", scope={scope}"),
                            None => String::new(),
                        };
                        ngx_log_error!(
                            NGX_LOG_NOTICE,
                            request.log(),
                            "gateway: forward {} {} (route-not-declared, unmatched=forward{}) \
                             gateway={}",
                            method,
                            uri,
                            scope_note,
                            name
                        );
                    }
                }
                Decision::RouteNotDeclared => {
                    ngx_log_error!(
                        NGX_LOG_NOTICE,
                        request.log(),
                        "gateway: 404 {} {} reason=route-not-declared gateway={} rid={}",
                        method,
                        uri,
                        name,
                        rid
                    );
                    if DIAG_ENABLED.load(Ordering::Acquire) {
                        request.add_header_out("X-Gateway-Name", name);
                        request.add_header_out("X-Gateway-Reason", outcome.decision.reason());
                        if let Some(field) = outcome.field.as_deref() {
                            request.add_header_out("X-Gateway-Field", field);
                        }
                    }
                    return deny_with_override(request, &gateway, outcome.decision);
                }
                Decision::MethodNotAllowed => {
                    ngx_log_error!(
                        NGX_LOG_NOTICE,
                        request.log(),
                        "gateway: 405 {} {} allow={} gateway={} rid={}",
                        method,
                        uri,
                        outcome.allow.unwrap_or("-"),
                        name,
                        rid
                    );
                    if let Some(allow) = outcome.allow {
                        request.add_header_out("Allow", allow);
                    }
                    if DIAG_ENABLED.load(Ordering::Acquire) {
                        request.add_header_out("X-Gateway-Name", name);
                        request.add_header_out("X-Gateway-Reason", outcome.decision.reason());
                        if let Some(field) = outcome.field.as_deref() {
                            request.add_header_out("X-Gateway-Field", field);
                        }
                    }
                    return deny_with_override(request, &gateway, outcome.decision);
                }
                Decision::MissingParameter
                | Decision::InvalidParameter
                | Decision::UnsupportedMediaType => {
                    let status = outcome.decision.status_code().unwrap_or(400);
                    // 协议 §5.6：过滤命中额外带 `filter=sqli|xss`（宏展开是语句，不能直接当 match 分支）。
                    let filter_note = match outcome.filter {
                        Some(kind) => format!(" filter={kind}"),
                        None => String::new(),
                    };
                    ngx_log_error!(
                        NGX_LOG_NOTICE,
                        request.log(),
                        "gateway: {} {} {} reason={} field={} rule={}{} gateway={} rid={}",
                        status,
                        method,
                        uri,
                        outcome.decision.reason(),
                        outcome.field.as_deref().unwrap_or("-"),
                        outcome.rule.unwrap_or("-"),
                        filter_note,
                        name,
                        rid
                    );
                    if DIAG_ENABLED.load(Ordering::Acquire) {
                        request.add_header_out("X-Gateway-Name", name);
                        request.add_header_out("X-Gateway-Reason", outcome.decision.reason());
                        if let Some(field) = outcome.field.as_deref() {
                            request.add_header_out("X-Gateway-Field", field);
                        }
                    }
                    return deny_with_override(request, &gateway, outcome.decision);
                }
            }
        }

        Status::NGX_DECLINED
    }
}

/// 请求级状态：access 阶段读 body 时用来避免重入重复读取。
struct GatewayReqCtx {
    /// `NGX_DONE` = 还在等请求体；`NGX_DECLINED` = body 已就绪，可以继续判定。
    status: ngx_int_t,
    /// `X-Request-Id` 是否已经回写（handler 会因读 body 重入）。
    header_added: bool,
    /// 本次请求的 id，日志与响应头共用。
    rid: String,
}

/// 请求体读取完成后的回调：恢复 phase 处理，让 access handler 重入。
unsafe extern "C" fn after_body_read(r: *mut ngx_http_request_t) {
    unsafe {
        let module = &*Module::module();
        let slot = (*r).ctx.add(module.ctx_index);
        if !(*slot).is_null() {
            let state = (*slot).cast::<GatewayReqCtx>();
            (*state).status = NGX_DECLINED;
        }
        (*r).write_event_handler = Some(ngx_http_core_run_phases);
        ngx_http_core_run_phases(r);
    }
}

/// 取出请求体字节。body 落在临时文件里时（超过 client_body_buffer_size）读文件。
unsafe fn collect_body(r: *mut ngx_http_request_t) -> Option<Vec<u8>> {
    unsafe {
        let body = (*r).request_body;
        if body.is_null() {
            return Some(Vec::new());
        }

        // 超过 client_body_buffer_size 时 nginx 会把 body 落到临时文件（可能已经 unlink，
        // 但 fd 还在）。用 nginx 自己的 ngx_read_file 读，避免依赖文件路径。
        if !(*body).temp_file.is_null() {
            let temp = (*body).temp_file;
            // `received` 在这里还是 0，实际字节数看临时文件的写入偏移。
            let total = (*temp).file.offset;
            if total <= 0 {
                return Some(Vec::new());
            }
            let mut out = vec![0u8; total as usize];
            let mut offset: i64 = 0;
            while offset < total {
                let n = ngx::ffi::ngx_read_file(
                    &mut (*temp).file,
                    out.as_mut_ptr().add(offset as usize),
                    (total - offset) as usize,
                    offset,
                );
                if n <= 0 {
                    return None;
                }
                offset += n as i64;
            }
            return Some(out);
        }

        let mut out = Vec::new();
        let mut chain = (*body).bufs;
        while !chain.is_null() {
            let buf = (*chain).buf;
            if !buf.is_null() && !(*buf).pos.is_null() {
                let len = (*buf).last as usize - (*buf).pos as usize;
                if len > 0 {
                    out.extend_from_slice(std::slice::from_raw_parts((*buf).pos, len));
                }
            }
            chain = (*chain).next;
        }
        Some(out)
    }
}

/// 取某个请求头的值（大小写不敏感，同名取第一行）。
fn header_value<'a>(headers: &'a [(&'a str, Option<&'a str>)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .and_then(|(_, value)| *value)
}

/// 带契约响应覆盖的拒绝。
fn deny_with_override(
    request: &mut http::Request,
    gateway: &Gateway,
    decision: Decision,
) -> Status {
    match gateway.response_override(decision) {
        Some(resp) => send_override(request, resp.status, resp.retry_after, resp.body.as_deref()),
        None => denial_status(decision),
    }
}

/// 请求体超过 `body.max_bytes`：413，可用 `responses.body_too_large` 覆盖响应体。
fn reject_body_too_large(
    request: &mut http::Request,
    name: &str,
    gateway: &Gateway,
    rid: &str,
    method: &http::Method,
    uri: &str,
    limit: u64,
) -> Status {
    ngx_log_error!(
        NGX_LOG_NOTICE,
        request.log(),
        "gateway: 413 {} {} reason=body-too-large limit={} gateway={} rid={}",
        method,
        uri,
        limit,
        name,
        rid
    );
    if DIAG_ENABLED.load(Ordering::Acquire) {
        request.add_header_out("X-Gateway-Name", name);
        request.add_header_out("X-Gateway-Reason", "body-too-large");
    }
    match gateway.body_too_large_override() {
        Some(resp) => send_override(request, resp.status, resp.retry_after, resp.body.as_deref()),
        None => Status(413),
    }
}

/// 发送一份自定义响应（契约里的 `responses.*`）。
///
/// nginx 的 `output_filter` 需要一个 `ngx_chain_t`，这里手工拼一条单 buffer 链。
fn send_override(
    request: &mut http::Request,
    status: u16,
    retry_after: Option<u32>,
    body: Option<&str>,
) -> Status {
    request.set_status(HTTPStatus(status as usize));
    if let Some(secs) = retry_after {
        request.add_header_out("Retry-After", &secs.to_string());
    }

    let Some(body) = body else {
        request.set_content_length_n(0);
        return request.send_header();
    };

    request.add_header_out("Content-Type", "application/json");
    request.set_content_length_n(body.len());
    let rc = request.send_header();
    if rc != Status::NGX_OK || body.is_empty() || request.header_only() {
        return rc;
    }

    let Some(mut buffer) = request.pool().create_buffer_from_str(body) else {
        return Status::NGX_ERROR;
    };
    buffer.set_last_buf(true);
    buffer.set_last_in_chain(true);
    let mut chain = ngx_chain_t {
        buf: buffer.as_ngx_buf_mut(),
        next: ptr::null_mut(),
    };
    request.output_filter(&mut chain)
}

/// 每个请求一个 id，写进 `X-Request-Id` 与 error_log 的 `rid=`，方便按 id 串联排障。
fn make_request_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{:x}{:08x}", std::process::id(), seq)
}

/// 拒绝状态码由 `gateway-core` 统一定义，协议测试与线上共用同一份映射。
fn denial_status(decision: Decision) -> Status {
    match decision.status_code() {
        Some(code) => Status(code as ngx_int_t),
        None => HTTPStatus::INTERNAL_SERVER_ERROR.into(),
    }
}

/// 从原始 request target 取出用于判定的 path。
///
/// - 去掉 `?query`（query 不参与路由判定）；
/// - 兼容 absolute-form（`GET http://host/path HTTP/1.1`，代理场景）；
/// - 空 path 按 `/` 处理。
///
/// 注意：**不合并** `//`、**不做**百分号解码——网关要判定的就是上游会收到的那个 path。
/// nginx 的 `merge_slashes` 只影响 `$uri` 与 location 匹配，不影响这里。
fn raw_path(uri: &str) -> &str {
    let target = match uri.split_once("://") {
        Some((scheme, rest))
            if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") =>
        {
            rest.find('/').map_or("/", |idx| &rest[idx..])
        }
        _ => uri,
    };

    let path = target.split_once('?').map_or(target, |(path, _)| path);
    if path.is_empty() {
        "/"
    } else {
        path
    }
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
            ngx_conf_log_error!(NGX_LOG_EMERG, cf, "`gateway_name` value is not valid UTF-8");
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
            source: None,
            allow_localhost_ca: false,
        });
    }

    ngx::core::NGX_CONF_OK
}

/// `gateway_info_url <name> <url>;` —— 给已声明的网关指定契约地址（HTTP 拉取）。
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

        // 可选第三参数：仅 `allow-localhost-ca`，且只对本地 https 地址生效。
        let mut allow_localhost_ca = false;
        if let Some(flag) = args.get(3) {
            let Ok(flag) = flag.to_str() else {
                ngx_conf_log_error!(
                    NGX_LOG_EMERG,
                    cf,
                    "`gateway_info_url {}` third argument is not valid UTF-8",
                    name
                );
                return ngx::core::NGX_CONF_ERROR;
            };
            if flag != "allow-localhost-ca" {
                ngx_conf_log_error!(
                    NGX_LOG_EMERG,
                    cf,
                    "unknown option \"{}\" for `gateway_info_url {}`; \
                     only allow-localhost-ca is supported",
                    flag,
                    name
                );
                return ngx::core::NGX_CONF_ERROR;
            }
            if !is_local_https_url(url) {
                ngx_conf_log_error!(
                    NGX_LOG_EMERG,
                    cf,
                    "`gateway_info_url {}`: allow-localhost-ca only applies to \
                     https://localhost, https://127.0.0.1 or https://[::1], got \"{}\"",
                    name,
                    url
                );
                return ngx::core::NGX_CONF_ERROR;
            }
            allow_localhost_ca = true;
            ngx_conf_log_error!(
                NGX_LOG_WARN,
                cf,
                "gateway \"{}\" disables TLS certificate verification for local contract URL: {}",
                name,
                url
            );
        }

        if !store_source(
            cf,
            name,
            InfoSource::Url(url.to_string()),
            allow_localhost_ca,
        ) {
            return ngx::core::NGX_CONF_ERROR;
        }
    }

    ngx::core::NGX_CONF_OK
}

/// `gateway_info_file <name> <path>;` —— 给已声明的网关指定契约文件（本地读取）。
///
/// 相对路径按 nginx prefix（`-p`，默认编译时的 prefix）解析，便于把契约放在
/// `conf/contracts/` 下随配置一起发布。
extern "C" fn ngx_http_my_gateway_set_info_file(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    _conf: *mut c_void,
) -> *mut c_char {
    unsafe {
        let args: &[ngx_str_t] = (*(*cf).args).as_slice();

        let (Ok(name), Ok(path)) = (args[1].to_str(), args[2].to_str()) else {
            ngx_conf_log_error!(
                NGX_LOG_EMERG,
                cf,
                "`gateway_info_file` arguments are not valid UTF-8"
            );
            return ngx::core::NGX_CONF_ERROR;
        };

        if path.is_empty() || path.ends_with('/') {
            ngx_conf_log_error!(
                NGX_LOG_EMERG,
                cf,
                "`gateway_info_file {}` must be a file path, got \"{}\"",
                name,
                path
            );
            return ngx::core::NGX_CONF_ERROR;
        }

        let full = resolve_conf_path(cf, path);
        if !store_source(cf, name, InfoSource::File(full), false) {
            return ngx::core::NGX_CONF_ERROR;
        }
    }

    ngx::core::NGX_CONF_OK
}

/// 把契约来源挂到已声明的网关上；重复指定（url 与 file 同时出现也算）直接拒绝。
/// `allow-localhost-ca` 的适用范围：只放行 `https://localhost`、`https://127.0.0.1`、`https://[::1]`。
///
/// 这项能力是给本地自签证书联调用的；一旦允许任意主机就等于全局关掉 TLS 校验。
fn is_local_https_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = match authority.strip_prefix('[') {
        // IPv6 字面量：`[::1]:8443`
        Some(rest) => rest.split(']').next().unwrap_or(""),
        None => authority.split(':').next().unwrap_or(""),
    };
    let host = host.to_ascii_lowercase();
    matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1")
}

fn store_source(
    cf: *mut ngx_conf_t,
    name: &str,
    source: InfoSource,
    allow_localhost_ca: bool,
) -> bool {
    let mut defs = lock(&DEFS);
    let Some(def) = defs.iter_mut().find(|d| d.name == name) else {
        ngx_conf_log_error!(
            NGX_LOG_EMERG,
            cf,
            "unknown gateway \"{}\"; declare `gateway_name {}` before `{}`",
            name,
            name,
            source.command()
        );
        return false;
    };

    if let Some(existing) = &def.source {
        ngx_conf_log_error!(
            NGX_LOG_EMERG,
            cf,
            "gateway \"{}\" already has a contract source (`{}`); \
             use exactly one of `gateway_info_url` / `gateway_info_file`",
            name,
            existing.command()
        );
        return false;
    }

    def.source = Some(source);
    def.allow_localhost_ca = allow_localhost_ca;
    true
}

/// 相对路径按 nginx prefix（`-p`）展开，与 `error_log logs/...` 等指令的规则一致，
/// 不依赖进程 CWD。
fn resolve_conf_path(cf: *mut ngx_conf_t, path: &str) -> String {
    if path.starts_with('/') {
        return path.to_string();
    }
    unsafe {
        let prefix = (*(*cf).cycle).prefix;
        match prefix.to_str() {
            Ok(prefix) if !prefix.is_empty() => format!("{prefix}{path}"),
            _ => path.to_string(),
        }
    }
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
                "unknown gateway \"{}\"; declare `gateway_name {}` + \
                 (`gateway_info_url {} <url>` or `gateway_info_file {} <path>`) \
                 in the http block before this location",
                name,
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

extern "C" fn ngx_http_my_gateway_set_diagnostics(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    _conf: *mut c_void,
) -> *mut c_char {
    unsafe {
        let cf = &mut *cf;
        if DIAG_SEEN.swap(true, Ordering::SeqCst) {
            ngx_conf_log_error!(
                NGX_LOG_EMERG,
                cf,
                "duplicate `gateway_diagnostics`; declare it once in the http block"
            );
            return NGX_CONF_ERROR;
        }
        let args: &[ngx_str_t] = (*(*cf).args).as_slice();
        if args.len() < 2 {
            ngx_conf_log_error!(
                NGX_LOG_EMERG,
                cf,
                "`gateway_diagnostics` requires `on` or `off`"
            );
            return NGX_CONF_ERROR;
        }
        let val = match args[1].to_str() {
            Ok(v) => v,
            Err(_) => {
                ngx_conf_log_error!(
                    NGX_LOG_EMERG,
                    cf,
                    "`gateway_diagnostics` value is not valid UTF-8"
                );
                return NGX_CONF_ERROR;
            }
        };
        match val {
            "on" => {
                DIAG_ENABLED.store(true, Ordering::SeqCst);
                ngx_conf_log_error!(
                    NGX_LOG_WARN,
                    cf,
                    "gateway diagnostics enabled: `X-Gateway-Name` / `-Reason` / `-Field` will be sent to clients; use `gateway_diagnostics off` (default) in production"
                );
                NGX_CONF_OK
            }
            "off" => {
                DIAG_ENABLED.store(false, Ordering::SeqCst);
                NGX_CONF_OK
            }
            _ => {
                ngx_conf_log_error!(
                    NGX_LOG_EMERG,
                    cf,
                    "`gateway_diagnostics` must be `on` or `off`, got \"{}\"",
                    val
                );
                NGX_CONF_ERROR
            }
        }
    }
}
