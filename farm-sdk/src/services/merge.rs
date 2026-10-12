//! Merge 服务的占位契约形状（GLOSSARY：agent 只能提议，宿主校验后执行真正的 merge）。

use serde::{Deserialize, Serialize};

/// merge 提议状态（占位闭集）。
#[derive(
    Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MergeProposalState {
    Proposed,
    /// 机械闸门通过（worktree 干净、FF 可行、测试过）。
    Validated,
    Merged,
    Rejected,
}

impl MergeProposalState {
    /// 闭集的权威清单。
    ///
    /// 宿主私有存储的 `CHECK` 约束由它**生成**（与 `EventKind::ALL` 同款纪律，
    /// ADR-0001 决策 6）：闭集的单一源仍在 farm-sdk，SQL 只是第二道闸。
    pub const ALL: [MergeProposalState; 4] = [
        MergeProposalState::Proposed,
        MergeProposalState::Validated,
        MergeProposalState::Merged,
        MergeProposalState::Rejected,
    ];
}

/// agent 的 merge 提议（提议即全部；执行归宿主）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MergeProposal {
    pub ticket_id: String,
    /// 提议的源 worktree / 分支。
    pub worktree_branch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// 提议受理结果。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MergeProposalResponse {
    pub proposal_id: String,
    pub state: MergeProposalState,
}

/// 人工审阅决策（approve/reject 请求体；宿主管理 API，#25）。
///
/// 管理面操作经**管理 token**（ADR-0002 §1：管理面与契约边界分明），不是契约后门。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MergeReviewDecision {
    /// 审阅人（面板操作者；进审批账）。
    pub reviewer: String,
    /// 拒绝理由（approve 时省略；reject 时可省略，宿主给通用理由）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// 提议详情（面板审阅队列呈现：记录字段 + diff，#25）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MergeProposalDetail {
    pub proposal_id: String,
    pub ticket_id: String,
    pub worktree_branch: String,
    pub integration_branch: String,
    pub state: MergeProposalState,
    pub message: String,
    /// 完成信号（`merged` 时必有）：集成分支上的 merge commit。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_commit: Option<String>,
    /// 拒收梯级（机械闸门或人工拒绝）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejection_stage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejection_detail: Option<String>,
    /// 提议的 diff：集成分支 tip 与 worktree 分支 tip 的差异（面板呈现）。
    pub diff: String,
}

/// 审阅队列（面板主视图，#25）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MergeProposalListResponse {
    pub proposals: Vec<MergeProposalDetail>,
}

/// 审阅操作结果（approve/reject 响应，#25）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MergeReviewResponse {
    pub proposal_id: String,
    pub state: MergeProposalState,
    /// 批准且机械闸门过 → merge commit（完成信号）；拒绝/机械拒收 → `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_commit: Option<String>,
}
