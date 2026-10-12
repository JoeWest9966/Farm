//! Workspace 服务契约面（ticket #14）：`GET /v1/workspace/mappings`。
//!
//! GLOSSARY：Workspace 服务拥有 spec ↔ 集成分支 ↔ worktree 映射；面板与 merge 服务
//! **读**映射、不拥有它。这里只把宿主私有存储里的映射投影成契约视图
//! （[`farm_core::workspace::WorkspaceService::mapping_view`]）——SQLite schema
//! 永不外泄（ADR-0005 §5），对外只有 farm-sdk 的 [`MappingListResponse`]。

use axum::extract::State;
use axum::Json;
use farm_core::AppState;
use farm_sdk::services::workspace::MappingListResponse;

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
