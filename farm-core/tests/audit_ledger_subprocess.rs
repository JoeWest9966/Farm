//! `ledger record` 子进程接缝（ticket #20，ADR-0004 决策 2）。
//!
//! 全部经**真实子进程**打接缝：宿主把 NDJSON 写进 fake `ledger` 替身的 stdin，
//! 替身把收到的原文 + 调用 argv 记进日志，测试按**已知字面量**断言 wire 形状。
//!
//! 覆盖验收：① fake ledger 收到形状正确的 NDJSON（serde + deny_unknown_fields 对照
//! Kron schema）；② 前半——ledger 不可用时宿主不当异常炸掉，只是拿到失败结果（旁路）。

use std::io::{BufRead, BufReader};
use std::path::Path;

use farm_core::audit::{KronRecord, LedgerWriter};
use farm_sdk::events::{EventEnvelope, EventKind, NewEvent};
use serde_json::Value;

/// fake `ledger` 替身的绝对路径（cargo 为集成测试提供）。
const FAKE_LEDGER: &str = env!("CARGO_BIN_EXE_fake-ledger");

/// 指向 fake 替身的 writer：日志路径经 argv 传入（避免进程级 env 在并行测试间竞争）。
fn fake_ledger_writer(log_path: &Path) -> LedgerWriter {
    LedgerWriter::new(FAKE_LEDGER).with_args(vec![
        "record".to_string(),
        "--log".to_string(),
        log_path.display().to_string(),
    ])
}

/// 读替身日志：`spawn` 记录（argv）+ `record` 记录（收到的 NDJSON 原文）。
fn read_log(path: &Path) -> (Vec<Value>, Vec<String>) {
    let file = std::fs::File::open(path).expect("fake ledger must have logged");
    let mut spawns = Vec::new();
    let mut lines = Vec::new();
    for line in BufReader::new(file).lines() {
        let entry: Value = serde_json::from_str(&line.expect("utf8 log line")).expect("json entry");
        match entry["t"].as_str() {
            Some("spawn") => spawns.push(entry),
            Some("record") => lines.push(entry["line"].as_str().expect("line str").to_string()),
            _ => {}
        }
    }
    (spawns, lines)
}

fn dispatch_record(id: &str) -> KronRecord {
    KronRecord::from_event(&EventEnvelope::from_new(
        NewEvent::new(EventKind::Dispatch, "agent-runner")
            .with_ticket("20")
            .with_attempt("att-1"),
        id,
        "2026-01-01T00:00:00Z",
    ))
    .expect("dispatch maps to a Kron record")
}

#[test]
fn the_ledger_subprocess_receives_correctly_shaped_ndjson() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log_path = dir.path().join("fake-ledger.log");
    std::fs::write(&log_path, "").expect("empty log");

    let writer = fake_ledger_writer(&log_path);
    let records = [dispatch_record("evt-1"), dispatch_record("evt-2")];
    let result = writer.record(&records).expect("record must not error");
    assert!(result.ok, "fake ledger exits 0 → 账本认下：{result:?}");

    let (spawns, lines) = read_log(&log_path);

    // 调用形状：子命令 `record`（argv 可断言）。
    assert_eq!(spawns.len(), 1, "一次 record 调用一个子进程");
    let argv = spawns[0]["argv"].as_array().expect("argv array");
    assert!(argv.iter().any(|a| a == "record"), "子命令是 record：{argv:?}");

    // wire 形状：每条 NDJSON 原文都能按 Kron schema（deny_unknown_fields）解回。
    assert_eq!(lines.len(), 2, "两条记录都写进了 ledger stdin");
    for line in &lines {
        let record: KronRecord =
            serde_json::from_str(line).expect("host NDJSON must match the Kron schema");
        assert_eq!(record.actor.as_str(), "farm", "写入方恒为 farm（ADR-0004）");
        assert!(!record.idempotency_key.is_empty(), "幂等自然键必填");
        assert!(!record.subject_id.is_empty(), "subject 必填");
    }
    // 两条记录的幂等键不同（不同事件 id）。
    let a: KronRecord = serde_json::from_str(&lines[0]).expect("parse");
    let b: KronRecord = serde_json::from_str(&lines[1]).expect("parse");
    assert_ne!(a.idempotency_key, b.idempotency_key);
}

#[test]
fn a_missing_ledger_program_surfaces_as_a_failure_not_a_panic() {
    // 验收②前半：ledger 不可用（程序不存在）→ record 返回 Err（宿主自有错误形状），
    // 而不是 panic——旁路失败不炸主流程（audited 不置位、告警，见 AuditSink::post）。
    let writer = LedgerWriter::new("/nonexistent/farm-ledger-binary");
    let err = writer
        .record(&[dispatch_record("evt-x")])
        .expect_err("a missing ledger program must surface as an error");
    assert!(
        err.to_string().contains("ledger"),
        "错误应指明是 ledger 旁路：{err}"
    );
}
