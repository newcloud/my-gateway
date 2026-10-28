//! 契约快照共享内存（`ngx_shm_zone`）。
//!
//! 只有一个进程负责写入（leader worker，见 [`crate::contract_manager`]），其余 worker
//! 只做 mmap 读取，不再各自拉取/轮询。布局：
//!
//! ```text
//! offset 0  magic:u32 = "GWCT"
//! offset 4  payload_len:u32
//! offset 8  generation:u64   （奇数 = 正在写入；偶数 = 可读；0 = 还没写过）
//! offset 16 heartbeat:u64    （leader 约每秒刷新；用于发现 leader 已退出）
//! offset 24 leader:u64       （leader 的 pid；0 = 无 leader）
//! offset 32 config_epoch:u64 （reload 时 +1，旧 leader 看到变化就收工）
//! offset 40 reserved:u64
//! offset 48 payload…
//! ```
//!
//! payload 是若干条记录拼接：`name_len:u16 | name | body_len:u32 | body`（小端），
//! `body` 是原始契约文本。worker 端在 generation 变化时才重新 `Gateway::parse`，
//! 这样只拉一次、每个 worker 各解析一次，而不是各拉一次网络。
//!
//! 写读并发用 seqlock：写入前把 generation 改成奇数，写完再改成下一个偶数；
//! 读方发现是奇数或读到的 generation 前后不一致就放弃本次读取（下个请求重试）。

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use gateway_core::Gateway;

const MAGIC: u32 = 0x5443_5747; // "GWCT" (little endian)
const HEADER_LEN: usize = 48;

// 头部字段偏移
const OFF_LEN: usize = 4;
const OFF_GENERATION: usize = 8;
const OFF_HEARTBEAT: usize = 16;
const OFF_LEADER: usize = 24;
const OFF_EPOCH: usize = 32;

/// 共享内存区起始地址；0 表示当前配置周期没有启用共享快照（例如 `nginx -t`）。
pub static SHM_ADDR: AtomicUsize = AtomicUsize::new(0);
pub static SHM_SIZE: AtomicUsize = AtomicUsize::new(0);

/// worker 本地缓存的 generation，避免每个请求都反序列化。
static CACHED_GENERATION: AtomicU64 = AtomicU64::new(0);
/// leader 本地上一次写心跳的秒数；刷新循环是 100ms 粒度，节流到每秒一次。
static LAST_HEARTBEAT_SEC: AtomicU64 = AtomicU64::new(0);

fn atomic_u64(addr: usize, off: usize) -> &'static AtomicU64 {
    unsafe { AtomicU64::from_ptr((addr + off) as *mut u64) }
}

/// 由 zone 初始化回调调用：记录映射地址。
///
/// 同一个 zone 在 reload 时会被 nginx 复用（内存内容还在）：这时保留已有快照，
/// 只把 `config_epoch` 加一、清掉 leader，让新配置周期的 worker 重新选主并全量加载。
///
/// # Safety
/// `addr` 必须是 nginx 传入的、大小至少为 `size` 的共享内存起始地址。
pub unsafe fn attach(addr: usize, size: usize) {
    if addr == 0 || size <= HEADER_LEN {
        SHM_ADDR.store(0, Ordering::Release);
        SHM_SIZE.store(0, Ordering::Release);
        return;
    }

    let magic = unsafe { std::ptr::read_volatile(addr as *const u32) };
    if magic == MAGIC {
        // reload：沿用快照，换一代配置
        let epoch = atomic_u64(addr, OFF_EPOCH).load(Ordering::Acquire);
        atomic_u64(addr, OFF_EPOCH).store(epoch.wrapping_add(1), Ordering::Release);
        atomic_u64(addr, OFF_LEADER).store(0, Ordering::Release);
    } else {
        unsafe {
            std::ptr::write_volatile(addr as *mut u32, MAGIC);
            std::ptr::write_volatile((addr + OFF_LEN) as *mut u32, 0);
        }
        atomic_u64(addr, OFF_GENERATION).store(0, Ordering::Release);
        atomic_u64(addr, OFF_HEARTBEAT).store(0, Ordering::Release);
        atomic_u64(addr, OFF_LEADER).store(0, Ordering::Release);
        atomic_u64(addr, OFF_EPOCH).store(1, Ordering::Release);
    }

    SHM_SIZE.store(size, Ordering::Release);
    SHM_ADDR.store(addr, Ordering::Release);
    // 新配置周期的 worker 从共享内存重新装一次快照（master fork 前的值没有意义）。
    CACHED_GENERATION.store(0, Ordering::Release);
}

pub fn is_attached() -> bool {
    SHM_ADDR.load(Ordering::Acquire) != 0
}

