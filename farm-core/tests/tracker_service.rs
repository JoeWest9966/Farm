//! Tracker 服务 v1 的公共接缝测试（ticket #13）。
//!
//! GLOSSARY：Tracker 服务 = 归一化 Issue 模型 + 原生 blocking 边 + 原子 claim
//! （CAS 语义，assignee 即 claim），把 provider 语义关进 adapter。面板与编排器
//! 读同一个归一化视图——tracker 是总线。
//!
//! 接缝选在**服务面**（`TrackerService`）与宿主路由（farm-host 集成测试），
//! 不碰 SQLite 内部：schema 是宿主私有实现细节（ADR-0005 §5）。

use std::sync::Arc;

use farm_core::state::AppState;
use farm_core::tracker::{
    IssueState, NativeAdapter, NativeTicket, TrackerAdapter, TrackerService,
};
use farm_core::{HostConfig};

fn config() -> HostConfig {
    HostConfig {
        bind: "127.0.0.1:3721".parse().expect("valid bind"),
        op_token: "tracker-test-token".into(),
        admin_token: None,
        db_path: std::env::temp_dir().join("farm-tracker-test.db"),
    }
}

fn state_with(adapter: Arc<dyn TrackerAdapter>) -> AppState {
    let mut state = AppState::new_in_memory(config()).expect("in-memory state");
    state.tracker = TrackerService::new(state.db.clone(), adapter);
    state
}

fn native(tickets: Vec<NativeTicket>) -> Arc<dyn TrackerAdapter> {
    Arc::new(NativeAdapter::from_tickets(tickets))
}

/// 三张原生票：#1、#2 是 #13 的 blocker（provider 原生依赖边）。
fn three_tickets() -> Vec<NativeTicket> {
    vec![
        NativeTicket {
            number: 1,
            title: "host skeleton".into(),
            ..Default::default()
        },
        NativeTicket {
            number: 2,
            title: "workflow config".into(),
            ..Default::default()
        },
        NativeTicket {
            number: 13,
            title: "tracker service".into(),
            labels: vec!["tracker".into()],
            blocked_by: vec![1, 2],
            ..Default::default()
        },
    ]
}

#[test]
fn the_normalized_view_exposes_native_blocking_edges() {
    let state = state_with(native(three_tickets()));

    let issues = state.tracker.issues().expect("normalized view");

    let find = |id: &str| {
        issues
            .iter()
            .find(|i| i.issue_id == id)
            .unwrap_or_else(|| panic!("归一化视图必须包含票 {id}"))
            .clone()
    };

    let ticket = find("13");
    assert_eq!(ticket.title, "tracker service");
    assert_eq!(ticket.labels, vec!["tracker".to_string()]);
    assert_eq!(ticket.state, IssueState::Open);
    assert_eq!(ticket.assignee, None, "未被 claim 的票没有 assignee");

    // 边是双向可读的：声明方读 blocked_by，被堵方读 blocking。
    assert_eq!(ticket.blocked_by, vec!["1".to_string(), "2".to_string()]);
    assert_eq!(find("1").blocking, vec!["13".to_string()]);
    assert_eq!(find("2").blocking, vec!["13".to_string()]);
    assert!(find("1").blocked_by.is_empty(), "无依赖的票 blocked_by 为空");
}
