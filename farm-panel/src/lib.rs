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

/// 面板标题。宿主 embed 的 `index.html` 与面板自身渲染同一份自描述。
pub const PANEL_TITLE: &str = "Farm 操作台";

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

/// 占位页根组件：自描述 + 操作台（merge 审阅流 + 宿主管理面，ticket #25）。
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
            OperatorConsole {}
        }
    }
}

/// 提议在看板/审阅队列里的一行标题（纯函数，便于断言）。
#[cfg(target_arch = "wasm32")]
fn proposal_heading(detail: &farm_sdk::services::merge::MergeProposalDetail) -> String {
    format!("票 {} · {:?}", detail.ticket_id, detail.state)
}

/// 单条 merge 提议审阅卡：呈现 diff + （待审时）批准/拒绝按钮。
/// 跑一次批准/拒绝：拼 ADMIN 请求 → 传输 → 解 → 把结论写进消息信号（wasm 侧）。
#[cfg(target_arch = "wasm32")]
async fn do_review(
    base: Signal<String>,
    admin_token: Signal<String>,
    mut message: Signal<String>,
    proposal_id: String,
    approve: bool,
) {
    use crate::core::admin::{AdminClient, AdminDecoded, AdminOp};
    use crate::core::transport;
    let client = AdminClient::new(base(), admin_token());
    let op = if approve {
        AdminOp::ApproveMerge { proposal_id, reviewer: "panel-operator".into() }
    } else {
        AdminOp::RejectMerge { proposal_id, reviewer: "panel-operator".into(), reason: None }
    };
    let req = client.request_for(&op);
    let verdict = match transport::send(client.base_url(), &req).await {
        Ok(resp) => match client.decode(&op, resp) {
            Ok(AdminDecoded::Reviewed(r)) => format!("{} → {:?}", r.proposal_id, r.state),
            Ok(_) => "unexpected admin response".to_string(),
            Err(e) => format!("{e}"),
        },
        Err(e) => e,
    };
    message.set(verdict);
}

/// 单条 merge 提议审阅卡：呈现 diff + （待审时）批准/拒绝按钮。
///
/// 自持（Copy 的 signal 作 props）：批准/拒绝经 [`do_review`] 打 ADMIN API（管理 token）。
#[cfg(target_arch = "wasm32")]
#[component]
fn ProposalCard(
    detail: farm_sdk::services::merge::MergeProposalDetail,
    base: Signal<String>,
    admin_token: Signal<String>,
    message: Signal<String>,
) -> Element {
    let key = detail.proposal_id.clone();
    let awaiting = detail.state == farm_sdk::services::merge::MergeProposalState::Proposed;
    let approve = {
        let id = detail.proposal_id.clone();
        move |_| {
            spawn(do_review(base, admin_token, message, id.clone(), true));
        }
    };
    let reject = {
        let id = detail.proposal_id.clone();
        move |_| {
            spawn(do_review(base, admin_token, message, id.clone(), false));
        }
    };
    rsx! {
        article { key: "{key}",
            h3 { "{proposal_heading(&detail)}" }
            pre { "{detail.diff}" }
            if awaiting {
                button { onclick: approve, "批准" }
                button { onclick: reject, "拒绝" }
            }
        }
    }
}

/// 操作台（ticket #25 批次三）：merge 审阅流 + 宿主管理面，全经管理 token 打 ADMIN API。
///
/// 请求由数据核（[`AdminClient`]）拼、字节由传输层（[`transport::send`]）搬；本组件
/// 只持少量 UI 状态（base url / 管理 token / 队列 / 消息），不碰路径、不碰 schema。
/// op token 与管理 token 都不落盘，由使用者输入（spec 安全裁决）。
#[cfg(target_arch = "wasm32")]
#[component]
fn OperatorConsole() -> Element {
    use crate::core::admin::{AdminClient, AdminDecoded, AdminOp};
    use crate::core::transport;
    use farm_sdk::services::merge::MergeProposalDetail;

    let mut base = use_signal(|| "http://127.0.0.1:3721".to_string());
    let mut admin_token = use_signal(String::new);
    let mut queue = use_signal(Vec::<MergeProposalDetail>::new);
    let mut message = use_signal(String::new);

    // 加载审阅队列：GET /admin/merge/proposals（管理 token）。
    let load_queue = move |_| {
        let base = base();
        let token = admin_token();
        spawn(async move {
            let client = AdminClient::new(base, token);
            let op = AdminOp::LoadMergeQueue;
            let req = client.request_for(&op);
            match transport::send(client.base_url(), &req).await {
                Ok(resp) => match client.decode(&op, resp) {
                    Ok(AdminDecoded::MergeQueue(q)) => {
                        message.set(format!("审阅队列：{} 条待审", q.proposals.len()));
                        queue.set(q.proposals);
                    }
                    Ok(_) => message.set("unexpected admin response".into()),
                    Err(e) => message.set(format!("{e}")),
                },
                Err(e) => message.set(e),
            }
        });
    };

    rsx! {
        section {
            h2 { "merge 审阅流（ticket #25）" }
            p { "人工闸门叠加在机械闸门之上：批准后机械校验照跑出 merge commit，拒绝走闭集错误回 agent。" }
            div { class: "console-auth",
                input { value: "{base}", oninput: move |e| base.set(e.value()) }
                input { r#type: "password", placeholder: "管理 token（不落盘）", value: "{admin_token}", oninput: move |e| admin_token.set(e.value()) }
                button { onclick: load_queue, "加载审阅队列" }
            }
            p { class: "console-message", "{message}" }
            for detail in queue() {
                ProposalCard { detail: detail, base: base, admin_token: admin_token, message: message }
            }
        }
        section {
            h2 { "宿主管理面（#23/#25）" }
            ul {
                li { "插件装卸 / quarantined 一键恢复（/admin/plugins）" }
                li { "tracker provider 配置（/admin/tracker/provider）" }
                li { "服务启停（/admin/services）" }
                li { "通知通道配置（依赖 #21）" }
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
