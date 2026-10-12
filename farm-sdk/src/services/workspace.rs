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

/// 建立 ticket worktree 的请求（`POST /v1/workspace/worktrees`，ticket #26 加法接通）。
///
/// GLOSSARY「Workspace 服务」负责 worktree 的创建；编排器以契约客户端身份触发它
/// （write-ahead 映射先落、`git worktree add` 后建）。路径用字符串（契约面只过 JSON），
/// 由宿主翻译成内部 `EstablishRequest`。这是**加法**契约面：既有的 `GET /v1/workspace/mappings`
/// 只读投影不变，本条只补「编排器可委托建 worktree」这一个原语——没有它，契约客户端
/// 无法驱动整链（claim 之后、dispatch 之前无处建 worktree）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorktreeEstablishRequest {
    pub spec_id: String,
    pub integration_branch: String,
    pub ticket_id: String,
    /// 源仓库路径（`git worktree add -C` 的目标）。
    pub repo_path: String,
    /// 要建的 worktree 目录路径。
    pub worktree_path: String,
    /// worktree 分支名（不存在则从 `start_point` 新建）。
    pub worktree_branch: String,
    /// 建分支的起点（commit/branch）；缺省基于 HEAD。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_point: Option<String>,
}
