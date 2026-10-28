//! 路由编译与判定。这里不依赖 nginx，也不做 I/O，可直接 `cargo test`。

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

use matchit::Router;
use regex::Regex;

use crate::body::{self, BodyError, BodyInput, BodyKind, BodyLimits, OwnedValue};
use crate::contract::{
    BodyBlock, Contract, FilterScope, FiltersBlock, Group, ParamRule, QueryBlock,
    ResponseOverride as ContractResponse, ResponsesConfig, Route, RouteEntry, UnknownMode,
    Unmatched, SUPPORTED_PROTOCOL,
};
use crate::params::{self, ParamCompileError, ParamErrorKind, ParamInput, ParamSet, ParamValue};
use crate::query::{self, QueryError};

/// 契约解析/编译错误。带上是第几条路由，方便定位后端上报的问题。
#[derive(Debug)]
pub enum ContractError {
    Json(serde_json::Error),
    UnsupportedProtocol {
        protocol: u32,
        /// 插件支持的版本。
        supported: u32,
    },
    UnknownField {
        scope: String,
        name: String,
    },
    InvalidRoute {
        index: usize,
        path: String,
        detail: String,
    },
    EmptyMethod {
        index: usize,
        path: String,
    },
    /// 方法名不是合法的 HTTP token（可能带 CR/LF、空格、逗号等）。
    /// 它会原样写进 `Allow` 响应头与日志，必须在加载期拒绝。
    InvalidMethod {
        index: usize,
        path: String,
        method: String,
    },
    InvalidResponses(String),
    InvalidFilters(String),
}

impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContractError::Json(e) => write!(f, "invalid contract json: {e}"),
            ContractError::UnsupportedProtocol {
                protocol,
                supported,
            } => write!(
                f,
                "unsupported protocol {protocol} (plugin supports {supported}); upgrade plugin or lower backend protocol"
            ),
            ContractError::UnknownField { scope, name } => {
                write!(f, "unknown field {name} in {scope}")
            }
            ContractError::InvalidRoute {
                index,
                path,
                detail,
            } => write!(f, "invalid route #{index} ({path}): {detail}"),
            ContractError::EmptyMethod { index, path } => {
                write!(f, "route #{index} ({path}) has an empty method")
            }
            ContractError::InvalidMethod {
                index,
                path,
                method,
            } => write!(
                f,
                "route #{index} ({path}) has an invalid method {method:?}: must be an RFC 9110 token of 1-32 chars"
            ),
            ContractError::InvalidResponses(detail) => {
                write!(f, "invalid responses config: {detail}")
            }
            ContractError::InvalidFilters(detail) => {
                write!(f, "invalid filters config: {detail}")
            }
        }
    }
}

impl std::error::Error for ContractError {}

/// 判定结果。状态码在这里定死，适配层不再做语义判断。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    RouteNotDeclared,
    MethodNotAllowed,
    /// 必填参数缺失（Header / Query / Body）。
    MissingParameter,
    /// 参数本身非法（重复、类型/范围/长度/枚举/正则不符、未知字段、解析失败）。
    InvalidParameter,
    /// Body 的媒体类型 / charset / Content-Encoding 不支持。
    UnsupportedMediaType,
}

impl Decision {
    pub fn reason(self) -> &'static str {
        match self {
            Decision::Allow => "allow",
            Decision::RouteNotDeclared => "route-not-declared",
            Decision::MethodNotAllowed => "method-not-allowed",
            Decision::MissingParameter => "missing-parameter",
            Decision::InvalidParameter => "invalid-parameter",
            Decision::UnsupportedMediaType => "unsupported-media-type",
        }
    }

    /// 该判定对应的 HTTP 状态码。
    ///
    /// `Allow` 不产生响应（继续走 `proxy_pass`），返回 `None`；拒绝类判定在这里定死，
    /// 适配层直接使用，保证协议测试与线上是同一份映射。
    pub fn status_code(self) -> Option<u16> {
        match self {
            Decision::Allow => None,
            Decision::RouteNotDeclared => Some(404),
            Decision::MethodNotAllowed => Some(405),
            Decision::MissingParameter
            | Decision::InvalidParameter
            | Decision::UnsupportedMediaType => Some(400),
        }
    }
}

/// 判定结果 + 需要回写给客户端的附加信息。
#[derive(Debug, Clone)]
pub struct Outcome<'a> {
    pub decision: Decision,
    /// 405 时回写 `Allow` 头。
    pub allow: Option<&'a str>,
    /// 路径未声明，但契约要求转发给后端（`unmatched: "forward"`）。此时 `decision` 是 `Allow`，
    /// 适配层只做日志、继续走 proxy，不写响应头。
    pub forwarded: bool,
    /// 参数错误时的 `X-Gateway-Field`，格式为 `<dimension>.<name>`。
    pub field: Option<String>,
    /// 参数错误时命中的规则名（只写名字，不写阈值/正则），用于日志。
    pub rule: Option<&'static str>,
    /// `unmatched: "forward"` 时提供策略的分组前缀（顶层策略为 `None`）。
    pub scope: Option<&'a str>,
    /// 过滤命中时记录 `filter=sqli|xss`。
    pub filter: Option<&'static str>,
}

/// 请求上下文：只保留判定需要的东西，方便在测试里构造。
#[derive(Debug, Clone, Copy)]
pub struct RequestCtx<'a> {
    pub method: &'a str,
    pub path: &'a str,
}

/// Body 参数块编译结果。
#[derive(Debug, Clone)]
struct BodyRules {
    params: ParamSet,
    allowed: Vec<BodyKind>,
    /// 原始 body 字节上限；`None` 表示不限制。
    max_bytes: Option<u64>,
    /// JSON 扫描上限（深度 / 字符串字节 / 数组元素）。
    limits: BodyLimits,
}

/// 编译后的拒绝响应覆盖。
#[derive(Debug, Clone)]
pub struct ResponseOverride {
    pub status: u16,
    pub body: Option<String>,
    pub retry_after: Option<u32>,
}

#[derive(Debug, Clone, Default)]
struct ResponseOverrides {
    route_not_declared: Option<ResponseOverride>,
    method_not_allowed: Option<ResponseOverride>,
    contract_not_ready: Option<ResponseOverride>,
    missing_parameter: Option<ResponseOverride>,
    invalid_parameter: Option<ResponseOverride>,
    unsupported_media_type: Option<ResponseOverride>,
    body_too_large: Option<ResponseOverride>,
}

/// 分组作用域：`unmatched` 只影响未命中路由的路径。
#[derive(Debug, Clone)]
struct GroupScope {
    prefix: String,
    parent: Option<usize>,
    unmatched: Option<Unmatched>,
}

