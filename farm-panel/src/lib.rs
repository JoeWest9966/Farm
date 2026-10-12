//! Farm 面板（Operator Console，ADR-0002）—— Dioxus 纯 CSR 占位页（ticket #11）。
//!
//! ADR-0003 决策 1/4：面板是 Dioxus **纯 CSR** crate，编译为
//! `wasm32-unknown-unknown`，由 `farm-host` 经 rust-embed 嵌入、以 `/` 伺服。
//! 本票只落地「能在浏览器打开的占位页」：页面自描述契约版本、五服务与事件闭集，
//! 让宿主 embed 后的产物**可验证**（面板与宿主测试共用 [`panel_lines`]）。
//!
//! 面板作为**契约客户端**的真实调用（读看板、HITL、dispatch、审阅流）归 #22/#24/#25；
//! 本票不发任何请求——op token 也不落盘，由使用者在页面输入。

pub mod core;

use farm_sdk::{EventKind, ServiceName, CONTRACT_VERSION, SDK_VERSION};

/// 占位页标题。宿主 embed 的 `index.html` 与面板自身渲染同一份自描述。
pub const PANEL_TITLE: &str = "Farm 操作台 — 占位页";

/// 占位页正文行：契约版本、五服务、事件闭集、接入方式。
///
/// 纯函数（不依赖 DOM / wasm）：宿主侧测试与面板渲染共用同一份期望，
/// 所以「面板有没有把契约版本亮出来」是可以被断言的。
pub fn panel_lines() -> Vec<String> {
    let services = ServiceName::ALL
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(" / ");
    vec![
        format!("契约版本（contract version）：{CONTRACT_VERSION}"),
        format!("五服务（ADR-0001）：{services}"),
        format!("事件闭集（event closed set）：{} 种", EventKind::ALL.len()),
        format!("farm-sdk：{SDK_VERSION}"),
        "接入方式：契约 HTTP API + WS 事件流；op token 由使用者输入，不落盘（归 #22）".to_string(),
    ]
}

#[cfg(target_arch = "wasm32")]
use dioxus::prelude::*;

/// 占位页根组件。
#[cfg(target_arch = "wasm32")]
#[component]
fn App() -> Element {
    rsx! {
        main {
            h1 { "{PANEL_TITLE}" }
            ul {
                for line in panel_lines() {
                    li { key: "{line}", "{line}" }
                }
            }
        }
    }
}

/// wasm 入口：`wasm-bindgen(start)` 让 lib crate 也能被 `wasm-bindgen --target web`
/// 找到入口（cdylib 没有 bin 的 `main` 符号可依赖）。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn mount_panel() {
    dioxus::launch(App);
}

#[cfg(test)]
mod tests {
    use super::*;
    use farm_sdk::EventKind;

    #[test]
    fn placeholder_page_advertises_the_contract_version() {
        let joined = panel_lines().join("\n");
        assert!(
            joined.contains(CONTRACT_VERSION),
            "占位页必须亮出契约版本，实际：{joined}"
        );
    }

    #[test]
    fn placeholder_page_lists_the_five_services() {
        let joined = panel_lines().join("\n");
        for service in ServiceName::ALL {
            assert!(
                joined.contains(service.as_str()),
                "占位页缺服务 {}（ADR-0001 五服务）",
                service
            );
        }
    }

    #[test]
    fn placeholder_page_mentions_the_event_closed_set_size() {
        let joined = panel_lines().join("\n");
        assert!(
            joined.contains(&format!("{}", EventKind::ALL.len())),
            "占位页应说明事件闭集规模，实际：{joined}"
        );
    }

    #[test]
    fn panel_title_names_the_operator_console() {
        assert!(PANEL_TITLE.contains("操作台"), "GLOSSARY 正式名是操作台");
    }
}
