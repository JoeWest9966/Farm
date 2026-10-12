//! 宿主管理 API（host admin API，GLOSSARY）：独立前缀 `/admin` + 独立管理 token（ticket #23）。
//!
//! 与编排器契约**边界分明**（ADR-0002 §1）：管理面管宿主自身（插件装卸/恢复、tracker provider
//! 配置、服务启停），契约面管编排原语。两者同进程同端口，只是前缀与凭据都不同：
//!
//! - 前缀独立（`/admin`，与 `/v1` 不重合）；
//! - token 独立（`X-Farm-Admin-Token`，见 [`farm_sdk::AuthScheme::admin_header`]）——
//!   **管理 token ≠ 操作 token**（spec 新增裁决）。[`require_admin_token`] 校验管理 token，
//!   交叉访问被拒：op token 递到管理面 → `forbidden`；管理 token 递到契约面 → 契约面只认
//!   op token，回 `unauthorized`。契约 API 无法触达任何管理面操作（不是私有后门）。
//! - 插件装卸/恢复（[`plugins`] / [`recover`]）读 [#19](farm_core::AppState::plugin) 的监督状态；
//!   tracker provider 配置（[`tracker_provider`] / [`set_tracker_provider`]）与服务启停
//!   （[`services`] / [`stop_service`] / [`start_service`]）同在此面。

use std::sync::LazyLock;

use axum::extract::State;
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderName};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use farm_core::AppState;
use farm_sdk::admin::{AdminPluginListResponse, AdminStatusResponse};
use farm_sdk::{AuthScheme, ContractError, ServiceName, CONTRACT_VERSION, HOST_VERSION};

use crate::error::ApiError;

/// 管理 token header（`AuthScheme::admin_header`，解析一次复用；与 op token header 不同）。
static ADMIN_TOKEN_HEADER: LazyLock<HeaderName> = LazyLock::new(|| {
    let scheme = AuthScheme::default();
    HeaderName::from_bytes(scheme.admin_header.as_bytes())
        .expect("AuthScheme::admin_header must be a valid header name")
});

/// 缺 / 错管理 token 的统一消息（与 op token 的同码策略一致：不区分缺与错）。
const ADMIN_UNAUTHORIZED_MESSAGE: &str = "missing or invalid host admin token";

/// 交叉访问：op token 递到管理面（已鉴权但越面）→ `forbidden`。
const ADMIN_FORBIDDEN_MESSAGE: &str = "the orchestrator op token is not valid on the host admin API";

/// 管理面未配置管理 token = 整体禁用（默认安全：没配就不开）。
const ADMIN_DISABLED_MESSAGE: &str = "host admin API is not enabled (no admin token configured)";

/// 宿主管理 API 鉴权中间件（ticket #23）：**独立管理 token**，与操作 token 边界分明。
///
/// 判定顺序（与 [`crate::auth::require_op_token`] 相映，但凭据与 header 都不同）：
/// 1. 管理 token 命中 → 放行；
/// 2. 递上来的是**另一个面的有效凭据**（op token）→ `forbidden`（已鉴权但越面，
///    区别于「没凭据 / 乱凭据」的 `unauthorized`，帮客户端定位「用错 token 了」）；
/// 3. 其余（缺 / 错管理 token）→ `unauthorized`（同码，不供预言机）。
///
/// 没配管理 token（`HostConfig::admin_token = None`）→ 管理面整体禁用，一律 `forbidden`。
pub async fn require_admin_token(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let Some(admin_token) = state.config.admin_token.as_ref() else {
        return ApiError(ContractError::forbidden(ADMIN_DISABLED_MESSAGE)).into_response();
    };
    if admin_token_matches(request.headers(), admin_token) {
        return next.run(request).await;
    }
    if crate::auth::op_token_matches(request.headers(), &state.config.op_token) {
        return ApiError(ContractError::forbidden(ADMIN_FORBIDDEN_MESSAGE)).into_response();
    }
    ApiError(ContractError::admin_unauthorized(ADMIN_UNAUTHORIZED_MESSAGE)).into_response()
}