/// 合并后的字段过滤开关（或语义）；`areas` 是各维度位或的结果。
#[derive(Debug, Clone, Copy, Default)]
struct FilterState {
    sqli: bool,
    xss: bool,
    areas: u8,
}

/// 默认检查全部四个输入维度。
const FILTER_ALL_AREAS: u8 = 0b1111;

impl FilterState {
    fn enabled(self) -> bool {
        self.sqli || self.xss
    }

    fn covers(self, dimension: &str) -> bool {
        let bit = match dimension {
            "path" => FilterScope::Path.mask(),
            "query" => FilterScope::Query.mask(),
            "header" => FilterScope::Header.mask(),
            "body" => FilterScope::Body.mask(),
            _ => 0,
        };
        self.areas & bit != 0
    }
}

/// 合并后的 Query 规则。
#[derive(Debug, Clone)]
struct MergedQuery {
    params: Vec<(String, ParamRule)>,
    unknown: UnknownMode,
}

/// 合并后的 Body 规则。
#[derive(Debug, Clone)]
struct MergedBody {
    params: Vec<(String, ParamRule)>,
    unknown: UnknownMode,
    allowed: Vec<BodyKind>,
    /// 合并后的原始 body 字节上限；`None` 表示不限制。
    max_bytes: Option<u64>,
    max_depth: Option<u64>,
    max_string_length: Option<u64>,
    max_array_entries: Option<u64>,
}

/// 分组展开上下文：沿结构父子关系继承。
struct ExpandContext {
    prefix: String,
    parent_group: Option<usize>,
    /// 已合并的组级方法约束；`None` 表示组层没有非空 methods。
    methods: Option<Vec<String>>,
    headers: Vec<(String, ParamRule)>,
    query: Option<MergedQuery>,
    body: Option<MergedBody>,
    path_params: Vec<(String, ParamRule)>,
    path_regexes: Vec<Regex>,
    filters: FilterState,
}

#[derive(Default)]
struct BuildState {
    /// 每个 priority 一棵路由树；优先级高的先试。
    routers: BTreeMap<i32, Router<usize>>,
    paths: BTreeSet<String>,
    allow_headers: Vec<String>,
    header_rules: Vec<Option<ParamSet>>,
    query_rules: Vec<Option<ParamSet>>,
    body_rules: Vec<Option<BodyRules>>,
    path_rules: Vec<Option<ParamSet>>,
    path_regexes: Vec<Vec<Regex>>,
    filters: Vec<FilterState>,
    group_scopes: Vec<GroupScope>,
    group_prefixes: BTreeSet<String>,
    route_count: usize,
}

impl BuildState {
    fn expand_entries(
        &mut self,
        entries: &[RouteEntry],
        context: &ExpandContext,
        depth: usize,
    ) -> Result<(), ContractError> {
        let mut sibling_prefixes = BTreeSet::new();

        for entry in entries {
            match entry {
                RouteEntry::Route(route) => self.expand_route(route, context)?,
                RouteEntry::Group(group) => {
                    if !sibling_prefixes.insert(group.prefix.clone()) {
                        return Err(ContractError::InvalidRoute {
                            index: self.route_count,
                            path: group.prefix.clone(),
                            detail: "duplicate group prefix under the same parent".into(),
                        });
                    }
                    self.expand_group(group, context, depth)?;
                }
            }
        }

        Ok(())
    }

    fn expand_route(
        &mut self,
        route: &Route,
        context: &ExpandContext,
    ) -> Result<(), ContractError> {
        let index = self.route_count;
        if let Some(name) = route.extra.keys().next() {
            return Err(ContractError::UnknownField {
                scope: format!("route #{index}"),
                name: name.clone(),
            });
        }

        let full_path = join_path(&context.prefix, &route.path);
        validate_route_path(index, &full_path)?;
        // 参数名语法与重复与“有没有写 path_params”无关：非法名字会让 matchit 静默
        // 当成任意段通配，但规则名又被名字校验拒绝，等于一条永远无法约束的参数。
        path_param_names(&full_path).map_err(|detail| ContractError::InvalidRoute {
            index,
            path: full_path.clone(),
            detail,
        })?;
        if route.methods.iter().any(|method| method.trim().is_empty()) {
            return Err(ContractError::EmptyMethod {
                index,
                path: full_path,
            });
        }
        if let Some(method) = route
            .methods
            .iter()
            .map(|method| method.trim())
            .find(|method| !is_valid_method(method))
        {
            return Err(ContractError::InvalidMethod {
                index,
                path: full_path,
                method: method.to_owned(),
            });
        }

        let methods = merge_methods(
            index,
            &full_path,
            context.methods.as_deref(),
            &route.methods,
        )?;
        let allow_header = methods
            .as_ref()
            .map(|methods| methods.join(", "))
            .unwrap_or_default();

        let mut headers = context.headers.clone();
        if let Some(block) = &route.headers {
            if let Some(name) = block.extra.keys().next() {
                return Err(ContractError::UnknownField {
                    scope: format!("route #{index} headers"),
                    name: name.clone(),
                });
            }
            headers.extend(block.params.iter().cloned());
        }

        let query = merge_query(
            &format!("route #{index}"),
            context.query.as_ref(),
            route.query.as_ref(),
            UnknownMode::Ignore,
        )?;
        let body = merge_body(
            index,
            &full_path,
            &format!("route #{index}"),
            context.body.as_ref(),
            route.body.as_ref(),
            UnknownMode::Reject,
        )?;

        let mut path_regexes = context.path_regexes.clone();
        if let Some(pattern) = &route.path_regex {
            path_regexes.push(compile_path_regex(index, &full_path, pattern)?);
        }

        let mut path_params = context.path_params.clone();
        if let Some(block) = &route.path_params {
            if let Some(name) = block.extra.keys().next() {
                return Err(ContractError::UnknownField {
                    scope: format!("route #{index} path_params"),
                    name: name.clone(),
                });
            }
            path_params.extend(block.params.iter().cloned());
        }

        let filters = merge_filters(
            index,
            &format!("route #{index}"),
            context.filters,
            route.filters.as_ref(),
        )?;

        // 单条路由（含分组继承）的参数规则合计上限，避免超大契约拖慢加载与每请求判定。
        let total_rules = headers.len()
            + query.as_ref().map_or(0, |query| query.params.len())
            + body.as_ref().map_or(0, |body| body.params.len())
            + path_params.len();
        if total_rules > 256 {
            return Err(ContractError::InvalidRoute {
                index,
                path: full_path.clone(),
                detail: format!("route declares {total_rules} parameter rules, limit is 256"),
            });
        }

        let header_set = compile_headers_entries(index, &full_path, &headers)?;
        let query_set = match &query {
            Some(query) => Some(compile_query_entries(
                index,
                &full_path,
                &query.params,
                query.unknown,
            )?),
            None => None,
        };
        let body_set = match &body {
            Some(body) => Some(compile_body_entries(
                index,
                &full_path,
                &body.params,
                body.unknown,
                &body.allowed,
                body.max_bytes,
                BodyLimits {
                    max_depth: body.max_depth.map(|value| value as usize),
                    max_string_length: body.max_string_length.map(|value| value as usize),
                    max_array_entries: body.max_array_entries.map(|value| value as usize),
                },
            )?),
            None => None,
        };
        let path_set = compile_path_params_entries(index, &full_path, &path_params)?;

        if !self.paths.insert(full_path.clone()) {
            return Err(ContractError::InvalidRoute {
                index,
                path: full_path.clone(),
                detail: "duplicate route path".into(),
            });
        }
        let priority = route.priority.unwrap_or(0);
        self.routers
            .entry(priority)
            .or_default()
            .insert(full_path.as_str(), index)
            .map_err(|error| ContractError::InvalidRoute {
                index,
                path: full_path.clone(),
                detail: error.to_string(),
            })?;

        self.allow_headers.push(allow_header);
        self.header_rules.push(header_set);
        self.query_rules.push(query_set);
        self.body_rules.push(body_set);
        self.path_rules.push(path_set);
        self.path_regexes.push(path_regexes);
        self.filters.push(filters);
        self.route_count += 1;

        if self.route_count > 4096 {
            return Err(ContractError::InvalidRoute {
                index,
                path: full_path,
                detail: "expanded route count exceeds 4096".into(),
            });
        }

        Ok(())
    }

