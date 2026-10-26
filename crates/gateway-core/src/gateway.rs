//! 路由编译与判定。这里不依赖 nginx，也不做 I/O，可直接 `cargo test`。

use core::fmt;

use matchit::Router;

use crate::contract::{Contract, SUPPORTED_PROTOCOL, Unmatched};

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
            ContractError::UnknownField { scope, name } => write!(
                f,
                "unknown field \"{name}\" in {scope}; see docs/PROTOCOL.md for the allowed fields"
            ),
            ContractError::InvalidRoute {
                index,
                path,
                detail,
            } => write!(f, "route #{index} \"{path}\": {detail}"),
            ContractError::EmptyMethod { index, path } => {
                write!(f, "route #{index} \"{path}\": empty method")
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
}

impl Decision {
    pub fn reason(self) -> &'static str {
        match self {
            Decision::Allow => "allow",
            Decision::RouteNotDeclared => "route-not-declared",
            Decision::MethodNotAllowed => "method-not-allowed",
        }
    }
}

/// 判定结果 + 需要回写给客户端的附加信息。
#[derive(Debug, Clone, Copy)]
pub struct Outcome<'a> {
    pub decision: Decision,
    /// 405 时回写 `Allow` 头。
    pub allow: Option<&'a str>,
    /// 路径未声明，但契约要求转发给后端（`unmatched: "forward"`）。此时 `decision` 是 `Allow`，
    /// 适配层只做日志、继续走 proxy，不写响应头。
    pub forwarded: bool,
}

/// 请求上下文：只保留判定需要的东西，方便在测试里构造。
#[derive(Debug, Clone, Copy)]
pub struct RequestCtx<'a> {
    pub method: &'a str,
    pub path: &'a str,
}

/// 编译好的契约。
pub struct Gateway {
    router: Router<usize>,
    allow_headers: Vec<String>,
    protocol: u32,
    version: Option<String>,
    ttl: Option<u64>,
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

        let mut router = Router::new();
        let mut allow_headers = Vec::with_capacity(contract.routes.len());

        for (index, route) in contract.routes.iter().enumerate() {
            if let Some(name) = route.extra.keys().next() {
                return Err(ContractError::UnknownField {
                    scope: format!("route #{index}"),
                    name: name.clone(),
                });
            }
            if !route.path.starts_with('/') {
                return Err(ContractError::InvalidRoute {
                    index,
                    path: route.path.clone(),
                    detail: "path must start with '/'".into(),
                });
            }
            if route.path.len() > 1 && route.path.ends_with('/') {
                return Err(ContractError::InvalidRoute {
                    index,
                    path: route.path.clone(),
                    detail: "trailing slash is not allowed".into(),
                });
            }
            if route.methods.iter().any(|m| m.trim().is_empty()) {
                return Err(ContractError::EmptyMethod {
                    index,
                    path: route.path.clone(),
                });
            }

            router
                .insert(route.path.as_str(), index)
                .map_err(|e| ContractError::InvalidRoute {
                    index,
                    path: route.path.clone(),
                    detail: e.to_string(),
                })?;

            allow_headers.push(
                route
                    .methods
                    .iter()
                    .map(|m| m.trim())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }

        Ok(Self {
            router,
            allow_headers,
            protocol: contract.protocol,
            version: contract.version,
            ttl: contract.ttl,
            route_count: contract.routes.len(),
            forward_unmatched: contract.unmatched == Some(Unmatched::Forward),
        })
    }

    pub fn check(&self, ctx: &RequestCtx<'_>) -> Outcome<'_> {
        let Ok(matched) = self.router.at(ctx.path) else {
            // 未声明：默认拒绝；契约显式 `unmatched: "forward"` 时交给后端处理。
            return if self.forward_unmatched {
                Outcome {
                    decision: Decision::Allow,
                    allow: None,
                    forwarded: true,
                }
            } else {
                Outcome {
                    decision: Decision::RouteNotDeclared,
                    allow: None,
                    forwarded: false,
                }
            };
        };

        let index = *matched.value;
        let allow_header = &self.allow_headers[index];

        if allow_header.is_empty()
            || allow_header
                .split(", ")
                .any(|m| m.eq_ignore_ascii_case(ctx.method))
        {
            Outcome {
                decision: Decision::Allow,
                allow: None,
                forwarded: false,
            }
        } else {
            Outcome {
                decision: Decision::MethodNotAllowed,
                allow: Some(allow_header.as_str()),
                forwarded: false,
            }
        }
    }

    pub fn protocol(&self) -> u32 {
        self.protocol
    }

    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    pub fn ttl(&self) -> Option<u64> {
        self.ttl
    }

    pub fn route_count(&self) -> usize {
        self.route_count
    }

    /// 未声明路由是否转发给后端（`unmatched: "forward"`）。
    pub fn forward_unmatched(&self) -> bool {
        self.forward_unmatched
    }
}
