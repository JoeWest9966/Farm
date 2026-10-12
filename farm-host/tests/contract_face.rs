//! 契约面（contract face）HTTP 接缝测试（ticket #11）。
//!
//! 全部经 axum `tower::ServiceExt::oneshot` 打**真实 router**（[`farm_host::build_router`]），
//! 不 mock 内部协作者。测试名用 GLOSSARY 词汇：握手、闭集错误、占位路由。

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use farm_core::workspace::EstablishRequest;
use farm_host::build_router;
use farm_sdk::{
    ClientInfo, ClientKind, ContractError, ErrorCode, HandshakeRequest, HandshakeResponse,
    CONTRACT_ROUTES, CONTRACT_VERSION, HANDSHAKE_PATH,
};
use farm_sdk::services::workspace::{MappingListResponse, WorktreeState};
use tower::ServiceExt;

mod common;
use common::{app, OP_TOKEN};

/// 构造一个带（或不带）op token 的请求。
fn request(method: Method, uri: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("x-farm-token", token);
    }
    builder.body(Body::empty()).expect("request")
}

/// 带 JSON 体的 POST。
fn json_request(uri: &str, token: Option<&str>, body: &str) -> Request<Body> {
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

fn handshake_request_body(contract_version: &str) -> String {
    serde_json::to_string(&HandshakeRequest {
        contract_version: contract_version.to_string(),
        client: ClientInfo {
            name: "orchestrator-skeleton".into(),
            version: "0.1.0".into(),
            kind: ClientKind::Orchestrator,
        },
    })
    .expect("serialize")
}

/// 解出闭集错误信封（HTTP 与 WS 同一形状）。
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

// ---------------------------------------------------------------------------
// 握手（contract version handshake）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn handshake_with_the_op_token_and_a_compatible_contract_version_returns_the_host_self_description() {
    let app = app();
    let response = app
        .oneshot(json_request(
            HANDSHAKE_PATH,
            Some(OP_TOKEN),
            &handshake_request_body(CONTRACT_VERSION),
        ))
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::OK);
    let parsed: HandshakeResponse = body_json(response).await;
    assert_eq!(parsed.contract_version, CONTRACT_VERSION);
    assert_eq!(parsed.supported_contract_versions, vec![CONTRACT_VERSION]);
    assert_eq!(parsed.services.len(), 5, "ADR-0001 五服务");
    assert_eq!(parsed.event_kinds.len(), 17, "事件闭集（#19 加法追加 plugin_restarted）");
    assert_eq!(parsed.auth.http_header, "X-Farm-Token");
    assert!(parsed.auth.ws_upgrade_requires_token);
}

#[tokio::test]
async fn handshake_accepts_a_newer_minor_version_of_the_same_major() {
    let app = app();
    let response = app
        .oneshot(json_request(
            HANDSHAKE_PATH,
            Some(OP_TOKEN),
            &handshake_request_body("1.7.3"),
        ))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::OK, "同主版本内宿主保持兼容");
}

#[tokio::test]
async fn handshake_with_an_incompatible_major_version_is_a_closed_set_contract_version_unsupported() {
    let app = app();
    let response = app
        .oneshot(json_request(
            HANDSHAKE_PATH,
            Some(OP_TOKEN),
            &handshake_request_body("2.0.0"),
        ))
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let err = closed_set_error(response).await;
    assert_eq!(err.code, ErrorCode::ContractVersionUnsupported);
    assert!(err.message.contains("2.0.0"), "{}", err.message);
}

#[tokio::test]
async fn handshake_without_the_op_token_is_unauthorized() {
    let app = app();
    let response = app
        .oneshot(json_request(
            HANDSHAKE_PATH,
            None,
            &handshake_request_body(CONTRACT_VERSION),
        ))
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(closed_set_error(response).await.code, ErrorCode::Unauthorized);
}

#[tokio::test]
async fn handshake_with_a_malformed_body_is_a_closed_set_invalid_request() {
    let app = app();
    let response = app
        .oneshot(json_request(HANDSHAKE_PATH, Some(OP_TOKEN), "{\"contract_version\":"))
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(closed_set_error(response).await.code, ErrorCode::InvalidRequest);
}

