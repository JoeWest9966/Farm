//! farm-core 错误类型。宿主侧统一转成契约闭集错误（`ContractError`）。

use farm_sdk::ContractError;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("host-private store error: {0}")]
    Store(#[from] rusqlite::Error),

    #[error("host-private store mutex poisoned")]
    StorePoisoned,

    #[error("migration v{version} ({name}) failed: {source}")]
    Migration {
        version: u32,
        name: String,
        #[source]
        source: rusqlite::Error,
    },

    #[error("invalid migration set: {0}")]
    InvalidMigrations(String),

    #[error("config error: {0}")]
    Config(String),

    /// 事件日志里有一行不是合法的事件信封（kind 不在闭集 / payload 不是 JSON）。
    /// 宿主绝不把它静默降级成一个假事件——闭集是契约（ADR-0001 决策 6）。
    #[error("event log row {id} is not a contract event envelope: {detail}")]
    CorruptEventLogRow { id: String, detail: String },

    /// 查询条件不合法（如 `since` 不是 RFC3339 时间戳）。
    #[error("invalid event log filter: {0}")]
    InvalidFilter(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("os randomness unavailable: {0}")]
    Randomness(String),
}

impl CoreError {
    /// 投影到契约闭集错误：内部故障对客户端只有 `internal` / `service_unavailable` 两种形状。
    pub fn to_contract_error(&self) -> ContractError {
        match self {
            CoreError::Store(_) | CoreError::StorePoisoned => {
                ContractError::service_unavailable("host-private store is unavailable")
            }
            CoreError::Migration { version, name, .. } => {
                ContractError::service_unavailable(format!("migration v{version} ({name}) failed"))
            }
            CoreError::InvalidMigrations(msg) => {
                ContractError::internal(format!("host migration set is invalid: {msg}"))
            }
            CoreError::Config(msg) => ContractError::internal(format!("host config is invalid: {msg}")),
            CoreError::InvalidFilter(msg) => {
                ContractError::invalid_request(format!("invalid event log filter: {msg}"))
            }
            CoreError::CorruptEventLogRow { id, detail } => ContractError::internal(format!(
                "event log row {id} is not a contract event envelope: {detail}"
            )),
            CoreError::Io(_) => ContractError::internal("host io failure"),
            CoreError::Randomness(msg) => {
                ContractError::internal(format!("host entropy failure: {msg}"))
            }
        }
    }
}

impl From<CoreError> for ContractError {
    fn from(e: CoreError) -> Self {
        e.to_contract_error()
    }
}
