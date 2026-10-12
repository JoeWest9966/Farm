//! 宿主管理 API（host admin API）接缝测试（ticket #23，GLOSSARY「宿主管理 API」）。
//!
//! 全部经 axum `tower::ServiceExt::oneshot` 打**真实 router**（[`farm_host::build_router`]），
//! 不 mock 内部协作者。核心不变量（ADR-0002 §1 + spec 新裁决）：管理面与编排器契约**边界分明**——
//! 独立 `/admin` 前缀 + 独立管理 token（`X-Farm-Admin-Token`），与操作 token 不是同一个凭据，
//! **交叉访问被拒**；契约 API 无法触达管理面操作（负向）。

use std::path::PathBuf;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{header, Method, Request, StatusCode};
use farm_core::plugin::{PluginHost, PluginSpec, PluginState, PluginStatus, RetryPolicy};
use farm_host::build_router;
use farm_sdk::admin::{AdminPluginListResponse, PluginSummary, TrackerProviderConfig};
use farm_sdk::{ContractError, ErrorCode};
use tower::ServiceExt;

mod common;
use common::{admin_app, admin_state, ADMIN_TOKEN, OP_TOKEN};

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

// ---------------------------------------------------------------------------
// 插件装卸：GET /admin/plugins 投影监督状态（#19 plugin_state → PluginSummary）
// ---------------------------------------------------------------------------

/// 直接往宿主私有存储落一条插件状态（不经子进程，测投影 wiring）。
fn seed_plugin(state: &farm_core::AppState, id: &str, name: &str, st: PluginState, restarts: u32) {
    state
        .plugin
        .store()
        .upsert(&PluginStatus {
            plugin_id: id.into(),
            name: name.into(),
            program: format!("/usr/bin/{id}"),
            state: st,
            restarts,
            pid: None,
            last_error: None,
            next_restart_at: None,
            updated_at: "2025-01-01T00:00:00Z".into(),
        })
        .expect("seed plugin row");
}

async fn get_plugins(app: axum::Router) -> (StatusCode, AdminPluginListResponse) {
    let response = app
        .oneshot(admin_request(Method::GET, "/admin/plugins", Some(ADMIN_TOKEN)))
        .await
        .expect("oneshot");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.expect("body");
    (status, serde_json::from_slice(&bytes).expect("plugin list json"))
}

#[tokio::test]
async fn the_admin_plugin_list_projects_the_supervised_plugin_states() {
    // 宿主监督的插件（#19 plugin_state 表）经管理面投影成 PluginSummary，按 plugin_id 排序。
    let state = admin_state();
    seed_plugin(&state, "orch", "orchestrator", PluginState::Running, 0);
    seed_plugin(&state, "notifier", "http notifier", PluginState::Quarantined, 4);

    let (status, body) = get_plugins(build_router(state)).await;
    assert_eq!(status, StatusCode::OK);
    let plugins = body.plugins;
    assert_eq!(plugins.len(), 2, "两个被监督插件都要投影出来");
    assert_eq!(plugins[0].plugin_id, "notifier", "按 plugin_id 排序稳定");
    assert_eq!(plugins[0].state, PluginState::Quarantined);
    assert_eq!(plugins[0].restarts, 4, "隔离后的重试账面随投影带出");
    assert_eq!(plugins[1].plugin_id, "orch");
    assert_eq!(plugins[1].state, PluginState::Running);
}

#[tokio::test]
async fn the_admin_plugin_list_is_empty_when_no_plugin_is_supervised() {
    let (status, body) = get_plugins(admin_app()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.plugins.is_empty());
}

