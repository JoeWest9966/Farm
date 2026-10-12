//! 面板伺服（ticket #11）：rust-embed 嵌入的 Dioxus CSR 产物。
//!
//! ADR-0003 决策 4（单二进制）：面板 wasm 由 `build.rs` 编好、连同 `index.html`
//! 一起嵌进 `farm-host` 二进制，运行时从 `/` 伺服——集群节点捡起来就能用，
//! 没有外部静态目录。
//!
//! 资产清单在编译期由 `PanelAssets::iter()` 给出，所以这里**逐个资产注册路由**
//! 而不是挂一个 `/{*path}` 通配：通配会抢走契约面/管理面的未知路径，让
//! 「未知契约路由 → `not_found`」退化成「未知契约路由 → 面板资产 404」。
//! 代价是路由清单依赖编译期资产集合，换来三个面的 404 语义各自干净。
//!
//! 面板是静态资产，**不带 op token**：token 由使用者在页面输入后随契约请求发送
//! （面板作为契约客户端，与编排器同路；归 #22）。

mod assets {
    //! build.rs 生成的 `RustEmbed` 派生（`#[folder]` 只接受字面量，路径由
    //! `PANEL_DIST` 环境变量在编译期写进来）。
    include!(concat!(env!("OUT_DIR"), "/panel_assets.rs"));
}

pub use assets::PanelAssets;

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use farm_core::AppState;
use farm_sdk::ContractError;

use crate::error::ApiError;

/// 面板路由：`/` 与每个嵌入资产的路径。
pub fn router() -> Router<AppState> {
    let mut router: Router<AppState> = Router::new().route("/", get(index));
    for path in PanelAssets::iter() {
        if is_static_asset_path(&path) {
            let route = format!("/{path}");
            router = router.route(&route, get(asset));
        }
    }
    router
}

/// `GET /`：面板入口页。
async fn index() -> Response {
    asset_response("index.html")
}

/// `GET /<asset>`：嵌入的面板产物（wasm / JS 胶水 / 源码片段）。
async fn asset(axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    asset_response(&path)
}

fn asset_response(path: &str) -> Response {
    match PanelAssets::get(path) {
        Some(file) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, content_type_for(path))],
            file.data.into_owned(),
        )
            .into_response(),
        None => ApiError(ContractError::not_found(format!(
            "no panel asset at /{path} on this host"
        )))
        .into_response(),
    }
}

/// 只注册能安全变成 axum 静态路由的路径（排除 `{`/`}`/`*` 等 matchit 保留字符）。
fn is_static_asset_path(path: &str) -> bool {
    !path.is_empty()
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
}

/// 面板产物的 MIME：wasm 必须是 `application/wasm`，否则浏览器拒绝实例化。
fn content_type_for(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("json") => "application/json",
        Some("css") => "text/css; charset=utf-8",
        Some("map") => "application/json",
        Some("ts") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_panel_contains_an_entry_page() {
        let paths: Vec<String> = PanelAssets::iter().map(|p| p.to_string()).collect();
        assert!(
            paths.iter().any(|p| p == "index.html"),
            "面板必须带 index.html，实际嵌入：{paths:?}"
        );
    }

    #[test]
    fn the_embedded_panel_contains_wasm_and_its_js_glue() {
        let paths: Vec<String> = PanelAssets::iter().map(|p| p.to_string()).collect();
        assert!(
            paths.iter().any(|p| p.ends_with(".wasm")),
            "面板 wasm 必须嵌入，实际嵌入：{paths:?}"
        );
        assert!(
            paths.iter().any(|p| p.ends_with(".js")),
            "wasm-bindgen JS 胶水必须嵌入，实际嵌入：{paths:?}"
        );
    }

    #[test]
    fn wasm_is_served_with_the_wasm_content_type() {
        assert_eq!(content_type_for("farm_panel_bg.wasm"), "application/wasm");
        assert_eq!(content_type_for("index.html"), "text/html; charset=utf-8");
        assert_eq!(content_type_for("farm_panel.js"), "text/javascript; charset=utf-8");
    }

    #[test]
    fn only_plain_asset_paths_become_routes() {
        assert!(is_static_asset_path("farm_panel.js"));
        assert!(is_static_asset_path("snippets/dioxus-web-abc/src/js/eval.js"));
        assert!(!is_static_asset_path("weird{*path"));
    }
}
