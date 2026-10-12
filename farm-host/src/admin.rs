//! 宿主管理 API（host admin API，GLOSSARY）：独立前缀 `/admin`。
//!
//! 与编排器契约**边界分明**：管理面管宿主自身（插件装卸、tracker 配置、服务启停），
//! 契约面管编排原语。本票只留**接口位**：
//!
//! - 前缀独立（`/admin`，与 `/v1` 不重合）；
//! - token header 独立（`X-Farm-Admin-Token`，与 op token 不是同一个凭据，
//!   见 [`farm_sdk::AuthScheme::admin_header`]）；
//! - **管理 token 校验机制本票不实现**（归 #23）：路由已挂上、可返回自描述，
//!   但还没有任何鉴权。`#23` 落地时应在此处加管理面中间件，而不是改路由表。
//!
//! 插件装卸能力归 #19（`GET /admin/plugins` 现在恒回空清单）。

use axum::extract::State;
use axum::Json;
use farm_core::AppState;
use farm_sdk::admin::{AdminPluginListResponse, AdminStatusResponse};
use farm_sdk::{ServiceName, CONTRACT_VERSION, HOST_VERSION};

/// `GET /admin/status`：宿主管理面自描述。
pub async fn status(State(state): State<AppState>) -> Json<AdminStatusResponse> {
    Json(AdminStatusResponse {
        host_version: HOST_VERSION.to_string(),
        contract_version: CONTRACT_VERSION.to_string(),
        bind_addr: state.config.bind.to_string(),
        services: ServiceName::ALL.to_vec(),
    })
}

/// `GET /admin/plugins`：插件清单与状态（装卸能力占位，#19 落地）。
pub async fn plugins() -> Json<AdminPluginListResponse> {
    Json(AdminPluginListResponse { plugins: Vec::new() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use farm_core::HostConfig;
    use farm_sdk::ADMIN_ROUTES;

    fn state() -> AppState {
        AppState::new_in_memory(HostConfig {
            bind: "127.0.0.1:3721".parse().expect("valid bind"),
            op_token: "unit-op-token".into(),
            admin_token: None,
            db_path: std::env::temp_dir().join("farm-host-unit.db"),
        })
        .expect("in-memory state")
    }

    #[tokio::test]
    async fn the_admin_status_describes_the_host() {
        let Json(status) = status(State(state())).await;
        assert_eq!(status.contract_version, CONTRACT_VERSION);
        assert_eq!(status.services.len(), 5, "ADR-0001 五服务");
        assert_eq!(status.bind_addr, "127.0.0.1:3721");
    }

    #[tokio::test]
    async fn the_admin_plugin_list_is_an_empty_placeholder() {
        let Json(list) = plugins().await;
        assert!(list.plugins.is_empty(), "插件装卸归 #19");
    }

    #[test]
    fn every_admin_route_lives_under_the_admin_prefix() {
        for route in ADMIN_ROUTES {
            assert!(route.path.starts_with("/admin"), "{}", route.path);
        }
    }
}
