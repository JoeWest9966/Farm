//! 版本化顺序 migration runner（ADR-0005 §7）。
//!
//! 全局单库、顺序 migration：版本号严格升序，已落地的跳过，未落地的一次一条事务。
//!
//! 组织（按五服务分域，ADR-0005 §5；先例由 #13 建立）：服务域表的 SQL 住在各服务
//! 自己的 `farm-core/src/<service>/migrations.rs`，本文件只是**版本登记表**
//! （`up: tracker::migrations::v3_...`）。早期票（#12/#14）曾内联其 SQL，保留原样。
//!
//! 版本分配（集成分支上协调）：#11 = v1 host_meta；#12 workflow LKG = v2；
//! #13 tracker 票快照/状态历史 = v3；#14 workspace 映射 = v4；#15 事件日志 = v5；
//! #16 attempts = v6。只准追加、不准改历史版本；合并时按版本升序重排。

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

/// 骨架 migration 集：runner 自己的工作表 + 宿主级 key/value 表 + 各服务域表。
///
/// v3 的 SQL 住 [`crate::tracker::migrations`]（按五服务分域）；v2/v4 为早期内联，保留原样。
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
];
