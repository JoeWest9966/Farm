//! Workflow 配置读取与校验（farm-core::workflow::config）：宿主私有逻辑，不进契约。
//!
//! GLOSSARY：非法配置不致崩溃——校验失败由上层回退 last-known-good。这里只负责
//! 「合法 → WorkflowConfig；非法 → ConfigError」的判定，落在公共接缝上断言。

use farm_core::workflow::config::{parse_and_validate, validate, ConfigError};
use farm_sdk::workflow::{ProjectionSwitches, RetryBackoff, WorkflowConfig};

/// 一份合法配置的 JSON 文本（测试夹具；字段值独立于实现）。
fn valid_json() -> String {
    r#"{
      "dispatch_labels": ["agent-ready"],
      "max_concurrency": 2,
      "retry_backoff": { "max_retries": 3, "base_delay_ms": 500, "backoff_multiplier": 2 },
      "stall_threshold_secs": 300,
      "prompt_template": "do {{ticket_id}}",
      "projection": { "open_pr": true, "close_issue": true }
    }"#
    .to_string()
}

#[test]
fn a_valid_config_text_parses_and_validates() {
    let config = parse_and_validate(&valid_json()).expect("valid config must parse");
    assert_eq!(config.dispatch_labels, vec!["agent-ready"]);
    assert_eq!(config.max_concurrency, 2);
    assert_eq!(config.stall_threshold_secs, 300);
    assert!(config.projection.open_pr && config.projection.close_issue);
}

#[test]
fn malformed_json_is_a_parse_error_not_a_panic() {
    let err = parse_and_validate("{ not json ").expect_err("malformed json must be rejected");
    assert!(matches!(err, ConfigError::Parse(_)), "got {err:?}");
}

#[test]
fn a_missing_required_field_is_a_parse_error() {
    // 缺 max_concurrency（必需字段）。
    let text = r#"{
      "dispatch_labels": ["x"],
      "retry_backoff": { "max_retries": 0, "base_delay_ms": 1, "backoff_multiplier": 1 },
      "stall_threshold_secs": 1,
      "prompt_template": "p",
      "projection": { "open_pr": false, "close_issue": false }
    }"#;
    let err = parse_and_validate(text).expect_err("missing field must be rejected");
    assert!(matches!(err, ConfigError::Parse(_)), "got {err:?}");
}

#[test]
fn unknown_fields_are_ignored_for_forward_compatibility() {
    // 在合法配置里注入一个未知字段：解析应从宽（前向兼容），不因此判非法。
    let mut value: serde_json::Value = serde_json::from_str(&valid_json()).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("future_field".into(), serde_json::json!(42));
    let config = parse_and_validate(&serde_json::to_string(&value).unwrap())
        .expect("unknown fields must be tolerated");
    assert_eq!(config.max_concurrency, 2);
}

#[test]
fn zero_concurrency_is_semantically_invalid() {
    let mut value: serde_json::Value = serde_json::from_str(&valid_json()).unwrap();
    value["max_concurrency"] = serde_json::json!(0);
    let err = parse_and_validate(&serde_json::to_string(&value).unwrap())
        .expect_err("zero concurrency must be rejected");
    assert!(matches!(err, ConfigError::Invalid(_)), "got {err:?}");
    assert!(err.to_string().contains("max_concurrency"), "{err}");
}

#[test]
fn empty_dispatch_labels_is_semantically_invalid() {
    let mut value: serde_json::Value = serde_json::from_str(&valid_json()).unwrap();
    value["dispatch_labels"] = serde_json::json!([]);
    let err = parse_and_validate(&serde_json::to_string(&value).unwrap())
        .expect_err("no dispatchable labels must be rejected");
    assert!(matches!(err, ConfigError::Invalid(_)), "got {err:?}");
    assert!(err.to_string().contains("dispatch_labels"), "{err}");
}

#[test]
fn zero_stall_threshold_is_semantically_invalid() {
    let mut value: serde_json::Value = serde_json::from_str(&valid_json()).unwrap();
    value["stall_threshold_secs"] = serde_json::json!(0);
    let err = parse_and_validate(&serde_json::to_string(&value).unwrap())
        .expect_err("zero stall threshold must be rejected");
    assert!(matches!(err, ConfigError::Invalid(_)), "got {err:?}");
}

#[test]
fn empty_prompt_template_is_semantically_invalid() {
    let mut value: serde_json::Value = serde_json::from_str(&valid_json()).unwrap();
    value["prompt_template"] = serde_json::json!("   ");
    let err = parse_and_validate(&serde_json::to_string(&value).unwrap())
        .expect_err("blank prompt template must be rejected");
    assert!(matches!(err, ConfigError::Invalid(_)), "got {err:?}");
}

#[test]
fn a_zero_backoff_delay_with_retries_enabled_is_invalid() {
    let mut value: serde_json::Value = serde_json::from_str(&valid_json()).unwrap();
    value["retry_backoff"]["max_retries"] = serde_json::json!(3);
    value["retry_backoff"]["base_delay_ms"] = serde_json::json!(0);
    let err = parse_and_validate(&serde_json::to_string(&value).unwrap())
        .expect_err("retries without backoff delay must be rejected");
    assert!(matches!(err, ConfigError::Invalid(_)), "got {err:?}");
}

#[test]
fn a_zero_backoff_multiplier_is_invalid() {
    let mut value: serde_json::Value = serde_json::from_str(&valid_json()).unwrap();
    value["retry_backoff"]["backoff_multiplier"] = serde_json::json!(0);
    let err = parse_and_validate(&serde_json::to_string(&value).unwrap())
        .expect_err("a zero multiplier makes no sense");
    assert!(matches!(err, ConfigError::Invalid(_)), "got {err:?}");
}

#[test]
fn validate_accepts_a_structurally_sound_config_directly() {
    // validate 是纯判定，直接喂一个合法结构体（不经 JSON）。
    let config = WorkflowConfig {
        dispatch_labels: vec!["ready".into()],
        max_concurrency: 1,
        retry_backoff: RetryBackoff {
            max_retries: 0,
            base_delay_ms: 1,
            backoff_multiplier: 1,
        },
        stall_threshold_secs: 1,
        prompt_template: "p".into(),
        projection: ProjectionSwitches {
            open_pr: false,
            close_issue: false,
        },
    };
    assert!(validate(&config).is_ok());
}
