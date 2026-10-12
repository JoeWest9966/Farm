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
