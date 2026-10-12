//! 快照 API（ticket #15）：`GET /v1/events/history` 在**真实 router** 上的投影面。
//!
//! ADR-0002 §4：面板只消费宿主投影层（快照 API + 增量推送），不直连存储。
//! ADR-0005 §5：SQLite schema 永不外泄——所以这里只经 HTTP 契约面断言，
//! 断言的是「可过滤事件日志」这个能力，不是表长什么样。

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use farm_core::{AppState, HostConfig};
use farm_host::build_router;
use farm_sdk::events::{EventKind, NewEvent};
use farm_sdk::services::events::{EventHistoryQuery, EventHistoryResponse};
use tower::ServiceExt;

mod common;
use common::OP_TOKEN;

/// 同一个状态同时供「发布」与「伺服」用：测试就是宿主自己。
fn state_and_app() -> (AppState, Router) {
    let state = AppState::new_in_memory(HostConfig {
        bind: "127.0.0.1:3721".parse().expect("valid bind"),
        op_token: OP_TOKEN.into(),
        admin_token: None,
        db_path: std::env::temp_dir().join("farm-host-event-history.db"),
    })
    .expect("in-memory state");
    let app = build_router(state.clone());
    (state, app)
}

fn get(uri: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method(Method::GET).uri(uri);
    if let Some(token) = token {
        builder = builder.header("x-farm-token", token);
    }
    builder.body(Body::empty()).expect("request")
}

async fn json<T: serde::de::DeserializeOwned>(response: axum::http::Response<Body>) -> T {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body bytes");
    serde_json::from_slice(&bytes).expect("json body")
}

/// 经宿主发布路径发几条事件（#16/#18 就是这样把事件灌进总线的）。
fn publish(state: &AppState, kind: EventKind, ticket: &str, at: &str) -> String {
    // 事件时间戳由宿主盖章；测试只固定票与类型，次序由发布次序决定。
    let _ = at;
    state
        .publish(NewEvent::new(kind, "agent-runner").with_ticket(ticket))
        .expect("publish")
        .id
}

#[tokio::test]
async fn the_snapshot_api_returns_the_recent_events_after_a_publish() {
    let (state, app) = state_and_app();
    let first = publish(&state, EventKind::Dispatch, "15", "");
    let second = publish(&state, EventKind::Blocked, "16", "");

    let response = app
        .oneshot(get("/v1/events/history", Some(OP_TOKEN)))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::OK);

    let page: EventHistoryResponse = json(response).await;
    let ids: Vec<&str> = page.events.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, vec![first.as_str(), second.as_str()], "按发布次序");
    assert_eq!(page.events[0].kind, EventKind::Dispatch);
    assert_eq!(page.events[1].kind, EventKind::Blocked);
    assert!(!page.truncated);
}

#[tokio::test]
async fn the_snapshot_api_filters_by_ticket_and_kind() {
    let (state, app) = state_and_app();
    publish(&state, EventKind::Dispatch, "15", "");
    publish(&state, EventKind::Blocked, "16", "");
    publish(&state, EventKind::Dispatch, "16", "");

    let response = app
        .clone()
        .oneshot(get("/v1/events/history?ticket_id=16", Some(OP_TOKEN)))
        .await
        .expect("oneshot");
    let page: EventHistoryResponse = json(response).await;
    assert_eq!(page.events.len(), 2, "按票过滤");
    assert!(page.events.iter().all(|e| e.ticket_id.as_deref() == Some("16")));

    let response = app
        .clone()
        .oneshot(get("/v1/events/history?kind=blocked", Some(OP_TOKEN)))
        .await
        .expect("oneshot");
    let page: EventHistoryResponse = json(response).await;
    assert_eq!(page.events.len(), 1, "按类型过滤");
    assert_eq!(page.events[0].kind, EventKind::Blocked);
    assert_eq!(page.events[0].ticket_id.as_deref(), Some("16"));
}

