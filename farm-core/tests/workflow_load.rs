//! Workflow 配置从被编排仓库读取（farm-core::workflow::config::load_from_repo）。

use farm_core::workflow::config::{config_path, load_from_repo, ConfigError, WORKFLOW_CONFIG_REL_PATH};
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

#[test]
fn a_valid_config_file_in_the_orchestrated_repo_is_loaded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path();
    fs::create_dir_all(repo.join(".farm")).unwrap();
    fs::write(config_path(repo), VALID).unwrap();

    let config = load_from_repo(repo).expect("valid repo config must load");
    assert_eq!(config.max_concurrency, 2);
    assert_eq!(config.dispatch_labels, vec!["agent-ready"]);
}

#[test]
fn the_config_lives_at_the_documented_relative_path() {
    let repo = Path::new("/srv/orchestrated-repo");
    assert_eq!(
        config_path(repo),
        repo.join(WORKFLOW_CONFIG_REL_PATH),
        "workflow 配置文件路径是单一源"
    );
    assert_eq!(WORKFLOW_CONFIG_REL_PATH, ".farm/workflow.json");
}

#[test]
fn a_missing_config_file_is_a_read_error_not_a_panic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let err = load_from_repo(dir.path()).expect_err("no config file must be a Read error");
    assert!(matches!(err, ConfigError::Read(_)), "got {err:?}");
}

#[test]
fn an_invalid_config_file_in_the_repo_is_reported_for_lkg_fallback() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path();
    fs::create_dir_all(repo.join(".farm")).unwrap();
    fs::write(config_path(repo), r#"{"max_concurrency": 0}"#).unwrap();

    let err = load_from_repo(repo).expect_err("invalid repo config must be reported");
    assert!(matches!(err, ConfigError::Invalid(_) | ConfigError::Parse(_)), "got {err:?}");
}
