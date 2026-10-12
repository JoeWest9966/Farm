//! Workspace 服务契约面（ticket #14）：`GET /v1/workspace/mappings`。
//!
//! GLOSSARY：Workspace 服务拥有 spec ↔ 集成分支 ↔ worktree 映射；面板与 merge 服务
//! **读**映射、不拥有它。这里只把宿主私有存储里的映射投影成契约视图
//! （[`farm_core::workspace::WorkspaceService::mapping_view`]）——SQLite schema
//! 永不外泄（ADR-0005 §5），对外只有 farm-sdk 的 [`MappingListResponse`]。

use axum::body::Bytes;
use axum::extract::State;
use axum::Json;
use farm_core::AppState;
use farm_core::workspace::EstablishRequest;
use farm_sdk::services::workspace::{MappingListResponse, WorktreeEstablishRequest, WorktreeRef};

use crate::error::ApiError;

/// `GET /v1/workspace/mappings`：spec ↔ 集成分支 ↔ worktree 映射视图。
pub async fn list_mappings(
    State(state): State<AppState>,
) -> Result<Json<MappingListResponse>, ApiError> {
    let mappings = state
        .workspace
        .mapping_view()
        .map_err(|e| ApiError(e.to_contract_error()))?;
    Ok(Json(MappingListResponse { mappings }))
}

/// `POST /v1/workspace/worktrees`：契约客户端委托 workspace 建 ticket worktree（#26 接通）。
///
/// GLOSSARY「Workspace 服务」负责 worktree 的创建；编排器（契约客户端）在 claim 之后、
/// dispatch 之前经此原语触发它。宿主把契约请求翻译成内部 [`EstablishRequest`]
/// （write-ahead 映射先落、`git worktree add` 后建），返回投影成的 [`WorktreeRef`]。
///
/// 错误一律走闭集：缺字段 → `invalid_request`；git 失败 → `internal`（宿主故障，
/// 见 [`farm_core::workspace::WorkspaceError::to_contract_error`]）。
pub async fn establish_worktree(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<WorktreeRef>, ApiError> {
    let request: WorktreeEstablishRequest = serde_json::from_slice(&body).map_err(|e| {
        ApiError(farm_sdk::ContractError::invalid_request(format!(
            "malformed worktree establish request: {e}"
        )))
    })?;
    if request.ticket_id.trim().is_empty()
        || request.repo_path.trim().is_empty()
        || request.worktree_path.trim().is_empty()
        || request.worktree_branch.trim().is_empty()
        || request.integration_branch.trim().is_empty()
    {
        return Err(ApiError(farm_sdk::ContractError::invalid_request(
            "a worktree establish request needs ticket_id, integration_branch, repo_path, worktree_path, worktree_branch",
        )));
    }

    let repo_path_str = request.repo_path.clone();
    let spec_id_str = request.spec_id.clone();
    let row = state
        .workspace
        .establish(&EstablishRequest {
            spec_id: request.spec_id,
            integration_branch: request.integration_branch,
            ticket_id: request.ticket_id,
            repo_path: std::path::PathBuf::from(&request.repo_path),
            worktree_path: std::path::PathBuf::from(&request.worktree_path),
            worktree_branch: request.worktree_branch,
            start_point: request.start_point,
        })
        .map_err(|e| ApiError(e.to_contract_error()))?;

    // 装载该仓库的 workflow 配置（GLOSSARY「Workflow 配置」= 投影开关的源）。
    // 键用两把，桥接本仓既有的 keying 分工：merge 服务的投影按 `mapping.repo_path`
    // 读生效配置（farm_core::merge::projection::switches_for），而宿主管理面
    // `GET /admin/workflow/config/{repo_id}` 按单段 repo_id 读——两者各装一份同一份
    // `.farm/workflow.json`，让机械投影与契约客户端/面板都读得到。建立 worktree 这一刻
    // （宿主首次在该仓库上编排）把配置装成 LKG。只在配置文件存在时装载：仓库没配
    // workflow 就保持「无生效配置」（投影全关的安全默认），不滥发 needs_attention。
    let repo_root = std::path::Path::new(&repo_path_str);
    if repo_root.join(".farm/workflow.json").exists() {
        let _ = state.workflow.load_repo(&repo_path_str, repo_root);
        let _ = state.workflow.load_repo(&spec_id_str, repo_root);
    }

    Ok(Json(WorktreeRef {
        ticket_id: row.ticket_id,
        path: row.worktree_path,
        branch: row.worktree_branch,
        state: row.status.to_worktree_state(),
    }))
}
