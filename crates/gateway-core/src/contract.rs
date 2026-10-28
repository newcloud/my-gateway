//! 契约 JSON 的数据结构（协议 v1）。
//!
//! 约定文档：[`docs/PROTOCOL.md`](../../../docs/PROTOCOL.md) —— 字段、语义、演进规则都以它为准。
//!
//! 严格模式：顶层与 `routes[]` 内未定义的字段会被收集到 `extra`，由 [`Gateway::parse`] 拒绝整份契约
//! （fail-closed）。这样后端误发新字段时会立刻暴露，而不是被静默忽略、误以为已经生效。
//!
//! [`Gateway::parse`]: crate::Gateway::parse

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

/// 当前协议版本。开发期只有 v1，加字段不改版本号；只有正式上线后
/// 需要同时兼容旧契约时才递增（见协议文档第 0 节）。
pub const SUPPORTED_PROTOCOL: u32 = 1;

fn default_protocol() -> u32 {
    SUPPORTED_PROTOCOL
}

/// 未声明路由策略。
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
    /// 自由信息，插件必须忽略。字段名加 `_` 前缀表示"只解析不使用"，
    /// 保留它是为了占住 `meta` 这个键，避免落进 `extra` 被当成未知字段。
    #[serde(default, rename = "meta")]
    pub _meta: Option<serde_json::Value>,
    /// 未声明路由策略：`deny`（默认）或 `forward`。见协议文档第 2.1 节。
    #[serde(default)]
    pub unmatched: Option<Unmatched>,
    /// 全局字段过滤开关。
    #[serde(default)]
    pub filters: Option<FiltersBlock>,
    /// 拒绝响应覆盖。
    #[serde(default)]
    pub responses: Option<ResponsesConfig>,
    #[serde(default)]
    pub routes: Vec<RouteEntry>,
    /// 顶层未定义字段。非空即整份拒绝。
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// `routes[]` 的元素：叶子路由（有 `path`）或分组（有 `prefix`）。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RouteEntry {
    Route(Route),
    Group(Group),
}

#[derive(Debug, Clone, Deserialize)]
pub struct Route {
    pub path: String,
    /// 允许的方法；空数组表示不限制方法（渐进式契约的默认语义）。
    #[serde(default)]
    pub methods: Vec<String>,
    /// Header 参数块。
    #[serde(default)]
    pub headers: Option<HeadersBlock>,
    /// Query 参数块。
    #[serde(default)]
    pub query: Option<QueryBlock>,
    /// Body 参数块。
    #[serde(default)]
    pub body: Option<BodyBlock>,
    /// 路由级附加路径正则。
    #[serde(default)]
    pub path_regex: Option<String>,
    /// 路由优先级；越大越先匹配，默认 0。
    #[serde(default)]
    pub priority: Option<i32>,
    /// 路径参数值校验。
    #[serde(default)]
    pub path_params: Option<PathParamsBlock>,
    /// 字段过滤开关。
    #[serde(default)]
    pub filters: Option<FiltersBlock>,
    /// 路由内未定义字段（例如未实现的 `priority` / `path_regex` / `filters`）。非空即整份拒绝。
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// 分组路由。
///
/// 当前只实现 `headers` 继承、`unmatched` 作用域与 `path_regex`；其他分组字段
/// （`methods` / `query` / `body` / `path_params` / `filters`）仍按严格模式拒绝。
#[derive(Debug, Clone, Deserialize)]
pub struct Group {
    pub prefix: String,
    #[serde(default)]
    pub router: Vec<RouteEntry>,
    /// 组级方法约束；与后代取交集，空数组表示本层不限制。
    #[serde(default)]
    pub methods: Vec<String>,
    #[serde(default)]
    pub unmatched: Option<Unmatched>,
    #[serde(default)]
    pub path_regex: Option<String>,
    #[serde(default)]
    pub headers: Option<HeadersBlock>,
    #[serde(default)]
    pub query: Option<QueryBlock>,
    #[serde(default)]
    pub body: Option<BodyBlock>,
    #[serde(default)]
    pub path_params: Option<PathParamsBlock>,
    /// 字段过滤开关。
    #[serde(default)]
    pub filters: Option<FiltersBlock>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// `path_params` 参数块。
#[derive(Debug, Clone, Deserialize)]
pub struct PathParamsBlock {
    #[serde(default, deserialize_with = "deserialize_params")]
    pub params: Vec<(String, ParamRule)>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// `headers` 参数块。
///
/// Header 只校验显式声明的头，因此没有 `unknown` 选项。
#[derive(Debug, Clone, Deserialize)]
pub struct HeadersBlock {
    #[serde(default, deserialize_with = "deserialize_params")]
    pub params: Vec<(String, ParamRule)>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// `query` 参数块。
#[derive(Debug, Clone, Deserialize)]
pub struct QueryBlock {
    #[serde(default, deserialize_with = "deserialize_params")]
    pub params: Vec<(String, ParamRule)>,
    /// 未声明 query 参数策略；缺省 `ignore`。
    #[serde(default)]
    pub unknown: Option<UnknownMode>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// `body` 参数块。
#[derive(Debug, Clone, Deserialize)]
pub struct BodyBlock {
    #[serde(default, deserialize_with = "deserialize_params")]
    pub params: Vec<(String, ParamRule)>,
    /// 未声明 body 字段策略；缺省 `reject`（防 Mass Assignment）。
    #[serde(default)]
    pub unknown: Option<UnknownMode>,
    /// 允许的媒体类型；缺省 JSON + form。
    #[serde(default)]
    pub content_types: Option<Vec<String>>,
    /// 原始 body 字节上限；`0` 表示关闭本层继承的上限。
    #[serde(default)]
    pub max_bytes: Option<u64>,
    /// JSON 嵌套深度上限；`0` 表示关闭本层继承的上限。
    #[serde(default)]
    pub max_depth: Option<u64>,
    /// JSON 单个字符串的原始字节上限；`0` 表示关闭。
    #[serde(default)]
    pub max_string_length: Option<u64>,
    /// JSON 单个数组的元素个数上限；`0` 表示关闭。
    #[serde(default)]
    pub max_array_entries: Option<u64>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// 未声明字段策略（Query / Body）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UnknownMode {
    Ignore,
    Reject,
}

/// 参数块共用同一套规则：`true` 简写、存在性、值规则与 `required_if`。
fn deserialize_params<'de, D>(deserializer: D) -> Result<Vec<(String, ParamRule)>, D::Error>
where
    D: Deserializer<'de>,
{
    struct ParamsVisitor;

    impl<'de> Visitor<'de> for ParamsVisitor {
        type Value = Vec<(String, ParamRule)>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a map of parameter names to rules")
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            let mut params = Vec::new();
            let mut seen = BTreeSet::new();
            while let Some((name, rule)) = map.next_entry::<String, ParamRule>()? {
                if !seen.insert(name.clone()) {
                    return Err(de::Error::custom(format!(
                        "duplicate parameter rule \"{name}\": keys must be unique"
                    )));
                }
                params.push((name, rule));
            }
            Ok(params)
        }
    }

