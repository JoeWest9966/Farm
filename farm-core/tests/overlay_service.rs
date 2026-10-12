//! 覆盖层服务（farm-core::overlay）：生效配置 = 基线 + 覆盖层，事件回流，重启提示。
//!
//! 接缝是 [`farm_core::overlay::OverlayService`]（挂在 [`farm_core::AppState`] 上）。
//! ADR-0002 §2：覆盖层是**临时运行时覆盖**——显式标记、重启后提示、不写回仓库文件；
//! ADR-0005 §6：落宿主私有 SQLite，per-仓库 keyed。
//!
//! 三条被钉住的行为：
//! 1. **生效**：`effective = baseline + overlay`（基线仍可读，覆盖只进宿主库）；
//! 2. **可见且带标记**：清单里每条都带 `temporary` 与标记文案；
//! 3. **可观测**：每次施加/清除都发布到事件总线，payload 带标记与**生效配置**
//!    （编排器折叠它即得当前生效视图，不必另开读路口）。

use farm_core::AppState;
use farm_core::HostConfig;
use farm_sdk::overlay::{
    OverlayClearRequest, OverlayKey, OverlaySetRequest, OverlayStatus, OVERLAY_TEMPORARY_MARKER,
};
use farm_sdk::workflow::{ProjectionSwitches, RetryBackoff, WorkflowConfig};
use farm_sdk::{ContractError, EventKind};

const VALID: &str = r#"{
  "dispatch_labels": ["agent-ready"],
  "max_concurrency": 2,
  "retry_backoff": { "max_retries": 2, "base_delay_ms": 100, "backoff_multiplier": 2 },
  "stall_threshold_secs": 300,
  "prompt_template": "do {{ticket_id}}",
  "projection": { "open_pr": true, "close_issue": true }
}"#;

fn state() -> AppState {
    AppState::new_in_memory(HostConfig {
        bind: "127.0.0.1:3721".parse().expect("bind"),
        op_token: "unit-op-token".into(),
        admin_token: None,
        db_path: std::env::temp_dir().join("farm-core-overlay-service.db"),
    })
    .expect("in-memory state")
}

/// 一个装着合法 workflow 配置的被编排仓库（基线来源）。
fn repo_with_baseline() -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("repo");
    std::fs::create_dir_all(repo.path().join(".farm")).expect("mkdir");
    std::fs::write(repo.path().join(".farm/workflow.json"), VALID).expect("write baseline");
    repo
}

fn load(state: &AppState, repo: &std::path::Path) {
    state
        .workflow
        .load_repo("repo-a", repo)
        .expect("baseline loads");
}

#[test]
fn an_overlay_takes_effect_on_top_of_the_workflow_baseline() {
    let state = state();
    let repo = repo_with_baseline();
    load(&state, repo.path());

    let status = state
        .overlay
        .set(
            &state,
            "repo-a",
            &OverlaySetRequest {
                key: OverlayKey::MaxConcurrency,
                value: serde_json::json!(1),
                note: Some("压并发做一次临时实验".into()),
            },
        )
        .expect("set overlay");

    assert_eq!(
        status.baseline.as_ref().expect("baseline").max_concurrency,
        2,
        "基线仍是仓库里的值"
    );
    assert_eq!(
        status.effective.as_ref().expect("effective").max_concurrency,
        1,
        "生效值 = 基线 + 覆盖层"
    );
    assert!(status.overlay_applied);
    assert_eq!(status.overlays.len(), 1);
}

#[test]
fn an_overlay_can_override_every_element_of_the_workflow_config() {
    let state = state();
    let repo = repo_with_baseline();
    load(&state, repo.path());

    let cases: Vec<(OverlayKey, serde_json::Value)> = vec![
        (OverlayKey::StallThresholdSecs, serde_json::json!(60)),
        (OverlayKey::DispatchLabels, serde_json::json!(["panel-only"])),
        (OverlayKey::PromptTemplate, serde_json::json!("temporary {{ticket_id}}")),
        (OverlayKey::RetryMaxRetries, serde_json::json!(0)),
        (OverlayKey::RetryBaseDelayMs, serde_json::json!(10)),
        (OverlayKey::RetryBackoffMultiplier, serde_json::json!(3)),
        (OverlayKey::ProjectionOpenPr, serde_json::json!(false)),
        (OverlayKey::ProjectionCloseIssue, serde_json::json!(false)),
    ];
    for (key, value) in cases {
        state
            .overlay
            .set(
                &state,
                "repo-a",
                &OverlaySetRequest {
                    key,
                    value: value.clone(),
                    note: None,
                },
            )
            .unwrap_or_else(|e| panic!("set {key}: {e}"));
    }

    let status = state.overlay.status(&state, "repo-a").expect("status");
    let effective = status.effective.expect("effective");
    assert_eq!(effective.stall_threshold_secs, 60);
    assert_eq!(effective.dispatch_labels, vec!["panel-only".to_string()]);
    assert_eq!(effective.prompt_template, "temporary {{ticket_id}}");
    assert_eq!(
        effective.retry_backoff,
        RetryBackoff {
            max_retries: 0,
            base_delay_ms: 10,
            backoff_multiplier: 3,
        }
    );
    assert_eq!(
        effective.projection,
        ProjectionSwitches {
            open_pr: false,
            close_issue: false,
            comment_merge_commit: false,
            add_label: false,
        }
    );
    assert_eq!(status.overlays.len(), 8, "八格全被临时覆盖");
}

