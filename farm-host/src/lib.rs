//! # farm-host — Farm 宿主（唯一 binary，ADR-0003 五分法第五层）
//!
//! axum 承载三个面：
//!
//! 1. **编排器契约面**：`POST /handshake` + `CONTRACT_ROUTES` 九条路由（其中
//!    `GET /v1/events/stream` 是 WS、`GET /v1/workspace/mappings` 是 #14 映射视图，
//!    二者已真正打通）；其余返回闭集 `not_implemented`——服务域实现归 #13–#18。
//! 2. **宿主管理 API**：`ADMIN_ROUTES` 两条，独立前缀 `/admin`；管理 token
//!    校验机制是接口位，归 #23。
//! 3. **面板伺服**：`build.rs` 把 `farm-panel` 编成 wasm 并嵌入，`/` 伺服。
//!
//! 鉴权（ticket #11）：契约面（含 `/handshake` 与 WS upgrade）单 token 静态鉴权，
//! 缺 token 与错 token 同码同消息（`unauthorized` / 401），避免预言机。
//!
//! [`build_router`] 是宿主路由的唯一装配点：`main` 与测试用它拿到**同一个**
//! axum `Router`，所以测试在真实接缝上断言，不另造一个 app。

pub mod admin;
pub mod auth;
pub mod contract;
pub mod error;
pub mod events_ws;
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

/// 契约面上已真正打通的 route：WS 事件流（#11）+ Workspace 映射视图（#14）；
/// 其余仍是占位（返回闭集 `not_implemented`）。
const EVENTS_STREAM_PATH: &str = "/v1/events/stream";

/// Workspace 服务映射视图（ticket #14 打通）：`GET /v1/workspace/mappings`。
const WORKSPACE_MAPPINGS_PATH: &str = "/v1/workspace/mappings";

/// 装配宿主路由：三个面 + 统一的闭集 404/405 兜底。
///
/// 路由表是单一源（`farm-sdk::routes`）：宿主按表挂载，所以表里新增一条路由
/// 而这里没跟上时，会在装配时直接 panic——契约表和宿主不允许悄悄漂移。
pub fn build_router(state: AppState) -> Router {
    // --- 契约面：按 CONTRACT_ROUTES 单一源挂载 ---
    let mut contract = Router::new().route(HANDSHAKE_PATH, post(contract::handshake));
    for route in CONTRACT_ROUTES {
        let endpoint = if route.path == EVENTS_STREAM_PATH {
            get(events_ws::events_stream)
        } else if route.path == WORKSPACE_MAPPINGS_PATH {
            get(workspace::list_mappings)
        } else {
            match route.method {
                "GET" => get(contract::not_implemented_placeholder),
                "POST" => post(contract::not_implemented_placeholder),
                other => panic!(
                    "CONTRACT_ROUTES 只声明 GET/POST；{} 用了 {other}",
                    route.path
                ),
            }
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

    // --- 宿主管理 API：前缀独立，管理 token 机制是接口位（#23）---
    let mut admin = Router::new();
    for route in ADMIN_ROUTES {
        let endpoint = match (route.method, route.path) {
            ("GET", "/admin/plugins") => get(admin::plugins),
            ("GET", "/admin/status") => get(admin::status),
            ("GET", "/admin/workflow/config/{repo_id}") => get(workflow::config_status),
            _ => panic!(
                "ADMIN_ROUTES 里的 {} {} 未在 farm-host 挂载",
                route.method, route.path
            ),
        };
        admin = admin.route(route.path, endpoint);
    }

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
