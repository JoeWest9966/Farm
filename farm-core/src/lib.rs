//! # farm-core — Farm 宿主核心
//!
//! ADR-0003 五分法的第二层：tokio runtime、共享 `AppState`、宿主私有存储
//! （rusqlite + WAL，ADR-0005）、版本化顺序 migration runner、内存事件总线原语。
//!
//! 本票（#11）只搭骨架：五服务的**实现**归 #13–#18，事件总线的服务面归 #15；
//! 这里提供它们共同的底座。事件总线的持久面在 [`event_log`]（ticket #15）。

pub mod agent_runner;
pub mod audit;
pub mod config;
pub mod db;
pub mod error;
pub mod event_log;
pub mod events;
pub mod merge;
pub mod migrations;
pub mod overlay;
pub mod recovery;
pub mod state;
pub mod tracker;
pub mod workspace;
pub mod workflow;

pub use audit::{AuditSink, AuditOutcome};
pub use config::HostConfig;
pub use db::{AppliedMigration, Database};
pub use error::CoreError;
pub use event_log::{EventLog, EventLogFilter};
pub use migrations::{Migration, Migrator, MIGRATIONS};
pub use overlay::{OverlayStore, StoredOverlay};
pub use merge::{
    MergeConfig, MergeError, MergeProposalRecord, MergeReport, MergeService, ProjectionOutcome,
    ProjectionStatus, ProjectionSwitch, RejectionStage,
};
pub use state::AppState;
pub use tracker::{Issue, IssueState, TrackerError, TrackerService};
pub use workspace::{
    EstablishRequest, MappingRow, MappingStatus, ReconcileReport, WorkspaceError, WorkspaceService,
};
pub use workflow::WorkflowService;

/// 当前 UTC 时间的 RFC3339 文本（事件时间戳 / migration applied_at）。
///
/// 秒级精度（`YYYY-MM-DDTHH:MM:SSZ`，20 字符）：事件次序由 `EventBus` 的
/// 单调 `seq`/`id` 保证，时间戳只作展示与审计可读，不承载排序职责。
pub fn now_rfc3339() -> String {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::now_utc()
        .replace_nanosecond(0)
        .ok()
        .and_then(|t| t.format(&Rfc3339).ok())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}