    fn expand_group(
        &mut self,
        group: &Group,
        context: &ExpandContext,
        depth: usize,
    ) -> Result<(), ContractError> {
        let index = self.route_count;
        if depth >= 8 {
            return Err(ContractError::InvalidRoute {
                index,
                path: group.prefix.clone(),
                detail: "group nesting depth exceeds 8".into(),
            });
        }
        if let Some(name) = group.extra.keys().next() {
            return Err(ContractError::UnknownField {
                scope: format!("group #{index}"),
                name: name.clone(),
            });
        }

        validate_prefix(index, &group.prefix)?;
        let full_prefix = join_path(&context.prefix, &group.prefix);
        validate_prefix(index, &full_prefix)?;

        if !self.group_prefixes.insert(full_prefix.clone()) {
            return Err(ContractError::InvalidRoute {
                index,
                path: full_prefix,
                detail: "duplicate expanded group prefix".into(),
            });
        }

        if group.methods.iter().any(|method| method.trim().is_empty()) {
            return Err(ContractError::EmptyMethod {
                index,
                path: full_prefix,
            });
        }
        if let Some(method) = group
            .methods
            .iter()
            .map(|method| method.trim())
            .find(|method| !is_valid_method(method))
        {
            return Err(ContractError::InvalidMethod {
                index,
                path: full_prefix,
                method: method.to_owned(),
            });
        }
        let methods = merge_methods(
            index,
            &full_prefix,
            context.methods.as_deref(),
            &group.methods,
        )?;

        let mut path_regexes = context.path_regexes.clone();
        if let Some(pattern) = &group.path_regex {
            path_regexes.push(compile_path_regex(index, &full_prefix, pattern)?);
        }

        let mut headers = context.headers.clone();
        if let Some(block) = &group.headers {
            if let Some(name) = block.extra.keys().next() {
                return Err(ContractError::UnknownField {
                    scope: format!("group #{index} headers"),
                    name: name.clone(),
                });
            }
            headers.extend(block.params.iter().cloned());
        }

        let query = merge_query(
            &format!("group #{index}"),
            context.query.as_ref(),
            group.query.as_ref(),
            UnknownMode::Ignore,
        )?;
        let body = merge_body(
            index,
            &full_prefix,
            &format!("group #{index}"),
            context.body.as_ref(),
            group.body.as_ref(),
            UnknownMode::Reject,
        )?;

        let mut path_params = context.path_params.clone();
        if let Some(block) = &group.path_params {
            if let Some(name) = block.extra.keys().next() {
                return Err(ContractError::UnknownField {
                    scope: format!("group #{index} path_params"),
                    name: name.clone(),
                });
            }
            path_params.extend(block.params.iter().cloned());
        }

        let filters = merge_filters(
            index,
            &format!("group #{index}"),
            context.filters,
            group.filters.as_ref(),
        )?;

        let group_index = self.group_scopes.len();
        self.group_scopes.push(GroupScope {
            prefix: full_prefix.clone(),
            parent: context.parent_group,
            unmatched: group.unmatched,
        });

        let child = ExpandContext {
            prefix: full_prefix,
            parent_group: Some(group_index),
            methods,
            headers,
            query,
            body,
            path_params,
            path_regexes,
            filters,
        };
        self.expand_entries(&group.router, &child, depth + 1)
    }
}

/// 编译好的契约。
pub struct Gateway {
    routers: Vec<Router<usize>>,
    allow_headers: Vec<String>,
    /// 与 `allow_headers` 同下标：每条叶子路由生效的参数规则。
    header_rules: Vec<Option<ParamSet>>,
    query_rules: Vec<Option<ParamSet>>,
    body_rules: Vec<Option<BodyRules>>,
    path_rules: Vec<Option<ParamSet>>,
    /// 分组继承的 `path_regex`（AND）；空表示不限制。
    path_regexes: Vec<Vec<Regex>>,
    /// 每条叶子路由的过滤开关。
    filters: Vec<FilterState>,
    responses: ResponseOverrides,
    /// 分组作用域，用于未命中路由时的 `unmatched` 选择。
    group_scopes: Vec<GroupScope>,
    protocol: u32,
    version: Option<String>,
    route_count: usize,
    forward_unmatched: bool,
}

impl fmt::Debug for Gateway {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Gateway")
            .field("protocol", &self.protocol)
            .field("version", &self.version)
            .field("route_count", &self.route_count)
            .field("forward_unmatched", &self.forward_unmatched)
            .field(
                "header_rule_routes",
                &self.header_rules.iter().filter(|r| r.is_some()).count(),
            )
            .field(
                "query_rule_routes",
                &self.query_rules.iter().filter(|r| r.is_some()).count(),
            )
            .field(
                "body_rule_routes",
                &self.body_rules.iter().filter(|r| r.is_some()).count(),
            )
            .field(
                "filter_routes",
                &self.filters.iter().filter(|f| f.enabled()).count(),
            )
            .finish_non_exhaustive()
    }
}

