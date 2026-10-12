//! 事件日志（event log）：宿主私有存储里的事件持久面（ticket #15）。
//!
//! GLOSSARY：事件总线是五服务之一；ADR-0005 §2/§10：**可过滤事件日志**落宿主私有
//! 存储，`events` 表**有界**（保留窗口，超限归档进 Kron 归 #20）。
//! 面板与编排器都不直连 DB，一切读走契约 API / 宿主投影 API（ADR-0005 §5）。

use farm_core::db::Database;
use farm_core::event_log::{EventLog, EventLogFilter};
use farm_core::migrations::{Migrator, MIGRATIONS};
use farm_sdk::events::{EventEnvelope, EventKind, NewEvent};

fn store() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Database::open(dir.path().join("farm.db")).expect("open store");
    db.migrate(&Migrator::new(MIGRATIONS)).expect("migrate");
    (dir, db)
}

fn log() -> EventLog {
    EventLog::new()
}

#[test]
fn a_published_event_lands_in_the_event_log_and_reads_back_identically() {
    let (_dir, db) = store();
    let log = log();

    let published = EventEnvelope::from_new(
        NewEvent::new(EventKind::Blocked, "agent-runner")
            .with_ticket("15")
            .with_attempt("attempt-1")
            .with_payload(serde_json::json!({"reason": "waiting on human"})),
        "evt-1700000000000-000001",
        "2026-01-01T00:00:00Z",
    );
    log.append(&db, &published).expect("append");

    let page = log
        .history(&db, &EventLogFilter::default())
        .expect("history");
    assert_eq!(page.events, vec![published], "事件必须原样回读");
    assert_eq!(page.events[0].kind, EventKind::Blocked);
    assert!(!page.events[0].audited);
    assert!(!page.truncated);
}

#[test]
fn every_published_event_is_persisted_in_publish_order() {
    let (_dir, db) = store();
    let log = log();

    let mut ids = Vec::new();
    for i in 0..5 {
        let envelope = EventEnvelope::from_new(
            NewEvent::new(EventKind::Dispatch, "agent-runner").with_ticket(format!("t{i}")),
            format!("evt-1700000000000-{i:06}"),
            "2026-01-01T00:00:00Z",
        );
        log.append(&db, &envelope).expect("append");
        ids.push(envelope.id);
    }

    let page = log
        .history(&db, &EventLogFilter::default())
        .expect("history");
    let got: Vec<&str> = page.events.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(got, ids, "事件日志必须保持发布次序");
}

fn append(log: &EventLog, db: &Database, kind: EventKind, ticket: &str, at: &str) -> EventEnvelope {
    let envelope = EventEnvelope::from_new(
        NewEvent::new(kind, "agent-runner").with_ticket(ticket),
        format!("evt-{at}-{ticket}-{}", kind.as_str()),
        at,
    );
    log.append(db, &envelope).expect("append");
    envelope
}

