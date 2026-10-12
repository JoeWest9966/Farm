//! 宿主管理 API（host admin API）接缝测试（ticket #23，GLOSSARY「宿主管理 API」）。
//!
//! 全部经 axum `tower::ServiceExt::oneshot` 打**真实 router**（[`farm_host::build_router`]），
//! 不 mock 内部协作者。核心不变量（ADR-0002 §1 + spec 新裁决）：管理面与编排器契约**边界分明**——
//! 独立 `/admin` 前缀 + 独立管理 token（`X-Farm-Admin-Token`），与操作 token 不是同一个凭据，
//! **交叉访问被拒**；契约 API 无法触达管理面操作（负向）。

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use farm_host::build_router;
use farm_sdk::{ContractError, ErrorCode};
use tower::ServiceExt;

mod common;
use common::{admin_app, ADMIN_TOKEN, OP_TOKEN};

/// 带（或不带）管理 token 的请求。
fn admin_request(method: Method, uri: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("x-farm-admin-token", token);
    }
    builder.body(Body::empty()).expect("request")
}

/// 带 op token 的请求（打到管理面，验证交叉访问被拒）。
fn op_token_request(uri: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header("x-farm-token", OP_TOKEN)
        .body(Body::empty())
        .expect("request")
}

async fn closed_set_error(response: axum::http::Response<Body>) -> ContractError {
    let status = response.status();
    let err: ContractError = {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.expect("body");
        serde_json::from_slice(&bytes).expect("envelope json")
    };
    assert_eq!(status.as_u16(), err.http_status(), "HTTP 状态码须与闭集码一致");
    err
}

// ---------------------------------------------------------------------------
// 管理 token 鉴权（独立于操作 token；交叉访问被拒）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_admin_face_accepts_the_admin_token() {
    let app = admin_app();
    let response = app
        .oneshot(admin_request(Method::GET, "/admin/status", Some(ADMIN_TOKEN)))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::OK, "管理 token 必须放行管理面");
}

#[tokio::test]
async fn the_admin_face_rejects_the_op_token_as_forbidden() {
    // op token 是**另一个面的有效凭据**：递到管理面是「已鉴权但越面」→ forbidden（403），
    // 与「没凭据 / 乱凭据」的 unauthorized（401）区分开。
    let app = admin_app();
    let response = app
        .oneshot(op_token_request("/admin/status"))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(closed_set_error(response).await.code, ErrorCode::Forbidden);
}

#[tokio::test]
async fn the_admin_face_rejects_a_missing_admin_token_as_unauthorized() {
    let app = admin_app();
    let response = app
        .oneshot(admin_request(Method::GET, "/admin/status", None))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(closed_set_error(response).await.code, ErrorCode::Unauthorized);
}

#[tokio::test]
async fn the_admin_face_rejects_a_wrong_admin_token_as_unauthorized() {
    let app = admin_app();
    let response = app
        .oneshot(admin_request(Method::GET, "/admin/status", Some("not-the-admin-token")))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(closed_set_error(response).await.code, ErrorCode::Unauthorized);
}

#[tokio::test]
async fn the_contract_face_rejects_the_admin_token() {
    // 管理 token 递到契约面：契约面只认 op token，管理 token 不是它的凭据 → unauthorized。
    let app = admin_app();
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/tracker/issues")
                .header("x-farm-admin-token", ADMIN_TOKEN)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(closed_set_error(response).await.code, ErrorCode::Unauthorized);
}

#[tokio::test]
async fn the_admin_face_is_disabled_when_no_admin_token_is_configured() {
    // 没配管理 token（FARM_ADMIN_TOKEN 未设）：管理面整体禁用，任何凭据都进不去。
    let app = build_router(admin_state_without_admin_token());
    let response = app
        .oneshot(admin_request(Method::GET, "/admin/status", Some("anything")))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::FORBIDDEN, "管理面未启用即禁用");
}

/// admin_token = None 的宿主（管理面未启用）。
fn admin_state_without_admin_token() -> farm_core::AppState {
    let mut config = common::admin_config();
    config.admin_token = None;
    farm_core::AppState::new_in_memory(config).expect("in-memory state")
}