impl Gateway {
    /// 解析并编译契约。任何一条路由非法都会整体失败（fail-closed，不用半份契约）。
    ///
    /// 严格模式见 `docs/PROTOCOL.md`：协议版本不支持、出现未定义字段，都直接拒绝整份契约。
    pub fn parse(json: &str) -> Result<Self, ContractError> {
        let contract: Contract = serde_json::from_str(json).map_err(ContractError::Json)?;

        if contract.protocol != SUPPORTED_PROTOCOL {
            return Err(ContractError::UnsupportedProtocol {
                protocol: contract.protocol,
                supported: SUPPORTED_PROTOCOL,
            });
        }

        if let Some(name) = contract.extra.keys().next() {
            return Err(ContractError::UnknownField {
                scope: "contract".into(),
                name: name.clone(),
            });
        }

        let root_filters = merge_filters(
            0,
            "contract",
            FilterState::default(),
            contract.filters.as_ref(),
        )?;
        let responses = compile_responses(contract.responses.as_ref())?;

        let mut builder = BuildState::default();
        let root = ExpandContext {
            prefix: "/".into(),
            parent_group: None,
            methods: None,
            headers: Vec::new(),
            query: None,
            body: None,
            path_params: Vec::new(),
            path_regexes: Vec::new(),
            filters: root_filters,
        };
        builder.expand_entries(&contract.routes, &root, 0)?;

        Ok(Self {
            routers: builder
                .routers
                .into_iter()
                .rev()
                .map(|(_, router)| router)
                .collect(),
            allow_headers: builder.allow_headers,
            header_rules: builder.header_rules,
            query_rules: builder.query_rules,
            body_rules: builder.body_rules,
            path_rules: builder.path_rules,
            path_regexes: builder.path_regexes,
            filters: builder.filters,
            responses,
            group_scopes: builder.group_scopes,
            protocol: contract.protocol,
            version: contract.version,
            route_count: builder.route_count,
            forward_unmatched: contract.unmatched == Some(Unmatched::Forward),
        })
    }

    /// 不携带任何参数的判定入口。带 Header / Query 规则时请用专用入口。
    pub fn check(&self, ctx: &RequestCtx<'_>) -> Outcome<'_> {
        self.check_without_body(ctx, &[], None)
    }

    /// 带请求头的判定入口。
    ///
    /// `headers` 按请求里的行逐条传入：`Some(value)` 是已解码的 UTF-8 值，
    /// `None` 表示该行存在但值不是合法 UTF-8（只有声明了值规则时才拒绝）。
    pub fn check_with_headers(
        &self,
        ctx: &RequestCtx<'_>,
        headers: &[(&str, Option<&str>)],
    ) -> Outcome<'_> {
        self.check_without_body(ctx, headers, None)
    }

