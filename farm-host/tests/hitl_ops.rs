//! HITL 操作面的契约 HTTP 接缝测试（ticket #22）。
//!
//! 面板是契约的**另一个客户端**（ADR-0002 §1）：claim/release、steer/follow_up、
//! abort、clear_queue 全部经契约路由、同一枚 op token——没有私有后门。
//! 本文件打的是真实 router（[`farm_host::build_router`]），断言的是**接缝行为**：
//! 路由挂通、token 同源、闭集错误形状。
//!
//! steer/follow_up/abort/clear_queue 的**业务结局**（disposition 回流、abort 探针）
//! 归 #16 的 `agent_runner_dispatch.rs`；这里只证明契约面把它们接上了。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use farm_core::tracker::{NativeAdapter, NativeTicket, TrackerAdapter, TrackerService};
use farm_host::build_router;
use farm_sdk::services::tracker::{ReleaseOutcome, ReleaseResponse};
use farm_sdk::{ContractError, ErrorCode};
use tower::ServiceExt;

mod common;
use common::OP_TOKEN;

/// native provider 的一张票（面板看板与 HITL 的操作对象）。
fn tickets() -> Vec<NativeTicket> {
    vec![NativeTicket {
        number: 22,
        title: "panel batch 1: read + HITL".into(),
        labels: vec!["panel".into()],
        ..Default::default()
    }]
}

/// 带 seeded native adapter 的真实 router（同一个装配点，与 `main` 同路）。
fn seeded_app() -> Router {
    let mut state = common::state();
    state.tracker = TrackerService::new(
        state.db.clone(),
        Arc::new(NativeAdapter::from_tickets(tickets())) as Arc<dyn TrackerAdapter>,
    );
    build_router(state)
}

fn post(uri: &str, token: Option<&str>, body: &str) -> Request<Body> {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        builder = builder.header("x-farm-token", token);
    }
    builder.body(Body::from(body.to_string())).expect("request")
}

async fn body_json<T: serde::de::DeserializeOwned>(response: axum::http::Response<Body>) -> T {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body bytes");
    serde_json::from_slice(&bytes).expect("json body")
}

async fn closed_set_error(response: axum::http::Response<Body>) -> ContractError {
    let status = response.status();
    let err: ContractError = body_json(response).await;
    assert_eq!(
        status.as_u16(),
        err.http_status(),
        "HTTP 状态码必须与闭集码一致：{err:?}"
    );
    err
}

/// 以 `assignee` 的身份 claim 一张票（面板 HITL 的前置）。
async fn claim(app: &Router, issue_id: &str, assignee: &str) {
    let response = app
        .clone()
        .oneshot(post(
            "/v1/tracker/claims",
            Some(OP_TOKEN),
            &format!(r#"{{"issue_id":"{issue_id}","assignee":"{assignee}"}}"#),
        ))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::OK, "claim 必须成功");
}

// ---------------------------------------------------------------------------
// release：交还排他所有权（面板 claim/release 同路）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_holder_releases_a_ticket_and_the_board_sees_it_unclaimed() {
    let app = seeded_app();
    claim(&app, "22", "panel").await;

    let response = app
        .clone()
        .oneshot(post(
            "/v1/tracker/releases",
            Some(OP_TOKEN),
            r#"{"issue_id":"22","assignee":"panel"}"#,
        ))
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::OK);
    let parsed: ReleaseResponse = body_json(response).await;
    assert_eq!(parsed.issue_id, "22");
    assert_eq!(parsed.outcome, ReleaseOutcome::Released);

    // 交还后的归一化视图：无人持有（之后他人可 claim）。
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/tracker/issues")
                .header("x-farm-token", OP_TOKEN)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("oneshot");
    let issues: farm_sdk::services::tracker::IssueListResponse = body_json(response).await;
    let ticket = issues.issues.iter().find(|i| i.issue_id == "22").expect("#22");
    assert_eq!(ticket.assignee, None, "release 后无人持有");
}