#[tokio::test]
async fn handshake_with_the_wrong_method_is_a_closed_set_method_not_allowed() {
    let app = app();
    let response = app
        .oneshot(request(Method::GET, HANDSHAKE_PATH, Some(OP_TOKEN)))
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        closed_set_error(response).await.code,
        ErrorCode::MethodNotAllowed
    );
}

// ---------------------------------------------------------------------------
// 单 token 静态鉴权（auth）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_contract_route_without_the_op_token_is_unauthorized() {
    let app = app();
    let response = app
        .oneshot(request(Method::GET, "/v1/tracker/issues", None))
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let err = closed_set_error(response).await;
    assert_eq!(err.code, ErrorCode::Unauthorized);
    assert!(!err.retryable, "unauthorized 不可重试");
}

#[tokio::test]
async fn a_missing_token_and_a_wrong_token_get_the_same_closed_set_answer() {
    let missing = app()
        .oneshot(request(Method::GET, "/v1/tracker/issues", None))
        .await
        .expect("oneshot");
    let missing = closed_set_error(missing).await;

    let wrong = app()
        .oneshot(request(Method::GET, "/v1/tracker/issues", Some("not-the-op-token")))
        .await
        .expect("oneshot");
    let wrong = closed_set_error(wrong).await;

    // 同码、同状态、同消息：客户端无法据此区分「缺」与「错」（避免预言机）。
    assert_eq!(missing.code, wrong.code);
    assert_eq!(missing.http_status(), wrong.http_status());
    assert_eq!(missing.message, wrong.message);
    assert_eq!(missing, wrong);
}