    /// 只做“不依赖 body”的判定：路径 / 方法 / Query / Header。
    ///
    /// 路由声明了 `body` 时，调用方应在所有网关都返回 `Allow` 后用
    /// [`Gateway::needs_body`] 判断是否需要读取请求体，再调用 [`Gateway::check_with_body`]。
    pub fn check_without_body(
        &self,
        ctx: &RequestCtx<'_>,
        headers: &[(&str, Option<&str>)],
        raw_query: Option<&str>,
    ) -> Outcome<'_> {
        self.check_inner(ctx, headers, raw_query, None)
    }

    /// 带 body 的完整判定。会重新执行 Query / Header 检查，因此可以直接替代
    /// [`Gateway::check_without_body`]；body 解析失败时返回对应的 400。
    pub fn check_with_body(
        &self,
        ctx: &RequestCtx<'_>,
        headers: &[(&str, Option<&str>)],
        raw_query: Option<&str>,
        body: &BodyInput<'_>,
    ) -> Outcome<'_> {
        self.check_inner(ctx, headers, raw_query, Some(body))
    }

    /// 当前判定对应的拒绝响应覆盖（没有则使用默认状态码/nginx 默认响应体）。
    pub fn response_override(&self, decision: Decision) -> Option<&ResponseOverride> {
        match decision {
            Decision::RouteNotDeclared => self.responses.route_not_declared.as_ref(),
            Decision::MethodNotAllowed => self.responses.method_not_allowed.as_ref(),
            Decision::MissingParameter => self.responses.missing_parameter.as_ref(),
            Decision::InvalidParameter => self.responses.invalid_parameter.as_ref(),
            Decision::UnsupportedMediaType => self.responses.unsupported_media_type.as_ref(),
            Decision::Allow => None,
        }
    }

    /// `body-too-large` 响应覆盖（读取请求体时由 nginx 返回 413 后使用）。
    pub fn body_too_large_override(&self) -> Option<&ResponseOverride> {
        self.responses.body_too_large.as_ref()
    }

    /// 契约未就绪的响应覆盖（当前只在已有快照时可用）。
    pub fn contract_not_ready_override(&self) -> Option<&ResponseOverride> {
        self.responses.contract_not_ready.as_ref()
    }

    /// 该路由是否声明了 `body` 块（决定是否需要读取请求体）。
    pub fn needs_body(&self, ctx: &RequestCtx<'_>) -> bool {
        self.body_route_index(ctx).is_some()
    }

    /// 该路由合并后的 body 字节上限；`None` 表示未配置（或被子级 `0` 关闭）。
    pub fn body_max_bytes(&self, ctx: &RequestCtx<'_>) -> Option<u64> {
        let index = self.body_route_index(ctx)?;
        self.body_rules[index]
            .as_ref()
            .and_then(|rules| rules.max_bytes)
    }

    fn body_route_index(&self, ctx: &RequestCtx<'_>) -> Option<usize> {
        let index = self.match_route_index(ctx.path)?;
        let matches = self.path_regexes[index]
            .iter()
            .all(|regex| regex.is_match(ctx.path));
        (matches && self.body_rules[index].is_some()).then_some(index)
    }

    fn match_route_index(&self, path: &str) -> Option<usize> {
        self.routers
            .iter()
            .find_map(|router| router.at(path).ok().map(|matched| *matched.value))
    }

    /// 未命中路由时的结果：按分组作用域选 `unmatched`，并返回提供策略的 scope。
    fn unmatched_outcome(&self, path: &str) -> Outcome<'_> {
        if let Some(mut index) = self.longest_group_scope(path) {
            loop {
                let scope = &self.group_scopes[index];
                if let Some(unmatched) = scope.unmatched {
                    return match unmatched {
                        Unmatched::Forward => Outcome {
                            decision: Decision::Allow,
                            allow: None,
                            forwarded: true,
                            field: None,
                            rule: None,
                            scope: Some(scope.prefix.as_str()),
                            filter: None,
                        },
                        Unmatched::Deny => Outcome {
                            decision: Decision::RouteNotDeclared,
                            allow: None,
                            forwarded: false,
                            field: None,
                            rule: None,
                            scope: None,
                            filter: None,
                        },
                    };
                }
                match scope.parent {
                    Some(parent) => index = parent,
                    None => break,
                }
            }
        }

        if self.forward_unmatched {
            Outcome {
                decision: Decision::Allow,
                allow: None,
                forwarded: true,
                field: None,
                rule: None,
                scope: None,
                filter: None,
            }
        } else {
            Outcome {
                decision: Decision::RouteNotDeclared,
                allow: None,
                forwarded: false,
                field: None,
                rule: None,
                scope: None,
                filter: None,
            }
        }
    }

    fn longest_group_scope(&self, path: &str) -> Option<usize> {
        let mut best: Option<usize> = None;
        for (index, scope) in self.group_scopes.iter().enumerate() {
            if prefix_contains(&scope.prefix, path)
                && best.is_none_or(|current| {
                    scope.prefix.len() > self.group_scopes[current].prefix.len()
                })
            {
                best = Some(index);
            }
        }
        best
    }

    fn check_inner(
        &self,
        ctx: &RequestCtx<'_>,
        headers: &[(&str, Option<&str>)],
        raw_query: Option<&str>,
        body: Option<&BodyInput<'_>>,
    ) -> Outcome<'_> {
        let Some(matched) = self
            .routers
            .iter()
            .find_map(|router| router.at(ctx.path).ok())
        else {
            return self.unmatched_outcome(ctx.path);
        };

        let index = *matched.value;

        // 分组 path_regex 是附加条件；任一不通过等同于“路径未命中”，不回退到次优路由。
        if !self.path_regexes[index]
            .iter()
            .all(|regex| regex.is_match(ctx.path))
        {
            return self.unmatched_outcome(ctx.path);
        }

        let allow_header = &self.allow_headers[index];

        if !allow_header.is_empty()
            && !allow_header
                .split(", ")
                .any(|m| m.eq_ignore_ascii_case(ctx.method))
        {
            return Outcome {
                decision: Decision::MethodNotAllowed,
                allow: Some(allow_header.as_str()),
                forwarded: false,
                field: None,
                rule: None,
                scope: None,
                filter: None,
            };
        }

        // 维度顺序：Path → Query → Header → Body（协议 5.5）。
        if let Some(params) = &self.path_rules[index] {
            let values = matched
                .params
                .iter()
                .map(|(name, value)| ParamInput {
                    name,
                    value: ParamValue::Text(value),
                })
                .collect::<Vec<_>>();
            if let Err(error) = params.check(&values) {
                return param_error_outcome(error);
            }
        }
        if self.filters[index].enabled() {
            let values = matched
                .params
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect::<Vec<_>>();
            if let Some(outcome) = filter_outcome("path", &values, self.filters[index]) {
                return outcome;
            }
        }

        if let Some(params) = &self.query_rules[index] {
            let raw_query = raw_query.unwrap_or("");
            let pairs = match query::parse(raw_query) {
                Ok(pairs) => pairs,
                Err(error) => return query_error_outcome(error),
            };
            let values = pairs
                .iter()
                .map(|(name, value)| ParamInput {
                    name,
                    value: ParamValue::Text(value),
                })
                .collect::<Vec<_>>();
            if let Err(error) = params.check(&values) {
                return param_error_outcome(error);
            }
            if let Some(outcome) = filter_outcome("query", &pairs, self.filters[index]) {
                return outcome;
            }
        }

        if let Some(params) = &self.header_rules[index] {
            let values = headers
                .iter()
                .map(|(name, value)| ParamInput {
                    name,
                    value: match value {
                        Some(value) => ParamValue::Text(value),
                        None => ParamValue::NonUtf8,
                    },
                })
                .collect::<Vec<_>>();
            if let Err(error) = params.check(&values) {
                return param_error_outcome(error);
            }
            let filter_values = params.declared_text_values(&values);
            if let Some(outcome) = filter_outcome("header", &filter_values, self.filters[index]) {
                return outcome;
            }
        }

        if let Some(rules) = &self.body_rules[index] {
            if let Some(input) = body {
                let parsed = match body::parse(input, &rules.allowed, rules.limits) {
                    Ok(parsed) => parsed,
                    Err(error) => return body_error_outcome(error),
                };
                let values = parsed
                    .iter()
                    .map(|(name, value)| ParamInput {
                        name,
                        value: match value {
                            OwnedValue::Text(value) => ParamValue::Text(value),
                            OwnedValue::Int(value) => ParamValue::Int(*value),
                            OwnedValue::Bool => ParamValue::Bool,
                            OwnedValue::Float => ParamValue::Float,
                            OwnedValue::Composite => ParamValue::Composite,
                            OwnedValue::Null => ParamValue::Null,
                        },
                    })
                    .collect::<Vec<_>>();
                if let Err(error) = rules.params.check(&values) {
                    return param_error_outcome(error);
                }
                let filter_values = rules.params.all_text_values(&values);
                if let Some(outcome) = filter_outcome("body", &filter_values, self.filters[index]) {
                    return outcome;
                }
            }
        }

        Outcome {
            decision: Decision::Allow,
            allow: None,
            forwarded: false,
            field: None,
            rule: None,
            scope: None,
            filter: None,
        }
    }

    pub fn protocol(&self) -> u32 {
        self.protocol
    }

    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    pub fn route_count(&self) -> usize {
        self.route_count
    }

    /// 未声明路由是否转发给后端（`unmatched: "forward"`）。
    pub fn forward_unmatched(&self) -> bool {
        self.forward_unmatched
    }
}

fn param_error_outcome(error: params::ParamError) -> Outcome<'static> {
    Outcome {
        decision: match error.kind {
            ParamErrorKind::Missing => Decision::MissingParameter,
            ParamErrorKind::Invalid => Decision::InvalidParameter,
        },
        allow: None,
        forwarded: false,
        field: Some(error.field),
        rule: Some(error.rule),
        scope: None,
        filter: None,
    }
}

fn filter_outcome(
    dimension: &str,
    values: &[(String, String)],
    filters: FilterState,
) -> Option<Outcome<'static>> {
    if !filters.enabled() || !filters.covers(dimension) {
        return None;
    }

    // 固定顺序：先扫完所有值的 sqli，再扫 xss，保证同时命中时只报 sqli。
    if filters.sqli {
        for (name, value) in values {
            if libinjectionrs::detect_sqli(value.as_bytes()).is_injection() {
                return Some(filter_denied(dimension, name, "sqli"));
            }
        }
    }
    if filters.xss {
        for (name, value) in values {
            if libinjectionrs::detect_xss(value.as_bytes()).is_injection() {
                return Some(filter_denied(dimension, name, "xss"));
            }
        }
    }

    None
}