#[test]
fn an_overlay_that_would_make_the_effective_config_invalid_writes_nothing() {
    // 跨字段约束（与 workflow 配置校验同判据）：启用重试时基础延迟必须 >= 1。
    let state = state();
    let repo = repo_with_baseline();
    load(&state, repo.path());

    state
        .overlay
        .set(
            &state,
            "repo-a",
            &OverlaySetRequest {
                key: OverlayKey::RetryMaxRetries,
                value: serde_json::json!(3),
                note: None,
            },
        )
        .expect("enable retries");

    let err = state
        .overlay
        .set(
            &state,
            "repo-a",
            &OverlaySetRequest {
                key: OverlayKey::RetryBaseDelayMs,
                value: serde_json::json!(0),
                note: None,
            },
        )
        .expect_err("合并后的生效配置非法必须被拒");
    assert_eq!(
        err.to_contract_error().code,
        farm_sdk::ErrorCode::InvalidRequest
    );

    let status = state.overlay.status(&state, "repo-a").expect("status");
    assert_eq!(status.overlays.len(), 1, "被拒的那格什么都没落");
    assert_eq!(
        status.effective.expect("effective").retry_backoff.base_delay_ms,
        100,
        "生效配置仍是上一次合法状态"
    );
}

#[test]
fn every_listed_overlay_carries_the_temporary_marker() {
    let state = state();
    let repo = repo_with_baseline();
    load(&state, repo.path());

    state
        .overlay
        .set(
            &state,
            "repo-a",
            &OverlaySetRequest {
                key: OverlayKey::MaxConcurrency,
                value: serde_json::json!(1),
                note: None,
            },
        )
        .expect("set");

    let status = state.overlay.status(&state, "repo-a").expect("status");
    assert_eq!(status.overlays.len(), 1);
    let entry = &status.overlays[0];
    assert!(entry.temporary, "ADR-0002 §2：临时性必须显式");
    assert!(
        status
            .restore_prompt
            .as_deref()
            .is_some_and(|prompt| prompt.contains("恢复基线")),
        "有覆盖就有恢复提示：{:?}",
        status.restore_prompt
    );
    // 标记文案是单一源（面板与宿主同一句）。
    assert_eq!(OVERLAY_TEMPORARY_MARKER.contains("临时覆盖"), true);
}

#[test]
fn a_wrong_typed_overlay_value_is_a_closed_set_invalid_request() {
    let state = state();
    let repo = repo_with_baseline();
    load(&state, repo.path());

    let err = state
        .overlay
        .set(
            &state,
            "repo-a",
            &OverlaySetRequest {
                key: OverlayKey::MaxConcurrency,
                value: serde_json::json!("2"),
                note: None,
            },
        )
        .expect_err("字符串不能当并发配额");
    let contract: ContractError = err.to_contract_error();
    assert_eq!(contract.code, farm_sdk::ErrorCode::InvalidRequest);
    assert!(
        contract.message.contains("整数"),
        "消息要说明原因：{}",
        contract.message
    );
    // 拒了就什么都没落：库还是空的。
    assert!(state
        .overlay
        .entries("repo-a")
        .expect("entries")
        .is_empty());
}

