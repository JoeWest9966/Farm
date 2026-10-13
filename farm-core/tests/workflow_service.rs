//! Workflow 服务句柄（farm-core::workflow::service）：装载 → LKG / 回退 / 拒绝 + 告警。
//!
//! GLOSSARY：非法配置不致崩溃——回退 last-known-good 并发告警；首次即非法 → 拒绝编排
//! （needs_attention）。告警走**持久化发布路径**（[`farm_core::AppState::publish`]，
//! code review P2-4）：先落事件日志再广播，所以活着的订阅者与晚到的快照查询看到的是
//! 同一条事实。LKG 经重启仍生效（SQLite 持久）。

use farm_core::workflow::service::LoadOutcome;
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

fn host_state(db_path: std::path::PathBuf) -> AppState {
    AppState::new_in_memory(HostConfig {
        bind: "127.0.0.1:3721".parse().expect("bind"),
        op_token: "workflow-test-token".into(),
        admin_token: None,
        db_path,
    })
    .expect("in-memory state")
}

/// 内存库宿主（`workflow` 句柄 + `events` 总线都在上面）。
fn in_memory_service() -> (tempfile::TempDir, AppState) {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = host_state(std::env::temp_dir().join("farm-core-workflow-service-test.db"));
    (dir, state)
}

#[test]
fn a_valid_repo_config_becomes_the_effective_config_and_a_new_lkg() {
    let (dir, state) = in_memory_service();
    write_config(dir.path(), VALID);

    let outcome = state.workflow.load_repo(&state, "repo-a", dir.path()).expect("load");
    let config = match outcome {
        LoadOutcome::Applied(c) => c,
        other => panic!("expected Applied, got {other:?}"),
    };
    assert_eq!(config.max_concurrency, 2);

    let status = state.workflow.status("repo-a").expect("status");
    assert_eq!(status.effective, Some(config));
    assert_eq!(status.source, Some(EffectiveSource::Fresh));
    assert!(status.last_known_good.is_some(), "合法配置成为新 LKG 并落库");
    assert!(status.last_alert.is_none(), "合法配置无告警");
}

#[test]
fn an_invalid_repo_config_falls_back_to_the_lkg_and_alerts_on_the_bus() {
    let (dir, state) = in_memory_service();
    // 先载入一份合法配置，建立 LKG。
    write_config(dir.path(), VALID);
    assert!(matches!(
        state.workflow.load_repo(&state, "repo-a", dir.path()).expect("load valid"),
        LoadOutcome::Applied(_)
    ));

    // 订阅总线，然后把仓库配置改坏，重新装载。
    let mut rx = state.events.subscribe();
    write_config(dir.path(), INVALID);
    let outcome = state.workflow.load_repo(&state, "repo-a", dir.path()).expect("load invalid");

    let (effective, reason) = match outcome {
        LoadOutcome::FellBack { effective, reason } => (effective, reason),
        other => panic!("expected FellBack, got {other:?}"),
    };
    assert_eq!(effective.max_concurrency, 2, "生效值回退到 last-known-good");

    let status = state.workflow.status("repo-a").expect("status");
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
    let (dir, state) = in_memory_service();
    write_config(dir.path(), INVALID);
    let mut rx = state.events.subscribe();

    let outcome = state.workflow.load_repo(&state, "repo-a", dir.path()).expect("load invalid");
    let reason = match outcome {
        LoadOutcome::Refused { reason } => reason,
        other => panic!("expected Refused, got {other:?}"),
    };

    let status = state.workflow.status("repo-a").expect("status");
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
    let (dir, state) = in_memory_service();
    write_config(dir.path(), "{ this is : not json ");
    let outcome = state.workflow.load_repo(&state, "repo-a", dir.path()).expect("load must not panic");
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
        // 文件库（不是内存库）才谈得上「重启后从库里引导」。
        let state = AppState::new(HostConfig {
            bind: "127.0.0.1:3721".parse().expect("bind"),
            op_token: "workflow-test-token".into(),
            admin_token: None,
            db_path: path.clone(),
        })
        .expect("on-disk state");
        match state.workflow.load_repo(&state, "repo-a", repo.path()).expect("load") {
            LoadOutcome::Applied(c) => c,
            other => panic!("expected Applied, got {other:?}"),
        }
    }; // state drop = “重启”

    let restarted = AppState::new(HostConfig {
        bind: "127.0.0.1:3721".parse().expect("bind"),
        op_token: "workflow-test-token".into(),
        admin_token: None,
        db_path: path,
    })
    .expect("reopened state");

    let status = restarted.workflow.status("repo-a").expect("status after restart");
    assert_eq!(status.effective, Some(stored_config), "重启后 LKG 仍生效");
    assert_eq!(status.source, Some(EffectiveSource::LastKnownGood));
    assert!(status.last_known_good.is_some(), "LKG 落库信息仍在");
}

#[test]
fn a_repo_that_was_never_loaded_reports_no_effective_config() {
    let (_dir, state) = in_memory_service();
    let status = state.workflow.status("never-seen").expect("status");
    assert!(status.effective.is_none());
    assert!(status.last_known_good.is_none());
    assert!(status.last_alert.is_none());
}

#[test]
fn needs_attention_alerts_carry_the_workflow_actor() {
    let (dir, state) = in_memory_service();
    write_config(dir.path(), INVALID);
    let mut rx = state.events.subscribe();
    state.workflow.load_repo(&state, "repo-a", dir.path()).expect("load");
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
        .load_repo(&state, "repo-a", repo.path())
        .expect("load via AppState handle");
    assert!(matches!(outcome, LoadOutcome::Applied(_)));

    let status = state.workflow.status("repo-a").expect("status via AppState handle");
    assert_eq!(status.source, Some(EffectiveSource::Fresh));
    assert!(status.effective.is_some());
}

#[test]
fn a_workflow_config_alert_lands_in_the_event_log_not_only_on_the_bus() {
    // code review P2-4：告警原先只发内存总线——事件历史 / WS 快照 / 通知通道都看不见
    // 它，而 `needs_attention` 是一等事件（GLOSSARY：面板必渲染）。现在必须走持久化
    // 发布路径：先落事件日志，再广播。
    let (dir, state) = in_memory_service();
    write_config(dir.path(), INVALID);
    let mut rx = state.events.subscribe();

    assert!(matches!(
        state.workflow.load_repo(&state, "repo-a", dir.path()).expect("load"),
        LoadOutcome::Refused { .. }
    ));

    // 活面：订阅者当场收到。
    let on_bus = rx.try_recv().expect("alert on the bus");
    assert_eq!(on_bus.kind, EventKind::NeedsAttention);

    // 持久面：事件历史里查得到同一条（晚到的订阅者 / 面板快照因此不漏渲染）。
    let history = state
        .event_history(&farm_core::EventLogFilter {
            since: None,
            kind: Some(EventKind::NeedsAttention),
            ticket_id: None,
            limit: None,
        })
        .expect("history");
    let persisted = history
        .events
        .iter()
        .find(|event| event.payload["config_state"] == "refused")
        .unwrap_or_else(|| panic!("告警必须落进事件日志：{:?}", history.events));
    assert_eq!(persisted.actor, "workflow");
    assert_eq!(persisted.payload["repo_id"], "repo-a");
    assert_eq!(persisted.id, on_bus.id, "同一个事件 id：先落库再广播");
}