    deserializer.deserialize_map(ParamsVisitor)
}

/// 参数规则值。
///
/// 支持三种写法：
/// - `true`（等价 `{ "required": true }`）
/// - `{ "required": true | false }`
/// - 带值规则的对象：`type` / `min` / `max` / `min_len` / `max_len` / `len` /
///   `values` / `ignore_case` / `regex` / `required_if`
///
/// 分组继承仍未实现，会由 `gateway.rs` 的严格校验拒绝。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ParamRule {
    Presence(bool),
    Object(Box<ParamRuleObject>),
}

#[derive(Debug, Clone, Deserialize)]
pub struct ParamRuleObject {
    #[serde(default)]
    pub required: Option<bool>,
    /// 条件必填；字符串形式只判断被引用字段是否存在，对象形式还要比较值。
    #[serde(default)]
    pub required_if: Option<RequiredIf>,
    /// `char` / `int` / `bool` / `enum`；省略表示不校验值（`regex` 例外，按字符串处理）。
    #[serde(default, rename = "type")]
    pub value_type: Option<ValueType>,
    /// `int` 的闭区间下限/上限。
    #[serde(default)]
    pub min: Option<i64>,
    #[serde(default)]
    pub max: Option<i64>,
    /// `char` 的长度约束，单位是 Unicode 码点。
    #[serde(default)]
    pub min_len: Option<usize>,
    #[serde(default)]
    pub max_len: Option<usize>,
    #[serde(default)]
    pub len: Option<usize>,
    /// `enum` 的允许值。
    #[serde(default)]
    pub values: Option<Vec<String>>,
    #[serde(default)]
    pub ignore_case: Option<bool>,
    /// 字符串值全匹配正则；只能用于 `char` 或省略 `type`。
    #[serde(default)]
    pub regex: Option<String>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// `required_if` 的两种写法。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RequiredIf {
    /// 被引用字段出现即成立（空值也算）。
    Field(String),
    /// 被引用字段出现且值完全相等才成立。
    Object(RequiredIfObject),
}

#[derive(Debug, Clone, Deserialize)]
pub struct RequiredIfObject {
    #[serde(default)]
    pub field: Option<String>,
    #[serde(default)]
    pub equals: Option<String>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// 过滤器作用的参数维度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilterScope {
    Path,
    Query,
    Header,
    Body,
}

impl FilterScope {
    /// 维度位；合并时按位或。
    pub fn mask(self) -> u8 {
        match self {
            FilterScope::Path => 0b0001,
            FilterScope::Query => 0b0010,
            FilterScope::Header => 0b0100,
            FilterScope::Body => 0b1000,
        }
    }
}

/// `filters` 开关。`sqli` / `xss` 只接受布尔；`scope` 限制检查维度。
#[derive(Debug, Clone, Deserialize)]
pub struct FiltersBlock {
    #[serde(default)]
    pub sqli: Option<bool>,
    #[serde(default)]
    pub xss: Option<bool>,
    /// 只检查这些维度；省略表示 path/query/header/body 全部。
    #[serde(default)]
    pub scope: Option<Vec<FilterScope>>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// 顶层 `responses`：只覆盖拒绝类响应。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ResponsesConfig {
    #[serde(default)]
    pub route_not_declared: Option<ResponseOverride>,
    #[serde(default)]
    pub method_not_allowed: Option<ResponseOverride>,
    #[serde(default)]
    pub contract_not_ready: Option<ResponseOverride>,
    #[serde(default)]
    pub missing_parameter: Option<ResponseOverride>,
    #[serde(default)]
    pub invalid_parameter: Option<ResponseOverride>,
    #[serde(default)]
    pub unsupported_media_type: Option<ResponseOverride>,
    #[serde(default)]
    pub body_too_large: Option<ResponseOverride>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResponseOverride {
    #[serde(default)]
    pub status: Option<u16>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub retry_after: Option<u32>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// 值类型。只用于参数校验，和 HTTP 层无关。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ValueType {
    Char,
    Int,
    Bool,
    Enum,
}