#[tokio::test]
async fn a_second_release_is_an_idempotent_no_op() {
    let app = seeded_app();
    claim(&app, "22", "panel").await;
    let first = app
        .clone()
        .oneshot(post(
            "/v1/tracker/releases",
            Some(OP_TOKEN),
            r#"{"issue_id":"22","assignee":"panel"}"#,
        ))
        .await
        .expect("oneshot");
    assert_eq!(first.status(), StatusCode::OK);

    let second = app
        .oneshot(post(
            "/v1/tracker/releases",
            Some(OP_TOKEN),
            r#"{"issue_id":"22","assignee":"panel"}"#,
        ))
        .await
        .expect("oneshot");

    assert_eq!(second.status(), StatusCode::OK, "重复 release 不报错");
    let parsed: ReleaseResponse = body_json(second).await;
    assert_eq!(parsed.outcome, ReleaseOutcome::NotClaimed);
}

#[tokio::test]
async fn releasing_someone_elses_ticket_is_a_closed_set_conflict_naming_the_holder() {
    let app = seeded_app();
    claim(&app, "22", "orchestrator-a").await;

    let response = app
        .oneshot(post(
            "/v1/tracker/releases",
            Some(OP_TOKEN),
            r#"{"issue_id":"22","assignee":"panel"}"#,
        ))
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let err = closed_set_error(response).await;
    assert_eq!(err.code, ErrorCode::Conflict);
    assert_eq!(
        err.details.as_ref().and_then(|d| d.get("holder")),
        Some(&serde_json::json!("orchestrator-a")),
        "conflict 必须指名当前持有者：{err:?}"
    );
}

#[tokio::test]
async fn releasing_an_unknown_ticket_is_a_closed_set_not_found() {
    let app = seeded_app();

    let response = app
        .oneshot(post(
            "/v1/tracker/releases",
            Some(OP_TOKEN),
            r#"{"issue_id":"999","assignee":"panel"}"#,
        ))
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let err = closed_set_error(response).await;
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn a_release_without_the_op_token_is_unauthorized_like_every_contract_route() {
    let app = seeded_app();

    let response = app
        .oneshot(post(
            "/v1/tracker/releases",
            None,
            r#"{"issue_id":"22","assignee":"panel"}"#,
        ))
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let err = closed_set_error(response).await;
    assert_eq!(err.code, ErrorCode::Unauthorized);
}

#[tokio::test]
async fn a_malformed_release_body_is_a_closed_set_invalid_request() {
    let app = seeded_app();

    let response = app
        .oneshot(post(
            "/v1/tracker/releases",
            Some(OP_TOKEN),
            r#"{"issue_id":"22","assignee":}"#,
        ))
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let err = closed_set_error(response).await;
    assert_eq!(err.code, ErrorCode::InvalidRequest);
}

// ---------------------------------------------------------------------------
// follow_up / clear_queue：注入路由与 steer/abort 同路
// ---------------------------------------------------------------------------

#[tokio::test]
async fn follow_up_on_an_unknown_attempt_is_a_closed_set_not_found() {
    let app = seeded_app();

    let response = app
        .oneshot(post(
            "/v1/agent-runner/attempts/att-nope/follow-up",
            Some(OP_TOKEN),
            r#"{"message":"also update the docs"}"#,
        ))
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let err = closed_set_error(response).await;
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn an_empty_follow_up_message_is_a_closed_set_invalid_request() {
    let app = seeded_app();

    let response = app
        .oneshot(post(
            "/v1/agent-runner/attempts/att-nope/follow-up",
            Some(OP_TOKEN),
            r#"{"message":"   "}"#,
        ))
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let err = closed_set_error(response).await;
    assert_eq!(err.code, ErrorCode::InvalidRequest);
}

#[tokio::test]
async fn clear_queue_on_an_unknown_attempt_is_a_closed_set_not_found() {
    let app = seeded_app();

    let response = app
        .oneshot(post(
            "/v1/agent-runner/attempts/att-nope/clear-queue",
            Some(OP_TOKEN),
            "{}",
        ))
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let err = closed_set_error(response).await;
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn the_injection_routes_require_the_same_op_token() {
    let app = seeded_app();

    for (uri, body) in [
        (
            "/v1/agent-runner/attempts/att-nope/follow-up",
            r#"{"message":"hi"}"#,
        ),
        ("/v1/agent-runner/attempts/att-nope/clear-queue", "{}"),
    ] {
        let response = app
            .clone()
            .oneshot(post(uri, None, body))
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
        let err = closed_set_error(response).await;
        assert_eq!(err.code, ErrorCode::Unauthorized, "{uri}");
    }
}
