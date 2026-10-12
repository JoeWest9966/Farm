//! 覆盖层（Overlay）与控制面（tick）的契约形状（ticket #24）。
//!
//! 夹具 JSON 全部手写（独立于实现的真相源）：类型一旦与这些线格式漂移，测试就红。
//! 最重要的一条：**临时标记不允许缺席**——`OverlayEntry.temporary` 是必填字段，
//! 少它就是契约形状不成立（ADR-0002 §2「显式标记这是临时覆盖」）。

use farm_sdk::control::{TickControlStatus, TickState};
use farm_sdk::overlay::{
    validate_value, OverlayClearRequest, OverlayEntry, OverlayKey, OverlaySetRequest,
    OverlayStatus, OverlayValueKind, OVERLAY_TEMPORARY_MARKER,
};

#[test]
fn the_overlay_key_set_covers_the_workflow_config_elements() {
    // GLOSSARY「Workflow 配置」六要素：dispatchable 标签、并发配额、重试退避、
    // stall 阈值、派发 prompt 模板、投影开关——每一条可覆盖字段都在闭集里。
    let keys = OverlayKey::ALL.iter().map(|k| k.as_str()).collect::<Vec<_>>();
    for want in [
        "max_concurrency",
        "stall_threshold_secs",
        "dispatch_labels",
        "prompt_template",
        "retry_backoff.max_retries",
        "retry_backoff.base_delay_ms",
        "retry_backoff.backoff_multiplier",
        "projection.close_issue",
        "projection.open_pr",
        "projection.comment_merge_commit",
        "projection.add_label",
    ] {
        assert!(keys.contains(&want), "覆盖层键闭集缺 {want}：{keys:?}");
    }
    assert_eq!(keys.len(), 11, "闭集大小固定，加键 = 加法契约变更");
}

#[test]
fn an_overlay_key_parses_from_its_wire_string_and_rejects_unknown_keys() {
    assert_eq!(OverlayKey::parse("max_concurrency"), Some(OverlayKey::MaxConcurrency));
    assert_eq!(
        OverlayKey::parse("projection.open_pr"),
        Some(OverlayKey::ProjectionOpenPr)
    );
    assert_eq!(OverlayKey::parse("max_concurrencyy"), None, "未知键不猜");
    assert_eq!(OverlayKey::parse(""), None);
}

#[test]
fn every_overlay_key_declares_the_value_kind_it_accepts() {
    assert_eq!(OverlayKey::MaxConcurrency.value_kind(), OverlayValueKind::Count);
    assert_eq!(
        OverlayKey::StallThresholdSecs.value_kind(),
        OverlayValueKind::Seconds
    );
    assert_eq!(
        OverlayKey::DispatchLabels.value_kind(),
        OverlayValueKind::Labels
    );
    assert_eq!(OverlayKey::PromptTemplate.value_kind(), OverlayValueKind::Text);
    assert_eq!(
        OverlayKey::ProjectionOpenPr.value_kind(),
        OverlayValueKind::Flag
    );
}

#[test]
fn a_value_is_validated_against_the_kind_its_key_declares() {
    // 合法：类型对、范围对。
    assert!(validate_value(OverlayKey::MaxConcurrency, &serde_json::json!(1)).is_ok());
    assert!(validate_value(OverlayKey::MaxConcurrency, &serde_json::json!(0)).is_err(),
        "并发配额至少 1（否则没有票可派发）");
    assert!(validate_value(OverlayKey::StallThresholdSecs, &serde_json::json!(300)).is_ok());
    assert!(validate_value(OverlayKey::StallThresholdSecs, &serde_json::json!(0)).is_err());
    // 重试次数与基础延迟可以是 0（0 = 不重试，RetryBackoff::default() 即此形状）。
    assert!(validate_value(OverlayKey::RetryMaxRetries, &serde_json::json!(0)).is_ok());
    assert!(validate_value(OverlayKey::RetryBaseDelayMs, &serde_json::json!(0)).is_ok());
    // 退避倍率至少 1（0 倍永不退避，配置校验同判据）。
    assert!(validate_value(OverlayKey::RetryBackoffMultiplier, &serde_json::json!(1)).is_ok());
    assert!(validate_value(OverlayKey::RetryBackoffMultiplier, &serde_json::json!(0)).is_err());
    assert!(validate_value(
        OverlayKey::DispatchLabels,
        &serde_json::json!(["agent-ready", "panel"])
    )
    .is_ok());
    assert!(validate_value(OverlayKey::DispatchLabels, &serde_json::json!([])).is_err(),
        "标签至少一个");
    assert!(validate_value(OverlayKey::PromptTemplate, &serde_json::json!("do {{ticket_id}}")).is_ok());
    assert!(validate_value(OverlayKey::PromptTemplate, &serde_json::json!("   ")).is_err(),
        "prompt 模板不能是空白");
    assert!(validate_value(OverlayKey::ProjectionOpenPr, &serde_json::json!(true)).is_ok());
    // 类型不符：把字符串塞进计数键必须被拒（不能静默强转）。
    assert!(validate_value(OverlayKey::MaxConcurrency, &serde_json::json!("2")).is_err());
    assert!(validate_value(OverlayKey::ProjectionOpenPr, &serde_json::json!(1)).is_err());
}