fn filter_denied(dimension: &str, name: &str, kind: &'static str) -> Outcome<'static> {
    Outcome {
        decision: Decision::InvalidParameter,
        allow: None,
        forwarded: false,
        field: Some(params::format_field(dimension, name)),
        rule: Some("filter"),
        scope: None,
        filter: Some(kind),
    }
}

fn query_error_outcome(error: QueryError) -> Outcome<'static> {
    let (field, rule) = match error {
        QueryError::Duplicate(name) => (Some(params::format_field("query", &name)), "duplicate"),
        QueryError::Malformed => (None, "encoding"),
        QueryError::EmptyKey => (None, "empty-key"),
        QueryError::TooMany => (None, "limit"),
    };

    Outcome {
        decision: Decision::InvalidParameter,
        allow: None,
        forwarded: false,
        field,
        rule: Some(rule),
        scope: None,
        filter: None,
    }
}

fn body_error_outcome(error: BodyError) -> Outcome<'static> {
    match error {
        BodyError::Unsupported { rule } => Outcome {
            decision: Decision::UnsupportedMediaType,
            allow: None,
            forwarded: false,
            field: None,
            rule: Some(rule),
            scope: None,
            filter: None,
        },
        BodyError::Invalid { rule } => Outcome {
            decision: Decision::InvalidParameter,
            allow: None,
            forwarded: false,
            field: None,
            rule: Some(rule),
            scope: None,
            filter: None,
        },
        BodyError::Duplicate(name) => Outcome {
            decision: Decision::InvalidParameter,
            allow: None,
            forwarded: false,
            field: Some(params::format_field("body", &name)),
            rule: Some("duplicate"),
            scope: None,
            filter: None,
        },
    }
}

fn compile_headers_entries(
    index: usize,
    path: &str,
    entries: &[(String, ParamRule)],
) -> Result<Option<ParamSet>, ContractError> {
    if entries.is_empty() {
        return Ok(None);
    }

    params::ParamSet::compile(params::ParamSetSpec {
        dimension: "header",
        case_insensitive: true,
        trim_ows: true,
        unknown: UnknownMode::Ignore,
        params: entries,
        allow_required: true,
        name_validator: is_valid_header_name,
        name_rule: "invalid header name: must be an HTTP field-name of 1-128 ASCII token chars",
    })
    .map(Some)
    .map_err(|error| compile_error(index, path, "headers", error))
}

fn compile_responses(config: Option<&ResponsesConfig>) -> Result<ResponseOverrides, ContractError> {
    let Some(config) = config else {
        return Ok(ResponseOverrides::default());
    };

    if let Some(name) = config.extra.keys().next() {
        return Err(ContractError::UnknownField {
            scope: "responses".into(),
            name: name.clone(),
        });
    }

    Ok(ResponseOverrides {
        route_not_declared: compile_response(
            "responses.route_not_declared",
            config.route_not_declared.as_ref(),
        )?,
        method_not_allowed: compile_response(
            "responses.method_not_allowed",
            config.method_not_allowed.as_ref(),
        )?,
        contract_not_ready: compile_response(
            "responses.contract_not_ready",
            config.contract_not_ready.as_ref(),
        )?,
        missing_parameter: compile_response(
            "responses.missing_parameter",
            config.missing_parameter.as_ref(),
        )?,
        invalid_parameter: compile_response(
            "responses.invalid_parameter",
            config.invalid_parameter.as_ref(),
        )?,
        unsupported_media_type: compile_response(
            "responses.unsupported_media_type",
            config.unsupported_media_type.as_ref(),
        )?,
        body_too_large: compile_response(
            "responses.body_too_large",
            config.body_too_large.as_ref(),
        )?,
    })
}

fn compile_response(
    scope: &str,
    value: Option<&ContractResponse>,
) -> Result<Option<ResponseOverride>, ContractError> {
    let Some(value) = value else {
        return Ok(None);
    };

    if let Some(name) = value.extra.keys().next() {
        return Err(ContractError::UnknownField {
            scope: scope.to_owned(),
            name: name.clone(),
        });
    }

    let status = value
        .status
        .ok_or_else(|| ContractError::InvalidResponses(format!("{scope}.status is required")))?;
    if !(400..=599).contains(&status) {
        return Err(ContractError::InvalidResponses(format!(
            "{scope}.status must be 4xx or 5xx, got {status}"
        )));
    }

    if let Some(body) = &value.body {
        if body.len() > 1024 {
            return Err(ContractError::InvalidResponses(format!(
                "{scope}.body is longer than 1024 bytes"
            )));
        }
    }

    Ok(Some(ResponseOverride {
        status,
        body: value.body.clone(),
        retry_after: value.retry_after,
    }))
}

fn merge_filters(
    _index: usize,
    scope: &str,
    current: FilterState,
    block: Option<&FiltersBlock>,
) -> Result<FilterState, ContractError> {
    let Some(block) = block else {
        return Ok(current);
    };

    if let Some(name) = block.extra.keys().next() {
        return Err(ContractError::UnknownField {
            scope: format!("{scope} filters"),
            name: name.clone(),
        });
    }

    let enables_sqli = block.sqli.unwrap_or(false);
    let enables_xss = block.xss.unwrap_or(false);
    let mut areas = current.areas;

    if let Some(scope_list) = &block.scope {
        // `scope` 的意义是“这一层开启过滤时只看这些维度”；若同一层没有开启任何过滤器，
        // 它既不会收窄祖先的开关，也容易被误解，直接拒绝。
        if !(enables_sqli || enables_xss) {
            return Err(ContractError::InvalidFilters(format!(
                "{scope} filters: `scope` requires `sqli` or `xss` enabled in the same block"
            )));
        }
        if scope_list.is_empty() {
            return Err(ContractError::InvalidFilters(format!(
                "{scope} filters: `scope` must list at least one of path/query/header/body"
            )));
        }

        let mut selected = 0u8;
        for area in scope_list {
            if selected & area.mask() != 0 {
                return Err(ContractError::InvalidFilters(format!(
                    "{scope} filters: duplicate scope `{area:?}`"
                )));
            }
            selected |= area.mask();
        }
        areas |= selected;
    } else if enables_sqli || enables_xss {
        areas |= FILTER_ALL_AREAS;
    }

    Ok(FilterState {
        sqli: current.sqli || enables_sqli,
        xss: current.xss || enables_xss,
        areas,
    })
}

fn merge_methods(
    index: usize,
    path: &str,
    parent: Option<&[String]>,
    child: &[String],
) -> Result<Option<Vec<String>>, ContractError> {
    if child.is_empty() {
        return Ok(parent.map(<[String]>::to_vec));
    }

    let mut methods = Vec::new();
    for method in child {
        let method = method.trim();
        let allowed = parent.is_none_or(|parent| {
            parent
                .iter()
                .any(|candidate| candidate.trim().eq_ignore_ascii_case(method))
        });
        if allowed
            && !methods
                .iter()
                .any(|value: &String| value.eq_ignore_ascii_case(method))
        {
            methods.push(method.to_owned());
        }
    }

    if methods.is_empty() {
        return Err(ContractError::InvalidRoute {
            index,
            path: path.to_owned(),
            detail: "methods intersection is empty".into(),
        });
    }

    Ok(Some(methods))
}