#[tokio::test]
async fn the_snapshot_api_filters_by_time_and_paginates_with_limit() {
    let (state, app) = state_and_app();
    for _ in 0..5 {
        publish(&state, EventKind::Dispatch, "15", "");
    }

    let response = app
        .clone()
        .oneshot(get("/v1/events/history?limit=2", Some(OP_TOKEN)))
        .await
        .expect("oneshot");
    let page: EventHistoryResponse = json(response).await;
    assert_eq!(page.events.len(), 2, "limit 取最近两条");
    assert!(page.truncated, "还有更旧的事件 ⇒ truncated");

    // since 是含端点的下界：第一条事件的时间戳一定在库里。
    let since = page.events[0].occurred_at.clone();
    let response = app
        .clone()
        .oneshot(get(&format!("/v1/events/history?since={since}"), Some(OP_TOKEN)))
        .await
        .expect("oneshot");
    let page: EventHistoryResponse = json(response).await;
    assert_eq!(page.events.len(), 5, "since 含端点：最早那条也回来");
}

#[tokio::test]
async fn the_snapshot_api_caps_a_single_page() {
    let (state, app) = state_and_app();
    for _ in 0..3 {
        publish(&state, EventKind::Dispatch, "15", "");
    }

    let response = app
        .oneshot(get("/v1/events/history?limit=99999", Some(OP_TOKEN)))
        .await
        .expect("oneshot");
    let page: EventHistoryResponse = json(response).await;
    assert_eq!(
        page.events.len(),
        3,
        "超过上限的 limit 被夹到上限（库里只有 3 条）"
    );
}

#[tokio::test]
async fn an_empty_event_log_returns_an_empty_snapshot() {
    let (state, app) = state_and_app();
    let _ = state;
    let response = app
        .oneshot(get("/v1/events/history", Some(OP_TOKEN)))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::OK);
    let page: EventHistoryResponse = json(response).await;
    assert!(page.events.is_empty());
    assert!(!page.truncated);
}

#[tokio::test]
async fn the_snapshot_api_requires_the_op_token() {
    let (_state, app) = state_and_app();
    let response = app
        .oneshot(get("/v1/events/history", None))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_malformed_since_is_a_closed_set_invalid_request() {
    let (_state, app) = state_and_app();
    let response = app
        .oneshot(get("/v1/events/history?since=yesterday", Some(OP_TOKEN)))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let err: farm_sdk::ContractError = json(response).await;
    assert_eq!(err.code, farm_sdk::ErrorCode::InvalidRequest);
    assert!(err.message.contains("since"), "{}", err.message);
}

#[tokio::test]
async fn an_unknown_query_parameter_is_a_closed_set_invalid_request() {
    let (_state, app) = state_and_app();
    let response = app
        .oneshot(get("/v1/events/history?nope=1", Some(OP_TOKEN)))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let err: farm_sdk::ContractError = json(response).await;
    assert_eq!(err.code, farm_sdk::ErrorCode::InvalidRequest);
}

#[tokio::test]
async fn the_history_query_schema_round_trips_the_contract_shape() {
    // 契约形状（farm-sdk 单一源）与查询串互转：面板照它拼 URL。
    let query = EventHistoryQuery {
        since: Some("2026-01-01T00:00:00Z".into()),
        kind: Some(EventKind::Blocked),
        ticket_id: Some("15".into()),
        limit: Some(50),
    };
    let encoded = serde_urlencoded::to_string(&query).expect("encode");
    let decoded: EventHistoryQuery = serde_urlencoded::from_str(&encoded).expect("decode");
    assert_eq!(decoded, query);
    assert!(encoded.contains("kind=blocked"), "{encoded}");
    assert!(encoded.contains("ticket_id=15"), "{encoded}");
    assert!(!encoded.contains("attempt_id"), "契约查询没有 attempt_id：{encoded}");
}

#[tokio::test]
async fn the_history_response_is_the_contract_shape() {
    let (state, app) = state_and_app();
    publish(&state, EventKind::NeedsAttention, "15", "");
    let response = app
        .oneshot(get("/v1/events/history", Some(OP_TOKEN)))
        .await
        .expect("oneshot");
    let json_text = {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        String::from_utf8(bytes.to_vec()).expect("utf8")
    };
    // 一等事件必须原样出现在投影面上（GLOSSARY：面板必渲染）。
    assert!(json_text.contains("\"needs_attention\""), "{json_text}");
    assert!(json_text.contains("\"audited\""), "{json_text}");
}
