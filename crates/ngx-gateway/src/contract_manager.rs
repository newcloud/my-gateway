//! 统一契约管理器（单点获取 → 共享内存分发）。
//!
//! - **获取**：`ContractSource` 抽象文件 / HTTP（未来 etcd），只由一个进程执行；
//! - **校验**：复用 `gateway_core::Gateway::parse`，适配层不重复实现规则；
//! - **分发**：把通过的原始契约写进 `ngx_shm_zone`（见 [`crate::contract_shm`]），
//!   其余 worker 只读；
//! - **容灾**：首次加载失败 → 不发布快照（请求 503 fail-closed），后台每 2s 重试；
//!   已成功的快照一直保留；
//! - **更新**：不做周期轮询（没有 TTL）；URL 契约在启动/接管时拉一次，后续由主动通知触发
//!   （当前是 `nginx -s reload`）。
//!
//! 为什么不是 master 线程：nginx 在 `ngx_init_cycle`（zone 初始化）之后才 `daemon()`，
//! 只有调用线程能活下来，master 里起的线程会被 fork 掉。于是采用 **leader worker**：
//! 共享内存里放 `leader` pid，谁抢到谁负责拉取；leader 退出后心跳超时由其它 worker 接管。
//! 对上层而言仍然是「单点获取」，worker 侧只有 leader 会发网络请求。
//!
//! 相关设计见 `docs/CONTRACT_UNIFICATION_DESIGN.md`。

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use ngx::ffi::NGX_LOG_NOTICE;
use ngx::ngx_log_error;

use crate::contract_shm::{self, publish};
use crate::contract_source::{ContractSource, FileSource, HttpSource};

/// 一个网关的契约来源。配置期已经保证「两者有且只有一个」，
/// 这里用枚举把非法状态排除掉。
#[derive(Clone)]
pub enum GatewaySourceConfig {
    File {
        name: String,
        path: String,
    },
    Url {
        name: String,
        url: String,
        allow_localhost_ca: bool,
    },
}

struct ManagedSource {
    name: String,
    /// 只用于日志：`file` / `url`。
    kind: &'static str,
    source: Box<dyn ContractSource>,
}

static CFGS: Mutex<Vec<GatewaySourceConfig>> = Mutex::new(Vec::new());

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// 后台线程不能持有请求日志，统一写进程级 error_log。
fn log_notice(msg: &str) {
    unsafe {
        let cycle = ngx::ffi::ngx_cycle;
        if cycle.is_null() {
            eprintln!("my_gateway: {msg}");
            return;
        }
        ngx_log_error!(NGX_LOG_NOTICE, (*cycle).log, "gateway: {}", msg);
    }
}

pub fn set_configs(cfgs: Vec<GatewaySourceConfig>) {
    *lock(&CFGS) = cfgs;
}

/// worker 启动时调用一次。
///
/// 抢到 leader 的 worker 会在**服务请求之前**同步完成首次加载（worker 之间通过共享内存
/// 的 pid 选举，只有一个会成功），保证其它 worker 一开始就能读到快照。
pub fn worker_bootstrap() {
    if !contract_shm::is_attached() {
        return;
    }
    if !contract_shm::try_claim_leader() {
        return;
    }
    let epoch = contract_shm::config_epoch();
    let mut sources = build_sources(lock(&CFGS).clone());
    let loaded = load_all(&mut sources);
    publish_snapshot(&loaded);
    start_manager_loop(sources, epoch, loaded);
}

/// 请求路径调用：leader 挂掉后由其它 worker 接管。
pub fn worker_tick() {
    if !contract_shm::is_attached() || contract_shm::is_leader() {
        return;
    }
    if !contract_shm::leader_stale() {
        return;
    }
    // leader 已死但 pid 还留着：必须 CAS 旧的 pid，不能只抢 0。
    if !contract_shm::try_take_over(contract_shm::leader_pid()) {
        return;
    }
    let epoch = contract_shm::config_epoch();
    let sources = build_sources(lock(&CFGS).clone());
    // 接管走后台线程，避免第一个请求等一次网络拉取。
    let spawned = thread::Builder::new()
        .name("my-gateway-contract-manager".to_string())
        .spawn(move || {
            let mut sources = sources;
            let loaded = load_all(&mut sources);
            publish_snapshot(&loaded);
            manager_loop(sources, epoch, loaded);
        });
    if spawned.is_err() {
        contract_shm::release_leader();
        log_notice("contract manager thread spawn failed");
    }
}

fn build_sources(configs: Vec<GatewaySourceConfig>) -> Vec<ManagedSource> {
    configs
        .into_iter()
        .map(|cfg| match cfg {
            GatewaySourceConfig::File { name, path } => ManagedSource {
                name,
                kind: "file",
                source: Box::new(FileSource::new(path)),
            },
            GatewaySourceConfig::Url {
                name,
                url,
                allow_localhost_ca,
            } => ManagedSource {
                name,
                kind: "url",
                source: Box::new(HttpSource::new(url, allow_localhost_ca)),
            },
        })
        .collect()
}