#[test]
fn clearing_one_key_restores_the_baseline_for_that_field_only() {
    let state = state();
    let repo = repo_with_baseline();
    load(&state, repo.path());
    for key in [OverlayKey::MaxConcurrency, OverlayKey::StallThresholdSecs] {
        state
            .overlay
            .set(
                &state,
                "repo-a",
                &OverlaySetRequest {
                    key,
                    value: serde_json::json!(1),
                    note: None,
                },
            )
            .expect("set");
    }

    let status = state
        .overlay
        .clear(
            &state,
            "repo-a",
            &OverlayClearRequest {
                key: Some(OverlayKey::MaxConcurrency),
            },
        )
        .expect("clear one");

    let effective = status.effective.expect("effective");
    assert_eq!(effective.max_concurrency, 2, "清掉的格子回基线");
    assert_eq!(effective.stall_threshold_secs, 1, "没清的格子仍被覆盖");
    assert_eq!(status.overlays.len(), 1);
}

#[test]
fn clearing_all_overlays_makes_the_effective_config_equal_the_baseline() {
    let state = state();
    let repo = repo_with_baseline();
    load(&state, repo.path());
    state
        .overlay
        .set(
            &state,
            "repo-a",
            &OverlaySetRequest {
                key: OverlayKey::MaxConcurrency,
                value: serde_json::json!(1),
                note: None,
            },
        )
        .expect("set");

    let status = state
        .overlay
        .clear(&state, "repo-a", &OverlayClearRequest { key: None })
        .expect("clear all");

    assert!(status.overlays.is_empty());
    assert!(!status.overlay_applied);
    assert_eq!(status.baseline, status.effective, "生效值回落到基线");
    assert!(status.restore_prompt.is_none(), "没有覆盖就无需提示");
}

#[test]
fn an_overlay_change_reaches_the_event_bus_with_the_marker_and_the_effective_config() {
    let state = state();
    let repo = repo_with_baseline();
    load(&state, repo.path());

    state
        .overlay
        .set(
            &state,
            "repo-a",
            &OverlaySetRequest {
                key: OverlayKey::MaxConcurrency,
                value: serde_json::json!(1),
                note: Some("压并发".into()),
            },
        )
        .expect("set");

    let history = state
        .event_history(&farm_core::EventLogFilter::default())
        .expect("history");
    let overlay_events: Vec<_> = history
        .events
        .iter()
        .filter(|event| event.kind == EventKind::NeedsAttention)
        .collect();
    assert_eq!(overlay_events.len(), 1, "施加覆盖层发一条");
    let payload = &overlay_events[0].payload;
    assert_eq!(payload["kind"], "overlay");
    assert_eq!(payload["action"], "applied");
    assert_eq!(payload["repo_id"], "repo-a");
    assert_eq!(payload["temporary"], true, "事件里也带临时标记");
    assert!(
        payload["marker"].as_str().is_some_and(|m| m.contains("临时")),
        "标记文案随事件走：{payload}"
    );
    // 编排器折叠这条事件就能更新自己的生效视图（生效配置随事件带回）。
    assert_eq!(
        payload["effective"]["max_concurrency"], 1,
        "生效配置随事件回流：{payload}"
    );

    // 清除也发一条（fold 的对面）。
    state
        .overlay
        .clear(&state, "repo-a", &OverlayClearRequest { key: None })
        .expect("clear");
    let history = state
        .event_history(&farm_core::EventLogFilter::default())
        .expect("history");
    let actions: Vec<String> = history
        .events
        .iter()
        .filter(|event| event.payload["kind"] == "overlay")
        .map(|event| event.payload["action"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(actions, vec!["applied".to_string(), "cleared".to_string()]);
}

#[test]
fn an_overlay_on_a_repo_without_a_baseline_is_listed_but_has_nothing_to_override() {
    let state = state();

    let status = state
        .overlay
        .set(
            &state,
            "ghost",
            &OverlaySetRequest {
                key: OverlayKey::MaxConcurrency,
                value: serde_json::json!(1),
                note: None,
            },
        )
        .expect("set on an unloaded repo is still recorded");

    assert!(status.baseline.is_none(), "从未装载也没有 LKG");
    assert!(status.effective.is_none(), "没有基线就没有生效值");
    assert!(!status.overlay_applied);
    assert_eq!(status.overlays.len(), 1, "覆盖层仍可见（等着基线落地）");
    assert!(status.restore_prompt.is_some(), "仍要提示恢复");
}

#[test]
fn the_effective_config_of_a_repo_without_overlays_is_just_the_baseline() {
    let state = state();
    let repo = repo_with_baseline();
    load(&state, repo.path());

    let status = state.overlay.status(&state, "repo-a").expect("status");
    assert!(!status.overlay_applied);
    assert!(status.overlays.is_empty());
    assert_eq!(status.baseline, status.effective);
    let expected: WorkflowConfig = serde_json::from_str(VALID).expect("fixture");
    assert_eq!(status.effective, Some(expected));
}