#[tokio::test]
async fn the_bearer_authorization_header_is_accepted_as_the_op_token() {
    let app = app();
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/tracker/issues")
                .header(header::AUTHORIZATION, format!("Bearer {OP_TOKEN}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("oneshot");

    // Bearer 鉴权被接受：落到已打通的 tracker 视图（200），而非 401。
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn an_empty_token_header_is_unauthorized() {
    let app = app();
    let response = app
        .oneshot(request(Method::GET, "/v1/tracker/issues", Some("")))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// 占位路由（placeholder routes）/ 404 / 405
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_placeholder_contract_route_answers_not_implemented_with_the_op_token() {
    for route in CONTRACT_ROUTES {
        // 已打通的路由不再是占位：事件投影两条（#15）、tracker 三条（#13/#22）、
        // workspace 映射（#14）、agent-runner 五条（#16/#22）。
        if matches!(
            route.path,
            "/v1/events/stream"
                | "/v1/events/history"
                | "/v1/tracker/issues"
                | "/v1/tracker/claims"
                | "/v1/tracker/releases"
                | "/v1/workspace/mappings"
                | "/v1/agent-runner/dispatches"
                | "/v1/agent-runner/attempts/{attempt_id}/steer"
                | "/v1/agent-runner/attempts/{attempt_id}/abort"
                | "/v1/agent-runner/attempts/{attempt_id}/follow-up"
                | "/v1/agent-runner/attempts/{attempt_id}/clear-queue"
                | "/v1/merge/proposals"
        ) {
            continue;
        }
        let method: Method = route.method.parse().expect("GET/POST");
        let app = app();
        let response = app
            .oneshot(request(method, route.path, Some(OP_TOKEN)))
            .await
            .expect("oneshot");

        assert_eq!(
            response.status(),
            StatusCode::NOT_IMPLEMENTED,
            "{} {} 必须回答闭集 not_implemented",
            route.method,
            route.path
        );
        let err = closed_set_error(response).await;
        assert_eq!(err.code, ErrorCode::NotImplemented);
        assert!(
            err.message.contains(route.path),
            "错误消息要点名命中的路由：{}",
            err.message
        );
    }
}

#[tokio::test]
async fn an_unknown_contract_path_is_a_closed_set_not_found() {
    let app = app();
    let response = app
        .oneshot(request(Method::GET, "/v1/tracker/nope", Some(OP_TOKEN)))
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let err = closed_set_error(response).await;
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn an_unknown_path_is_not_found_even_without_the_op_token() {
    // 未知路径不属契约面：回答与 token 无关，两个面都不泄露信息。
    let app = app();
    let response = app
        .oneshot(request(Method::GET, "/v1/tracker/nope", None))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(closed_set_error(response).await.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn a_known_route_with_the_wrong_method_is_a_closed_set_method_not_allowed() {
    // GET-only 路由收到 POST。
    let app1 = app();
    let response = app1
        .oneshot(request(Method::POST, "/v1/tracker/issues", Some(OP_TOKEN)))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        closed_set_error(response).await.code,
        ErrorCode::MethodNotAllowed
    );

    // POST-only 路由收到 GET。
    let app2 = app();
    let response = app2
        .oneshot(request(Method::GET, "/v1/tracker/claims", Some(OP_TOKEN)))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        closed_set_error(response).await.code,
        ErrorCode::MethodNotAllowed
    );
}

#[tokio::test]
async fn error_envelopes_are_the_same_shape_on_every_face() {
    let responses = [
        (Method::GET, "/v1/tracker/issues", None),     // unauthorized
        (Method::GET, "/v1/nope", Some(OP_TOKEN)),      // not_found
    ];
    let mut codes = Vec::new();
    for (method, uri, token) in responses {
        let app = app();
        let response = app
            .oneshot(request(method, uri, token))
            .await
            .expect("oneshot");
        let err = closed_set_error(response).await;
        // 信封形状：code + message + retryable，无 details。
        let json = err.to_json();
        assert!(json.contains("\"code\""), "{json}");
        assert!(json.contains("\"retryable\""), "{json}");
        assert!(!json.contains("\"details\""), "details 缺省不出现：{json}");
        codes.push(err.code);
    }
    assert_eq!(codes, vec![ErrorCode::Unauthorized, ErrorCode::NotFound]);
}

#[tokio::test]
async fn the_router_is_reusable_across_requests() {
    // axum Router 是 cheap-clone 的 Service：同一个实例能连续接多个请求。
    let app: Router = app();
    for _ in 0..3 {
        let response = app
            .clone()
            .oneshot(request(Method::GET, "/v1/tracker/issues", Some(OP_TOKEN)))
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
    }
}

// ---------------------------------------------------------------------------
// Workspace 服务（ticket #14）：GET /v1/workspace/mappings 返回映射视图
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_workspace_mappings_route_returns_an_empty_view_when_nothing_is_mapped() {
    let app = app();
    let response = app
        .oneshot(request(Method::GET, "/v1/workspace/mappings", Some(OP_TOKEN)))
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::OK);
    let body: MappingListResponse = body_json(response).await;
    assert!(body.mappings.is_empty(), "无映射时返回空清单");
}

#[tokio::test]
async fn the_workspace_mappings_route_projects_the_persisted_mapping_view() {
    let mut state = common::state();
    // 经 Workspace 服务 write-ahead 落一条 pending 映射（投影为 Creating）。
    state
        .workspace
        .reserve(&EstablishRequest {
            spec_id: "spec-10".into(),
            integration_branch: "implement/spec-10".into(),
            ticket_id: "14".into(),
            repo_path: "/repo".into(),
            worktree_path: "/wt/t14".into(),
            worktree_branch: "farm/t14".into(),
            start_point: None,
        })
        .expect("reserve seeds the host-private store");
    let app = build_router(state);

    let response = app
        .oneshot(request(Method::GET, "/v1/workspace/mappings", Some(OP_TOKEN)))
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::OK);
    let body: MappingListResponse = body_json(response).await;
    assert_eq!(body.mappings.len(), 1);
    assert_eq!(body.mappings[0].spec_id, "spec-10");
    assert_eq!(body.mappings[0].integration_branch, "implement/spec-10");
    assert_eq!(body.mappings[0].worktrees.len(), 1);
    let wt = &body.mappings[0].worktrees[0];
    assert_eq!(wt.ticket_id, "14");
    assert_eq!(wt.branch, "farm/t14");
    assert_eq!(wt.state, WorktreeState::Creating, "pending 投影为 Creating");
}
