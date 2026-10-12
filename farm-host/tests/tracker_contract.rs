//! Tracker 契约面 HTTP 接缝测试（ticket #13）。
//!
//! 全部经 axum `tower::ServiceExt::oneshot` 打**真实 router**（[`farm_host::build_router`]），
//! 不 mock 内部协作者。测试名用 GLOSSARY 词汇：归一化视图、原子 claim、闭集错误。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use farm_core::tracker::{NativeAdapter, NativeTicket, TrackerAdapter, TrackerService};
use farm_host::build_router;
use farm_sdk::events::EventKind;
use farm_sdk::services::tracker::{
    ClaimRequest, ClaimResponse, IssueListResponse, IssueState,
};
use farm_sdk::{ContractError, ErrorCode};
use tower::ServiceExt;

mod common;
use common::OP_TOKEN;

/// native provider 的三张票：#1、#2 堵住 #13（provider 原生依赖边）。
fn tickets() -> Vec<NativeTicket> {
    vec![
        NativeTicket {
            number: 1,
            title: "host skeleton".into(),
            ..Default::default()
        },
        NativeTicket {
            number: 2,
            title: "workflow config".into(),
            ..Default::default()
        },
        NativeTicket {
            number: 13,
            title: "tracker service".into(),
            labels: vec!["tracker".into()],
            blocked_by: vec![1, 2],
            ..Default::default()
        },
    ]
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

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header("x-farm-token", OP_TOKEN)
        .body(Body::empty())
        .expect("request")
}

fn post_json(uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-farm-token", OP_TOKEN)
        .body(Body::from(body.to_string()))
        .expect("request")
}

fn claim_body(issue_id: &str, assignee: &str) -> String {
    serde_json::to_string(&ClaimRequest {
        issue_id: issue_id.into(),
        assignee: assignee.into(),
    })
    .expect("serialize")
}

async fn json<T: serde::de::DeserializeOwned>(response: axum::http::Response<Body>) -> T {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body bytes");
    serde_json::from_slice(&bytes).expect("json body")
}

async fn closed_set_error(response: axum::http::Response<Body>) -> ContractError {
    let status = response.status();
    let err: ContractError = json(response).await;
    assert_eq!(
        status.as_u16(),
        err.http_status(),
        "HTTP 状态码必须与闭集码一致：{err:?}"
    );
    err
}

// ---------------------------------------------------------------------------
// 归一化视图（GET /v1/tracker/issues）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_normalized_issue_view_is_readable_through_the_contract_api() {
    let response = seeded_app()
        .oneshot(get("/v1/tracker/issues"))
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::OK);
    let parsed: IssueListResponse = json(response).await;
    assert_eq!(parsed.issues.len(), 3, "native provider 的三张票都要进视图");

    let ticket = parsed
        .issues
        .iter()
        .find(|i| i.issue_id == "13")
        .expect("票 13 必须在视图里");
    assert_eq!(ticket.title, "tracker service");
    assert_eq!(ticket.labels, vec!["tracker".to_string()]);
    assert_eq!(ticket.state, IssueState::Open);
    assert_eq!(ticket.assignee, None);
    // blocking 边随视图一起过契约面。
    assert_eq!(ticket.blocked_by, vec!["1".to_string(), "2".to_string()]);
    assert_eq!(
        parsed
            .issues
            .iter()
            .find(|i| i.issue_id == "1")
            .expect("票 1")
            .blocking,
        vec!["13".to_string()]
    );
}

#[tokio::test]
async fn the_contract_api_needs_the_op_token_before_it_answers_the_view() {
    let response = seeded_app()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/tracker/issues")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(closed_set_error(response).await.code, ErrorCode::Unauthorized);
}

// ---------------------------------------------------------------------------
// 原子 claim（POST /v1/tracker/claims）——CAS 语义，assignee 即 claim
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_claim_through_the_contract_api_takes_exclusive_ownership() {
    let app = seeded_app();
    let response = app
        .clone()
        .oneshot(post_json(
            "/v1/tracker/claims",
            &claim_body("13", "orchestrator-a"),
        ))
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::OK);
    let parsed: ClaimResponse = json(response).await;
    assert_eq!(parsed.issue_id, "13");
    assert!(parsed.claimed);
    assert_eq!(parsed.holder.as_deref(), Some("orchestrator-a"));

    // 视图与 claim 之后一致（面板与编排器读同一个视图）。
    let view: IssueListResponse = json(
        app.oneshot(get("/v1/tracker/issues"))
            .await
            .expect("oneshot"),
    )
    .await;
    let ticket = view
        .issues
        .iter()
        .find(|i| i.issue_id == "13")
        .expect("票 13");
    assert_eq!(ticket.assignee.as_deref(), Some("orchestrator-a"));
    assert_eq!(
        ticket.state,
        IssueState::Claimed
    );
}

