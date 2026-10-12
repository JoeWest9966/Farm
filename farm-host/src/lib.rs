//! # farm-host — Farm 宿主（唯一 binary，ADR-0003 五分法第五层）
//!
//! axum 承载三个面：
//!
//! 1. **编排器契约面**：`POST /handshake` + `CONTRACT_ROUTES` 九条路由（其中
//!    `GET /v1/events/stream` 是 WS、`GET /v1/events/history` 是事件快照（#15 投影面），
//!    `GET /v1/workspace/mappings`（#14 映射视图）、tracker 的 `GET /v1/tracker/issues` 与
//!    `POST /v1/tracker/claims`（#13）也已真正打通；其余返回闭集 `not_implemented`——归 #16–#18。
//! 2. **宿主管理 API**：`ADMIN_ROUTES` 若干，独立前缀 `/admin` + **独立管理 token**
//!    （`X-Farm-Admin-Token`，#23）：插件装卸/恢复、tracker provider 配置、服务启停。
//!    管理 token ≠ 操作 token（spec 新裁决）——`admin::require_admin_token` 校验，交叉访问被拒。
//! 3. **面板伺服**：`build.rs` 把 `farm-panel` 编成 wasm 并嵌入，`/` 伺服。
//!
//! 鉴权：契约面（ticket #11）单 op token 静态鉴权，缺 token 与错 token 同码同消息
//! （`unauthorized` / 401）；管理面（ticket #23）独立管理 token，op token 递到管理面 →
//! `forbidden`（越面），缺/错管理 token → `unauthorized`。两面凭据不同、边界分明（ADR-0002 §1）。
//!
//! [`build_router`] 是宿主路由的唯一装配点：`main` 与测试用它拿到**同一个**
//! axum `Router`，所以测试在真实接缝上断言，不另造一个 app。

pub mod admin;
pub mod agent_runner;
pub mod auth;
pub mod contract;
pub mod error;
pub mod events;
pub mod events_ws;
pub mod merge;
pub mod panel;
pub mod workspace;
pub mod workflow;

use axum::middleware;
use axum::routing::{get, post};
use axum::Router;
use farm_core::AppState;
use farm_sdk::routes::{ADMIN_ROUTES, CONTRACT_ROUTES};
use farm_sdk::HANDSHAKE_PATH;

pub use error::ApiError;
pub use panel::PanelAssets;

/// 契约面上已真正打通的 route：WS 事件流 + 事件快照（#15）、tracker 两条（#13）、
/// workspace 映射（#14）；其余仍是占位（返回闭集 `not_implemented`）。
const EVENTS_STREAM_PATH: &str = "/v1/events/stream";
const EVENTS_HISTORY_PATH: &str = "/v1/events/history";
const TRACKER_ISSUES_PATH: &str = "/v1/tracker/issues";
const TRACKER_CLAIMS_PATH: &str = "/v1/tracker/claims";

/// Workspace 服务映射视图（ticket #14 打通）：`GET /v1/workspace/mappings`。
const WORKSPACE_MAPPINGS_PATH: &str = "/v1/workspace/mappings";

/// Agent-Runner 三条路由（ticket #16 打通）：派发 + steer/abort 注入。
/// #22 追加 follow-up / clear-queue 两条注入路由（面板 HITL：改道、放弃改道）。
const AGENT_DISPATCH_PATH: &str = "/v1/agent-runner/dispatches";
const AGENT_STEER_PATH: &str = "/v1/agent-runner/attempts/{attempt_id}/steer";
const AGENT_ABORT_PATH: &str = "/v1/agent-runner/attempts/{attempt_id}/abort";
const AGENT_FOLLOW_UP_PATH: &str = "/v1/agent-runner/attempts/{attempt_id}/follow-up";
const AGENT_CLEAR_QUEUE_PATH: &str = "/v1/agent-runner/attempts/{attempt_id}/clear-queue";

/// Tracker 的 release 路由（ticket #22 打通）：交还排他所有权。
const TRACKER_RELEASE_PATH: &str = "/v1/tracker/releases";

/// Merge 服务提议路由（ticket #18 打通）：`POST /v1/merge/proposals`。
const MERGE_PROPOSALS_PATH: &str = "/v1/merge/proposals";
/// Agent 提交 merge 提议进人工审阅队列（ticket #25 打通）：`POST /v1/merge/reviews`。
const MERGE_REVIEWS_PATH: &str = "/v1/merge/reviews";

