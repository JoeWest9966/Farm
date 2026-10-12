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
