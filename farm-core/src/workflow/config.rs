//! Workflow 配置读取与校验：宿主私有逻辑，**不进契约**（ADR-0005 §5）。
//!
//! GLOSSARY：非法配置不致崩溃——这里的校验失败由上层（[`crate::workflow::service`]）
//! 回退 last-known-good 并发告警，绝不 panic。合法 → [`WorkflowConfig`]；非法 →
//! [`ConfigError`]。

use std::path::{Path, PathBuf};

use farm_sdk::workflow::WorkflowConfig;

/// 被编排仓库里 workflow 配置文件的相对路径（GLOSSARY：避免叫 WORKFLOW.md）。
pub const WORKFLOW_CONFIG_REL_PATH: &str = ".farm/workflow.json";

/// 配置读取/校验错误。三分：IO 读失败、JSON/缺字段解析失败、语义非法。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("workflow config file unreadable: {0}")]
    Read(String),
    #[error("workflow config is not valid JSON or is missing required fields: {0}")]
    Parse(String),
    #[error("workflow config is semantically invalid: {0}")]
    Invalid(String),
}

/// 某被编排仓库的 workflow 配置文件路径。
pub fn config_path(repo_root: &Path) -> PathBuf {
    repo_root.join(WORKFLOW_CONFIG_REL_PATH)
}

/// 从文本解析并校验 workflow 配置。
pub fn parse_and_validate(text: &str) -> Result<WorkflowConfig, ConfigError> {
    let config: WorkflowConfig =
        serde_json::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))?;
    validate(&config)?;
    Ok(config)
}

/// 从被编排仓库读取并校验 workflow 配置。
pub fn load_from_repo(repo_root: &Path) -> Result<WorkflowConfig, ConfigError> {
    let path = config_path(repo_root);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| ConfigError::Read(format!("{}: {e}", path.display())))?;
    parse_and_validate(&text)
}

/// 语义校验（GLOSSARY 六要素的最小可派发约束）。缺一即判非法 → 上层回退 LKG。
pub fn validate(config: &WorkflowConfig) -> Result<(), ConfigError> {
    if config.dispatch_labels.is_empty() {
        return Err(ConfigError::Invalid(
            "dispatch_labels must list at least one label (nothing would be dispatchable)".into(),
        ));
    }
    if config.max_concurrency == 0 {
        return Err(ConfigError::Invalid(
            "max_concurrency must be >= 1 (zero concurrency can never dispatch)".into(),
        ));
    }
    if config.stall_threshold_secs == 0 {
        return Err(ConfigError::Invalid(
            "stall_threshold_secs must be >= 1 (zero would stall-kill every attempt)".into(),
        ));
    }
    if config.prompt_template.trim().is_empty() {
        return Err(ConfigError::Invalid(
            "prompt_template must not be empty (dispatch needs a prompt)".into(),
        ));
    }
    if config.retry_backoff.backoff_multiplier < 1 {
        return Err(ConfigError::Invalid(
            "retry_backoff.backoff_multiplier must be >= 1".into(),
        ));
    }
    if config.retry_backoff.max_retries >= 1 && config.retry_backoff.base_delay_ms == 0 {
        return Err(ConfigError::Invalid(
            "retry_backoff.base_delay_ms must be >= 1 when retries are enabled".into(),
        ));
    }
    Ok(())
}
