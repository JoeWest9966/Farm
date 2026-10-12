//! 覆盖层的重启提示与「仓库零污染」（ticket #24）。
//!
//! ADR-0002 §2：覆盖层重启后**提示恢复基线**（纯内存覆盖被否的正是重启行为漂移
//! 不可解释）；「不写回仓库文件」是硬验收——被编排仓库的 diff 必须零污染。
//!
//! 本文件用**真库文件**模拟重启（关库 → 重开同路径），因为提示的数据源正是持久层。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use farm_core::AppState;
use farm_core::HostConfig;
use farm_sdk::overlay::{OverlayClearRequest, OverlayKey, OverlaySetRequest};
use farm_sdk::workflow::WorkflowConfigStatus;
use farm_sdk::{EventKind, HANDSHAKE_PATH};

const VALID: &str = r#"{
  "dispatch_labels": ["agent-ready"],
  "max_concurrency": 2,
  "retry_backoff": { "max_retries": 2, "base_delay_ms": 100, "backoff_multiplier": 2 },
  "stall_threshold_secs": 300,
  "prompt_template": "do {{ticket_id}}",
  "projection": { "open_pr": true, "close_issue": true }
}"#;

fn state_on(path: &Path) -> AppState {
    AppState::new(HostConfig {
        bind: "127.0.0.1:3721".parse().expect("bind"),
        op_token: "unit-op-token".into(),
        admin_token: None,
        db_path: path.to_path_buf(),
    })
    .expect("state on a file store")
}

fn repo_with_baseline() -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("repo");
    std::fs::create_dir_all(repo.path().join(".farm")).expect("mkdir");
    std::fs::write(repo.path().join(".farm/workflow.json"), VALID).expect("write baseline");
    repo
}

/// 目录快照：相对路径 → 字节（用于证明仓库一个字节都没被动）。
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let relative = path.strip_prefix(root).expect("relative").to_path_buf();
                out.insert(relative, std::fs::read(&path).expect("read file"));
            }
        }
    }
    out
}

fn set(state: &AppState, key: OverlayKey, value: serde_json::Value) {
    state
        .overlay
        .set(
            state,
            "repo-a",
            &OverlaySetRequest {
                key,
                value,
                note: Some("面板临时压并发".into()),
            },
        )
        .expect("set overlay");
}

#[test]
fn a_host_restart_prompts_to_restore_the_baseline_for_every_repo_with_overlays() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("farm.db");
    let repo = repo_with_baseline();

    // 第一次运行：装载基线 + 施加覆盖层。
    {
        let state = state_on(&db_path);
        state
            .workflow
            .load_repo("repo-a", repo.path())
            .expect("load baseline");
        set(&state, OverlayKey::MaxConcurrency, serde_json::json!(1));
    }

    // 重启：同一个库文件、全新的 AppState。
    let restarted = state_on(&db_path);
    let prompted = restarted.overlay.prompt_restore(&restarted).expect("prompt");
    assert_eq!(prompted, vec!["repo-a".to_string()], "被提示的仓库清单");

    let history = restarted
        .event_history(&farm_core::EventLogFilter::default())
        .expect("history");
    let prompts: Vec<_> = history
        .events
        .iter()
        .filter(|event| event.payload["kind"] == "overlay_restore_prompt")
        .collect();
    assert_eq!(prompts.len(), 1, "重启提示发一条");
    assert_eq!(prompts[0].kind, EventKind::NeedsAttention, "一等事件，面板必渲染");
    let payload = &prompts[0].payload;
    assert_eq!(payload["repo_id"], "repo-a");
    assert_eq!(payload["temporary"], true, "提示里也带临时标记");
    assert!(
        payload["prompt"].as_str().is_some_and(|p| p.contains("恢复基线")),
        "提示要问「是否恢复基线」：{payload}"
    );
    assert_eq!(payload["overlays"][0], "max_concurrency");

    // 覆盖层本身经重启仍在，且仍生效（提示归提示，不静默改回去）。
    let status: WorkflowConfigStatus = restarted.workflow.status("repo-a").expect("status");
    assert_eq!(status.effective.expect("effective").max_concurrency, 2, "基线经重启仍在");
    let overlay = restarted.overlay.status(&restarted, "repo-a").expect("overlay status");
    assert_eq!(
        overlay.effective.expect("effective").max_concurrency,
        1,
        "覆盖层经重启仍生效"
    );
}

#[test]
fn a_host_restart_without_overlays_prompts_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("farm.db");
    let repo = repo_with_baseline();
    {
        let state = state_on(&db_path);
        state
            .workflow
            .load_repo("repo-a", repo.path())
            .expect("load baseline");
    }
    let restarted = state_on(&db_path);
    assert!(
        restarted.overlay.prompt_restore(&restarted).expect("prompt").is_empty(),
        "没有覆盖层就不该打扰人类"
    );
    let history = restarted
        .event_history(&farm_core::EventLogFilter::default())
        .expect("history");
    assert!(
        !history
            .events
            .iter()
            .any(|event| event.payload["kind"] == "overlay_restore_prompt"),
        "没有覆盖层就没有恢复提示"
    );
}

#[test]
fn an_overlay_never_writes_back_to_the_orchestrated_repo() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("farm.db");
    let repo = repo_with_baseline();
    let before = snapshot(repo.path());

    let state = state_on(&db_path);
    state
        .workflow
        .load_repo("repo-a", repo.path())
        .expect("load baseline");
    set(&state, OverlayKey::MaxConcurrency, serde_json::json!(1));
    set(&state, OverlayKey::DispatchLabels, serde_json::json!(["panel-only"]));
    state
        .overlay
        .clear(
            &state,
            "repo-a",
            &OverlayClearRequest {
                key: Some(OverlayKey::MaxConcurrency),
            },
        )
        .expect("clear one");
    state
        .overlay
        .clear(&state, "repo-a", &OverlayClearRequest { key: None })
        .expect("clear all");

    // 重启一轮再提示一次：全程都不该碰仓库。
    drop(state);
    let restarted = state_on(&db_path);
    restarted.overlay.prompt_restore(&restarted).expect("prompt");

    assert_eq!(
        snapshot(repo.path()),
        before,
        "被编排仓库的 diff 必须零污染（ADR-0005 §6：覆盖层落宿主库，不落仓库）"
    );
    // 仓库里仍然只有那一个配置文件——没有 .farm/overlay.json 之类的副产品。
    let files: Vec<PathBuf> = snapshot(repo.path()).into_keys().collect();
    assert_eq!(files, vec![PathBuf::from(".farm/workflow.json")]);
}

#[test]
fn the_overlay_handle_is_reachable_from_the_shared_state_the_host_assembles() {
    // 加法接缝：宿主装配出的 AppState 一定带 overlay 句柄（面板经宿主管理 API 打到它）。
    let state = state_on(&tempfile::tempdir().expect("dir").path().join("farm.db"));
    let status = state.overlay.status(&state, "repo-a").expect("status");
    assert_eq!(status.repo_id, "repo-a");
    assert!(status.overlays.is_empty());
    // 契约面握手路径常量仍在（本票没动契约路由表）。
    assert_eq!(HANDSHAKE_PATH, "/handshake");
}
