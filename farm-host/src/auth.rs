//! 单 token 静态鉴权（ticket #11）：契约面唯一的鉴权机制。
//!
//! 安全与权限模型（spec 新增裁决）：**单 token、静态配置、默认绑回环**。
//! 契约面（含 `/handshake`、五服务占位路由与 WS upgrade）全部过这一层中间件：
//! 接受 `X-Farm-Token: <op_token>` 或 `Authorization: Bearer <op_token>`
//! （方式由 [`farm_sdk::AuthScheme`] 自描述，宿主不另立标准）。
//!
//! **缺 token 与错 token 返回同一个闭集错误**（`unauthorized` / 401 / 同一消息）：
//! 不做区分，客户端无法据此探测 token 内容（避免预言机）。HTTP 与 WS upgrade
//! 走同一条中间件，所以两个面的拒绝形状逐字节一致。
//!
//! 宿主管理 API 的 token（`X-Farm-Admin-Token`）是**另一个**凭据，机制归 #23；
//! 本票不在这里实现管理面鉴权。

use std::sync::LazyLock;

use axum::extract::State;
use axum::extract::Request;
use axum::http::{header, HeaderMap, HeaderName};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use farm_core::AppState;
use farm_sdk::{AuthScheme, ContractError};

use crate::error::ApiError;

/// 契约面 token header（`AuthScheme::http_header`，解析一次复用）。
static OP_TOKEN_HEADER: LazyLock<HeaderName> = LazyLock::new(|| {
    let scheme = AuthScheme::default();
    HeaderName::from_bytes(scheme.http_header.as_bytes())
        .expect("AuthScheme::http_header must be a valid header name")
});

/// 缺 token / 错 token 的统一消息。**不要**在这里区分两种失败。
const UNAUTHORIZED_MESSAGE: &str = "missing or invalid orchestrator op token";

/// Bearer  scheme 前缀（RFC 7235：scheme 大小写不敏感）。
const BEARER_PREFIX: &str = "bearer ";

/// 契约面鉴权中间件：token 不对一律同一形状的 `unauthorized`。
pub async fn require_op_token(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if op_token_matches(request.headers(), &state.config.op_token) {
        next.run(request).await
    } else {
        ApiError(ContractError::unauthorized(UNAUTHORIZED_MESSAGE)).into_response()
    }
}

/// 从请求头取 op token 并与配置里的静态 token 比对。
///
/// `X-Farm-Token` 与 `Authorization: Bearer` 等价（`AuthScheme::also_accepts_bearer`）。
pub fn op_token_matches(headers: &HeaderMap, expected: &str) -> bool {
    let scheme = AuthScheme::default();

    if let Some(value) = headers.get(&*OP_TOKEN_HEADER) {
        if let Ok(token) = value.to_str() {
            if constant_time_eq(token.trim().as_bytes(), expected.as_bytes()) {
                return true;
            }
        }
    }

    if scheme.also_accepts_bearer {
        if let Some(value) = headers.get(header::AUTHORIZATION) {
            if let Ok(credentials) = value.to_str() {
                if let Some(token) = strip_bearer(credentials) {
                    if constant_time_eq(token.trim().as_bytes(), expected.as_bytes()) {
                        return true;
                    }
                }
            }
        }
    }

    false
}

/// 剥掉大小写不敏感的 `Bearer ` 前缀。
fn strip_bearer(credentials: &str) -> Option<&str> {
    let trimmed = credentials.trim_start();
    if trimmed.len() >= BEARER_PREFIX.len()
        && trimmed[..BEARER_PREFIX.len()].eq_ignore_ascii_case(BEARER_PREFIX)
    {
        Some(&trimmed[BEARER_PREFIX.len()..])
    } else {
        None
    }
}

/// 定长比较：不因首个不同字节提前返回，避免经响应时间探测 token 前缀。
///
/// 本地回环 + 静态 token 的真实威胁模型很薄，但拒绝预言机的成本只是几行。
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

#[cfg(test)]
mod tests {
    use super::*;

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

    const TOKEN: &str = "op-token-abc123";

    #[test]
    fn the_op_token_header_is_the_one_auth_scheme_advertises() {
        assert_eq!(OP_TOKEN_HEADER.as_str(), "x-farm-token");
    }

    #[test]
    fn the_token_header_is_accepted() {
        let headers = headers_with(&[("x-farm-token", TOKEN)]);
        assert!(op_token_matches(&headers, TOKEN));
    }

    #[test]
    fn the_bearer_authorization_header_is_accepted() {
        let headers = headers_with(&[("authorization", &format!("Bearer {TOKEN}"))]);
        assert!(op_token_matches(&headers, TOKEN));
        // scheme 大小写不敏感（RFC 7235）。
        let lower = headers_with(&[("authorization", &format!("bearer {TOKEN}"))]);
        assert!(op_token_matches(&lower, TOKEN));
    }

    #[test]
    fn a_missing_token_is_rejected() {
        assert!(!op_token_matches(&HeaderMap::new(), TOKEN));
    }

    #[test]
    fn a_wrong_token_is_rejected() {
        let headers = headers_with(&[("x-farm-token", "not-the-token")]);
        assert!(!op_token_matches(&headers, TOKEN));
        let bearer = headers_with(&[("authorization", "Bearer not-the-token")]);
        assert!(!op_token_matches(&bearer, TOKEN));
    }

    #[test]
    fn an_empty_token_is_rejected() {
        let headers = headers_with(&[("x-farm-token", "")]);
        assert!(!op_token_matches(&headers, TOKEN));
        let bearer = headers_with(&[("authorization", "Bearer ")]);
        assert!(!op_token_matches(&bearer, TOKEN));
    }

    #[test]
    fn a_bearer_prefix_without_a_token_is_rejected() {
        let headers = headers_with(&[("authorization", "Basic b3BfdG9rZW4=")]);
        assert!(!op_token_matches(&headers, TOKEN));
    }

    #[test]
    fn the_admin_header_is_not_the_op_token_header() {
        // 宿主管理 API 用另一个 header（GLOSSARY：宿主管理 API 与契约边界分明）。
        let headers = headers_with(&[("x-farm-admin-token", TOKEN)]);
        assert!(!op_token_matches(&headers, TOKEN));
    }
}