fn merge_query(
    scope: &str,
    current: Option<&MergedQuery>,
    block: Option<&QueryBlock>,
    default_unknown: UnknownMode,
) -> Result<Option<MergedQuery>, ContractError> {
    let Some(block) = block else {
        return Ok(current.cloned());
    };

    if let Some(name) = block.extra.keys().next() {
        return Err(ContractError::UnknownField {
            scope: format!("{scope} query"),
            name: name.clone(),
        });
    }

    let mut merged = current.cloned().unwrap_or(MergedQuery {
        params: Vec::new(),
        unknown: default_unknown,
    });
    merged.params.extend(block.params.iter().cloned());

    let block_unknown = block.unknown.unwrap_or(default_unknown);
    merged.unknown =
        if merged.unknown == UnknownMode::Reject || block_unknown == UnknownMode::Reject {
            UnknownMode::Reject
        } else {
            UnknownMode::Ignore
        };

    Ok(Some(merged))
}

fn merge_body(
    index: usize,
    path: &str,
    scope: &str,
    current: Option<&MergedBody>,
    block: Option<&BodyBlock>,
    default_unknown: UnknownMode,
) -> Result<Option<MergedBody>, ContractError> {
    let Some(block) = block else {
        return Ok(current.cloned());
    };

    if let Some(name) = block.extra.keys().next() {
        return Err(ContractError::UnknownField {
            scope: format!("{scope} body"),
            name: name.clone(),
        });
    }

    if let Some(depth) = block.max_depth {
        if depth > MAX_BODY_DEPTH {
            return Err(ContractError::InvalidRoute {
                index,
                path: path.to_owned(),
                detail: format!("{scope} body.max_depth must be <= {MAX_BODY_DEPTH}"),
            });
        }
    }

    let kinds = body::allowed_kinds(block.content_types.as_deref()).map_err(|detail| {
        ContractError::InvalidRoute {
            index,
            path: path.to_owned(),
            detail: format!("{scope} body.content_types: {detail}"),
        }
    })?;

    let block_unknown = block.unknown.unwrap_or(default_unknown);

    let mut merged = match current {
        Some(current) => {
            let mut merged = current.clone();
            merged.allowed = intersect_kinds(&merged.allowed, &kinds);
            if merged.allowed.is_empty() {
                return Err(ContractError::InvalidRoute {
                    index,
                    path: path.to_owned(),
                    detail: format!("{scope} body.content_types intersection is empty"),
                });
            }
            merged
        }
        None => {
            if kinds.is_empty() {
                return Err(ContractError::InvalidRoute {
                    index,
                    path: path.to_owned(),
                    detail: format!("{scope} body.content_types is empty"),
                });
            }
            MergedBody {
                params: Vec::new(),
                unknown: block_unknown,
                allowed: kinds,
                max_bytes: None,
                max_depth: Some(DEFAULT_BODY_MAX_DEPTH),
                max_string_length: Some(DEFAULT_BODY_MAX_STRING_LENGTH),
                max_array_entries: Some(DEFAULT_BODY_MAX_ARRAY_ENTRIES),
            }
        }
    };

    merged.params.extend(block.params.iter().cloned());

    let block_unknown = block.unknown.unwrap_or(default_unknown);
    merged.unknown =
        if merged.unknown == UnknownMode::Reject || block_unknown == UnknownMode::Reject {
            UnknownMode::Reject
        } else {
            UnknownMode::Ignore
        };

    // 各类上限按“更严格者”继承：子级可设更小值；显式 `0` 关闭本子树的上限。
    merged.max_bytes = merge_body_limit(merged.max_bytes, block.max_bytes);
    merged.max_depth = merge_body_limit(merged.max_depth, block.max_depth);
    merged.max_string_length = merge_body_limit(merged.max_string_length, block.max_string_length);
    merged.max_array_entries = merge_body_limit(merged.max_array_entries, block.max_array_entries);

    if merged.max_depth.is_some_and(|depth| depth > MAX_BODY_DEPTH) {
        return Err(ContractError::InvalidRoute {
            index,
            path: path.to_owned(),
            detail: format!("{scope} body.max_depth must be <= {MAX_BODY_DEPTH}"),
        });
    }

    Ok(Some(merged))
}

/// JSON 嵌套深度硬上限（同时是扫描器栈大小）；serde_json 自身默认递归上限也是 128。
const MAX_BODY_DEPTH: u64 = 128;
const DEFAULT_BODY_MAX_DEPTH: u64 = 32;
const DEFAULT_BODY_MAX_STRING_LENGTH: u64 = 8192;
const DEFAULT_BODY_MAX_ARRAY_ENTRIES: u64 = 500;

fn merge_body_limit(current: Option<u64>, block: Option<u64>) -> Option<u64> {
    match block {
        None => current,
        Some(0) => None,
        Some(value) => Some(current.map_or(value, |current| current.min(value))),
    }
}

fn intersect_kinds(left: &[BodyKind], right: &[BodyKind]) -> Vec<BodyKind> {
    left.iter()
        .copied()
        .filter(|kind| right.contains(kind))
        .collect()
}

fn compile_query_entries(
    index: usize,
    path: &str,
    params: &[(String, ParamRule)],
    unknown: UnknownMode,
) -> Result<ParamSet, ContractError> {
    params::ParamSet::compile(params::ParamSetSpec {
        dimension: "query",
        case_insensitive: false,
        trim_ows: false,
        unknown,
        params,
        allow_required: true,
        name_validator: is_valid_param_name,
        name_rule: "invalid query name: must be 1-128 UTF-8 characters without control characters",
    })
    .map_err(|error| compile_error(index, path, "query", error))
}

fn compile_body_entries(
    index: usize,
    path: &str,
    params: &[(String, ParamRule)],
    unknown: UnknownMode,
    allowed: &[BodyKind],
    max_bytes: Option<u64>,
    limits: BodyLimits,
) -> Result<BodyRules, ContractError> {
    let params = params::ParamSet::compile(params::ParamSetSpec {
        dimension: "body",
        case_insensitive: false,
        trim_ows: false,
        unknown,
        params,
        allow_required: true,
        name_validator: is_valid_param_name,
        name_rule: "invalid body name: must be 1-128 UTF-8 characters without control characters",
    })
    .map_err(|error| compile_error(index, path, "body", error))?;

    Ok(BodyRules {
        params,
        allowed: allowed.to_vec(),
        max_bytes,
        limits,
    })
}

