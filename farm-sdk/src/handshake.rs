//! 契约版本握手（contract version handshake）。
//!
//! 编排器接入宿主的第一跳：声明自己的契约版本与身份，宿主回送自己支持的
//! 契约版本、五服务清单、事件闭集与鉴权方式。版本不兼容 → 闭集错误
//! `contract_version_unsupported`（HTTP 与 WS 同一形状）。

use serde::{Deserialize, Serialize};

use crate::events::EventKind;
use crate::services::ServiceName;

/// 宿主实现的契约版本（semver）。主版本号变更 = 不兼容。
pub const CONTRACT_VERSION: &str = "1.0.0";

/// 契约主版本号。握手兼容性判定只比主版本号。
pub const CONTRACT_MAJOR: u32 = 1;

/// 宿主软件版本（与契约版本分离：宿主可以发版而不动契约）。
pub const HOST_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 握手路由。契约面上**唯一**不带 `/v1` 前缀的路由。
pub const HANDSHAKE_PATH: &str = "/handshake";

/// 契约路由前缀。
pub const CONTRACT_PREFIX: &str = "/v1";

/// 宿主管理 API 前缀（GLOSSARY：宿主管理 API，与契约边界分明）。
pub const ADMIN_PREFIX: &str = "/admin";

/// 客户端身份种类（闭集）。
#[derive(
    Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    /// 编排器：以插件身份接入的独立进程（ADR-0001）。
    Orchestrator,
    /// 面板：操作台，与编排器同路调用契约。
    Panel,
    /// 其他进程级插件。
    Plugin,
}

/// 客户端自描述。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
    pub kind: ClientKind,
}

/// 握手请求。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HandshakeRequest {
    /// 客户端实现的契约版本，如 `1.0.0`。
    pub contract_version: String,
    pub client: ClientInfo,
}

/// 鉴权方式自描述：编排器据此知道往哪个 header 塞 token。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthScheme {
    /// 契约面（含 `/handshake`、五服务路由与 WS upgrade）的 token header。
    pub http_header: String,
    /// 同时接受 `Authorization: Bearer <token>`。
    pub also_accepts_bearer: bool,
    /// WS upgrade 同样验 token（与 HTTP 同一闭集错误）。
    pub ws_upgrade_requires_token: bool,
    /// 宿主管理 API 的**独立** token header（与管理 token 机制区分）。
    pub admin_header: String,
}

impl Default for AuthScheme {
    fn default() -> Self {
        Self {
            http_header: "X-Farm-Token".to_string(),
            also_accepts_bearer: true,
            ws_upgrade_requires_token: true,
            admin_header: "X-Farm-Admin-Token".to_string(),
        }
    }
}

/// 握手响应。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HandshakeResponse {
    /// 宿主实际使用的契约版本。
    pub contract_version: String,
    /// 宿主兼容的契约版本清单（当前只有 `[CONTRACT_VERSION]`）。
    pub supported_contract_versions: Vec<String>,
    pub host_version: String,
    /// v1 五服务（ADR-0001）。
    pub services: Vec<ServiceName>,
    /// 事件闭集：编排器据此穷举可订阅的事件种类。
    pub event_kinds: Vec<EventKind>,
    pub auth: AuthScheme,
}

impl HandshakeResponse {
    pub fn for_host(host_version: impl Into<String>) -> Self {
        Self {
            contract_version: CONTRACT_VERSION.to_string(),
            supported_contract_versions: vec![CONTRACT_VERSION.to_string()],
            host_version: host_version.into(),
            services: ServiceName::ALL.to_vec(),
            event_kinds: EventKind::ALL.to_vec(),
            auth: AuthScheme::default(),
        }
    }
}

/// 握手兼容性判定：只比主版本号（同主版本内宿主保持兼容）。
pub fn is_compatible(requested: &str) -> bool {
    match requested.split('.').next() {
        Some(major) => major.parse::<u32>().ok() == Some(CONTRACT_MAJOR),
        None => false,
    }
}