/// 从请求头取管理 token 并与配置里的静态管理 token 定长比对。
///
/// 只认 `X-Farm-Admin-Token`（[`farm_sdk::AuthScheme::admin_header`]），不接受 Bearer——
/// 管理面是面板专用面，凭据形态单一，减少歧义。
pub fn admin_token_matches(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get(&*ADMIN_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(|token| constant_time_eq(token.trim().as_bytes(), expected.as_bytes()))
        .unwrap_or(false)
}

/// 定长比较：不因首个不同字节提前返回，避免经响应时间探测 token 前缀。
///
/// 与 [`crate::auth`] 的反预言机比较同形，但**独立一份**：管理凭据的比对路径与操作凭据
/// 相互独立，正是「两个 token 不是同一个」在代码层的落地（不复用、不改 #11 的 auth.rs）。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let max = a.len().max(b.len());
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..max {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= x ^ y;
    }
    diff == 0
}

/// `GET /admin/status`：宿主管理面自描述。
pub async fn status(State(state): State<AppState>) -> Json<AdminStatusResponse> {
    Json(AdminStatusResponse {
        host_version: HOST_VERSION.to_string(),
        contract_version: CONTRACT_VERSION.to_string(),
        bind_addr: state.config.bind.to_string(),
        services: ServiceName::ALL.to_vec(),
    })
}

/// `GET /admin/plugins`：插件清单与状态（#19 监督状态 → [`PluginSummary`] 投影）。
///
/// 读源是 [`farm_core::AppState::plugin`] 的 `PluginHost::statuses`（v8 `plugin_state` 表）；
/// 存储故障 → 闭集 `service_unavailable`。
pub async fn plugins(
    State(state): State<AppState>,
) -> Result<Json<AdminPluginListResponse>, ApiError> {
    let statuses = state.plugin.statuses().map_err(ApiError::from)?;
    let plugins = statuses.iter().map(|status| status.to_summary()).collect();
    Ok(Json(AdminPluginListResponse { plugins }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderName;
    use farm_core::HostConfig;
    use farm_sdk::ADMIN_ROUTES;

    fn headers_with(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes()).expect("valid header name"),
                value.parse().expect("valid header value"),
            );
        }
        headers
    }

    const ADMIN: &str = "admin-token-xyz";
    fn state() -> AppState {
        AppState::new_in_memory(HostConfig {
            bind: "127.0.0.1:3721".parse().expect("valid bind"),
            op_token: "unit-op-token".into(),
            admin_token: None,
            db_path: std::env::temp_dir().join("farm-host-unit.db"),
        })
        .expect("in-memory state")
    }

    #[tokio::test]
    async fn the_admin_status_describes_the_host() {
        let Json(status) = status(State(state())).await;
        assert_eq!(status.contract_version, CONTRACT_VERSION);
        assert_eq!(status.services.len(), 5, "ADR-0001 五服务");
        assert_eq!(status.bind_addr, "127.0.0.1:3721");
    }

    #[tokio::test]
    async fn the_admin_plugin_list_is_empty_when_nothing_is_supervised() {
        let Json(list) = plugins(State(state())).await.expect("empty store reads ok");
        assert!(list.plugins.is_empty());
    }

    #[test]
    fn every_admin_route_lives_under_the_admin_prefix() {
        for route in ADMIN_ROUTES {
            assert!(route.path.starts_with("/admin"), "{}", route.path);
        }
    }

    // --- 管理 token 鉴权（独立于操作 token，#23）---

    #[test]
    fn the_admin_token_header_is_the_one_auth_scheme_advertises() {
        assert_eq!(ADMIN_TOKEN_HEADER.as_str(), "x-farm-admin-token");
    }

    #[test]
    fn the_admin_token_header_is_accepted() {
        let headers = headers_with(&[("x-farm-admin-token", ADMIN)]);
        assert!(admin_token_matches(&headers, ADMIN));
    }

    #[test]
    fn a_missing_or_wrong_admin_token_is_rejected() {
        assert!(!admin_token_matches(&HeaderMap::new(), ADMIN));
        let wrong = headers_with(&[("x-farm-admin-token", "nope")]);
        assert!(!admin_token_matches(&wrong, ADMIN));
        let empty = headers_with(&[("x-farm-admin-token", "")]);
        assert!(!admin_token_matches(&empty, ADMIN));
    }

    #[test]
    fn the_admin_token_on_the_op_token_header_is_not_counted() {
        // 两个 header 不是同一个：管理 token 递在 op header 上不算命中管理面。
        let headers = headers_with(&[("x-farm-token", ADMIN)]);
        assert!(!admin_token_matches(&headers, ADMIN));
    }
}
