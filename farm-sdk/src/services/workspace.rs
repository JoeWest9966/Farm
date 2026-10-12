//! Workspace 服务的占位契约形状（GLOSSARY：拥有 spec ↔ 集成分支 ↔ worktrees 映射）。

use serde::{Deserialize, Serialize};

/// worktree 状态（占位闭集）。
#[derive(
    Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeState {
    Creating,
    Ready,
    /// 映射在而目录缺/损（ADR-0005 §9 启动对账标记）。
    Broken,
    Removed,
}

/// worktree 引用。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorktreeRef {
    pub ticket_id: String,
    pub path: String,
    pub branch: String,
    pub state: WorktreeState,
}

/// spec ↔ 集成分支 ↔ worktrees 映射（worktree 半写：映射先写、worktree 后建）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceMapping {
    pub spec_id: String,
    pub integration_branch: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub worktrees: Vec<WorktreeRef>,
}

/// `GET /v1/workspace/mappings` 的响应信封（占位）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MappingListResponse {
    pub mappings: Vec<WorkspaceMapping>,
}
