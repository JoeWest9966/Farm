//! 事件总线契约：事件闭集、审计双写集、通知闭包、信封盖章。

use farm_sdk::events::{EventEnvelope, EventKind, NewEvent, WsHello};
use farm_sdk::{ContractError, CONTRACT_VERSION};

/// 独立已知清单（GLOSSARY + ADR-0005）：闭集一旦增删即失败。
#[test]
fn event_kind_closed_set_is_exactly_the_contract_documented_set() {
    let expected = [
        "dispatch",
        "attempt_started",
        "agent_settled",
        "attempt_finished",
        "blocked",
        "needs_attention",
        "steer",
        "abort",
        "stall_kill",
        "merge",
        "approval",
        "interrupted",
        "host_restarted",
        "claim_acquired",
        "plugin_quarantined",
        "notification_sent",
    ];
    let actual: Vec<&str> = EventKind::ALL.iter().map(|k| k.as_str()).collect();
    assert_eq!(actual, expected);
}

#[test]
fn blocked_and_needs_attention_are_first_class_events() {
    assert!(EventKind::ALL.contains(&EventKind::Blocked));
    assert!(EventKind::ALL.contains(&EventKind::NeedsAttention));
}

#[test]
fn notification_critical_closure_is_the_four_documented_events() {
    let critical: Vec<&str> = EventKind::NOTIFICATION_CRITICAL
        .iter()
        .map(|k| k.as_str())
        .collect();
    assert_eq!(critical, ["blocked", "needs_attention", "merge", "stall_kill"]);
    for kind in EventKind::NOTIFICATION_CRITICAL {
        assert!(kind.is_notification_critical(), "{} 必须在通知闭包内", kind);
    }
    assert!(!EventKind::AttemptStarted.is_notification_critical());
}

#[test]
fn audit_critical_events_match_adr_0005_double_write_list() {
    let audit: Vec<&str> = EventKind::ALL
        .iter()
        .filter(|k| k.is_audit_critical())
        .map(|k| k.as_str())
        .collect();
    assert_eq!(
        audit,
        ["dispatch", "stall_kill", "merge", "approval", "host_restarted"]
    );
}

#[test]
fn event_envelope_is_stamped_by_the_host_and_starts_unaudited() {
    let new = NewEvent::new(EventKind::Dispatch, "agent-runner")
        .with_ticket("11")
        .with_attempt("attempt-1")
        .with_payload(serde_json::json!({"backend": "local"}));

    let env = EventEnvelope::from_new(new, "evt-1", "2026-01-01T00:00:00Z");

    assert_eq!(env.id, "evt-1");
    assert_eq!(env.kind, EventKind::Dispatch);
    assert_eq!(env.actor, "agent-runner");
    assert_eq!(env.ticket_id.as_deref(), Some("11"));
    assert_eq!(env.attempt_id.as_deref(), Some("attempt-1"));
    assert_eq!(env.payload["backend"], "local");
    assert!(!env.audited, "未入账事件默认带 audited=false（ADR-0005 §8）");
}

#[test]
fn ws_hello_is_distinguishable_from_the_error_envelope() {
    let hello = WsHello::new(CONTRACT_VERSION, EventKind::ALL.to_vec());
    let json = serde_json::to_string(&hello).expect("serialize");
    let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");

    assert_eq!(value["type"], "hello");
    assert!(value.get("code").is_none(), "hello 不是错误信封");
    assert_eq!(value["contract_version"], CONTRACT_VERSION);

    let err = ContractError::unauthorized("ws upgrade without op token");
    let err_value: serde_json::Value =
        serde_json::from_str(&err.to_json()).expect("valid json");
    assert!(err_value.get("type").is_none(), "错误信封没有 type 字段");
    assert_eq!(err_value["code"], "unauthorized");
}