#[tokio::test]
async fn claiming_an_already_claimed_ticket_is_a_closed_set_conflict() {
    let app = seeded_app();
    app.clone()
        .oneshot(post_json(
            "/v1/tracker/claims",
            &claim_body("13", "orchestrator-a"),
        ))
        .await
        .expect("first claim");

    let response = app
        .oneshot(post_json(
            "/v1/tracker/claims",
            &claim_body("13", "orchestrator-b"),
        ))
        .await
        .expect("second claim");

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let err = closed_set_error(response).await;
    assert_eq!(err.code, ErrorCode::Conflict);
    assert!(err.message.contains("orchestrator-a"), "{}", err.message);
    assert_eq!(err.details.expect("details")["holder"], "orchestrator-a");
}

#[tokio::test]
async fn claiming_the_same_ticket_twice_with_the_same_assignee_is_idempotent() {
    let app = seeded_app();
    let first = app
        .clone()
        .oneshot(post_json(
            "/v1/tracker/claims",
            &claim_body("13", "orchestrator-a"),
        ))
        .await
        .expect("first claim");
    let second = app
        .oneshot(post_json(
            "/v1/tracker/claims",
            &claim_body("13", "orchestrator-a"),
        ))
        .await
        .expect("second claim");

    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(second.status(), StatusCode::OK);
    let first: ClaimResponse = json(first).await;
    let second: ClaimResponse = json(second).await;
    assert_eq!(first, second, "同一 assignee 的重复 claim 必须幂等");
}

#[tokio::test]
async fn concurrent_claims_through_the_contract_api_have_exactly_one_winner() {
    // 并发 claim：一家赢，其余全部拿到闭集 conflict（GLOSSARY：原子 claim 防双跑）。
    let claimants = [
        "orchestrator-a",
        "orchestrator-b",
        "orchestrator-c",
        "orchestrator-d",
        "orchestrator-e",
        "orchestrator-f",
        "orchestrator-g",
        "orchestrator-h",
    ];
    let app = seeded_app();

    let mut handles = Vec::new();
    for assignee in claimants {
        let app = app.clone();
        handles.push(tokio::spawn(async move {
            let response = app
                .oneshot(post_json(
                    "/v1/tracker/claims",
                    &claim_body("13", assignee),
                ))
                .await
                .expect("oneshot");
            (assignee, response.status())
        }));
    }

    let mut winners = Vec::new();
    let mut losers = 0usize;
    for handle in handles {
        let (assignee, status) = handle.await.expect("claim task must not panic");
        match status {
            StatusCode::OK => winners.push(assignee),
            StatusCode::CONFLICT => losers += 1,
            other => panic!("claim 只应给出 200 或 409，{assignee} 拿到 {other}"),
        }
    }

    assert_eq!(winners.len(), 1, "只有一家能抢到：{winners:?}");
    assert_eq!(losers, claimants.len() - 1, "其余全部必须失败");

    // 归一化视图里票归唯一赢家所有。
    let view: IssueListResponse = json(
        app.oneshot(get("/v1/tracker/issues"))
            .await
            .expect("oneshot"),
    )
    .await;
    let ticket = view
        .issues
        .iter()
        .find(|i| i.issue_id == "13")
        .expect("票 13");
    assert_eq!(ticket.assignee.as_deref(), Some(winners[0]));
}

#[tokio::test]
async fn a_malformed_claim_body_is_a_closed_set_invalid_request() {
    let response = seeded_app()
        .oneshot(post_json("/v1/tracker/claims", "{\"issue_id\": "))
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(closed_set_error(response).await.code, ErrorCode::InvalidRequest);
}

