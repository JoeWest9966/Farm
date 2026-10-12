//! Workflow 服务句柄（farm-core::workflow::service）：装载 → LKG / 回退 / 拒绝 + 告警。
//!
//! GLOSSARY：非法配置不致崩溃——回退 last-known-good 并发告警；首次即非法 → 拒绝编排
//! （needs_attention）。告警走事件总线（[`farm_core::events::EventBus`]，`NeedsAttention`
//! 是一等事件）。LKG 经重启仍生效（SQLite 持久）。

use farm_core::db::Database;
use farm_core::events::EventBus;
use farm_core::migrations::{Migrator, MIGRATIONS};
use farm_core::workflow::service::{LoadOutcome, WorkflowService};
use farm_core::{AppState, HostConfig};
use farm_sdk::events::EventKind;
use farm_sdk::workflow::EffectiveSource;
use std::fs;
use std::path::Path;

const VALID: &str = r#"{
  "dispatch_labels": ["agent-ready"],
  "max_concurrency": 2,
  "retry_backoff": { "max_retries": 3, "base_delay_ms": 500, "backoff_multiplier": 2 },
  "stall_threshold_secs": 300,
  "prompt_template": "do {{ticket_id}}",
  "projection": { "open_pr": true, "close_issue": true }
}"#;
const INVALID: &str = r#"{"dispatch_labels": [], "max_concurrency": 0}"#;

fn write_config(repo: &Path, text: &str) {
    fs::create_dir_all(repo.join(".farm")).unwrap();
    fs::write(repo.join(".farm/workflow.json"), text).unwrap();
}

fn in_memory_service() -> (tempfile::TempDir, WorkflowService, EventBus) {
    let db = Database::open_in_memory().expect("in-memory db");
    db.migrate(&Migrator::new(MIGRATIONS)).expect("migrate");
    let events = EventBus::new();
    let service = WorkflowService::new(db, events.clone());
    let dir = tempfile::tempdir().expect("tempdir");
    (dir, service, events)
}

#[test]
fn a_valid_repo_config_becomes_the_effective_config_and_a_new_lkg() {
    let (dir, service, _events) = in_memory_service();
    write_config(dir.path(), VALID);

    let outcome = service.load_repo("repo-a", dir.path()).expect("load");
    let config = match outcome {
        LoadOutcome::Applied(c) => c,
        other => panic!("expected Applied, got {other:?}"),
    };
    assert_eq!(config.max_concurrency, 2);

    let status = service.status("repo-a").expect("status");
    assert_eq!(status.effective, Some(config));
    assert_eq!(status.source, Some(EffectiveSource::Fresh));
    assert!(status.last_known_good.is_some(), "合法配置成为新 LKG 并落库");
    assert!(status.last_alert.is_none(), "合法配置无告警");
}

#[test]
fn an_invalid_repo_config_falls_back_to_the_lkg_and_alerts_on_the_bus() {
    let (dir, service, events) = in_memory_service();
    // 先载入一份合法配置，建立 LKG。
    write_config(dir.path(), VALID);
    assert!(matches!(
        service.load_repo("repo-a", dir.path()).expect("load valid"),
        LoadOutcome::Applied(_)
    ));

    // 订阅总线，然后把仓库配置改坏，重新装载。
    let mut rx = events.subscribe();
    write_config(dir.path(), INVALID);
    let outcome = service.load_repo("repo-a", dir.path()).expect("load invalid");

    let (effective, reason) = match outcome {
        LoadOutcome::FellBack { effective, reason } => (effective, reason),
        other => panic!("expected FellBack, got {other:?}"),
    };
    assert_eq!(effective.max_concurrency, 2, "生效值回退到 last-known-good");

    let status = service.status("repo-a").expect("status");
    assert_eq!(status.source, Some(EffectiveSource::LastKnownGood));
    assert_eq!(status.effective, Some(effective));
    assert!(status.last_alert.is_some(), "回退要带告警原因");
    assert!(status.last_known_good.is_some());

    // 事件总线上必须有一条 needs_attention 告警，payload 标出 fallback + repo + 原因。
    let env = rx.try_recv().expect("alert event must be on the bus");
    assert_eq!(env.kind, EventKind::NeedsAttention);
    assert_eq!(env.payload["config_state"], "fallback");
    assert_eq!(env.payload["repo_id"], "repo-a");
    assert_eq!(env.payload["reason"], reason, "告警 payload 带回退原因");
}