#[test]
fn an_overlay_entry_is_explicitly_marked_temporary() {
    let json = r#"{
      "repo_id": "repo-a",
      "key": "max_concurrency",
      "value": 1,
      "note": "压并发做一次临时实验",
      "temporary": true,
      "updated_at": "2026-10-20T00:00:00Z"
    }"#;
    let entry: OverlayEntry = serde_json::from_str(json).expect("fixture must parse");
    assert!(entry.temporary, "临时标记必须显式为真");
    assert_eq!(entry.key, OverlayKey::MaxConcurrency);
    assert_eq!(entry.value, serde_json::json!(1));
    assert_eq!(entry.note.as_deref(), Some("压并发做一次临时实验"));
    // 标记文案是单一源：面板与宿主渲染同一句话。
    assert!(OVERLAY_TEMPORARY_MARKER.contains("临时"), "{OVERLAY_TEMPORARY_MARKER}");
    assert!(OVERLAY_TEMPORARY_MARKER.contains("不写回仓库文件"), "{OVERLAY_TEMPORARY_MARKER}");
}

#[test]
fn an_overlay_entry_without_the_temporary_marker_is_not_a_contract_shape() {
    let json = r#"{"repo_id":"repo-a","key":"max_concurrency","value":1,"updated_at":"t"}"#;
    assert!(
        serde_json::from_str::<OverlayEntry>(json).is_err(),
        "temporary 是必填标记：缺席即形状不成立"
    );
}

#[test]
fn an_unknown_overlay_key_is_rejected_rather_than_guessed() {
    let json = r#"{"key":"orchestration.tick","value":"paused"}"#;
    assert!(
        serde_json::from_str::<OverlaySetRequest>(json).is_err(),
        "未知键不能靠「猜一个字段」落库"
    );
}

#[test]
fn a_set_request_carries_the_key_the_value_and_an_optional_note() {
    let json = r#"{"key":"dispatch_labels","value":["agent-ready"]}"#;
    let request: OverlaySetRequest = serde_json::from_str(json).expect("fixture");
    assert_eq!(request.key, OverlayKey::DispatchLabels);
    assert_eq!(request.note, None, "note 可省略");
}

#[test]
fn a_clear_request_without_a_key_means_clear_every_overlay_of_the_repo() {
    let json = r#"{}"#;
    let request: OverlayClearRequest = serde_json::from_str(json).expect("fixture");
    assert_eq!(request.key, None);

    let json = r#"{"key":"max_concurrency"}"#;
    let request: OverlayClearRequest = serde_json::from_str(json).expect("fixture");
    assert_eq!(request.key, Some(OverlayKey::MaxConcurrency));
}

#[test]
fn the_overlay_status_shows_baseline_effective_and_the_restore_prompt() {
    let json = r#"{
      "repo_id": "repo-a",
      "baseline": {
        "dispatch_labels": ["agent-ready"],
        "max_concurrency": 2,
        "stall_threshold_secs": 300,
        "prompt_template": "do {{ticket_id}}"
      },
      "effective": {
        "dispatch_labels": ["agent-ready"],
        "max_concurrency": 1,
        "stall_threshold_secs": 300,
        "prompt_template": "do {{ticket_id}}"
      },
      "overlay_applied": true,
      "overlays": [
        {
          "repo_id": "repo-a",
          "key": "max_concurrency",
          "value": 1,
          "note": "压并发",
          "temporary": true,
          "updated_at": "2026-10-20T00:00:00Z"
        }
      ],
      "restore_prompt": "repo-a 有 1 条临时覆盖，是否恢复基线？"
    }"#;
    let status: OverlayStatus = serde_json::from_str(json).expect("fixture");
    assert!(status.overlay_applied);
    assert_eq!(status.overlays.len(), 1);
    assert!(status.overlays[0].temporary);
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
    assert!(status.restore_prompt.is_some(), "有覆盖就有恢复提示");
}

#[test]
fn an_overlay_status_without_overlays_reports_the_baseline_as_effective() {
    let json = r#"{
      "repo_id": "repo-a",
      "baseline": {
        "dispatch_labels": ["agent-ready"],
        "max_concurrency": 2,
        "stall_threshold_secs": 300,
        "prompt_template": "do {{ticket_id}}"
      },
      "effective": {
        "dispatch_labels": ["agent-ready"],
        "max_concurrency": 2,
        "stall_threshold_secs": 300,
        "prompt_template": "do {{ticket_id}}"
      },
      "overlay_applied": false,
      "overlays": [],
      "restore_prompt": null
    }"#;
    let status: OverlayStatus = serde_json::from_str(json).expect("fixture");
    assert!(!status.overlay_applied);
    assert!(status.overlays.is_empty());
    assert_eq!(status.baseline, status.effective, "无覆盖时生效值就是基线");
    assert!(status.restore_prompt.is_none(), "无覆盖就无需提示");
}

#[test]
fn the_tick_state_closed_set_is_two_valued() {
    let states = TickState::ALL.iter().map(|s| s.as_str()).collect::<Vec<_>>();
    assert_eq!(states, vec!["running", "paused"]);
    assert!(!TickState::Running.is_paused());
    assert!(TickState::Paused.is_paused());
    assert_eq!(TickState::parse("paused"), Some(TickState::Paused));
    assert_eq!(TickState::parse("halted"), None, "未知态不猜");
}

#[test]
fn the_tick_control_status_carries_the_pause_reason_for_the_orchestrator() {
    let json = r#"{
      "state": "paused",
      "reason": "human paused the tick from the panel",
      "actor": "panel",
      "updated_at": "2026-10-20T00:00:00Z"
    }"#;
    let status: TickControlStatus = serde_json::from_str(json).expect("fixture");
    assert_eq!(status.state, TickState::Paused);
    assert!(status.state.is_paused(), "编排器循环据此跳过派发");
    assert_eq!(status.reason.as_deref(), Some("human paused the tick from the panel"));

    let json = r#"{"state":"running","reason":null,"actor":null,"updated_at":null}"#;
    let status: TickControlStatus = serde_json::from_str(json).expect("fixture");
    assert!(!status.state.is_paused());
}
