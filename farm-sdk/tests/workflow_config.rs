//! Workflow 配置契约形状（GLOSSARY：Workflow 配置住在被编排仓库，非法时保 LKG）。
//!
//! 域类型进 farm-sdk 是因为宿主的查询 API 暴露它们（ADR-0005 §5：SQLite schema 永不
//! 外泄，但契约响应形状是稳定面）。这些类型只被序列化/反序列化 + 生成 schema，零运行时依赖。

use farm_sdk::workflow::WorkflowConfig;

/// 已知合法配置（独立真值：一份手写的 JSON 字面量，非从实现反推）。
const VALID_CONFIG_JSON: &str = r#"{
  "dispatch_labels": ["agent-ready", "orch"],
  "max_concurrency": 3,
  "retry_backoff": {
    "max_retries": 5,
    "base_delay_ms": 1000,
    "backoff_multiplier": 2
  },
  "stall_threshold_secs": 600,
  "prompt_template": "Implement ticket {{ticket_id}}: {{title}}",
  "projection": {
    "open_pr": true,
    "close_issue": false
  }
}"#;

#[test]
fn a_known_good_workflow_config_json_parses_into_the_documented_shape() {
    let config: WorkflowConfig =
        serde_json::from_str(VALID_CONFIG_JSON).expect("known-good config must parse");

    // 六要素（GLOSSARY）逐个用字面量钉住：期望值来自 JSON 文本，不重算实现。
    assert_eq!(config.dispatch_labels, vec!["agent-ready", "orch"]);
    assert_eq!(config.max_concurrency, 3);
    assert_eq!(config.retry_backoff.max_retries, 5);
    assert_eq!(config.retry_backoff.base_delay_ms, 1000);
    assert_eq!(config.retry_backoff.backoff_multiplier, 2);
    assert_eq!(config.stall_threshold_secs, 600);
    assert_eq!(config.prompt_template, "Implement ticket {{ticket_id}}: {{title}}");
    assert!(config.projection.open_pr);
    assert!(!config.projection.close_issue);
}

#[test]
fn a_workflow_config_round_trips_through_json_without_drift() {
    let config: WorkflowConfig = serde_json::from_str(VALID_CONFIG_JSON).expect("parse");
    let json = serde_json::to_string_pretty(&config).expect("serialize");
    let reparsed: WorkflowConfig = serde_json::from_str(&json).expect("reparse");
    assert_eq!(config, reparsed, "round-trip 不得漂移");
}

#[test]
fn the_workflow_config_has_a_json_schema_for_the_contract() {
    // 查询 API 暴露该形状，故必须能生成 JSON Schema（进 docs/contract/schema.json）。
    let schema = schemars::schema_for!(WorkflowConfig);
    let value = serde_json::to_value(&schema).expect("schema must serialize");
    let props = value["properties"].as_object().expect("properties");
    for field in [
        "dispatch_labels",
        "max_concurrency",
        "retry_backoff",
        "stall_threshold_secs",
        "prompt_template",
        "projection",
    ] {
        assert!(props.contains_key(field), "WorkflowConfig schema 缺字段 {field}");
    }
}