fn addr_and_size() -> Option<(usize, usize)> {
    let addr = SHM_ADDR.load(Ordering::Acquire);
    let size = SHM_SIZE.load(Ordering::Acquire);
    if addr == 0 || size <= HEADER_LEN {
        None
    } else {
        Some((addr, size))
    }
}

/// 当前配置周期号（reload 时变化）。
pub fn config_epoch() -> u64 {
    addr_and_size()
        .map(|(addr, _)| atomic_u64(addr, OFF_EPOCH).load(Ordering::Acquire))
        .unwrap_or(0)
}

pub fn heartbeat_now() {
    if let Some((addr, _)) = addr_and_size() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // 刷新循环 100ms 一轮，但心跳只需要秒级精度（失效判定是 10s）。
        if LAST_HEARTBEAT_SEC.swap(now, Ordering::Relaxed) == now {
            return;
        }
        atomic_u64(addr, OFF_HEARTBEAT).store(now, Ordering::Release);
    }
}

/// 尝试成为 leader。已经有人当 leader 时返回 false。
pub fn try_claim_leader() -> bool {
    let Some((addr, _)) = addr_and_size() else {
        return false;
    };
    let me = std::process::id() as u64;
    let leader = atomic_u64(addr, OFF_LEADER);
    match leader.compare_exchange(0, me, Ordering::SeqCst, Ordering::SeqCst) {
        Ok(_) => true,
        Err(current) => current == me,
    }
}

/// 放弃 leader（后台循环起不来、或配置周期结束时调用）。
///
/// 只有当前 `leader == 自己` 时才清空，避免误伤新周期已经选出的 leader。
pub fn release_leader() {
    let Some((addr, _)) = addr_and_size() else {
        return;
    };
    let me = std::process::id() as u64;
    let leader = atomic_u64(addr, OFF_LEADER);
    let _ = leader.compare_exchange(me, 0, Ordering::SeqCst, Ordering::SeqCst);
}

pub fn is_leader() -> bool {
    let Some((addr, _)) = addr_and_size() else {
        return false;
    };
    atomic_u64(addr, OFF_LEADER).load(Ordering::Acquire) == std::process::id() as u64
}

/// 当前 leader 的 pid；`0` = 没有 leader。
pub fn leader_pid() -> u64 {
    addr_and_size()
        .map(|(addr, _)| atomic_u64(addr, OFF_LEADER).load(Ordering::Acquire))
        .unwrap_or(0)
}

