//! # audit —— Kron 审计双写（ticket #20）
//!
//! GLOSSARY「审计双写」：关键事件（派发、merge、审批、stall 判杀、host_restarted）既进
//! 宿主私有存储（面板读源）又经 `ledger record` 子进程进 Kron 账本（审计真源）。
//! ADR-0005 §3/§8：**Kron 写失败不阻塞主流程，只告警**——审计是旁路，不是运行依赖；
//! 未入账事件带 `audited` 标记位，启动时批量补账（ledger 幂等）。
//!
//! ```text
//! AppState::publish ──┬─► events 表（SQLite，面板唯一读源）
//!                     └─► AuditSink ──► `ledger record` 子进程 ──► Kron 账本
//!                            （kind 不在审计闭集 / 写失败：到此为止，只告警）
//! 启动：对账扫 audited=0 的关键事件批量补写（幂等）
//! ```
//!
//! ## 与兄弟仓库的边界（ADR-0004 决策 2）
//! - Kron **不是** Farm 的持久化层；面板读账走低频 + `--agg` 侧车（读面同模块，
//!   低频 + 侧车的调用形状由 fake `ledger` 验证）。
//! - `ledger record/read` 是**子进程**调用，Rust 侧只做 serde + `deny_unknown_fields`
//!   的形状映射（[`schema`]），不链接、不 fork Kron 内部包。
//! - **前置（跨仓协调，未完成）**：Kron `actor` 闭集扩位加入 `farm`。未完成时
//!   [`AuditSink`] 只把事件标成「未入账」（仍落 SQLite、`audited=0`、`host_meta` 留证），
//!   **不阻塞主流程**；跨仓落地后经 [`AuditSink::with_actor_farm_accepted`] 打开即自动补账。
//!
//! ## 红线「禁入」
//! Kron 账本禁入消息正文、文件内容、system prompt、任何凭据（report-nous-family §3.2）。
//! 所以 [`schema::KronRecord`] 只带标识字段，事件 payload 原样留在宿主私有存储里。

pub mod ledger;
pub mod schema;

pub use ledger::{LedgerEnvelope, LedgerWriter, RecordResult};
pub use schema::{KronActor, KronDetails, KronEventType, KronRecord};

use std::path::PathBuf;
use std::time::Duration;

use rusqlite::OptionalExtension;

use farm_sdk::events::{EventEnvelope, EventKind, NewEvent};

use crate::db::Database;
use crate::error::CoreError;
use crate::state::AppState;

/// `ledger` 可执行文件的默认程序名（PATH 上找；测试注入 fake 替身的绝对路径）。
pub const DEFAULT_LEDGER_PROGRAM: &str = "ledger";

/// `ledger record` 子进程的 argv（除程序名外）。
pub const RECORD_ARGS: &[&str] = &["record"];

/// 单次 `ledger` 调用的硬超时：审计是旁路，绝不允许它把主流程挂住。
pub const DEFAULT_LEDGER_TIMEOUT: Duration = Duration::from_secs(10);

/// 一次补账最多带多少笔（Kron `record` 收 NDJSON 流；分批只是让单次调用有界）。
pub const DEFAULT_BATCH_SIZE: usize = 100;

/// 审计旁路：把关键事件写进 Kron 账本（失败只告警，不阻塞）。
///
/// 句柄可廉价复制（内部全是值），挂在 [`crate::AppState`] 上。
/// **默认禁用**：宿主没配上 `ledger` 之前，双写旁路一个子进程都不起
/// （`AppState::publish` 的既有行为因此一字不变）。
#[derive(Clone, Debug)]
pub struct AuditSink {
    writer: LedgerWriter,
    /// Kron `actor` 闭集是否已扩位加入 `farm`（跨仓协调；未完成 → 只标未入账）。
    actor_farm_accepted: bool,
    /// 单次补账的批量上限。
    batch_size: usize,
}

impl AuditSink {
    /// 禁用的旁路（不起子进程、不告警、不置位）——默认值。
    pub fn disabled() -> Self {
        Self {
            writer: LedgerWriter::disabled(),
            actor_farm_accepted: false,
            batch_size: DEFAULT_BATCH_SIZE,
        }
    }

    /// 指向一个 `ledger` 程序（测试传 fake 替身的绝对路径）。
    ///
    /// `actor_farm_accepted` 沿用当前跨仓协调状态：未扩位前只标记未入账。
    pub fn new(program: impl Into<std::path::PathBuf>) -> Self {
        Self {
            writer: LedgerWriter::new(program),
            actor_farm_accepted: false,
            batch_size: DEFAULT_BATCH_SIZE,
        }
    }