#[test]
fn a_first_ever_invalid_config_is_refused_and_raises_needs_attention() {
    let (dir, service, events) = in_memory_service();
    write_config(dir.path(), INVALID);
    let mut rx = events.subscribe();

    let outcome = service.load_repo("repo-a", dir.path()).expect("load invalid");
    let reason = match outcome {
        LoadOutcome::Refused { reason } => reason,
        other => panic!("expected Refused, got {other:?}"),
    };

    let status = service.status("repo-a").expect("status");
    assert!(status.effective.is_none(), "首次即非法：无生效配置（拒绝编排）");
    assert!(status.source.is_none());
    assert!(status.last_known_good.is_none(), "从未有过合法配置");
    assert_eq!(status.last_alert.as_deref(), Some(reason.as_str()));

    let env = rx.try_recv().expect("needs_attention must be raised");
    assert_eq!(env.kind, EventKind::NeedsAttention);
    assert_eq!(env.payload["config_state"], "refused");
    assert_eq!(env.payload["repo_id"], "repo-a");
}

#[test]
fn an_invalid_config_never_panics_the_host() {
    // 非法输入（含结构怪异）只回退/拒绝，不 panic。
    let (dir, service, _events) = in_memory_service();
    write_config(dir.path(), "{ this is : not json ");
    let outcome = service.load_repo("repo-a", dir.path()).expect("load must not panic");
    assert!(matches!(outcome, LoadOutcome::Refused { .. }));
}

#[test]
fn the_lkg_survives_a_restart_on_the_same_store() {
    // ADR-0005 §4：LKG 落盘；用同库文件重建 service（模拟重启），status 仍反映 LKG。
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = tempfile::tempdir().expect("repo");
    let path = dir.path().join("farm.db");
    write_config(repo.path(), VALID);

    let stored_config = {
        let db = Database::open(&path).expect("open");
        db.migrate(&Migrator::new(MIGRATIONS)).expect("migrate");
        let service = WorkflowService::new(db, EventBus::new());
        match service.load_repo("repo-a", repo.path()).expect("load") {
            LoadOutcome::Applied(c) => c,
            other => panic!("expected Applied, got {other:?}"),
        }
    }; // service + db drop = “重启”

    let db = Database::open(&path).expect("reopen");
    db.migrate(&Migrator::new(MIGRATIONS)).expect("migrate");
    let restarted = WorkflowService::new(db, EventBus::new());

    let status = restarted.status("repo-a").expect("status after restart");
    assert_eq!(status.effective, Some(stored_config), "重启后 LKG 仍生效");
    assert_eq!(status.source, Some(EffectiveSource::LastKnownGood));
    assert!(status.last_known_good.is_some(), "LKG 落库信息仍在");
}

#[test]
fn a_repo_that_was_never_loaded_reports_no_effective_config() {
    let (_dir, service, _events) = in_memory_service();
    let status = service.status("never-seen").expect("status");
    assert!(status.effective.is_none());
    assert!(status.last_known_good.is_none());
    assert!(status.last_alert.is_none());
}

#[test]
fn needs_attention_alerts_carry_the_workflow_actor() {
    let (dir, service, events) = in_memory_service();
    write_config(dir.path(), INVALID);
    let mut rx = events.subscribe();
    service.load_repo("repo-a", dir.path()).expect("load");
    let env = rx.try_recv().expect("alert on bus");
    assert_eq!(env.kind, EventKind::NeedsAttention);
    assert_eq!(env.actor, "workflow", "告警 actor 稳定，供面板/通知过滤");
}

#[test]
fn app_state_exposes_a_wired_workflow_handle() {
    // ticket #12 共享文件要求：AppState 带 workflow 句柄，new / new_in_memory 都构造。
    let state = AppState::new_in_memory(HostConfig {
        bind: "127.0.0.1:3721".parse().expect("bind"),
        op_token: "t".into(),
        admin_token: None,
        db_path: std::env::temp_dir().join("farm-appstate-workflow-test.db"),
    })
    .expect("in-memory state");

    let repo = tempfile::tempdir().expect("repo");
    write_config(repo.path(), VALID);
    let outcome = state
        .workflow
        .load_repo("repo-a", repo.path())
        .expect("load via AppState handle");
    assert!(matches!(outcome, LoadOutcome::Applied(_)));

    let status = state.workflow.status("repo-a").expect("status via AppState handle");
    assert_eq!(status.source, Some(EffectiveSource::Fresh));
    assert!(status.effective.is_some());
}