#[tokio::test]
async fn the_admin_plugin_list_also_requires_the_admin_token() {
    // 与其它管理面路由同一中间件：缺管理 token → unauthorized。
    let app = admin_app();
    let response = app
        .oneshot(admin_request(Method::GET, "/admin/plugins", None))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// 插件恢复：POST /admin/plugins/{id}/recover（GLOSSARY「隔离」一键恢复，#19 recover）
// ---------------------------------------------------------------------------

/// 带管理 token 的 POST（恢复路由；空体，无 body 抽取，故不需要 content-type）。
fn admin_post(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("x-farm-admin-token", token)
        .body(Body::empty())
        .expect("request")
}

/// 带管理 token 的 JSON POST。
fn admin_post_json(uri: &str, token: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("x-farm-admin-token", token)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

/// fake-plugin 替身路径：cargo 把 bins 与测试二进制放进同一个 `<target>/<profile>/`
/// （测试二进制在 `deps/` 子目录，bins 在上一级），故从 `current_exe` 上跳两层。
fn fake_plugin_path() -> PathBuf {
    let mut path = std::env::current_exe().expect("test binary path");
    path.pop(); // <profile>/deps
    path.pop(); // <profile>
    path.join("fake-plugin")
}

#[tokio::test]
async fn the_recover_route_is_guarded_by_the_admin_token() {
    // 证明恢复路由已挂载且盖着管理面中间件：op token 递上来 → forbidden（403），
    // 而非「路由不存在」的 404。
    let app = admin_app();
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/plugins/fake-plugin/recover")
                .header("x-farm-token", OP_TOKEN)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn recovering_an_unknown_plugin_is_not_found() {
    let app = admin_app();
    let response = app
        .oneshot(admin_post("/admin/plugins/ghost/recover", ADMIN_TOKEN))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(closed_set_error(response).await.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn recover_relaunches_a_quarantined_plugin_through_the_admin_api() {
    // 端到端：经管理 API 恢复一个真被子进程崩溃耗尽而隔离的插件 → 重启并自愈（running）。
    let fake_plugin = fake_plugin_path();
    if !fake_plugin.exists() {
        eprintln!(
            "SKIP recover happy-path: fake-plugin not built at {}（跑 cargo test --workspace 即构建）",
            fake_plugin.display()
        );
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir").keep();
    let counter = dir.join("crash-once.counter");
    std::fs::write(&counter, "0").expect("counter starts at zero");
    let log = dir.join("fake-plugin.log");
    std::fs::write(&log, "").expect("empty log");

    let mut state = admin_state();
    // max_restarts=0：crash-once 第一次崩即越界 → 立刻隔离（不必真睡退避）。
    let host = PluginHost::with_policy(&state.db, RetryPolicy::with_schedule(vec![10], 0));
    state = state.with_plugin(host);
    let spec = PluginSpec::new("fake-plugin", "fake plugin", fake_plugin.display().to_string())
        .with_env("FAKE_PLUGIN_SCENARIO", "crash-once")
        .with_env("FAKE_PLUGIN_LOG", log.display().to_string())
        .with_env("FAKE_PLUGIN_COUNTER", counter.display().to_string())
        .with_handshake_timeout(Duration::from_millis(400))
        .with_shutdown_grace(Duration::from_millis(500));

    let _ = state.plugin.launch(&state, &spec).await; // 崩一次 → 越界 → 隔离
    state
        .plugin
        .wait_for_state("fake-plugin", PluginState::Quarantined, Duration::from_secs(10))
        .await
        .expect("crash-once must quarantine under max_restarts=0");

    // 经管理 API 恢复（clone 一份给 router，原句柄留着收尾）。
    let app = build_router(state.clone());
    let response = app
        .oneshot(admin_post("/admin/plugins/fake-plugin/recover", ADMIN_TOKEN))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::OK, "恢复路由必须放行并重启插件");
    let bytes = to_bytes(response.into_body(), usize::MAX).await.expect("body");
    let summary: PluginSummary = serde_json::from_slice(&bytes).expect("summary json");
    assert_eq!(summary.plugin_id, "fake-plugin");
    assert_eq!(summary.state, PluginState::Running, "恢复后回到 running（自愈）");
    assert_eq!(summary.restarts, 0, "恢复重置重试预算");

    state.plugin.shutdown_all().await;
}

// ---------------------------------------------------------------------------
// tracker provider 配置（adapter 选择）：GET/POST /admin/tracker/provider
// ---------------------------------------------------------------------------

async fn get_provider(app: axum::Router) -> (StatusCode, TrackerProviderConfig) {
    let response = app
        .oneshot(admin_request(Method::GET, "/admin/tracker/provider", Some(ADMIN_TOKEN)))
        .await
        .expect("oneshot");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.expect("body");
    (status, serde_json::from_slice(&bytes).expect("provider config json"))
}

#[tokio::test]
async fn the_tracker_provider_defaults_to_native() {
    // v1 唯一 adapter 是 native；未配置时管理面报 native + 已知清单。
    let (status, config) = get_provider(admin_app()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(config.provider, "native");
    assert_eq!(config.known_providers, vec!["native".to_string()]);
}

#[tokio::test]
async fn setting_a_known_tracker_provider_persists_across_requests() {
    let app = admin_app();
    let response = app
        .clone()
        .oneshot(admin_post_json(
            "/admin/tracker/provider",
            ADMIN_TOKEN,
            r#"{"provider":"native"}"#,
        ))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::OK);

    // 同一宿主（同一 host_meta）再读：设置已持久。
    let (status, config) = get_provider(app).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(config.provider, "native");
}

#[tokio::test]
async fn setting_an_unknown_tracker_provider_is_invalid_request() {
    // github adapter 在 v1 未实现：闭集 invalid_request（400），不是 500。
    let app = admin_app();
    let response = app
        .oneshot(admin_post_json(
            "/admin/tracker/provider",
            ADMIN_TOKEN,
            r#"{"provider":"github"}"#,
        ))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(closed_set_error(response).await.code, ErrorCode::InvalidRequest);
}

#[tokio::test]
async fn the_tracker_provider_route_is_guarded_by_the_admin_token() {
    // op token 递到 tracker provider 管理路由 → forbidden（403）。
    let app = admin_app();
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/admin/tracker/provider")
                .header("x-farm-token", OP_TOKEN)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// 服务启停：GET /admin/services + POST /admin/services/{name}/stop|start
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_service_control_status_lists_all_five_services_running() {
    let app = admin_app();
    let response = app
        .oneshot(admin_request(Method::GET, "/admin/services", Some(ADMIN_TOKEN)))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.expect("body");
    let status: farm_sdk::admin::ServiceControlStatus =
        serde_json::from_slice(&bytes).expect("service status json");
    assert_eq!(status.services.len(), 5, "ADR-0001 五服务");
    assert!(
        status
            .services
            .iter()
            .all(|entry| entry.state == farm_sdk::admin::ServiceRunState::Running),
        "初始五服务俱 running"
    );
}

#[tokio::test]
async fn stopping_a_service_through_the_admin_api_flips_only_that_service() {
    let app = admin_app();
    // 停机 tracker。
    let response = app
        .clone()
        .oneshot(admin_post("/admin/services/tracker/stop", ADMIN_TOKEN))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.expect("body");
    let entry: farm_sdk::admin::ServiceControlStatusEntry =
        serde_json::from_slice(&bytes).expect("entry json");
    assert_eq!(entry.service, farm_sdk::ServiceName::Tracker);
    assert_eq!(entry.state, farm_sdk::admin::ServiceRunState::Stopped);

    // 再读全量：tracker stopped，其余仍 running。
    let response = app
        .oneshot(admin_request(Method::GET, "/admin/services", Some(ADMIN_TOKEN)))
        .await
        .expect("oneshot");
    let bytes = to_bytes(response.into_body(), usize::MAX).await.expect("body");
    let status: farm_sdk::admin::ServiceControlStatus =
        serde_json::from_slice(&bytes).expect("status json");
    let tracker = status
        .services
        .iter()
        .find(|entry| entry.service == farm_sdk::ServiceName::Tracker)
        .expect("tracker present");
    assert_eq!(tracker.state, farm_sdk::admin::ServiceRunState::Stopped);
    assert!(
        status
            .services
            .iter()
            .filter(|entry| entry.service != farm_sdk::ServiceName::Tracker)
            .all(|entry| entry.state == farm_sdk::admin::ServiceRunState::Running),
        "只停被点的那个服务"
    );
}

#[tokio::test]
async fn starting_a_stopped_service_returns_it_to_running() {
    let app = admin_app();
    app.clone()
        .oneshot(admin_post("/admin/services/merge/stop", ADMIN_TOKEN))
        .await
        .expect("stop");
    let response = app
        .oneshot(admin_post("/admin/services/merge/start", ADMIN_TOKEN))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.expect("body");
    let entry: farm_sdk::admin::ServiceControlStatusEntry =
        serde_json::from_slice(&bytes).expect("entry json");
    assert_eq!(entry.state, farm_sdk::admin::ServiceRunState::Running);
}

#[tokio::test]
async fn controlling_an_unknown_service_is_invalid_request() {
    let app = admin_app();
    let response = app
        .oneshot(admin_post("/admin/services/nope/stop", ADMIN_TOKEN))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(closed_set_error(response).await.code, ErrorCode::InvalidRequest);
}

#[tokio::test]
async fn the_service_control_route_is_guarded_by_the_admin_token() {
    let app = admin_app();
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/admin/services")
                .header("x-farm-token", OP_TOKEN)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// 边界负向：契约 API 无法触达管理面操作（ADR-0002 §1：边界分明，不是私有后门）
// ---------------------------------------------------------------------------

/// 带 op token、指定方法的请求（负向测试用）。
fn op_token_request_at(method: Method, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("x-farm-token", OP_TOKEN)
        .body(Body::empty())
        .expect("request")
}

#[test]
fn the_contract_face_and_the_admin_face_live_under_disjoint_prefixes() {
    use farm_sdk::{ADMIN_ROUTES, CONTRACT_ROUTES, HANDSHAKE_PATH};
    for route in CONTRACT_ROUTES {
        assert!(route.path.starts_with("/v1"), "契约路由须在 /v1 下：{}", route.path);
        assert!(!route.path.starts_with("/admin"), "契约面不得伸进管理面前缀：{}", route.path);
    }
    assert!(!HANDSHAKE_PATH.starts_with("/admin"), "握手路由不属管理面");
    for route in ADMIN_ROUTES {
        assert!(route.path.starts_with("/admin"), "管理面路由须在 /admin 下：{}", route.path);
    }
}

#[tokio::test]
async fn the_contract_face_has_no_route_for_host_admin_operations() {
    // 即便带上合法 op token，契约面也没有插件/服务/tracker-config 这些管理操作的入口——
    // 它们不是 /v1 路由 → not_found。管理面操作只能经 /admin + 管理 token 触达。
    let app = admin_app();
    for (method, uri) in [
        (Method::GET, "/v1/plugins"),
        (Method::POST, "/v1/plugins/fake-plugin/recover"),
        (Method::GET, "/v1/services"),
        (Method::POST, "/v1/services/tracker/stop"),
        (Method::GET, "/v1/tracker/provider"),
        (Method::POST, "/v1/tracker/provider"),
    ] {
        let response = app
            .clone()
            .oneshot(op_token_request_at(method.clone(), uri))
            .await
            .expect("oneshot");
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "{method} {uri} 不应作为契约路由存在（管理操作不经契约面）"
        );
    }
}

#[tokio::test]
async fn the_admin_token_opens_no_contract_route() {
    // 管理 token 递到契约路由：契约面只认 op token → unauthorized。
    let app = admin_app();
    for uri in ["/v1/tracker/issues", "/v1/workspace/mappings", "/v1/events/history"] {
        let response = app
            .clone()
            .oneshot(admin_request(Method::GET, uri, Some(ADMIN_TOKEN)))
            .await
            .expect("oneshot");
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "管理 token 打不开契约路由 {uri}"
        );
    }
}

#[tokio::test]
async fn the_op_token_opens_no_admin_route() {
    // op token 递到任一管理面路由：越面 → forbidden。
    let app = admin_app();
    for (method, uri) in [
        (Method::GET, "/admin/plugins"),
        (Method::GET, "/admin/status"),
        (Method::GET, "/admin/services"),
        (Method::GET, "/admin/tracker/provider"),
        (Method::POST, "/admin/plugins/fake-plugin/recover"),
        (Method::POST, "/admin/services/tracker/stop"),
        (Method::POST, "/admin/services/tracker/start"),
    ] {
        let response = app
            .clone()
            .oneshot(op_token_request_at(method.clone(), uri))
            .await
            .expect("oneshot");
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "op token 打不开管理路由 {method} {uri}"
        );
    }
}
