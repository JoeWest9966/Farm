//! 面板伺服 HTTP 接缝测试。
//!
//! 背景（白屏实证回归）：资产**嵌入**二进制 ≠ 资产**能被伺服**。曾出现路由
//! 静态注册但 handler 用 `Path<String>` 提取器，请求期 axum 500（"Expected 1
//! but got 0"）——`index.html` 之外的资源全 500，浏览器白屏。单测当时只断言
//! 资产清单，抓不到。这里全部经 axum `oneshot` 打**真实 router** 真调路由。

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use farm_host::{build_router, PanelAssets};
use tower::ServiceExt;

mod common;

/// GET 一个面板路径，返回 (status, content-type, body)。
async fn get_panel(app: &Router, path: &str) -> (StatusCode, Option<String>, axum::body::Bytes) {
    let response = app
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).expect("request"))
        .await
        .expect("infallible");
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().expect("ascii").to_string());
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024 * 1024)
        .await
        .expect("body");
    (status, content_type, body)
}

/// 入口页 200 且引用 JS 胶水。
#[tokio::test]
async fn entry_page_serves_and_references_the_js_glue() {
    let app = common::app();
    let (status, content_type, body) = get_panel(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("text/html; charset=utf-8"));
    let html = String::from_utf8(body.to_vec()).expect("utf-8");
    assert!(
        html.contains("farm_panel.js"),
        "入口页必须引用 wasm 胶水，实际：{html}"
    );
}

/// 每个嵌入资产经**真实路由**伺服：200、状态不是 axum 提取错误的 500、
/// 内容非空且 MIME 与扩展名一致。这是白屏回归的主断言。
#[tokio::test]
async fn every_embedded_asset_serves_over_the_real_router() {
    let app = common::app();
    let paths: Vec<String> = PanelAssets::iter()
        .filter(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
        })
        .map(|p| p.to_string())
        .collect();
    assert!(
        paths.iter().any(|p| p.ends_with(".js")),
        " fixtures 里必须有 JS 胶水，实际：{paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.ends_with(".wasm")),
        " fixtures 里必须有 wasm，实际：{paths:?}"
    );
    for path in &paths {
        let (status, content_type, body) = get_panel(&app, &format!("/{path}")).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "面板资产 /{path} 必须能经真实路由伺服（白屏回归）"
        );
        assert!(!body.is_empty(), "/{path} 响应体为空");
        let expected = match path.rsplit('.').next() {
            Some("html") => "text/html; charset=utf-8",
            Some("js") => "text/javascript; charset=utf-8",
            Some("wasm") => "application/wasm",
            Some("json") => "application/json",
            Some("css") => "text/css; charset=utf-8",
            Some("ts") => "text/plain; charset=utf-8",
            _ => "application/octet-stream",
        };
        assert_eq!(
            content_type.as_deref(),
            Some(expected),
            "/{path} MIME 漂移：{content_type:?} ≠ {expected}"
        );
    }
}

/// 不存在的资产路径：404 语义归契约面的 `not_found`，不许退化成 500。
#[tokio::test]
async fn unknown_panel_asset_is_not_found_not_500() {
    let app = common::app();
    let (status, _, body) = get_panel(&app, "/definitely-not-an-asset.js").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let text = String::from_utf8(body.to_vec()).unwrap_or_default();
    assert!(
        !text.contains("Wrong number of path arguments"),
        "提取器错误不允许泄漏到响应"
    );
}
