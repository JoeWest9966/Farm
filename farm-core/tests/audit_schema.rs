//! Kron 账本记录形状（ticket #20，ADR-0005 §3/§8）。
//!
//! 接缝：[`farm_core::audit::KronRecord::from_event`] —— 宿主事件 → `ledger record`
//! 的一行 NDJSON。这一层把两件事钉死：
//!
//! 1. **形状照抄 Kron schema**（serde + `deny_unknown_fields`）：字段名/闭集/嵌套结构
//!    与 `docs/research/report-nous-family.md` §3.2 文档化的 Kron `internal/schema` 对齐；
//! 2. **红线「禁入」**：消息正文、prompt、文件内容一律不进账——`details` 只放
//!    标识字段，事件 payload 原样留下宿主私有存储里（面板读源）。
//!
//! 期望值全部是**已知字面量**（不重算实现），所以形状一漂测试就红。

use farm_core::audit::{KronActor, KronEventType, KronRecord};
use farm_sdk::events::{EventEnvelope, EventKind, NewEvent};

/// 一个已盖章的信封（id / occurred_at 由总线给；这里手搓以固定期望值）。
fn envelope(kind: EventKind, payload: serde_json::Value) -> EventEnvelope {
    EventEnvelope::from_new(
        NewEvent::new(kind, "agent-runner")
            .with_ticket("20")
            .with_attempt("att-1")
            .with_payload(payload),
        "evt-1700000000-000001",
        "2026-01-01T00:00:00Z",
    )
}

#[test]
fn a_dispatch_event_maps_into_the_documented_kron_record_shape() {
    let record = KronRecord::from_event(&envelope(
        EventKind::Dispatch,
        serde_json::json!({"profile": "implement-spec"}),
    ))
    .expect("dispatch 是审计双写关键事件，必须有 Kron 记录");

    // 已知字面量：字段名、闭集成员、嵌套形状一处都不能漂。
    let expected = serde_json::json!({
        "event": "run_boundary",
        "actor": "farm",
        "ts": "2026-01-01T00:00:00Z",
        "subject_id": "attempt:att-1",
        "idempotency_key": "farm:evt-1700000000-000001",
        "details": {
            "event_id": "evt-1700000000-000001",
            "kind": "dispatch",
            "occurred_at": "2026-01-01T00:00:00Z",
            "actor": "agent-runner",
            "ticket_id": "20",
            "attempt_id": "att-1",
        },
    });
    assert_eq!(
        serde_json::to_value(&record).expect("record must serialize"),
        expected,
        "Kron NDJSON 行必须是文档化形状（serde + deny_unknown_fields）"
    );
}

#[test]
fn only_the_six_audit_critical_kinds_reach_the_kron_ledger() {
    let critical = [
        (EventKind::Dispatch, "run_boundary"),
        (EventKind::Merge, "run_boundary"),
        (EventKind::Approval, "approval"),
        (EventKind::StallKill, "escalation"),
        (EventKind::HostRestarted, "escalation"),
        // #19：插件崩溃后分级退避重启（ADR-0004 决策 4）与宿主重启同路进账。
        (EventKind::PluginRestarted, "escalation"),
    ];
    for (kind, kron_event) in critical {
        let record = KronRecord::from_event(&envelope(kind, serde_json::json!({})))
            .unwrap_or_else(|| panic!("{kind} 是关键事件，必须能入账"));
        assert_eq!(record.event.as_str(), kron_event, "{kind} 的 Kron 事件类型");
        assert!(kind.is_audit_critical(), "{kind} 必须在闭集里就算关键");
    }

    // 负向：非关键事件不入账（它们只在宿主私有存储里，audited 对它们无意义）。
    for kind in EventKind::ALL.iter().filter(|k| !k.is_audit_critical()) {
        assert!(
            KronRecord::from_event(&envelope(*kind, serde_json::json!({}))).is_none(),
            "{kind} 不是审计双写关键事件，不得进 Kron"
        );
    }
}

#[test]
fn the_kron_record_rejects_unknown_fields_on_both_sides() {
    let record = KronRecord::from_event(&envelope(EventKind::Dispatch, serde_json::json!({})))
        .expect("dispatch maps to a kron record");
    let mut value = serde_json::to_value(&record).expect("serialize");

    // 上游多给了字段（Kron schema 扩位 / 我们漏抄）：必须响亮失败，绝不静默丢弃。
    value["cursor"] = serde_json::json!(12);
    let err = serde_json::from_value::<KronRecord>(value.clone())
        .expect_err("deny_unknown_fields：未知字段必须被拒");
    assert!(
        err.to_string().contains("cursor"),
        "错误信息要点名那个字段：{err}"
    );

    // 嵌套的 details 同样封闭。
    value.as_object_mut().unwrap().remove("cursor");
    value["details"]["prompt"] = serde_json::json!("禁入正文");
    let err = serde_json::from_value::<KronRecord>(value)
        .expect_err("details 也是封闭 struct");
    assert!(err.to_string().contains("prompt"), "{err}");
}

