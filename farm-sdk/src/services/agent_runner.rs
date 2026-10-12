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

/// 注入处置（disposition）：pi 对 `prompt`/`steer`/`follow_up` 的受理裁决。
///
/// 1.0.2 的 RPC 响应带 `disposition` 字段（0.84.3 还没有），其取值闭集未在本仓固定，
/// 故按**开放字符串**建模，只对已知值提供判据：未知值按「已受理、语义未知」处理——
/// 记录版本偏差而不是让请求失败。`deferred` 是「推迟到 settle 后执行」
/// （pi 侧 `_deferredSettledActions`）。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq, schemars::JsonSchema)]
#[serde(transparent)]
pub struct Disposition(pub String);

impl Disposition {
    /// 已知取值（文档化 1.0.2 观察到的一组；闭集未冻结，仅作判据用）。
    pub const ACCEPTED: &'static str = "accepted";
    pub const QUEUED: &'static str = "queued";
    pub const DEFERRED: &'static str = "deferred";
    pub const REJECTED: &'static str = "rejected";
    pub const EXPANDED: &'static str = "expanded";

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 已受理（accepted/queued/expanded/deferred；rejected 与未知值都不算）。
    ///
    /// 受理的权威判据其实是响应的 `success` 字段（文档：`success: true` 即 accepted/queued/
    /// handled）；disposition 只描述**以什么方式**受理，故这里只做已知值判据。
    pub fn is_accepted(&self) -> bool {
        matches!(
            self.0.as_str(),
            Self::ACCEPTED | Self::QUEUED | Self::EXPANDED | Self::DEFERRED
        )
    }

    /// 是否是已知取值（未知值 = 版本偏差：记录，不断失败）。
    pub fn is_known(&self) -> bool {
        matches!(
            self.0.as_str(),
            Self::ACCEPTED | Self::QUEUED | Self::DEFERRED | Self::REJECTED | Self::EXPANDED
        )
    }

    /// 是否「推迟到 settle 后执行」。
    pub fn is_deferred(&self) -> bool {
        self.0 == Self::DEFERRED
    }

    /// 是否被 pi 拒绝（受理前失败）。
    pub fn is_rejected(&self) -> bool {
        self.0 == Self::REJECTED
    }
}

impl std::fmt::Display for Disposition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Disposition {
    fn from(v: &str) -> Self {
        Self(v.to_string())
    }
}

/// abort：中止 attempt（30s 独立死线的 abort 探针由 #16 实现）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AbortRequest {
    pub attempt_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// steer 的受理结果：attempt 现状 + pi 给的 disposition（回流进事件总线）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SteerResponse {
    pub attempt_id: String,
    pub state: AttemptState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition: Option<Disposition>,
}

/// abort 的受理结果：attempt 现状（abort 探针可能仍在跑，故只是「已受理」）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AbortResponse {
    pub attempt_id: String,
    pub state: AttemptState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
