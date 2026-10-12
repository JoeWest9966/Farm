//! 契约闭集错误的 HTTP 投影（ticket #11）。
//!
//! ADR-0001 决策 6：错误码是**闭集**，HTTP 响应体与 WS 文本帧共用同一信封
//! （[`farm_sdk::ContractError`]）。本模块只负责把信封放上 HTTP 面：
//! 状态码取 `ErrorCode::http_status()`，体就是信封本身——客户端永远看到
//! 同一个 JSON 形状，包括 401/404/405/501 这些骨架回答。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use farm_core::tracker::TrackerError;
use farm_sdk::ContractError;

/// 契约错误的 axum 投影。handler 一律返回它，不自己拼状态码。
#[derive(Clone, Debug)]
pub struct ApiError(pub ContractError);

impl From<ContractError> for ApiError {
    fn from(err: ContractError) -> Self {
        Self(err)
    }
}

/// Tracker 服务的错误投影：闭集码不变，只是少写一层 `to_contract_error()`。
impl From<TrackerError> for ApiError {
    fn from(err: TrackerError) -> Self {
        Self(err.into())
    }
}

/// 宿主内部错误投影到闭集信封（`CoreError::to_contract_error`：只有
/// `internal` / `service_unavailable` 两种形状对客户端可见）。
impl From<farm_core::CoreError> for ApiError {
    fn from(err: farm_core::CoreError) -> Self {
        Self(err.to_contract_error())
    }
}

impl From<ApiError> for ContractError {
    fn from(err: ApiError) -> Self {
        err.0
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // `http_status()` 是闭集里的 400/401/403/404/405/409/500/501/503，
        // 全部是合法 HTTP 状态码；兜底只为满足 `from_u16` 的类型签名。
        let status = StatusCode::from_u16(self.0.http_status())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(self.0)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use farm_sdk::ErrorCode;

    #[test]
    fn every_closed_set_code_maps_to_an_http_status_the_response_layer_can_use() {
        for code in ErrorCode::ALL {
            let status = StatusCode::from_u16(code.http_status());
            assert!(
                status.is_ok(),
                "{} 的 HTTP 状态码 {} 不是合法状态码",
                code.as_str(),
                code.http_status()
            );
        }
    }

    #[tokio::test]
    async fn the_response_body_is_the_contract_error_envelope() {
        let response = ApiError(ContractError::unauthorized("missing or invalid orchestrator op token"))
            .into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let parsed: ContractError = serde_json::from_slice(&body).expect("envelope json");
        assert_eq!(parsed.code, ErrorCode::Unauthorized);
        assert_eq!(parsed.http_status(), 401);
        assert!(!parsed.retryable);
    }
}