#[test]
fn the_idempotency_key_is_stable_per_event_and_distinct_across_events() {
    let first = KronRecord::from_event(&envelope(EventKind::Dispatch, serde_json::json!({})))
        .expect("dispatch maps");
    let again = KronRecord::from_event(&envelope(EventKind::Dispatch, serde_json::json!({})))
        .expect("dispatch maps again");
    assert_eq!(
        first.idempotency_key, again.idempotency_key,
        "同一笔事件补账多少次都是同一个自然键（ledger 幂等的地基）"
    );

    let other = KronRecord::from_event(&EventEnvelope::from_new(
        NewEvent::new(EventKind::Dispatch, "agent-runner").with_ticket("20"),
        "evt-1700000000-000002",
        "2026-01-01T00:00:01Z",
    ))
    .expect("dispatch maps");
    assert_ne!(
        first.idempotency_key, other.idempotency_key,
        "不同事件是不同的键"
    );
}

#[test]
fn the_event_payload_never_reaches_the_ledger() {
    // Kron 红线「禁入」：消息正文 / prompt / 文件内容不进账，只进宿主私有存储。
    let secret = "SECRET-PROMPT-BODY";
    let record = KronRecord::from_event(&envelope(
        EventKind::Merge,
        serde_json::json!({"prompt": secret, "diff": "file contents"}),
    ))
    .expect("merge maps");
    let line = serde_json::to_string(&record).expect("serialize");
    assert!(!line.contains(secret), "正文禁入：{line}");
    assert!(!line.contains("file contents"), "文件内容禁入：{line}");
    assert!(!line.contains("payload"), "payload 整个字段都不进账：{line}");
}

#[test]
fn the_subject_falls_back_from_attempt_to_ticket_to_the_event_itself() {
    let with_attempt = KronRecord::from_event(&envelope(EventKind::Dispatch, serde_json::json!({})))
        .expect("dispatch maps");
    assert_eq!(with_attempt.subject_id, "attempt:att-1");

    let with_ticket = KronRecord::from_event(&EventEnvelope::from_new(
        NewEvent::new(EventKind::HostRestarted, "host").with_ticket("20"),
        "evt-1700000000-000003",
        "2026-01-01T00:00:02Z",
    ))
    .expect("host_restarted maps");
    assert_eq!(with_ticket.subject_id, "ticket:20");

    let bare = KronRecord::from_event(&EventEnvelope::from_new(
        NewEvent::new(EventKind::HostRestarted, "host"),
        "evt-1700000000-000004",
        "2026-01-01T00:00:03Z",
    ))
    .expect("host_restarted maps");
    assert_eq!(bare.subject_id, "event:evt-1700000000-000004");
    assert_eq!(bare.details.ticket_id, None, "缺 ticket 就不造一个假的");
}

#[test]
fn the_writer_actor_is_farm_and_the_cross_repo_extension_is_still_pending() {
    // ADR-0004 决策 2 前置：Kron actor 闭集扩位加入 farm 是跨仓协调。
    // 文档化闭集（今天 Kron 收的五位）+ 待扩位的 farm。
    assert_eq!(
        KronActor::ALL.iter().map(|a| a.as_str()).collect::<Vec<_>>(),
        vec!["sentinel", "whitehelmet", "slaveholder", "crossline", "human", "farm"],
        "actor 闭集 = 文档化五位 + 待扩位的 farm"
    );
    assert_eq!(KronActor::Farm.as_str(), "farm");
    assert!(
        !KronActor::Farm.in_kron_closed_set_today(),
        "farm 尚未进 Kron 闭集：未完成前只标记未入账，不阻塞主流程"
    );
    for actor in KronActor::ALL.iter().filter(|a| **a != KronActor::Farm) {
        assert!(
            actor.in_kron_closed_set_today(),
            "{} 是文档化闭集成员",
            actor.as_str()
        );
    }
}

#[test]
fn every_kron_event_type_is_one_of_the_documented_ten() {
    let all = KronEventType::ALL.iter().map(|e| e.as_str()).collect::<Vec<_>>();
    assert_eq!(
        all,
        vec![
            "token",
            "approval",
            "escalation",
            "read",
            "run_boundary",
            "session_boundary",
            "context_transition",
            "tool_outcome",
            "destruction",
            "integrity",
        ],
        "Kron 10 类事件闭集（report-nous-family §3.2）"
    );
}
