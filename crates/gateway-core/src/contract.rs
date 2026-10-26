//! 契约 JSON 的数据结构（协议 v1）。
//!
//! 约定文档：[`docs/PROTOCOL.md`](../../../docs/PROTOCOL.md) —— 字段、语义、演进规则都以它为准。
//!
//! 严格模式：顶层与 `routes[]` 内未定义的字段会被收集到 `extra`，由 [`Gateway::parse`] 拒绝整份契约
//! （fail-closed）。这样后端误发新字段时会立刻暴露，而不是被静默忽略、误以为已经生效。
//!
//! [`Gateway::parse`]: crate::Gateway::parse

use std::collections::BTreeMap;

use serde::Deserialize;

/// 当前插件支持的最高协议版本。新增/改变判定语义的字段必须递增版本（见协议文档第 0 节）。
///
/// v1 → v2：新增顶层可选 `unmatched`（未声明路由策略），旧契约行为不变。
pub const SUPPORTED_PROTOCOL: u32 = 2;
/// 插件支持的协议范围，用于日志与错误信息。
pub const SUPPORTED_PROTOCOLS: &str = "1..=2";
/// 最低协议版本；省略 `protocol` 时按它处理（老契约语义不变）。
pub const MIN_PROTOCOL: u32 = 1;

fn default_protocol() -> u32 {
    MIN_PROTOCOL
}

/// 未声明路由策略（protocol 2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Unmatched {
    /// 默认：网关直接 404，不转发。fail-closed。
    Deny,
    /// 交给后端处理（与命中路由一样放行）。仅迁移期/兼容旧客户端时使用。
    Forward,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Contract {
    /// 协议结构版本；缺省按 1 处理，便于最小契约。
    #[serde(default = "default_protocol")]
    pub protocol: u32,
    /// 内容修订号，仅用于日志/排查，不参与解析。
    #[serde(default)]
    pub version: Option<String>,
    /// 建议刷新周期（秒）；当前实现以 nginx `gateway_ttl` 为准。
    #[serde(default)]
    pub ttl: Option<u64>,
    /// 自由信息，插件必须忽略。字段名加 `_` 前缀表示"只解析不使用"，
    /// 保留它是为了占住 `meta` 这个键，避免落进 `extra` 被当成未知字段。
    #[serde(default, rename = "meta")]
    pub _meta: Option<serde_json::Value>,
    /// 未声明路由策略（protocol 2）：`deny`（默认）或 `forward`。见协议文档第 2.1 节。
    #[serde(default)]
    pub unmatched: Option<Unmatched>,
    #[serde(default)]
    pub routes: Vec<Route>,
    /// 顶层未定义字段。非空即整份拒绝。
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Route {
    pub path: String,
    /// 允许的方法；空数组表示不限制方法（渐进式契约的默认语义）。
    #[serde(default)]
    pub methods: Vec<String>,
    /// 路由内未定义字段（例如未实现的 `priority` / `query`）。非空即整份拒绝。
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}