#[tokio::test]
async fn a_claim_without_an_assignee_is_a_closed_set_invalid_request() {
    let response = seeded_app()
        .oneshot(post_json(
            "/v1/tracker/claims",
            r#"{"issue_id":"13","assignee":""}"#,
        ))
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let err = closed_set_error(response).await;
    assert_eq!(err.code, ErrorCode::InvalidRequest);
    assert!(err.message.contains("assignee"), "{}", err.message);
}

#[tokio::test]
async fn claiming_a_ticket_outside_the_normalized_view_is_a_closed_set_not_found() {
    let response = seeded_app()
        .oneshot(post_json(
            "/v1/tracker/claims",
            &claim_body("404", "orchestrator-a"),
        ))
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(closed_set_error(response).await.code, ErrorCode::NotFound);
}

// ---------------------------------------------------------------------------
// 事件投影：claim 成功进内存总线（闭集里的 claim_acquired）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_successful_claim_is_projected_onto_the_event_bus() {
    let mut state = common::state();
    state.tracker = TrackerService::new(
        state.db.clone(),
        Arc::new(NativeAdapter::from_tickets(tickets())) as Arc<dyn TrackerAdapter>,
    );
    let mut receiver = state.events.subscribe();
    let app = build_router(state);

    app.oneshot(post_json(
        "/v1/tracker/claims",
        &claim_body("13", "orchestrator-a"),
    ))
    .await
    .expect("oneshot");

    let envelope = receiver
        .try_recv()
        .expect("claim 成功必须投影到事件总线");
    assert_eq!(envelope.kind, EventKind::ClaimAcquired);
    assert_eq!(envelope.ticket_id.as_deref(), Some("13"));
    assert_eq!(envelope.payload["assignee"], "orchestrator-a");
    assert!(!envelope.audited, "未入账事件默认 audited=false（ADR-0005 §8）");
}

#[tokio::test]
async fn an_idempotent_reclaim_is_not_projected_as_a_new_claim_event() {
    let mut state = common::state();
    state.tracker = TrackerService::new(
        state.db.clone(),
        Arc::new(NativeAdapter::from_tickets(tickets())) as Arc<dyn TrackerAdapter>,
    );
    let mut receiver = state.events.subscribe();
    let app = build_router(state);

    for _ in 0..2 {
        app.clone()
            .oneshot(post_json(
                "/v1/tracker/claims",
                &claim_body("13", "orchestrator-a"),
            ))
            .await
            .expect("oneshot");
    }

    assert_eq!(receiver.try_recv().expect("first event").kind, EventKind::ClaimAcquired);
    assert!(
        matches!(receiver.try_recv(), Err(tokio::sync::broadcast::error::TryRecvError::Empty)),
        "幂等重claim 不是新事实，不得再投影一条"
    );
}

#[tokio::test]
async fn an_acquired_claim_is_persisted_in_the_event_log_not_only_broadcast() {
    // code review P2-4：claim 投影原先只发内存总线——晚到的订阅者、事件历史快照、
    // 通知通道都看不见它。claim 是票据状态的一等事实，必须走持久化发布路径。
    let mut state = common::state();
    state.tracker = TrackerService::new(
        state.db.clone(),
        Arc::new(NativeAdapter::from_tickets(tickets())) as Arc<dyn TrackerAdapter>,
    );
    let mut receiver = state.events.subscribe();
    let app = build_router(state.clone());

    app.oneshot(post_json(
        "/v1/tracker/claims",
        &claim_body("13", "orchestrator-a"),
    ))
    .await
    .expect("oneshot");

    let on_bus = receiver
        .try_recv()
        .expect("claim 成功必须投影到事件总线");
    assert_eq!(on_bus.kind, EventKind::ClaimAcquired);

    let history = state
        .event_history(&farm_core::EventLogFilter {
            since: None,
            kind: Some(EventKind::ClaimAcquired),
            ticket_id: None,
            limit: None,
        })
        .expect("history");
    let persisted = history
        .events
        .iter()
        .find(|event| event.ticket_id.as_deref() == Some("13"))
        .unwrap_or_else(|| panic!("claim 事件必须落进事件日志：{:?}", history.events));
    assert_eq!(persisted.id, on_bus.id, "同一个事件 id：先落库再广播");
    assert_eq!(persisted.payload["assignee"], "orchestrator-a");
}
