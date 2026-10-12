//! 通知通道配置的宿主管理 API 接缝测试（ticket #21，GLOSSARY「宿主管理 API / 通知通道」）。
//!
//! 全部经 axum `tower::ServiceExt::oneshot` 打**真实 router**（[`farm_host::build_router`]），
//! 不 mock 内部协作者。覆盖验收④：通道配置经管理 API 可改、**重启后生效**（文件库重开）。
//! 宿主零推送域知识：响应里没有 url/headers/模板/重试/限流字段（那些是插件侧自决载体）。

use std::path::PathBuf;

use axum::body::{Body, to_bytes};
use axum::http::{header, Method, Request, StatusCode};
use farm_host::build_router;
use farm_sdk::notifier::{NotificationChannelConfig, NotificationChannelUpdate};
use farm_sdk::{ContractError, ErrorCode};
use tower::ServiceExt;

mod common;
use common::{ADMIN_TOKEN, OP_TOKEN};

fn admin_request(method: Method, uri: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("x-farm-admin-token", token);
    }
    builder.body(Body::empty()).expect("request")
}

fn admin_post_json(uri: &str, token: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("x-farm-admin-token", token)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

async fn get_channel(app: axum::Router, repo: &str) -> (StatusCode, NotificationChannelConfig) {
    let response = app
        .oneshot(admin_request(Method::GET, &format!("/admin/notifier/channel/{repo}"), Some(ADMIN_TOKEN)))
        .await
        .expect("oneshot");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.expect("body");
    (status, serde_json::from_slice(&bytes).expect("channel config json"))
}

#[tokio::test]
async fn an_unconfigured_repo_is_served_the_default_filter_set() {
    let app = build_router(common::admin_state());
    let (status, config) = get_channel(app, "repo-a").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(config.repo_id, "repo-a");
    assert!(config.enabled, "默认通道开");
    assert!(config.filter.blocked && config.filter.needs_attention && config.filter.stall);
    assert!(!config.filter.merge_complete, "merge 完成默认关（GLOSSARY 默认过滤集）");
}

#[tokio::test]
async fn setting_the_channel_config_via_the_admin_api_is_read_back() {
    let app = build_router(common::admin_state());
    let response = app
        .clone()
        .oneshot(admin_post_json(
            "/admin/notifier/channel/repo-a",
            ADMIN_TOKEN,
            r#"{"enabled":false,"filter":{"blocked":false,"needs_attention":true,"stall":false,"merge_complete":true}}"#,
        ))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.expect("body");
    let saved: NotificationChannelConfig = serde_json::from_slice(&bytes).expect("config");
    assert!(!saved.enabled);
    assert!(saved.filter.merge_complete, "显式打开 merge 完成");

    let (status, read_back) = get_channel(app, "repo-a").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(read_back.filter, saved.filter, "同一宿主读回刚写的配置");
    assert!(!read_back.enabled);
}

#[tokio::test]
async fn the_channel_config_survives_a_restart_through_the_admin_api() {
    // 验收④：经管理 API 设置后，宿主重启（文件库重开）配置仍生效。
    let dir = tempfile::tempdir().expect("tempdir").keep();
    let db_path: PathBuf = dir.join("farm-notifier-restart.db");
    let config = || farm_core::HostConfig {
        bind: "127.0.0.1:3721".parse().expect("bind"),
        op_token: OP_TOKEN.into(),
        admin_token: Some(ADMIN_TOKEN.into()),
        db_path: db_path.clone(),
    };
    let body = r#"{"enabled":true,"filter":{"blocked":true,"needs_attention":false,"stall":true,"merge_complete":true}}"#;
    {
        let app = build_router(farm_core::AppState::new(config()).expect("first boot"));
        let response = app
            .oneshot(admin_post_json("/admin/notifier/channel/repo-a", ADMIN_TOKEN, body))
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
    }
    // 重启：同一文件库重新打开。
    let app = build_router(farm_core::AppState::new(config()).expect("second boot"));
    let (status, read_back) = get_channel(app, "repo-a").await;
    assert_eq!(status, StatusCode::OK);
    assert!(read_back.filter.merge_complete, "重启后 merge 完成开关仍在");
    assert!(!read_back.filter.needs_attention, "重启后 needs_attention 关的状态仍在");
    assert!(!read_back.updated_at.is_empty(), "持久配置带时间戳");
}

#[tokio::test]
async fn the_notifier_channel_route_is_guarded_by_the_admin_token() {
    // op token 递到通知通道管理路由：越面 → forbidden（403），不是 404。
    let app = build_router(common::admin_state());
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/admin/notifier/channel/repo-a")
                .header("x-farm-token", OP_TOKEN)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn setting_an_incomplete_filter_is_invalid_request() {
    // 过滤集形状必须完整（四个开关都在）；缺字段 → 闭集 invalid_request（400）。
    let app = build_router(common::admin_state());
    let response = app
        .oneshot(admin_post_json(
            "/admin/notifier/channel/repo-a",
            ADMIN_TOKEN,
            r#"{"enabled":true,"filter":{"blocked":true}}"#,
        ))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.expect("body");
    let err: ContractError = serde_json::from_slice(&bytes).expect("envelope");
    assert_eq!(err.code, ErrorCode::InvalidRequest);
}

#[tokio::test]
async fn the_channel_config_response_carries_no_push_domain_fields() {
    // 宿主零推送域知识：管理 API 投影里没有 url/headers/模板/重试/限流。
    let app = build_router(common::admin_state());
    let (status, config) = get_channel(app, "repo-a").await;
    assert_eq!(status, StatusCode::OK);
    let rendered = serde_json::to_string(&config).expect("serialize");
    for forbidden in ["url", "headers", "template", "retry", "rate_limit", "endpoint"] {
        assert!(!rendered.contains(forbidden), "通道配置不得含推送域字段 {forbidden}: {rendered}");
    }
}

#[allow(dead_code)]
fn _touch_update(update: &NotificationChannelUpdate) {
    let _ = update;
}