/// 装配宿主路由：三个面 + 统一的闭集 404/405 兜底。
///
/// 路由表是单一源（`farm-sdk::routes`）：宿主按表挂载，所以表里新增一条路由
/// 而这里没跟上时，会在装配时直接 panic——契约表和宿主不允许悄悄漂移。
pub fn build_router(state: AppState) -> Router {
    // --- 契约面：按 CONTRACT_ROUTES 单一源挂载 ---
    let mut contract = Router::new().route(HANDSHAKE_PATH, post(contract::handshake));
    for route in CONTRACT_ROUTES {
        let endpoint = match route.path {
            EVENTS_STREAM_PATH => get(events_ws::events_stream),
            EVENTS_HISTORY_PATH => get(events::events_history),
            TRACKER_ISSUES_PATH => get(contract::tracker_issues),
            TRACKER_CLAIMS_PATH => post(contract::tracker_claims),
            TRACKER_RELEASE_PATH => post(contract::tracker_release),
            WORKSPACE_MAPPINGS_PATH => get(workspace::list_mappings),
            AGENT_DISPATCH_PATH => post(agent_runner::dispatch),
            AGENT_STEER_PATH => post(agent_runner::steer),
            AGENT_ABORT_PATH => post(agent_runner::abort),
            AGENT_FOLLOW_UP_PATH => post(agent_runner::follow_up),
            AGENT_CLEAR_QUEUE_PATH => post(agent_runner::clear_queue),
            MERGE_PROPOSALS_PATH => post(merge::propose),
            MERGE_REVIEWS_PATH => post(merge::submit_review),
            _ => match route.method {
                "GET" => get(contract::not_implemented_placeholder),
                "POST" => post(contract::not_implemented_placeholder),
                other => panic!(
                    "CONTRACT_ROUTES 只声明 GET/POST；{} 用了 {other}",
                    route.path
                ),
            },
        };
        contract = contract.route(route.path, endpoint);
    }
    // 单 token 静态鉴权只盖契约面（含 /handshake 与 WS upgrade）。
    // `route_layer` 而非 `layer`：未挂载路径不过中间件，直接落到顶层 404，
    // 所以「未知契约路由」的回答与 token 无关。
    let contract = contract.route_layer(middleware::from_fn_with_state(
        state.clone(),
        auth::require_op_token,
    ));

    // --- 宿主管理 API：前缀独立，管理 token 校验机制（#23）---
    let mut admin = Router::new();
    for route in ADMIN_ROUTES {
        let endpoint = match (route.method, route.path) {
            ("GET", "/admin/plugins") => get(admin::plugins),
            ("POST", "/admin/plugins/{id}/recover") => post(admin::recover),
            ("GET", "/admin/status") => get(admin::status),
            ("GET", "/admin/tracker/provider") => get(admin::tracker_provider),
            ("POST", "/admin/tracker/provider") => post(admin::set_tracker_provider),
            ("GET", "/admin/services") => get(admin::services),
            ("POST", "/admin/services/{name}/stop") => post(admin::stop_service),
            ("POST", "/admin/services/{name}/start") => post(admin::start_service),
            ("GET", "/admin/workflow/config/{repo_id}") => get(workflow::config_status),
            ("GET", "/admin/merge/proposals") => get(admin::merge_proposals),
            ("GET", "/admin/merge/proposals/{id}") => get(admin::merge_proposal),
            ("POST", "/admin/merge/proposals/{id}/approve") => post(admin::approve_proposal),
            ("POST", "/admin/merge/proposals/{id}/reject") => post(admin::reject_proposal),
            _ => panic!(
                "ADMIN_ROUTES 里的 {} {} 未在 farm-host 挂载",
                route.method, route.path
            ),
        };
        admin = admin.route(route.path, endpoint);
    }
    // 管理 token 静态鉴权只盖管理面（#23）：独立凭据，与契约面的 op token 边界分明。
    // `route_layer` 而非 `layer`：未挂载路径不过中间件；交叉访问在中间件里被判
    // （op token → forbidden、缺/错管理 token → unauthorized，见 `admin::require_admin_token`）。
    let admin = admin.route_layer(middleware::from_fn_with_state(
        state.clone(),
        admin::require_admin_token,
    ));

    // --- 面板：嵌入资产，`/` 伺服，不带 op token ---
    let panel = panel::router();

    Router::new()
        .merge(contract)
        .merge(admin)
        .merge(panel)
        .fallback(contract::unknown_route)
        .method_not_allowed_fallback(contract::method_not_allowed)
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use farm_core::HostConfig;

    pub(crate) fn test_state() -> AppState {
        AppState::new_in_memory(HostConfig {
            bind: "127.0.0.1:3721".parse().expect("valid bind"),
            op_token: "test-op-token".into(),
            admin_token: None,
            db_path: std::env::temp_dir().join("farm-host-test.db"),
        })
        .expect("in-memory state")
    }

    #[test]
    fn the_router_assembles_from_the_contract_route_table() {
        // 装配本身即是断言：表与宿主不一致会在这里 panic。
        let _ = build_router(test_state());
    }
}
