//! 事件总线服务的占位契约形状（GLOSSARY：编排器与 agent-runner 的统一事件回流面）。

use serde::{Deserialize, Serialize};

use crate::events::{EventEnvelope, EventKind};

/// 事件历史查询（占位）。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EventHistoryQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<EventKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// 事件历史响应（占位）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EventHistoryResponse {
    pub events: Vec<EventEnvelope>,
    /// 是否因保留窗口被截断（ADR-0005 §10）。
    #[serde(default)]
    pub truncated: bool,
}

/// WS 订阅建立后宿主回送的首帧（与错误信封区分：有 `type` 无 `code`）。
pub use crate::events::WsHello;