#[test]
fn the_event_log_filters_by_ticket_kind_and_time() {
    let (_dir, db) = store();
    let log = log();

    append(&log, &db, EventKind::Dispatch, "15", "2026-01-01T00:00:00Z");
    append(&log, &db, EventKind::Blocked, "16", "2026-01-01T00:00:01Z");
    append(&log, &db, EventKind::Dispatch, "16", "2026-01-01T00:00:02Z");
    append(&log, &db, EventKind::NeedsAttention, "15", "2026-01-01T00:00:03Z");

    let by_ticket = log
        .history(
            &db,
            &EventLogFilter {
                ticket_id: Some("16".into()),
                ..Default::default()
            },
        )
        .expect("history");
    let tickets: Vec<&str> = by_ticket
        .events
        .iter()
        .map(|e| e.ticket_id.as_deref().unwrap_or_default())
        .collect();
    assert_eq!(tickets, vec!["16", "16"], "按票过滤");
    assert!(!by_ticket.truncated);

    let by_kind = log
        .history(
            &db,
            &EventLogFilter {
                kind: Some(EventKind::Blocked),
                ..Default::default()
            },
        )
        .expect("history");
    assert_eq!(by_kind.events.len(), 1);
    assert_eq!(by_kind.events[0].kind, EventKind::Blocked);
    assert_eq!(by_kind.events[0].ticket_id.as_deref(), Some("16"));

    let by_time = log
        .history(
            &db,
            &EventLogFilter {
                since: Some("2026-01-01T00:00:01Z".into()),
                ..Default::default()
            },
        )
        .expect("history");
    assert_eq!(
        by_time.events.len(),
        3,
        "since 是含端点的下界：00:00:01 起的三条"
    );

    let by_offset = log
        .history(
            &db,
            &EventLogFilter {
                since: Some("2026-01-01T08:00:02+08:00".into()),
                ..Default::default()
            },
        )
        .expect("history");
    assert_eq!(by_offset.events.len(), 2, "带偏移量的入参归一到同一时间轴");

    let combined = log
        .history(
            &db,
            &EventLogFilter {
                ticket_id: Some("16".into()),
                kind: Some(EventKind::Dispatch),
                ..Default::default()
            },
        )
        .expect("history");
    assert_eq!(combined.events.len(), 1, "票 + 类型可叠加");
}

#[test]
fn the_event_log_returns_the_most_recent_events_up_to_the_limit() {
    let (_dir, db) = store();
    let log = log();

    for i in 0..5 {
        append(
            &log,
            &db,
            EventKind::Dispatch,
            "15",
            &format!("2026-01-01T00:00:0{i}Z"),
        );
    }

    let page = log
        .history(
            &db,
            &EventLogFilter {
                limit: Some(2),
                ..Default::default()
            },
        )
        .expect("history");
    assert_eq!(page.events.len(), 2, "limit 取最近两条");
    assert_eq!(
        page.events.last().map(|e| e.occurred_at.as_str()),
        Some("2026-01-01T00:00:04Z"),
        "仍按发生次序升序"
    );
    assert!(page.truncated, "还有更旧的事件没回来 ⇒ truncated");
}

#[test]
fn the_event_log_window_is_bounded_and_drops_the_oldest_events() {
    let (_dir, db) = store();
    let log = EventLog::with_retention(3);

    for i in 0..6 {
        append(
            &log,
            &db,
            EventKind::Dispatch,
            "15",
            &format!("2026-01-01T00:00:0{i}Z"),
        );
    }

    assert_eq!(log.count(&db).expect("count"), 3, "表必须有界（ADR-0005 §10）");
    let page = log
        .history(&db, &EventLogFilter::default())
        .expect("history");
    let got: Vec<&str> = page.events.iter().map(|e| e.occurred_at.as_str()).collect();
    assert_eq!(got, vec!["2026-01-01T00:00:03Z", "2026-01-01T00:00:04Z", "2026-01-01T00:00:05Z"]);
    assert!(page.truncated, "保留窗口删过行 ⇒ truncated");
}

#[test]
fn an_event_kind_outside_the_closed_set_cannot_enter_the_event_log() {
    let (_dir, db) = store();
    let log = log();
    append(&log, &db, EventKind::Blocked, "15", "2026-01-01T00:00:00Z");

    // 绕开类型系统直塞一个自由字符串：存储层的 CHECK 约束（由 EventKind::ALL 生成）必须挡住。
    let err = db
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO events (id, kind, occurred_at, actor, payload, audited)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0)",
                rusqlite::params!["evt-free-string", "ticket_merged", "2026-01-01T00:00:01Z", "host", "{}"],
            )
        })
        .expect_err("自由字符串不得进事件日志");

    assert!(
        err.to_string().contains("CHECK"),
        "kind 列的闭集约束必须拒掉自由字符串：{err}"
    );
    assert_eq!(log.count(&db).expect("count"), 1, "一行都不许进");
}
