//! Agent-Runner 服务的占位契约形状（GLOSSARY：统一接口派发与管理 agent）。
//!
//! #16 负责本地后端（spawn `pi --mode rpc`）与判据阶梯；此处只固定路由形状与信封。

use serde::{Deserialize, Serialize};

/// attempt 状态（占位闭集；第四终态 `interrupted` 见 GLOSSARY）。
#[derive(
    Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    Running,
    /// 判据阶梯判终态（`agent_settled` + `stopReason === "stop"` 双条件）。
    Settled,
    Failed,
    /// 宿主重启导致的第四终态：非成功非失败，worktree 不清。
    Interrupted,
    Aborted,
}

/// 派发请求。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DispatchRequest {
    pub issue_id: String,
    /// 派内容物（skill / extension / runtime_pins）装配标识。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

/// 派发结果。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DispatchResponse {
    pub attempt_id: String,
    pub state: AttemptState,
}

/// steer：向运行中的 attempt 注入指令（GLOSSARY：人类经 steer/follow_up 回应）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SteerRequest {
    pub attempt_id: String,
    pub message: String,
}

/// abort：中止 attempt（30s 独立死线的 abort 探针由 #16 实现）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AbortRequest {
    pub attempt_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
