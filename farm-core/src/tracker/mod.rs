//! # Tracker 服务（五服务之一，ticket #13）
//!
//! GLOSSARY：归一化 Issue 模型 + 原生 blocking 边 + **原子 claim**（CAS 语义，
//! assignee 即 claim），把 provider 语义（GitHub/Gogs/native）关进 adapter。
//! 面板与编排器读同一个归一化视图——**tracker 是总线**。frontier/dispatchable
//! 的计算是编排器的职责，不在服务内（ADR-0001 决策 3）。
//!
//! 分层（provider 语义自外向内一关到底）：
//!
//! ```text
//! provider（GitHub/Gogs/native）
//!   └─ adapter::TrackerAdapter   原生记录 → 归一化 Issue（provider 语义止于此）
//!        └─ service::TrackerService  归一化视图 + 原子 claim + 状态历史
//!             └─ store::TrackerStore  票快照 / blocking 边 / 历史（SQLite，ADR-0005）
//! ```
//!
//! 持久化（ADR-0005 §2/§5）：票快照与状态历史落宿主私有存储，schema 永不外泄为
//! 契约；服务域 migration 住在 [`migrations`]，由 `crate::migrations` 按版本登记。

pub mod adapter;
pub mod migrations;
pub mod model;
pub mod provider;
pub mod service;
pub mod store;

pub use adapter::{NativeAdapter, NativeTicket, NativeTicketState, TrackerAdapter, NATIVE_PROVIDER};
pub use provider::{current_provider, set_provider, KNOWN_PROVIDERS, TRACKER_PROVIDER_KEY};
pub use farm_sdk::services::tracker::IssueState;
pub use model::{parse_state, state_to_str, BlockingEdge, Issue, StateTransition};
pub use service::{SyncReport, TrackerService};
pub use store::{ClaimOutcome, ReleaseOutcome};

use farm_sdk::ContractError;

/// Tracker 服务的错误面：投影到契约闭集错误码（不加新码，ADR-0001 决策 6）。
#[derive(Debug, thiserror::Error)]
pub enum TrackerError {
    /// 票不在归一化视图里（provider 没有这张票）。
    #[error("issue {issue_id} is not in the normalized view")]
    NotFound { issue_id: String },

    /// 原子 claim / release 撞上了别人的排他声明（GLOSSARY：失败即已被他人抢走）。
    #[error("issue {issue_id} is held by {holder}")]
    Conflict { issue_id: String, holder: String },

    /// 宿主私有存储故障。
    #[error(transparent)]
    Store(#[from] crate::error::CoreError),
}

impl TrackerError {
    /// 投影到契约闭集：`not_found` / `conflict` / `service_unavailable`。
    ///
    /// `conflict` 的 `details` 带当前持有者，客户端无需再查一次视图。
    pub fn to_contract_error(&self) -> ContractError {
        match self {
            TrackerError::NotFound { issue_id } => ContractError::not_found(format!(
                "issue {issue_id} is not in the normalized view"
            )),
            TrackerError::Conflict { issue_id, holder } => ContractError::conflict(format!(
                "issue {issue_id} is already claimed by {holder}"
            ))
            .with_details(serde_json::json!({ "issue_id": issue_id, "holder": holder })),
            TrackerError::Store(source) => source.to_contract_error(),
        }
    }
}

impl From<TrackerError> for ContractError {
    fn from(err: TrackerError) -> Self {
        err.to_contract_error()
    }
}
