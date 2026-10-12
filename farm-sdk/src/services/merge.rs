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
