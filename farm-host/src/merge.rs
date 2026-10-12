//! Merge 服务契约面（ticket #18 打通）：`POST /v1/merge/proposals`。
//!
//! GLOSSARY「Merge 服务」：agent 只能**提议**；宿主机械校验（worktree 干净、FF 可行、
//! 测试过）后执行**真正的 merge**。完成信号 = 集成分支上该票的 merge commit（可机械断言，
//! 见 #26）；agent 自述完成不产生任何完成信号。
//!
//! 错误一律走契约闭集（`MergeError::to_contract_error`：拒收 = 具体闭集码 + worktree 保留）。

use axum::body::Bytes;
use axum::extract::State;
use axum::Json;
use farm_core::AppState;
use farm_sdk::services::merge::{MergeProposal, MergeProposalResponse};
use farm_sdk::ContractError;

use crate::error::ApiError;

/// `POST /v1/merge/proposals`：agent 提议 merge；宿主校验后执行，返回提议终态。
///
/// 响应只带 `proposal_id` + `state`（契约闭集态）；merge commit 链接经 merge 事件
/// （总线/事件日志）与集成分支本身可查——完成信号是机械事实，不靠响应自述。
pub async fn propose(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<MergeProposalResponse>, ApiError> {
    let proposal: MergeProposal = serde_json::from_slice(&body).map_err(|e| {
        ApiError(ContractError::invalid_request(format!(
            "malformed merge proposal: {e}"
        )))
    })?;
    let report = state
        .merge
        .propose(&state, &proposal)
        .await
        .map_err(|err| ApiError(err.to_contract_error()))?;
    Ok(Json(MergeProposalResponse {
        proposal_id: report.proposal_id,
        state: report.state,
    }))
}
