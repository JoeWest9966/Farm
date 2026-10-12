//! tick 控制面对编排器循环的可观测性（ticket #24）。
//!
//! 验收：「暂停/恢复 tick 对编排器循环可观测生效」。本文件用两种观测面钉住它：
//!
//! 1. **宿主权威态**：`AppState::control`（同进程查询，如 #26 一致性套件）；
//! 2. **契约事件总线**：`GET /v1/events/history` / WS——进程外编排器唯一的观测面。
//!    编排器循环折叠 `kind = "tick_control"` 事件即得当前 tick 态。
//!
//! 两种观测面必须一致，否则「暂停了但编排器不知道」这种静默漂移就会发生。

use farm_core::AppState;
use farm_core::HostConfig;
use farm_core::{EventLogFilter, TickControl};
use farm_sdk::control::{TickControlRequest, TickControlStatus, TickState};

fn state() -> AppState {
    AppState::new_in_memory(HostConfig {
        bind: "127.0.0.1:3721".parse().expect("bind"),
        op_token: "unit-op-token".into(),
        admin_token: None,
        db_path: std::env::temp_dir().join("farm-core-tick-control.db"),
    })
    .expect("in-memory state")
}

/// 进程外编排器唯一的观测面：从事件历史折叠出当前 tick 态。
fn folded_tick_state(host: &AppState) -> TickState {
    host.event_history(&EventLogFilter::default())
        .expect("history")
        .events
        .iter()
        .rev()
        .find_map(|event| {
            (event.payload["kind"] == "tick_control")
                .then(|| TickState::parse(event.payload["state"].as_str().unwrap_or_default()))
                .flatten()
        })
        .unwrap_or(TickState::Running)
}

/// 一个编排器 tick 的最小形状：先读控制面，`paused` 就跳过本 tick。
fn orchestrator_tick(host: &AppState) -> bool {
    if folded_tick_state(host).is_paused() {
        return false;
    }
    true
}

#[test]
fn the_tick_starts_running_and_the_orchestrator_sees_the_same_thing() {
    let host = state();
    assert!(!host.control.is_paused());
    assert_eq!(folded_tick_state(&host), TickState::Running);
    assert!(orchestrator_tick(&host), "未暂停时 tick 照常派发");
}

#[test]
fn pausing_the_tick_is_observable_to_the_orchestrator_loop() {
    let host = state();
    host.control
        .pause(
            &host,
            &TickControlRequest {
                reason: Some("human paused the tick from the panel".into()),
            },
            "panel",
        )
        .expect("pause");

    assert!(host.control.is_paused(), "宿主权威态 = 暂停");
    assert_eq!(folded_tick_state(&host), TickState::Paused, "事件折叠态 = 暂停");
    assert!(!orchestrator_tick(&host), "编排器循环跳过本 tick 的派发");

    host.control.resume(&host, "panel").expect("resume");
    assert!(!host.control.is_paused());
    assert_eq!(folded_tick_state(&host), TickState::Running);
    assert!(orchestrator_tick(&host), "恢复后 tick 照常");
}

#[test]
fn the_pause_event_carries_the_reason_and_the_actor_for_the_orchestrator() {
    let host = state();
    let status: TickControlStatus = host
        .control
        .pause(
            &host,
            &TickControlRequest {
                reason: Some("holding the frontier for a human review".into()),
            },
            "panel",
        )
        .expect("pause");

    assert_eq!(status.state, TickState::Paused);
    assert_eq!(
        status.reason.as_deref(),
        Some("holding the frontier for a human review")
    );
    assert_eq!(status.actor.as_deref(), Some("panel"));
    assert!(status.updated_at.is_some(), "切换时间可审计");

    let history = host.event_history(&EventLogFilter::default()).expect("history");
    let event = history
        .events
        .iter()
        .find(|event| event.payload["kind"] == "tick_control")
        .expect("tick control event");
    assert_eq!(event.payload["state"], "paused");
    assert_eq!(event.payload["reason"], "holding the frontier for a human review");
    assert_eq!(event.payload["actor"], "panel");
    assert_eq!(event.kind, farm_sdk::EventKind::NeedsAttention, "一等事件");
}

#[test]
fn every_transition_is_folded_idempotently_by_the_orchestrator() {
    let host = state();
    for _ in 0..3 {
        host.control
            .pause(&host, &TickControlRequest { reason: None }, "panel")
            .expect("pause");
    }
    assert_eq!(folded_tick_state(&host), TickState::Paused, "连按三次还是暂停");
    host.control.resume(&host, "panel").expect("resume");
    host.control.resume(&host, "panel").expect("resume again");
    assert_eq!(folded_tick_state(&host), TickState::Running, "连按两次恢复还是运行");
}

#[test]
fn the_tick_control_is_volatile_across_a_restart_but_recoverable_from_event_history() {
    // ADR-0005 §2：调度瞬态易失。重启后宿主从 running 起，但编排器能从事件历史
    // （契约路由 GET /v1/events/history）折叠出重启前最后一次切换。
    use std::path::Path;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("farm.db");
    {
        let host = AppState::new(HostConfig {
            bind: "127.0.0.1:3721".parse().expect("bind"),
            op_token: "unit-op-token".into(),
            admin_token: None,
            db_path: path.clone(),
        })
        .expect("state");
        host.control
            .pause(&host, &TickControlRequest { reason: Some("afk".into()) }, "panel")
            .expect("pause");
        assert!(Path::new(&path).exists());
    }
    let restarted = AppState::new(HostConfig {
        bind: "127.0.0.1:3721".parse().expect("bind"),
        op_token: "unit-op-token".into(),
        admin_token: None,
        db_path: path,
    })
    .expect("restarted state");

    assert!(
        !restarted.control.is_paused(),
        "控制面本身易失：重启后回到 running"
    );
    assert_eq!(
        folded_tick_state(&restarted),
        TickState::Paused,
        "但编排器从事件历史折叠出的仍是暂停——恢复视图有据可依"
    );
}

#[test]
fn a_control_handle_can_be_swapped_on_the_shared_state() {
    // 加法接缝：宿主装配出的 AppState 一定带 control 句柄，且可整体替换（测试替身）。
    let host = state().with_control(TickControl::new());
    host.control
        .pause(&host, &TickControlRequest { reason: None }, "panel")
        .expect("pause");
    assert!(host.control.is_paused());
}
