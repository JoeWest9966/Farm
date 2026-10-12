//! 共享运行时状态（`AppState`）：farm-host 与五服务共同的底座。
//!
//! 底座（#11）：`config` + 宿主私有存储 `db` + 内存事件总线 `events`。
//! ticket #15 加法追加事件日志句柄 `event_log` 与**持久化发布路径** [`AppState::publish`]。
//! 服务域状态（tracker/agent-runner/merge/workspace/workflow 的句柄）由各服务票
//! **加法追加**，不改这里的历史形状。SQLite schema 永不外泄为契约（ADR-0005 §5）：
//! 面板与插件都不直连 DB，一切读走契约 API / 宿主投影 API。

use std::sync::Arc;

use crate::agent_runner::AgentRunner;
use crate::audit::AuditSink;
use crate::config::HostConfig;
use crate::db::Database;
use crate::error::CoreError;
use crate::event_log::EventLog;
use crate::events::EventBus;
use crate::merge::MergeService;
use crate::migrations::{Migrator, MIGRATIONS};
use crate::tracker::{NativeAdapter, TrackerService};
use crate::workflow::WorkflowService;
use crate::workspace::WorkspaceService;
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
    /// Tracker 服务句柄（#13）：归一化视图 + 原子 claim 的唯一入口。
    ///
    /// 面板与编排器都经契约路由打到这同一个句柄——tracker 是总线。
    pub tracker: TrackerService,
    /// Workflow 配置装载 + last-known-good（ticket #12；查询 API 的数据源）。
    pub workflow: WorkflowService,
    /// Workspace 服务句柄（#14，加法追加）：持有 spec↔集成分支↔worktree 映射。
    pub workspace: WorkspaceService,
    /// Agent-Runner 服务句柄（#16，加法追加）：本地后端（spawn pi --mode rpc）+ 判据阶梯。
    ///
    /// 测试经 [`AppState::with_agent_runner`] 换成 fake pi 替身；生产用默认
    /// （真 `pi` + 无凭据源，#17 之前的状态）。
    pub agent_runner: AgentRunner,
    /// Merge 服务句柄（#18，加法追加）：agent 只能提议，宿主机械校验后执行真 merge。
    ///
    /// 完成信号 = 集成分支上该票的 merge commit。测试经 [`AppState::with_merge`]
    /// 换成快的测试闸门（生产默认 `cargo test`）。
    pub merge: MergeService,
    /// 审计双写旁路（#20，加法追加）：关键事件经 `ledger record` 子进程进 Kron 账本。
    ///
    /// **默认禁用**（`AuditSink::disabled()`）：宿主没配上 `ledger` 之前一个子进程都不起，
    /// 所以 #15/#16 的发布路径行为一字不变。配上之后由 [`AppState::publish`] 顺带双写，
    /// 失败只告警不阻塞（ADR-0005 §3）。
    pub audit: AuditSink,
}

impl AppState {
    /// 宿主启动路径：打开私有存储、跑 migration runner（幂等）、建内存总线与事件日志。
    ///
    /// 审计旁路经 [`AuditSink::from_env`] 装配（`FARM_LEDGER` / `FARM_LEDGER_ACTOR_FARM`）。
    pub fn new(config: HostConfig) -> Result<Self, CoreError> {
        Self::new_with_audit(config, AuditSink::from_env())
    }

    /// 同 [`AppState::new`]，但审计旁路显式注入（测试注入 fake `ledger` 替身）。
    pub fn new_with_audit(config: HostConfig, audit: AuditSink) -> Result<Self, CoreError> {
        let db = Database::open(&config.db_path)?;
        db.migrate(&Migrator::new(MIGRATIONS))?;
        Ok(Self::assemble(db, config, audit))
    }

