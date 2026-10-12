//! token 三维归集的接缝测试（ticket #22，ADR-0002 §9 绝对累计法）。
//!
//! 夹具是从宿主终态事件的真实 payload 形状手抄的（`farm-core::accounting` 的 `to_json`）。

use farm_panel::core::{day_key, tokens_from_event, TokenAggregate, TokenPayload};
use farm_sdk::{EventEnvelope, EventKind, NewEvent};

/// `agent_settled` 事件上的 token 快照（绝对累计后的权威值）。
fn settled_event(id: &str, ticket: &str, attempt: &str, at: &str, total: u64) -> EventEnvelope {
    let new = NewEvent::new(EventKind::AgentSettled, "agent-runner")
        .with_ticket(ticket)
        .with_attempt(attempt)
        .with_payload(serde_json::json!({
            "state": "settled",
            "reason": "agent_settled with stopReason=stop",
            "tokens": {
                "input": total / 2,
                "output": total - total / 2,
                "cacheRead": 10,
                "cacheWrite": 20,
                "total": total,
                "costMicros": 420_000,
                "streamedTotal": total,
                "authoritativeTotal": total,
                "nestedUsageCalls": 1,
                "countedMessages": 2,
                "truncated": false,
                "note": null
            },
            "accountingTruncated": false
        }));
    EventEnvelope::from_new(new, id, at)
}

fn payload(total: u64) -> TokenPayload {
    TokenPayload {
        input: total / 2,
        output: total - total / 2,
        cache_read: 0,
        cache_write: 0,
        total,
        cost_micros: 0,
        truncated: false,
    }
}

#[test]
fn the_same_attempts_later_snapshot_replaces_the_earlier_one() {
    // 防重：流式对账会让同一 attempt 出现多份快照，账面取最后一份，不累加。
    let mut ledger = TokenAggregate::new();

    assert!(ledger.record("att-1", "22", "2026-01-02T03:00:00Z", payload(100)));
    assert!(ledger.record("att-1", "22", "2026-01-02T03:05:00Z", payload(180)));

    assert_eq!(ledger.global().total, 180, "后帧覆盖前帧，绝不累加");
    assert_eq!(ledger.global().attempts, 1, "一个 attempt 只计一次");
}

#[test]
fn an_out_of_order_older_snapshot_does_not_rewind_the_totals() {
    let mut ledger = TokenAggregate::new();
    ledger.record("att-1", "22", "2026-01-02T03:05:00Z", payload(180));

    let accepted = ledger.record("att-1", "22", "2026-01-02T03:00:00Z", payload(100));

    assert!(!accepted, "乱序旧帧不得入账");
    assert_eq!(ledger.global().total, 180, "账面不回滚");
}

#[test]
fn a_redelivered_frame_does_not_change_the_totals() {
    let mut ledger = TokenAggregate::new();
    ledger.record("att-1", "22", "2026-01-02T03:05:00Z", payload(180));

    let accepted = ledger.record("att-1", "22", "2026-01-02T03:05:00Z", payload(180));

    assert!(!accepted, "同一帧重发是空操作");
    assert_eq!(ledger.global().total, 180);
}

#[test]
fn tokens_aggregate_globally_per_ticket_and_per_day() {
    let mut ledger = TokenAggregate::new();
    ledger.record("att-1", "22", "2026-01-02T03:00:00Z", payload(100));
    ledger.record("att-2", "22", "2026-01-02T04:00:00Z", payload(50));
    ledger.record("att-3", "13", "2026-01-03T04:00:00Z", payload(70));

    assert_eq!(ledger.global().total, 220, "全局三维之一");
    assert_eq!(ledger.global().attempts, 3);

    let ticket = ledger.for_ticket("22");
    assert_eq!(ticket.total, 150, "每票一维");
    assert_eq!(ticket.attempts, 2);
    assert_eq!(ledger.for_ticket("13").total, 70);
    assert_eq!(ledger.for_ticket("999").total, 0, "没跑过的票是 0 不是 panic");

    let days = ledger.per_day();
    assert_eq!(days.len(), 2, "每天一维");
    assert_eq!(days[0].0, "2026-01-02");
    assert_eq!(days[0].1.total, 150);
    assert_eq!(days[1].0, "2026-01-03");
    assert_eq!(days[1].1.total, 70);
}

#[test]
fn a_truncated_snapshot_is_counted_and_flagged() {
    // GLOSSARY「中断态」：记账以最后快照为准并标截断。
    let mut ledger = TokenAggregate::new();
    ledger.record(
        "att-1",
        "22",
        "2026-01-02T03:00:00Z",
        TokenPayload {
            total: 42,
            truncated: true,
            ..payload(42)
        },
    );

    let global = ledger.global();
    assert_eq!(global.total, 42);
    assert_eq!(global.truncated_attempts, 1, "截断的 attempt 要被数出来");
    assert!(global.summary().contains("42 tokens"), "{}", global.summary());
}

#[test]
fn a_payload_without_tokens_is_ignored() {
    let new = NewEvent::new(EventKind::Blocked, "agent-runner")
        .with_ticket("22")
        .with_payload(serde_json::json!({"reason": "read loop died"}));
    let envelope = EventEnvelope::from_new(new, "evt-1", "2026-01-02T03:00:00Z");

    assert!(
        tokens_from_event(&envelope).is_none(),
        "非终态事件没有权威快照，不入账"
    );
}

#[test]
fn only_terminal_events_carry_the_authoritative_snapshot() {
    let settled = settled_event("evt-1", "22", "att-1", "2026-01-02T03:00:00Z", 300);
    assert!(tokens_from_event(&settled).is_some(), "agent_settled 带快照");

    let interrupted = EventEnvelope::from_new(
        NewEvent::new(EventKind::Interrupted, "agent-runner")
            .with_ticket("22")
            .with_attempt("att-2")
            .with_payload(serde_json::json!({
                "reason": "host restarted while the attempt was running",
                "accountingTruncated": true,
                "tokens": { "total": 99, "truncated": true }
            })),
        "evt-2",
        "2026-01-02T04:00:00Z",
    );
    let payload = tokens_from_event(&interrupted).expect("interrupted 带最后快照");
    assert_eq!(payload.total, 99);
    assert!(payload.truncated, "中断态快照必须标 truncated");

    let steer = EventEnvelope::from_new(
        NewEvent::new(EventKind::Steer, "human").with_attempt("att-1"),
        "evt-3",
        "2026-01-02T05:00:00Z",
    );
    assert!(tokens_from_event(&steer).is_none(), "steer 不是终态");
}

#[test]
fn the_day_key_comes_from_the_event_timestamp() {
    assert_eq!(day_key("2026-01-02T03:04:05Z"), "2026-01-02");
    assert_eq!(day_key("2026-01-02"), "2026-01-02");
    assert_eq!(day_key("not-a-timestamp"), "unknown");
}

#[test]
fn the_ledger_remembers_the_last_snapshot_per_attempt() {
    let mut ledger = TokenAggregate::new();
    ledger.record("att-1", "22", "2026-01-02T03:00:00Z", payload(100));
    ledger.record("att-1", "22", "2026-01-02T03:05:00Z", payload(180));

    assert_eq!(ledger.snapshot("att-1").map(|p| p.total), Some(180));
    assert_eq!(ledger.snapshot("att-nope"), None);
    assert_eq!(ledger.attempts(), 1);
    assert_eq!(ledger.tickets(), 1);
}