fn load_one(source: &mut dyn ContractSource) -> Result<(gateway_core::Gateway, Vec<u8>), String> {
    let raw = source.fetch().map_err(|e| e.to_string())?;
    let body = String::from_utf8_lossy(&raw.bytes).to_string();
    let gateway = gateway_core::Gateway::parse(&body).map_err(|e| e.to_string())?;
    Ok((gateway, raw.bytes))
}

fn describe(name: &str, kind: &str, gateway: &gateway_core::Gateway) -> String {
    format!(
        "[{}] contract loaded from {} protocol={} version={} routes={} unmatched={}",
        name,
        kind,
        gateway.protocol(),
        gateway.version().unwrap_or("-"),
        gateway.route_count(),
        if gateway.forward_unmatched() {
            "forward"
        } else {
            "deny"
        }
    )
}

/// 同一网关只有状态变化时才写日志，避免失败重试时刷屏。
fn log_if_changed(last: &mut BTreeMap<String, String>, name: &str, message: String) {
    if last.get(name) != Some(&message) {
        log_notice(&message);
        last.insert(name.to_string(), message);
    }
}

fn publish_snapshot(raw_by_name: &BTreeMap<String, Vec<u8>>) {
    let entries: Vec<(String, Vec<u8>)> = raw_by_name
        .iter()
        .map(|(name, raw)| (name.clone(), raw.clone()))
        .collect();
    publish(&entries, now_secs());
}

/// 首次加载：成功的进快照；失败的留空 → 该网关请求一律 503（fail-closed）。
fn load_all(sources: &mut [ManagedSource]) -> BTreeMap<String, Vec<u8>> {
    let mut raw_by_name = BTreeMap::new();
    for source in sources.iter_mut() {
        match load_one(source.source.as_mut()) {
            Ok((gateway, raw)) => {
                log_notice(&describe(&source.name, source.kind, &gateway));
                raw_by_name.insert(source.name.clone(), raw);
            }
            Err(err) => {
                log_notice(&format!(
                    "[{}] contract refresh failed: {} (no snapshot yet, requests fail-closed)",
                    source.name, err
                ));
            }
        }
    }
    log_notice(&format!(
        "contract manager: {} gateway(s) configured ({} loaded)",
        sources.len(),
        raw_by_name.len()
    ));
    contract_shm::heartbeat_now();
    raw_by_name
}

fn start_manager_loop(sources: Vec<ManagedSource>, epoch: u64, loaded: BTreeMap<String, Vec<u8>>) {
    let spawned = thread::Builder::new()
        .name("my-gateway-contract-manager".to_string())
        .spawn(move || manager_loop(sources, epoch, loaded));
    if spawned.is_err() {
        contract_shm::release_leader();
        log_notice("contract manager thread spawn failed");
    }
}

/// 首次加载失败的来源按固定间隔重试；**成功过的来源不再轮询**。
///
/// 契约更新走主动通知（当前是 `nginx -s reload` 触发新配置周期；专用刷新接口待实现），
/// 不再有 `gateway_url_ttl` 这类周期性拉取。重试只用于把 503 的网关救回来。
const FAILED_RETRY_INTERVAL: Duration = Duration::from_secs(2);

fn manager_loop(
    mut sources: Vec<ManagedSource>,
    epoch: u64,
    mut raw_by_name: BTreeMap<String, Vec<u8>>,
) {
    let mut last_message: BTreeMap<String, String> = BTreeMap::new();
    let mut last_attempt: BTreeMap<String, Instant> = BTreeMap::new();

    loop {
        // reload 后旧 worker 的 leader 必须让位给新配置周期。
        if contract_shm::config_epoch() != epoch {
            contract_shm::release_leader();
            break;
        }
        contract_shm::heartbeat_now();

        let mut changed = false;
        for source in sources.iter_mut() {
            // 已经成功加载的网关不再自动拉取，避免把“主动通知”变回 TTL 轮询。
            if raw_by_name.contains_key(&source.name) {
                continue;
            }
            let due = last_attempt
                .get(&source.name)
                .map(|at| at.elapsed() >= FAILED_RETRY_INTERVAL)
                .unwrap_or(true);
            if !due {
                continue;
            }
            last_attempt.insert(source.name.clone(), Instant::now());

            match load_one(source.source.as_mut()) {
                Ok((gateway, raw)) => {
                    log_if_changed(
                        &mut last_message,
                        &source.name,
                        describe(&source.name, source.kind, &gateway),
                    );
                    raw_by_name.insert(source.name.clone(), raw);
                    changed = true;
                }
                Err(err) => {
                    log_if_changed(
                        &mut last_message,
                        &source.name,
                        format!(
                            "[{}] contract refresh failed: {} (no snapshot yet, requests fail-closed)",
                            source.name, err
                        ),
                    );
                }
            }
        }

        if changed {
            publish_snapshot(&raw_by_name);
        }

        thread::sleep(Duration::from_secs(1));
    }
}
