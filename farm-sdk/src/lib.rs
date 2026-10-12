//! # farm-sdk — Farm 编排器契约（orchestrator contract）纯类型 crate
//!
//! ADR-0003 五分法：`farm-sdk` 是契约类型 + 宿主管理 API schema + 闭集错误码 +
//! 事件闭集的**单一源**。面板、编排器、插件都只 depend 它；宿主也 depend 它。
//!
//! 依赖只有 serde / serde_json / schemars（序列化与 schema 生成），**零运行时依赖**：
//! 没有 tokio、没有 IO、没有数据库，因此可以被 wasm 面板与独立插件进程同样链接。
//!
//! ## 文档是生成的
//! [`docs`] 模块把本 crate 的类型渲染成 `docs/contract/schema.json` 与
//! `docs/contract/README.md`（路由表 + 闭集清单）。`cargo run -p farm-sdk
//! --example gen_contract_docs` 重新生成；测试断言已签入的文件与生成结果逐字节一致，
//! 所以文档不可能与类型漂移。

pub mod admin;
pub mod docs;
pub mod error;
pub mod events;
pub mod handshake;
pub mod routes;
pub mod services;

pub use error::{ContractError, ErrorCode};
pub use events::{EventEnvelope, EventKind, NewEvent, WsHello};
pub use handshake::{
    is_compatible, AuthScheme, ClientInfo, ClientKind, HandshakeRequest, HandshakeResponse,
    ADMIN_PREFIX, CONTRACT_MAJOR, CONTRACT_PREFIX, CONTRACT_VERSION, HANDSHAKE_PATH, HOST_VERSION,
};
pub use routes::{contract_path, AdminRouteSpec, RouteSpec, ADMIN_ROUTES, CONTRACT_ROUTES};
pub use services::ServiceName;
pub use services::agent_runner::{
    AbortRequest, AbortResponse, AttemptState, DispatchRequest, DispatchResponse, Disposition,
    SteerRequest, SteerResponse,
};

/// farm-sdk 的 crate 版本（与契约版本分离）。
pub const SDK_VERSION: &str = env!("CARGO_PKG_VERSION");