    /// 从环境装配：`FARM_LEDGER`（程序路径，缺省 `ledger`）+
    /// `FARM_LEDGER_ACTOR_FARM=accepted`（跨仓扩位落地后才置）。
    pub fn from_env() -> Self {
        let program =
            std::env::var("FARM_LEDGER").unwrap_or_else(|_| DEFAULT_LEDGER_PROGRAM.to_string());
        let actor_farm_accepted = std::env::var("FARM_LEDGER_ACTOR_FARM")
            .map(|v| v == "accepted")
            .unwrap_or(false);
        Self {
            writer: LedgerWriter::new(std::path::PathBuf::from(program)),
            actor_farm_accepted,
            batch_size: DEFAULT_BATCH_SIZE,
        }
    }

    /// 覆写 `ledger record` 的 argv（除程序名外）。
    pub fn with_record_args(mut self, args: Vec<String>) -> Self {
        self.writer = self.writer.with_args(args);
        self
    }

    /// 覆写单次调用的硬超时。
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.writer = self.writer.with_timeout(timeout);
        self
    }

    /// 覆写补账批量上限（至少 1）。
    pub fn with_batch_size(mut self, batch_size: usize) -> Self {
        self.batch_size = batch_size.max(1);
        self
    }

    /// 声明 Kron `actor` 闭集已扩位加入 `farm`（跨仓协调完成）。
    pub fn with_actor_farm_accepted(mut self, accepted: bool) -> Self {
        self.actor_farm_accepted = accepted;
        self
    }

    /// 旁路是否启用（有 `ledger` 程序可调）。
    pub fn is_enabled(&self) -> bool {
        self.writer.is_enabled()
    }

    /// Kron `actor` 闭集是否已收 `farm`。
    pub fn actor_farm_accepted(&self) -> bool {
        self.actor_farm_accepted
    }

    /// `ledger` 程序路径（测试/诊断可读）。
    pub fn program(&self) -> String {
        self.writer.program()
    }

    pub fn batch_size(&self) -> usize {
        self.batch_size
    }

    /// 宿主发布路径的旁路：把**已落库**的信封补进 Kron 账本。
    ///
    /// 调用方（[`crate::AppState::publish`]）已经写完 `events` 表——所以这里无论成败，
    /// 主流程都已完成。返回值只描述「入账了没有」，供对账与测试断言：
    ///
    /// - kind 不在审计闭集 / 旁路禁用 → [`AuditOutcome::Skipped`]，一个子进程都不起；
    /// - `farm` 尚未进 Kron actor 闭集 → [`AuditOutcome::Unposted`]，只在 `host_meta`
    ///   留证 + `tracing` 告警；
    /// - `ledger record` 成功 → 置 `audited=1`，回 [`AuditOutcome::Posted`]；
    /// - `ledger record` 失败 → 发 `needs_attention` 告警事件，`audited` 保持 0，
    ///   留待启动对账补账（ledger 幂等）——**主流程不受任何影响**。
    pub fn post(&self, state: &AppState, envelope: &EventEnvelope) -> AuditOutcome {
        if !self.is_enabled() || !envelope.kind.is_audit_critical() {
            return AuditOutcome::Skipped;
        }
        if !self.actor_farm_accepted {
            // 跨仓前置未完成：记录「未入账」标记，绝不阻塞、绝不假装入账。
            let reason = "kron actor closed set does not include `farm` yet \
                          (cross-repo extension pending); events stay in SQLite with audited=0"
                .to_string();
            record_unposted_marker(&state.db, &reason);
            tracing::warn!(
                event_id = %envelope.id,
                kind = %envelope.kind,
                "audit double-write deferred: {reason}"
            );
            return AuditOutcome::Unposted { reason };
        }

        let record = match schema::KronRecord::from_event(envelope) {
            Some(record) => record,
            None => return AuditOutcome::Skipped,
        };
        let outcome = match self.writer.record(std::slice::from_ref(&record)) {
            Ok(result) if result.ok => match mark_audited(&state.db, &envelope.id) {
                Ok(()) => AuditOutcome::Posted {
                    duplicate: result.duplicates > 0,
                },
                Err(e) => AuditOutcome::Failed {
                    // 账已入、标记没落上：只告警。启动对账会按幂等键再补一次（duplicate）。
                    reason: format!("ledger accepted the record but the audited flag failed: {e}"),
                },
            },
            Ok(result) => AuditOutcome::Failed {
                reason: format!(
                    "ledger record rejected the event (exit {}): {}",
                    result.exit_code,
                    result
                        .message
                        .unwrap_or_else(|| "no error message".to_string())
                ),
            },
            Err(e) => AuditOutcome::Failed {
                reason: format!("ledger record subprocess failed: {e}"),
            },
        };
        if let AuditOutcome::Failed { reason } = &outcome {
            alert(state, envelope, reason);
        }
        outcome
    }

    /// 启动对账的批量补账（`reconcile` 用）：一批未入账事件一次子进程。
    pub(crate) fn post_batch(
        &self,
        envelopes: &[EventEnvelope],
    ) -> Result<RecordResult, CoreError> {
        let records: Vec<_> = envelopes
            .iter()
            .filter_map(schema::KronRecord::from_event)
            .collect();
        self.writer.record(&records)
    }
}

