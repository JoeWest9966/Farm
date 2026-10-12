//! 版本化顺序 migration runner（ADR-0005 §7）。
//!
//! 全局单库、顺序 migration：版本号严格升序，已落地的跳过，未落地的一次一条事务。
//!
//! 组织（按五服务分域，ADR-0005 §5；先例由 #13 建立）：服务域表的 SQL 住在各服务
//! 自己的 `farm-core/src/<service>/migrations.rs`，本文件只是**版本登记表**
//! （`up: tracker::migrations::v3_...`）。早期票（#12/#14/#15）曾内联其 SQL，保留原样。
//!
//! 版本分配（集成分支上协调）：#11 = v1 host_meta；#12 workflow LKG = v2；
//! #13 tracker 票快照/状态历史 = v3；#14 workspace 映射 = v4；#15 事件日志 = v5；
//! #16 attempts = v6；#18 merge 提议 = v7。只准追加、不准改历史版本；合并时按版本升序重排。

use rusqlite::Connection;

use crate::error::CoreError;

/// 一条 migration。`up` 在一个事务里执行，失败整体回滚。
#[derive(Clone)]
pub struct Migration {
    pub version: u32,
    pub name: &'static str,
    pub up: fn(&Connection) -> rusqlite::Result<()>,
}

impl std::fmt::Debug for Migration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Migration")
            .field("version", &self.version)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// migration 集合 + 顺序规则。
pub struct Migrator<'a> {
    migrations: &'a [Migration],
}

impl<'a> Migrator<'a> {
    pub fn new(migrations: &'a [Migration]) -> Self {
        Self { migrations }
    }

    pub fn migrations(&self) -> &'a [Migration] {
        self.migrations
    }

    /// 版本必须严格升序（顺序 migration 的硬规则）。
    pub fn validate(&self) -> Result<(), CoreError> {
        if self.migrations.is_empty() {
            return Err(CoreError::InvalidMigrations("migration set is empty".into()));
        }
        for pair in self.migrations.windows(2) {
            if pair[1].version <= pair[0].version {
                return Err(CoreError::InvalidMigrations(format!(
                    "versions must strictly increase: v{} ({}) then v{} ({})",
                    pair[0].version, pair[0].name, pair[1].version, pair[1].name
                )));
            }
        }
        Ok(())
    }
}

fn v1_host_meta(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS host_meta (
             key    TEXT PRIMARY KEY,
             value  TEXT NOT NULL
         );",
    )
}

/// v2（ticket #12）：workflow 配置 last-known-good 缓存，**per-仓库 keyed**
///（ADR-0005 §2/§6：LKG 是持久项，落宿主私有 SQLite，不落被编排仓库）。
/// 表名单一源在 [`crate::workflow::lkg::LKG_TABLE`]。
fn v2_workflow_config_lkg(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS {table} (
             repo_id     TEXT PRIMARY KEY,
             config_json TEXT NOT NULL,
             updated_at  TEXT NOT NULL
         );",
        table = crate::workflow::lkg::LKG_TABLE
    ))
}

/// Workspace 服务映射表（ticket #14，ADR-0005 §9）：spec ↔ 集成分支 ↔ worktree 映射。
///
/// write-ahead：`reserve` 先落一行 `status='pending'`（映射在、worktree 未必建），
/// 建好 worktree 再转 `built`；启动对账发现目录缺/损则重建或标 `broken`。
/// 一票一个 worktree，故 `ticket_id` 为主键；`worktree_branch` 全局唯一（加唯一索引）。
/// schema 是宿主私有实现细节，永不外泄为契约（ADR-0005 §5）。
fn v4_workspace_mapping(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS workspace_mapping (
             ticket_id          TEXT PRIMARY KEY,
             spec_id            TEXT NOT NULL,
             integration_branch TEXT NOT NULL,
             repo_path          TEXT NOT NULL,
             worktree_path      TEXT NOT NULL,
             worktree_branch    TEXT NOT NULL,
             start_point        TEXT,
             status             TEXT NOT NULL,
             created_at         TEXT NOT NULL,
             updated_at         TEXT NOT NULL
         );
         CREATE UNIQUE INDEX IF NOT EXISTS idx_workspace_mapping_branch
             ON workspace_mapping(worktree_branch);
         CREATE INDEX IF NOT EXISTS idx_workspace_mapping_status
             ON workspace_mapping(status);",
    )
}

/// ticket #15：事件日志表 + 有界保留窗口所需的索引。
///
/// ADR-0005 §2/§10：**可过滤事件日志**落宿主私有存储，`events` 表**有界**（保留窗口，
/// 超限归档进 Kron 冷层归 #20）。`seq` 用 `AUTOINCREMENT`：删过最旧的行之后 id 也不复用，
/// 所以它是持久的次序游标；`id` 是总线盖章的事件 id，全局唯一，作去重键。
///
/// `kind` 列的 `CHECK` 约束由 [`farm_sdk::events::EventKind::ALL`] **生成**——闭集的单一源
/// 仍在 farm-sdk（ADR-0001 决策 6），SQL 只是第二道闸：自由字符串在存储层就进不来。
fn v5_event_log(conn: &Connection) -> rusqlite::Result<()> {
    let kinds = farm_sdk::events::EventKind::ALL
        .iter()
        .map(|k| format!("'{}'", k.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS events (
             seq          INTEGER PRIMARY KEY AUTOINCREMENT,
             id           TEXT    NOT NULL UNIQUE,
             kind         TEXT    NOT NULL CHECK (kind IN ({kinds})),
             occurred_at  TEXT    NOT NULL,
             actor        TEXT    NOT NULL,
             ticket_id    TEXT,
             attempt_id   TEXT,
             payload      TEXT    NOT NULL,
             audited      INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS idx_events_kind        ON events(kind);
         CREATE INDEX IF NOT EXISTS idx_events_ticket_id   ON events(ticket_id);
         CREATE INDEX IF NOT EXISTS idx_events_occurred_at ON events(occurred_at);
         CREATE INDEX IF NOT EXISTS idx_events_audited     ON events(audited);"
    ))
}

/// 骨架 migration 集：runner 自己的工作表 + 宿主级 key/value 表 + 各服务域表。
///
/// v3 的 SQL 住 [`crate::tracker::migrations`]（按五服务分域）；v7 的 SQL 住
/// [`crate::merge::migrations`]（#18 跟随同一先例）；v8 的 SQL 住
/// [`crate::plugin::migrations`]（#19 跟随同一先例）；v2/v4/v5 为早期内联，保留原样。
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "host_meta",
        up: v1_host_meta,
    },
    Migration {
        version: 2,
        name: "workflow_config_lkg",
        up: v2_workflow_config_lkg,
    },
    Migration {
        version: 3,
        name: "tracker_issue_snapshot",
        up: crate::tracker::migrations::v3_tracker_issue_snapshot,
    },
    Migration {
        version: 4,
        name: "workspace_mapping",
        up: v4_workspace_mapping,
    },
    Migration {
        version: 5,
        name: "event_log",
        up: v5_event_log,
    },
    Migration {
        version: 6,
        name: "agent_runner_attempts",
        up: crate::agent_runner::migrations::v6_agent_runner_attempts,
    },
    Migration {
        version: 7,
        name: "merge_proposal",
        up: crate::merge::migrations::v7_merge_proposal,
    },
    Migration {
        version: 8,
        name: "plugin_state",
        up: crate::plugin::migrations::v8_plugin_state,
    },
];
