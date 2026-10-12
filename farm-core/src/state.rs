//! 共享运行时状态（`AppState`）：farm-host 与五服务共同的底座。
//!
//! 底座（#11）：`config` + 宿主私有存储 `db` + 内存事件总线 `events`。
//! 服务域状态（tracker/agent-runner/merge/workspace/workflow 的句柄）由各服务票
//! **加法追加**，不改这里的历史形状。SQLite schema 永不外泄为契约（ADR-0005 §5）：
//! 面板与插件都不直连 DB，一切读走契约 API / 宿主投影 API。

use std::sync::Arc;

use crate::config::HostConfig;
use crate::db::Database;
use crate::error::CoreError;
use crate::events::EventBus;
use crate::migrations::{Migrator, MIGRATIONS};
use crate::workflow::WorkflowService;
use crate::workspace::WorkspaceService;

/// 宿主共享状态。`Clone` 廉价（内部 `Arc`/`Clone`），可被 axum handler 广泛共享。
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<HostConfig>,
    pub db: Database,
    pub events: EventBus,
    /// Workflow 配置装载 + last-known-good（ticket #12；查询 API 的数据源）。
    pub workflow: WorkflowService,
    /// Workspace 服务句柄（#14，加法追加）：持有 spec↔集成分支↔worktree 映射。
    pub workspace: WorkspaceService,
}

impl AppState {
    /// 宿主启动路径：打开私有存储、跑 migration runner（幂等）、建内存总线。
    pub fn new(config: HostConfig) -> Result<Self, CoreError> {
        let db = Database::open(&config.db_path)?;
        db.migrate(&Migrator::new(MIGRATIONS))?;
        let events = EventBus::new();
        Ok(Self {
            config: Arc::new(config),
            workflow: WorkflowService::new(db.clone(), events.clone()),
            workspace: WorkspaceService::new(db.clone()),
            db,
            events,
        })
    }

    /// 用内存库构造（测试/一次性工具用；不落盘）。
    pub fn new_in_memory(config: HostConfig) -> Result<Self, CoreError> {
        let db = Database::open_in_memory()?;
        db.migrate(&Migrator::new(MIGRATIONS))?;
        let events = EventBus::new();
        Ok(Self {
            config: Arc::new(config),
            workflow: WorkflowService::new(db.clone(), events.clone()),
            workspace: WorkspaceService::new(db.clone()),
            db,
            events,
        })
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("bind", &self.config.bind)
            .field("db", &self.db)
            .finish_non_exhaustive()
    }
}