impl Default for AuditSink {
    fn default() -> Self {
        Self::disabled()
    }
}

/// 一次双写的结果（主流程不依赖它；只有对账与测试读它）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuditOutcome {
    /// 这一条不入账：kind 不在审计闭集，或旁路未启用。
    Skipped,
    /// 跨仓前置未完成：只标记未入账（SQLite + `audited=0` + `host_meta` 留证）。
    Unposted { reason: String },
    /// 已入 Kron 账本（`duplicate` = 幂等键撞上，账本原样收下）。
    Posted { duplicate: bool },
    /// 入账失败：主流程继续，只发告警事件，`audited` 不置位。
    Failed { reason: String },
}

impl AuditOutcome {
    /// 是否已入账（供测试与对账统计）。
    pub fn is_posted(&self) -> bool {
        matches!(self, AuditOutcome::Posted { .. })
    }
}

/// `host_meta` 里记「未入账」原因的键（复用 v1 的 key/value 表，不加 migration）。
pub const UNPOSTED_MARKER_KEY: &str = "audit.unposted_reason";

/// 把「未入账」标记写进宿主私有存储的 key/value 表（v1 `host_meta`）。
pub fn record_unposted_marker(db: &Database, reason: &str) {
    let _ = db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO host_meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![UNPOSTED_MARKER_KEY, reason],
        )
    });
}

/// 清掉「未入账」标记（跨仓扩位落地、补账成功后调用）。
pub fn clear_unposted_marker(db: &Database) {
    let _ = db.with_conn(|conn| {
        conn.execute(
            "DELETE FROM host_meta WHERE key = ?1",
            rusqlite::params![UNPOSTED_MARKER_KEY],
        )
    });
}

/// 读当前「未入账」标记（宿主管理面/诊断可读）。
pub fn unposted_marker(db: &Database) -> Option<String> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT value FROM host_meta WHERE key = ?1",
            rusqlite::params![UNPOSTED_MARKER_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()
    })
    .ok()
    .flatten()
}

/// 置 `audited` 标记位（ADR-0005 §8：`ledger record` 成功才置位）。
fn mark_audited(db: &Database, event_id: &str) -> Result<(), CoreError> {
    let updated = db.with_conn(|conn| {
        conn.execute(
            "UPDATE events SET audited = 1 WHERE id = ?1",
            rusqlite::params![event_id],
        )
    })?;
    if updated == 0 {
        return Err(CoreError::Config(format!(
            "event {event_id} is not in the event log any more (retention window?)"
        )));
    }
    Ok(())
}

/// 审计告警：`needs_attention` 一等事件（GLOSSARY：关键事件保证进总线且可订阅）。
///
/// 告警本身**不再入账**（`needs_attention` 不在审计闭集里），所以不存在递归双写。
fn alert(state: &AppState, envelope: &EventEnvelope, reason: &str) {
    let _ = state.publish_without_audit(
        NewEvent::new(EventKind::NeedsAttention, "host")
            .with_ticket(envelope.ticket_id.clone().unwrap_or_default())
            .with_payload(serde_json::json!({
                "reason": "kron_audit_write_failed",
                "detail": reason,
                "event_id": envelope.id,
                "kind": envelope.kind.as_str(),
            })),
    );
}

/// `ledger` 程序的注入点（测试传 fake 替身；生产用 PATH 上的 `ledger`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerProgram {
    path: PathBuf,
    args: Vec<String>,
}

impl LedgerProgram {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            args: RECORD_ARGS.iter().map(|s| s.to_string()).collect(),
        }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn args(&self) -> &[String] {
        &self.args
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_sink_is_disabled_and_never_spawns_a_subprocess() {
        let sink = AuditSink::default();
        assert!(!sink.is_enabled());
        assert!(!sink.actor_farm_accepted(), "跨仓扩位未落地前恒为 false");
        assert_eq!(sink.batch_size(), DEFAULT_BATCH_SIZE);
    }

    #[test]
    fn the_sink_carries_the_injected_program_and_the_actor_gate() {
        let sink = AuditSink::new("/fake/ledger")
            .with_batch_size(7)
            .with_actor_farm_accepted(true);
        assert!(sink.is_enabled());
        assert!(sink.actor_farm_accepted());
        assert_eq!(sink.program(), "/fake/ledger");
        assert_eq!(sink.batch_size(), 7);
        assert_eq!(AuditSink::new("ledger").with_batch_size(0).batch_size(), 1);
    }

    #[test]
    fn only_a_posted_outcome_counts_as_audited() {
        assert!(AuditOutcome::Posted { duplicate: false }.is_posted());
        assert!(AuditOutcome::Posted { duplicate: true }.is_posted());
        assert!(!AuditOutcome::Skipped.is_posted());
        assert!(!AuditOutcome::Unposted { reason: "x".into() }.is_posted());
        assert!(!AuditOutcome::Failed { reason: "x".into() }.is_posted());
    }
}
