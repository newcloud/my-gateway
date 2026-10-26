//! my_gateway 的纯逻辑核心：契约解析、路由编译、请求判定。
//!
//! 约束：不依赖 nginx、不做网络/文件 I/O。所有决策都能在 `cargo test` 里跑。
//! 协议约定（字段、语义、演进规则）见 [`docs/PROTOCOL.md`](../../../docs/PROTOCOL.md)。

mod contract;
mod gateway;

pub use gateway::{ContractError, Decision, Gateway, Outcome, RequestCtx};
