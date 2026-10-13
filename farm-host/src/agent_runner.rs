//! Agent-Runner 契约面（ticket #16 打通）：派发 / steer / abort 三条路由。
//!
//! 路由表来自 [`farm_sdk::routes::CONTRACT_ROUTES`]（单一源），路径在 #11 就定好了，
//! 本模块只是把占位 handler 换成真 handler。两条注入路由的 attempt_id **以路径为准**
//! （`/attempts/{attempt_id}/...`），请求体只带消息本身——id 在路径里再抄一遍没有意义。
//!
//! 错误一律走契约闭集（ADR-0001 决策 6）：`RunnerError::to_contract_error` 把宿主内部
//! 形状投影成 `not_found` / `conflict` / `service_unavailable` / `invalid_request`。

use axum::extract::{Path, State};
use axum::Json;
use farm_core::AppState;
use farm_sdk::services::agent_runner::{
    AbortRequest, AbortResponse, ClearQueueResponse, DispatchRequest, DispatchResponse, SteerRequest,
    SteerResponse,
};
use farm_sdk::ContractError;

use crate::error::ApiError;

/// `POST /v1/agent-runner/dispatches`：派发一个 attempt（本地后端 spawn `pi --mode rpc`）。
///
/// 同步返回 `attempt_id` + `running`：attempt 的生命周期由监督任务持有（判据阶梯），
/// 终态经事件总线回流（`dispatch` → `attempt_started` → `agent_settled`/`attempt_finished`）。
pub async fn dispatch(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> Result<Json<DispatchResponse>, ApiError> {
    let request: DispatchRequest = serde_json::from_slice(&body).map_err(|e| {
        ApiError(farm_sdk::ContractError::invalid_request(format!(
            "malformed dispatch request: {e}"
        )))
    })?;
    let response = state
        .agent_runner
        .dispatch(&state, &request)
        .await
        .map_err(|err| ApiError(err.to_contract_error()))?;
    Ok(Json(response))
}

/// `POST /v1/agent-runner/attempts/{attempt_id}/steer`：向运行中的 attempt 注入指令。
///
/// attempt_id 以**路径**为准；请求体是 `{"message": "..."}`。pi 给的 disposition
/// （`queued` / `deferred` / `rejected`…）随响应回到调用方，并发 `steer` 事件回流总线。
pub async fn steer(
    State(state): State<AppState>,
    Path(attempt_id): Path<String>,
    body: axum::body::Bytes,
) -> Result<Json<SteerResponse>, ApiError> {
    let message = parse_message(&body)?;
    let response = state
        .agent_runner
        .steer(&SteerRequest { attempt_id, message })
        .await
        .map_err(|err| ApiError(err.to_contract_error()))?;
    Ok(Json(response))
}

/// `POST /v1/agent-runner/attempts/{attempt_id}/abort`：中止 attempt。
///
/// 只保证「abort 已受理」：是否真的停下由 abort 探针的 30s 独立死线裁决
/// （pi 不理 abort 就强杀）。attempt_id 以**路径**为准。
pub async fn abort(
    State(state): State<AppState>,
    Path(attempt_id): Path<String>,
    body: axum::body::Bytes,
) -> Result<Json<AbortResponse>, ApiError> {
    let reason = parse_reason(&body)?;
    let response = state
        .agent_runner
        .abort(
            &state,
            &AbortRequest {
                attempt_id,
                reason,
            },
        )
        .await
        .map_err(|err| ApiError(err.to_contract_error()))?;
    Ok(Json(response))
}

/// `POST /v1/agent-runner/attempts/{attempt_id}/follow-up`：等 attempt 停下才送达的追加任务。
///
/// 与 steer 同一受理形状（`{message}` → `{attempt_id, state, disposition}`）：面板的
/// HITL 输入框在 attempt 已收尾时改走这一条（steer 只对运行中的 attempt 有意义）。
/// attempt_id 以**路径**为准。
pub async fn follow_up(
    State(state): State<AppState>,
    Path(attempt_id): Path<String>,
    body: axum::body::Bytes,
) -> Result<Json<SteerResponse>, ApiError> {
    let message = parse_message(&body)?;
    let response = state
        .agent_runner
        .follow_up(&attempt_id, &message)
        .await
        .map_err(|err| ApiError(err.to_contract_error()))?;
    Ok(Json(response))
}

/// `POST /v1/agent-runner/attempts/{attempt_id}/clear-queue`：清掉排队的注入。
///
/// 面板 HITL 的「放弃这次改道」：steering / followUp / 被推迟的注入一并清掉，
/// 回报清了几条。体可选（空体 = 没给理由也没有别的参数）。attempt_id 以**路径**为准。
pub async fn clear_queue(
    State(state): State<AppState>,
    Path(attempt_id): Path<String>,
    body: axum::body::Bytes,
) -> Result<Json<ClearQueueResponse>, ApiError> {
    // 体现在只作占位校验：clear_queue 没有别的参数，空体与 `{}` 都接受。
    if !body.iter().all(u8::is_ascii_whitespace) {
        let parsed: serde_json::Value = serde_json::from_slice(&body).map_err(|e| {
            ApiError(ContractError::invalid_request(format!(
                "malformed clear-queue request: {e}"
            )))
        })?;
        if !parsed.is_null() && !parsed.is_object() {
            return Err(ApiError(ContractError::invalid_request(
                "clear-queue takes no parameters",
            )));
        }
    }
    let cleared = state
        .agent_runner
        .clear_queue(&attempt_id)
        .await
        .map_err(|err| ApiError(err.to_contract_error()))?;
    Ok(Json(ClearQueueResponse {
        attempt_id,
        cleared,
    }))
}

/// 解出注入消息（`{"message": "..."}`）；空消息是 `invalid_request`。
fn parse_message(body: &[u8]) -> Result<String, ApiError> {
    #[derive(serde::Deserialize)]
    struct Body {
        message: String,
    }
    let parsed: Body = serde_json::from_slice(body).map_err(|e| {
        ApiError(farm_sdk::ContractError::invalid_request(format!(
            "malformed steer request: {e}"
        )))
    })?;
    if parsed.message.trim().is_empty() {
        return Err(ApiError(farm_sdk::ContractError::invalid_request(
            "steer requires a non-empty message",
        )));
    }
    Ok(parsed.message)
}

/// 解出 abort 理由（`{"reason": "..."}`，可选）。
fn parse_reason(body: &[u8]) -> Result<Option<String>, ApiError> {
    #[derive(serde::Deserialize)]
    struct Body {
        #[serde(default)]
        reason: Option<String>,
    }
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    serde_json::from_slice::<Body>(body)
        .map(|parsed| parsed.reason)
        .map_err(|e| {
            ApiError(farm_sdk::ContractError::invalid_request(format!(
                "malformed abort request: {e}"
            )))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use farm_core::HostConfig;
    use farm_sdk::ErrorCode;

    fn state() -> AppState {
        AppState::new_in_memory(HostConfig {
            bind: "127.0.0.1:3721".parse().expect("bind"),
            op_token: "unit-op-token".into(),
            admin_token: None,
            db_path: std::env::temp_dir().join("farm-host-agent-runner-unit.db"),
        })
        .expect("in-memory state")
    }

    #[tokio::test]
    async fn a_dispatch_without_an_issue_id_is_a_closed_set_invalid_request() {
        let err = dispatch(
            State(state()),
            axum::body::Bytes::from_static(b"{\"issue_id\":\"  \"}"),
        )
        .await
        .expect_err("empty issue id must be rejected");
        assert_eq!(err.0.code, ErrorCode::InvalidRequest);
    }

    #[tokio::test]
    async fn a_malformed_dispatch_body_is_a_closed_set_invalid_request() {
        let err = dispatch(
            State(state()),
            axum::body::Bytes::from_static(b"{\"issue_id\": "),
        )
        .await
        .expect_err("malformed json must be rejected");
        assert_eq!(err.0.code, ErrorCode::InvalidRequest);
    }

    #[tokio::test]
    async fn steering_an_unknown_attempt_is_a_closed_set_not_found() {
        let err = steer(
            State(state()),
            Path("att-nope".to_string()),
            axum::body::Bytes::from_static(b"{\"message\":\"hi\"}"),
        )
        .await
        .expect_err("unknown attempt");
        assert_eq!(err.0.code, ErrorCode::NotFound);
    }

    #[tokio::test]
    async fn an_empty_steer_message_is_a_closed_set_invalid_request() {
        let err = steer(
            State(state()),
            Path("att-nope".to_string()),
            axum::body::Bytes::from_static(b"{\"message\":\"   \"}"),
        )
        .await
        .expect_err("empty message must be rejected");
        assert_eq!(err.0.code, ErrorCode::InvalidRequest);
    }

    #[tokio::test]
    async fn an_empty_abort_body_is_accepted_as_no_reason() {
        // abort 的理由是可选的：空体 = 没给理由（不是格式错误）。
        let state = state();
        let err = abort(
            State(state),
            Path("att-nope".to_string()),
            axum::body::Bytes::new(),
        )
        .await
        .expect_err("unknown attempt still fails, but not on the body");
        assert_eq!(err.0.code, ErrorCode::NotFound);
    }
}
