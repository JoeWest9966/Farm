//! 共享运行时状态（`AppState`）：farm-host 与五服务共同的底座。
//!
//! 本票（#11）只含底座：`config` + 宿主私有存储 `db` + 内存事件总线 `events`。
//! ticket #15 加法追加事件日志句柄 `event_log` 与**持久化发布路径** [`AppState::publish`]。
//! 服务域状态（tracker/agent-runner/merge/workspace 的句柄）由 #13–#18 **加法追加**，
//! 不改这里的历史形状。SQLite schema 永不外泄为契约（ADR-0005 §5）：面板与插件
//! 都不直连 DB，一切读走契约 API / 宿主投影 API。

use std::sync::Arc;

use crate::config::HostConfig;
use crate::db::Database;
use crate::error::CoreError;
use crate::event_log::EventLog;
use crate::events::EventBus;
use crate::migrations::{Migrator, MIGRATIONS};
use farm_sdk::events::{EventEnvelope, NewEvent};

/// 宿主共享状态。`Clone` 廉价（内部 `Arc`/`Clone`），可被 axum handler 广泛共享。
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<HostConfig>,
    pub db: Database,
    /// 内存事件总线：事件回流面的**活面**（广播原语 + 订阅）。
    pub events: EventBus,
    /// 事件日志句柄：事件回流面的**持久面**（数据在 `db` 的 `events` 表里）。
    /// 只持有保留窗口，可廉价复制；保留窗口可覆写（数值归宿主/workflow 配置，#12）。
    pub event_log: EventLog,
}

impl AppState {
    /// 宿主启动路径：打开私有存储、跑 migration runner（幂等）、建内存总线与事件日志。
    pub fn new(config: HostConfig) -> Result<Self, CoreError> {
        let db = Database::open(&config.db_path)?;
        db.migrate(&Migrator::new(MIGRATIONS))?;
        Ok(Self {
            config: Arc::new(config),
            db,
            events: EventBus::new(),
            event_log: EventLog::new(),
        })
    }

    /// 用内存库构造（测试/一次性工具用；不落盘）。
    pub fn new_in_memory(config: HostConfig) -> Result<Self, CoreError> {
        let db = Database::open_in_memory()?;
        db.migrate(&Migrator::new(MIGRATIONS))?;
        Ok(Self {
            config: Arc::new(config),
            db,
            events: EventBus::new(),
            event_log: EventLog::new(),
        })
    }

    /// 发布一条事件（ticket #15）：宿主盖章 → 写事件日志 → 广播到内存总线。
    ///
    /// **日志先于总线**，所以「总线里出现过的事件必在日志里」——WS 投影才能
    /// 先订阅再取快照而不丢（无丢失）。append 与 broadcast 在同一把存储锁内完成，
    /// 所以日志 `seq` 的次序就是总线上的次序（按序收到）。
    ///
    /// 落库失败即整条发布失败、**不上总线**：面板从未见过它，也就无所谓丢；
    /// 调用方（#16 agent-runner / #18 merge 服务）拿到 `Err` 自行重试或上报。
    /// 想要「纯活面广播」用 [`AppState::events`] 上的 `EventBus::publish`。
    pub fn publish(&self, new: NewEvent) -> Result<EventEnvelope, CoreError> {
        let envelope = self.events.stamp(new);
        self.db.with_conn_core(|conn| {
            self.event_log.append_conn(conn, &envelope)?;
            self.events.broadcast(&envelope);
            Ok(())
        })?;
        Ok(envelope)
    }

    /// 事件日志历史查询（快照 API 与 WS 快照帧的后端）。
    pub fn event_history(
        &self,
        filter: &crate::event_log::EventLogFilter,
    ) -> Result<farm_sdk::services::events::EventHistoryResponse, CoreError> {
        self.event_log.history(&self.db, filter)
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("bind", &self.config.bind)
            .field("db", &self.db)
            .field("event_log", &self.event_log)
            .finish_non_exhaustive()
    }
}