    /// 用内存库构造（测试/一次性工具用；不落盘）。
    ///
    /// 内存库没有「上一次运行」：没有可恢复的 `running` attempt，也不补
    /// `host_restarted`——崩溃恢复是**文件库宿主重启**路径，显式走
    /// [`crate::recovery`]。审计旁路同样禁用（测试面不变量）。
    pub fn new_in_memory(config: HostConfig) -> Result<Self, CoreError> {
        let db = Database::open_in_memory()?;
        db.migrate(&Migrator::new(MIGRATIONS))?;
        Ok(Self::assemble(db, config, AuditSink::disabled()))
    }

    /// 三个构造器共用的装配（不含启动副作用：恢复/对账由调用方显式触发）。
    fn assemble(db: Database, config: HostConfig, audit: AuditSink) -> Self {
        let events = EventBus::new();
        Self {
            config: Arc::new(config),
            tracker: default_tracker(&db),
            workflow: WorkflowService::new(db.clone(), events.clone()),
            workspace: WorkspaceService::new(db.clone()),
            event_log: EventLog::new(),
            agent_runner: default_agent_runner(&db),
            merge: MergeService::new(db.clone()),
            audit,
            db,
            events,
        }
    }

    /// 覆写 agent-runner 句柄（测试注入 fake pi 替身 / 生产换 program 与凭据源）。
    ///
    /// 加法构造器：只换一个字段，别的服务句柄不动。
    pub fn with_agent_runner(mut self, agent_runner: AgentRunner) -> Self {
        self.agent_runner = agent_runner;
        self
    }

    /// 覆写 merge 句柄（测试换快的测试闸门 / 宿主按被编排仓库配置）。
    pub fn with_merge(mut self, merge: MergeService) -> Self {
        self.merge = merge;
        self
    }

    /// 覆写 tracker 句柄（测试注入 native 票源 / 宿主换 provider adapter）。
    pub fn with_tracker(mut self, tracker: TrackerService) -> Self {
        self.tracker = tracker;
        self
    }

    /// 覆写审计双写旁路（测试注入 fake `ledger` 替身 / 生产指向真 `ledger`）。
    pub fn with_audit_sink(mut self, audit: AuditSink) -> Self {
        self.audit = audit;
        self
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
    ///
    /// ## 审计双写（#20）
    /// 落库 + 广播之后顺带过一趟 [`AuditSink`]：审计闭集里的关键事件（派发、merge、
    /// 审批、stall 判杀、host_restarted）经 `ledger record` 子进程补进 Kron 账本。
    /// 这一步在发布路径**之外**：Kron 写失败只发 `needs_attention` 告警、`audited`
    /// 不置位，主流程（返回值、总线、SQLite）不受任何影响（ADR-0005 §3）。
    pub fn publish(&self, new: NewEvent) -> Result<EventEnvelope, CoreError> {
        let envelope = self.publish_without_audit(new)?;
        self.audit.post(self, &envelope);
        Ok(envelope)
    }

    /// 只走持久化发布路径（SQLite + 总线），**不过审计旁路**。
    ///
    /// 审计自己的告警事件（`needs_attention`）用它发：告警不在审计闭集里，
    /// 走这条路也就不可能递归双写。
    pub fn publish_without_audit(&self, new: NewEvent) -> Result<EventEnvelope, CoreError> {
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

/// 默认 Agent-Runner：本地后端跑真 `pi`，无凭据源（#17 之前的状态）。
fn default_agent_runner(db: &Database) -> AgentRunner {
    AgentRunner::new(db.clone())
}

/// 默认 Tracker 服务：native adapter（v1 唯一的 provider），启动时无 authored 票。
///
/// 票源由宿主管理面（#23）或测试夹具注入：换 provider = 换 adapter，服务与契约不动。
fn default_tracker(db: &Database) -> TrackerService {
    TrackerService::new(db.clone(), std::sync::Arc::new(NativeAdapter::empty()))
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("bind", &self.config.bind)
            .field("db", &self.db)
            .field("event_log", &self.event_log)
            .field("audit_enabled", &self.audit.is_enabled())
            .finish_non_exhaustive()
    }
}
