//! ticket #20 剩余验收：审计补账（reconcile）、崩溃恢复（recover_on_startup）、
//! 面板读账 `--agg` 侧车调用形状。

use std::path::Path;

use farm_core::audit::AuditSink;
use farm_core::event_log::EventLogFilter;
use farm_core::recovery::recover_on_startup;
use farm_core::{AppState, HostConfig};
use farm_sdk::events::{EventKind, NewEvent};
use serde_json::Value;

const FAKE_LEDGER: &str = env!("CARGO_BIN_EXE_fake-ledger");

fn state() -> AppState {
    AppState::new_in_memory(HostConfig {
        bind: "127.0.0.1:3721".parse().expect("bind"),
        op_token: "unit-op-token".into(),
        admin_token: None,
        db_path: std::env::temp_dir().join("farm-core-audit-recovery.db"),
    })
    .expect("in-memory state")
}

fn recording_sink(log_path: &Path) -> AuditSink {
    AuditSink::new(FAKE_LEDGER)
        .with_record_args(vec![
            "record".to_string(),
            "--log".to_string(),
            log_path.display().to_string(),
        ])
        .with_actor_farm_accepted(true)
}

#[test]
fn reconcile_backfills_unaudited_audit_critical_events_idempotently() {
    let state = state();
    // 旁路禁用时发布：审计关键事件落 SQLite 但 audited=0（主流程不受影响）。
    state
        .publish(NewEvent::new(EventKind::Dispatch, "agent-runner").with_ticket("20"))
        .expect("publish dispatch");
    state
        .publish(NewEvent::new(EventKind::Merge, "host").with_ticket("20"))
        .expect("publish merge");
    // 一条非审计关键事件：补账不应碰它。
    state
        .publish(NewEvent::new(EventKind::ClaimAcquired, "tracker").with_ticket("20"))
        .expect("publish claim");

    let unposted_before = state
        .event_log
        .unposted(&state.db, 100)
        .expect("unposted");
    assert_eq!(unposted_before.len(), 3, "三条都未入账（旁路禁用）");

    // 装上 fake ledger 旁路 + 对账补账。
    let dir = tempfile::tempdir().expect("tempdir");
    let log_path = dir.path().join("ledger.log");
    std::fs::write(&log_path, "").expect("empty log");
    let state = state.with_audit_sink(recording_sink(&log_path));

    let backfilled = state.audit.reconcile(&state).expect("reconcile");
    assert_eq!(backfilled, 2, "只补审计关键的两条（dispatch + merge）");

    // 补账后：审计关键事件 audited=1，非关键的 claim 仍 audited=0。
    let still_unposted = state.event_log.unposted(&state.db, 100).expect("unposted");
    assert_eq!(still_unposted.len(), 1, "只剩非审计关键的 claim 未入账");
    assert_eq!(still_unposted[0].kind, EventKind::ClaimAcquired);

    // 幂等：再对账一次不重复补（已无未入账的审计关键事件）。
    let again = state.audit.reconcile(&state).expect("reconcile again");
    assert_eq!(again, 0, "二次对账无账可补（幂等）");
}

#[test]
fn startup_recovery_emits_a_host_restarted_event() {
    let state = state();
    let report = recover_on_startup(&state).expect("recovery");
    // 无 running attempt（内存库空）→ 无 interrupted；旁路禁用 → 无补账。
    assert!(report.interrupted.is_empty());
    assert_eq!(report.backfilled, 0);

    // host_restarted 事件经持久化发布路径落进事件日志。
    let history = state
        .event_history(&EventLogFilter {
            since: None,
            kind: Some(EventKind::HostRestarted),
            ticket_id: None,
            limit: None,
        })
        .expect("history");
    assert_eq!(history.events.len(), 1, "重启补一条 host_restarted");
    assert_eq!(history.events[0].actor, "host");
}

#[test]
fn the_panel_reads_the_ledger_via_a_low_frequency_agg_sidecar() {
    let sink = AuditSink::new(FAKE_LEDGER).with_actor_farm_accepted(true);
    let out = sink.read_aggregate().expect("read --agg");
    let parsed: Value = serde_json::from_str(&out).expect("agg json");
    let argv = parsed["argv"].as_array().expect("argv array");
    assert!(argv.iter().any(|a| a == "read"), "侧车子命令是 read：{argv:?}");
    assert!(argv.iter().any(|a| a == "--agg"), "低频聚合走 --agg：{argv:?}");
}
