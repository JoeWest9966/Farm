//! 契约面（contract face）：握手 + 五服务占位路由 + 闭集 404/405。
//!
//! 路由表来自 [`farm_sdk::routes::CONTRACT_ROUTES`]（单一源）：宿主按表挂载，
//! 生成文档按表出表，编排器按表拼 URL。除 `GET /v1/events/stream`（WS，见
//! [`crate::events_ws`]）外，本票全部返回闭集 `not_implemented`——服务域实现
//! 归 #13–#18，落地时把占位 handler 换成真 handler 即可，路径不变。

use axum::body::Bytes;
use axum::extract::{MatchedPath, State};
use axum::http::{Method, Uri};
use axum::Json;
use farm_core::AppState;
use farm_sdk::{
    is_compatible, ContractError, HandshakeRequest, HandshakeResponse, ADMIN_PREFIX,
    CONTRACT_PREFIX, CONTRACT_VERSION, HANDSHAKE_PATH, HOST_VERSION,
};

use crate::error::ApiError;

/// `POST /handshake`：编排器接入第一跳（契约面上唯一不带 `/v1` 前缀的路由）。
///
/// 顺序：先验 token（中间件已做）→ 验契约版本兼容性 → 回送宿主自描述。
/// 版本不兼容 → 闭集 `contract_version_unsupported`（400）。
/// 请求体不符合 schema → 闭集 `invalid_request`（400，不用 axum 默认的纯文本拒绝）。
pub async fn handshake(
    State(_state): State<AppState>,
    body: Bytes,
) -> Result<Json<HandshakeResponse>, ApiError> {
    let request: HandshakeRequest = serde_json::from_slice(&body).map_err(|e| {
        ApiError(ContractError::invalid_request(format!(
            "malformed handshake request: {e}"
        )))
    })?;

    if !is_compatible(&request.contract_version) {
        return Err(ApiError(ContractError::contract_version_unsupported(
            format!(
                "host speaks contract version {} but client declared {}",
                CONTRACT_VERSION, request.contract_version
            ),
        )));
    }

    Ok(Json(HandshakeResponse::for_host(HOST_VERSION)))
}

/// 占位路由的统一回答：路由已占位、服务未实现（闭集 `not_implemented` / 501）。
///
/// 消息里带**具体路径**（来自 `MatchedPath`），客户端能知道命中的是哪条契约路由，
/// 而码仍是闭集里的一个。
pub async fn not_implemented_placeholder(matched: MatchedPath) -> ApiError {
    ApiError(ContractError::not_implemented(matched.as_str()))
}

/// 路由存在但方法不对 → 闭集 `method_not_allowed`（405）。
pub async fn method_not_allowed(method: Method, uri: Uri) -> ApiError {
    ApiError(ContractError::method_not_allowed(format!(
        "method {} is not allowed on {}",
        method,
        uri.path()
    )))
}

/// 顶层 404 兜底：任何未挂载的路径。消息按前缀给出可读说明，码仍是 `not_found`。
///
/// 契约面（`/v1`、`/handshake`）、宿主管理面（`/admin`）与面板/资产区各有各的措辞，
/// 但对客户端而言都是同一个闭集码。
pub async fn unknown_route(uri: Uri) -> ApiError {
    let path = uri.path();
    let message = if path == HANDSHAKE_PATH || path.starts_with(CONTRACT_PREFIX) {
        format!("no contract route at {path} on this host")
    } else if path.starts_with(ADMIN_PREFIX) {
        format!("no host admin route at {path} on this host")
    } else {
        format!("no route at {path} on this host")
    };
    ApiError(ContractError::not_found(message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use farm_core::HostConfig;
    use farm_sdk::{ClientInfo, ClientKind, ErrorCode};

    const OP_TOKEN: &str = "unit-op-token";

    fn state() -> AppState {
        AppState::new_in_memory(HostConfig {
            bind: "127.0.0.1:3721".parse().expect("valid bind"),
            op_token: OP_TOKEN.into(),
            admin_token: None,
            db_path: std::env::temp_dir().join("farm-host-unit.db"),
        })
        .expect("in-memory state")
    }

    fn handshake_body(version: &str) -> Bytes {
        let request = HandshakeRequest {
            contract_version: version.to_string(),
            client: ClientInfo {
                name: "unit".into(),
                version: "0.0.0".into(),
                kind: ClientKind::Orchestrator,
            },
        };
        Bytes::from(serde_json::to_vec(&request).expect("serialize"))
    }

    #[tokio::test]
    async fn a_compatible_contract_version_gets_the_host_self_description() {
        let response = handshake(State(state()), handshake_body("1.9.9"))
            .await
            .expect("compatible version must pass the handshake");
        let json = serde_json::to_string(&response.0).expect("serialize");
        assert!(json.contains(CONTRACT_VERSION), "{json}");
        assert!(json.contains(HOST_VERSION), "{json}");
    }

    #[tokio::test]
    async fn an_incompatible_major_version_is_a_closed_set_error() {
        let err = handshake(State(state()), handshake_body("2.0.0"))
            .await
            .expect_err("major version mismatch must be rejected");
        assert_eq!(err.0.code, ErrorCode::ContractVersionUnsupported);
        assert_eq!(err.0.http_status(), 400);
    }

    #[tokio::test]
    async fn a_malformed_body_is_a_closed_set_invalid_request() {
        let err = handshake(State(state()), Bytes::from_static(b"{\"contract_version\": "))
            .await
            .expect_err("malformed json must be rejected");
        assert_eq!(err.0.code, ErrorCode::InvalidRequest);
    }
}