fn compile_path_params_entries(
    index: usize,
    path: &str,
    entries: &[(String, ParamRule)],
) -> Result<Option<ParamSet>, ContractError> {
    if entries.is_empty() {
        return Ok(None);
    }

    let names = path_param_names(path).map_err(|detail| ContractError::InvalidRoute {
        index,
        path: path.to_owned(),
        detail: format!("path_params: {detail}"),
    })?;

    for (name, _) in entries {
        if !names.iter().any(|declared| declared == name) {
            return Err(ContractError::InvalidRoute {
                index,
                path: path.to_owned(),
                detail: format!("path_params rule \"{name}\" does not exist in route path"),
            });
        }
    }

    params::ParamSet::compile(params::ParamSetSpec {
        dimension: "path",
        case_insensitive: false,
        trim_ows: false,
        unknown: UnknownMode::Ignore,
        params: entries,
        allow_required: false,
        name_validator: is_valid_path_param_name,
        name_rule: "invalid path parameter name: must match [A-Za-z_][A-Za-z0-9_]{0,63}",
    })
    .map(Some)
    .map_err(|error| compile_error(index, path, "path_params", error))
}

fn compile_error(index: usize, path: &str, block: &str, error: ParamCompileError) -> ContractError {
    match error {
        ParamCompileError::UnknownRuleField { name, field } => ContractError::UnknownField {
            scope: format!("route #{index} {block}.params.{name}"),
            name: field,
        },
        ParamCompileError::UnknownRequiredIfField { name, field } => ContractError::UnknownField {
            scope: format!("route #{index} {block}.params.{name}.required_if"),
            name: field,
        },
        ParamCompileError::Invalid { name, detail } => ContractError::InvalidRoute {
            index,
            path: path.to_owned(),
            detail: match name {
                Some(name) => format!("{block} rule \"{name}\": {detail}"),
                None => format!("{block}: {detail}"),
            },
        },
    }
}

fn join_path(prefix: &str, child: &str) -> String {
    if prefix == "/" {
        child.to_owned()
    } else if child == "/" {
        prefix.to_owned()
    } else {
        format!("{prefix}{child}")
    }
}

fn validate_route_path(index: usize, path: &str) -> Result<(), ContractError> {
    let invalid = |detail: &str| ContractError::InvalidRoute {
        index,
        path: path.to_owned(),
        detail: detail.into(),
    };

    if !path.starts_with('/') {
        return Err(invalid("path must start with '/'"));
    }
    if path.len() > 1 && path.ends_with('/') {
        return Err(invalid("trailing slash is not allowed"));
    }
    if path.contains(['?', '#']) {
        return Err(invalid("path must not contain '?' or '#'"));
    }
    Ok(())
}

fn validate_prefix(index: usize, prefix: &str) -> Result<(), ContractError> {
    let invalid = |detail: &str| ContractError::InvalidRoute {
        index,
        path: prefix.to_owned(),
        detail: detail.into(),
    };

    if !prefix.starts_with('/') {
        return Err(invalid("group prefix must start with '/'"));
    }
    if prefix.len() > 1 && prefix.ends_with('/') {
        return Err(invalid("group prefix must not end with '/'"));
    }
    if prefix.len() > 512 {
        return Err(invalid("group prefix is longer than 512 characters"));
    }
    if prefix.contains(['?', '#']) {
        return Err(invalid("group prefix must not contain '?' or '#'"));
    }
    if prefix.contains(['{', '}']) {
        return Err(invalid(
            "group prefix must be literal (no {name} / {*rest})",
        ));
    }
    Ok(())
}

fn compile_path_regex(index: usize, path: &str, pattern: &str) -> Result<Regex, ContractError> {
    if pattern.chars().count() > 256 {
        return Err(ContractError::InvalidRoute {
            index,
            path: path.to_owned(),
            detail: "path_regex is longer than 256 characters".into(),
        });
    }

    Regex::new(&format!("^(?:{pattern})$")).map_err(|error| ContractError::InvalidRoute {
        index,
        path: path.to_owned(),
        detail: format!("invalid path_regex: {error}"),
    })
}

/// 从路由模式里取出 `{name}` / `{*rest}` 的参数名，并校验语法与重复。
///
/// 参数名必须是 `[A-Za-z_][A-Za-z0-9_]*`（最长 64）：matchit 对名字几乎不设限，
/// 非法名字会被它当成正常参数（能匹配任意段），但规则名又会被 `is_valid_path_param_name`
/// 拒绝，结果是一个“永远无法被类型/范围/regex 约束”的裸通配段。
fn path_param_names(path: &str) -> Result<Vec<String>, String> {
    let bytes = path.as_bytes();
    let mut names = Vec::new();
    let mut seen = BTreeSet::new();
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] != b'{' {
            index += 1;
            continue;
        }

        // matchit 用 `{{` / `}}` 表示字面量花括号，不是参数。
        if bytes.get(index + 1) == Some(&b'{') {
            index += 2;
            continue;
        }

        let Some(close) = path[index + 1..].find('}') else {
            index += 1;
            continue;
        };
        let inner = &path[index + 1..index + 1 + close];
        let name = inner.strip_prefix('*').unwrap_or(inner);
        if name.is_empty() {
            return Err("empty path parameter name".into());
        }
        if !is_valid_path_param_name(name) {
            return Err(format!(
                "invalid path parameter name \"{name}\": must match [A-Za-z_][A-Za-z0-9_]{{0,63}}"
            ));
        }
        if !seen.insert(name.to_owned()) {
            return Err(format!("duplicate path parameter \"{name}\""));
        }
        names.push(name.to_owned());
        index += close + 2;
    }

    Ok(names)
}

fn is_valid_path_param_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    name.len() <= 64
        && (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn prefix_contains(prefix: &str, path: &str) -> bool {
    if prefix == "/" {
        return path.starts_with('/');
    }
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// RFC 9110 token：`!#$%&'*+-.^_`|~` + ALPHA + DIGIT。
fn is_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

/// RFC 9110 field-name（token），最长 128。
fn is_valid_header_name(name: &str) -> bool {
    name.len() <= 128 && is_token(name)
}

/// 方法名：同样是 RFC 9110 token，最长 32。它会被原样写进 `Allow` 响应头与日志，
/// 因此必须挡住 CR/LF（响应头注入）、逗号（`Allow` 会被当成多个方法）与控制字符。
fn is_valid_method(method: &str) -> bool {
    method.len() <= 32 && is_token(method)
}

/// Query / Body 字段名：1–128 个 UTF-8 字符，不能为空、不能含控制字符；区分大小写。
fn is_valid_param_name(name: &str) -> bool {
    let chars = name.chars().count();
    (1..=128).contains(&chars) && !name.chars().any(char::is_control)
}