/// leader 失效后接管：把 `old`（失效的 pid）CAS 成自己。
///
/// 不能复用 `try_claim_leader`（它只接受 `0`），否则 leader 进程已经死掉、
/// 但 pid 还留在共享内存里时，其它 worker 永远抢不到。
pub fn try_take_over(old: u64) -> bool {
    let Some((addr, _)) = addr_and_size() else {
        return false;
    };
    let me = std::process::id() as u64;
    if old == me {
        return true;
    }
    atomic_u64(addr, OFF_LEADER)
        .compare_exchange(old, me, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}

/// leader 心跳过旧（或进程已经不存在）时返回 true，用于让其它 worker 接管。
pub fn leader_stale() -> bool {
    let Some((addr, _)) = addr_and_size() else {
        return false;
    };
    let leader = atomic_u64(addr, OFF_LEADER).load(Ordering::Acquire);
    if leader == 0 {
        return true;
    }
    let heartbeat = atomic_u64(addr, OFF_HEARTBEAT).load(Ordering::Acquire);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if heartbeat != 0 && now.saturating_sub(heartbeat) <= LEADER_TIMEOUT_SECS {
        return false;
    }
    // 心跳停了：再看进程是否还在（容器里 /proc 一定存在；读不到就保守地认为还活着）
    !std::path::Path::new(&format!("/proc/{leader}")).exists()
}

/// leader 心跳超时时间。ContractManager 的循环粒度是 1s，这个值只用于失败接管。
const LEADER_TIMEOUT_SECS: u64 = 10;

/// leader worker 发布一份新快照。返回是否真的写入（内容没变时跳过）。
pub fn publish(entries: &[(String, Vec<u8>)], now: u64) -> bool {
    let Some((addr, size)) = addr_and_size() else {
        return false;
    };

    let mut payload = Vec::new();
    for (name, body) in entries {
        if name.len() > u16::MAX as usize || body.len() > u32::MAX as usize {
            continue;
        }
        payload.extend_from_slice(&(name.len() as u16).to_le_bytes());
        payload.extend_from_slice(name.as_bytes());
        payload.extend_from_slice(&(body.len() as u32).to_le_bytes());
        payload.extend_from_slice(body);
    }

    if payload.len() + HEADER_LEN > size {
        return false;
    }

    // 内容没变就不发布：worker 端也不会白白重新解析。
    if let Some((_, current)) = read_raw(addr, size) {
        if current == payload {
            return false;
        }
    }

    unsafe {
        let generation = atomic_u64(addr, OFF_GENERATION).load(Ordering::Acquire);
        // 奇数 = 写入中
        atomic_u64(addr, OFF_GENERATION).store(generation.wrapping_add(1) | 1, Ordering::Release);

        std::ptr::copy_nonoverlapping(
            payload.as_ptr(),
            (addr + HEADER_LEN) as *mut u8,
            payload.len(),
        );
        std::ptr::write_volatile((addr + OFF_LEN) as *mut u32, payload.len() as u32);
        std::ptr::write_volatile(addr as *mut u32, MAGIC);

        // 偶数 = 可读，同时充当发布序号
        atomic_u64(addr, OFF_GENERATION).store(generation.wrapping_add(2) & !1, Ordering::Release);
        atomic_u64(addr, OFF_HEARTBEAT).store(now, Ordering::Release);
        LAST_HEARTBEAT_SEC.store(now, Ordering::Relaxed);
    }
    true
}

fn read_raw(addr: usize, size: usize) -> Option<(u64, Vec<u8>)> {
    for _ in 0..4 {
        let generation = atomic_u64(addr, OFF_GENERATION).load(Ordering::Acquire);
        if generation == 0 || generation & 1 == 1 {
            return None;
        }
        if unsafe { std::ptr::read_volatile(addr as *const u32) } != MAGIC {
            return None;
        }
        let len = unsafe { std::ptr::read_volatile((addr + OFF_LEN) as *const u32) } as usize;
        if len == 0 || len + HEADER_LEN > size {
            return None;
        }

        let mut payload = vec![0u8; len];
        unsafe {
            std::ptr::copy_nonoverlapping(
                (addr + HEADER_LEN) as *const u8,
                payload.as_mut_ptr(),
                len,
            );
        }

        let check = atomic_u64(addr, OFF_GENERATION).load(Ordering::Acquire);
        if check == generation {
            return Some((generation, payload));
        }
    }
    None
}

/// worker 侧：把一份新快照解析进本地 `SNAPSHOTS`。
fn install(generation: u64, payload: &[u8]) -> bool {
    if CACHED_GENERATION.load(Ordering::Acquire) == generation {
        return false;
    }

    let mut parsed = Vec::new();
    let mut cursor = 0usize;
    while cursor + 2 <= payload.len() {
        let name_len = u16::from_le_bytes([payload[cursor], payload[cursor + 1]]) as usize;
        cursor += 2;
        if cursor + name_len + 4 > payload.len() {
            break;
        }
        let Ok(name) = std::str::from_utf8(&payload[cursor..cursor + name_len]) else {
            break;
        };
        cursor += name_len;
        let body_len = u32::from_le_bytes([
            payload[cursor],
            payload[cursor + 1],
            payload[cursor + 2],
            payload[cursor + 3],
        ]) as usize;
        cursor += 4;
        if cursor + body_len > payload.len() {
            break;
        }
        let body = &payload[cursor..cursor + body_len];
        cursor += body_len;
        if let Ok(gateway) = Gateway::parse(&String::from_utf8_lossy(body)) {
            parsed.push((name.to_string(), std::sync::Arc::new(gateway)));
        }
    }

    {
        let mut snapshots = crate::SNAPSHOTS.write().unwrap_or_else(|e| e.into_inner());
        snapshots.clear();
        snapshots.extend(parsed);
    }
    CACHED_GENERATION.store(generation, Ordering::Release);
    true
}

/// 读当前发布序号；`0` = 还没有可读快照。只做一次原子读，供请求路径快速跳过。
fn current_generation(addr: usize) -> u64 {
    atomic_u64(addr, OFF_GENERATION).load(Ordering::Acquire)
}

/// 请求路径调用：有新快照就更新本地缓存。
///
/// 先读一次 `generation`，与本进程已安装的序号相同就直接返回，避免在热路径上
/// 每个请求都复制一遍 payload（设计见 `docs/CONTRACT_UNIFICATION_DESIGN.md` §7）。
pub fn refresh_local() {
    let Some((addr, size)) = addr_and_size() else {
        return;
    };
    if current_generation(addr) == CACHED_GENERATION.load(Ordering::Acquire) {
        return;
    }
    if let Some((generation, payload)) = read_raw(addr, size) {
        install(generation, &payload);
    }
}
