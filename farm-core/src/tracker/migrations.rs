//! tracker 服务域的 migration（ticket #13）——**按五服务分域**的先例。
//!
//! ADR-0005 §5：表按五服务分域，SQLite 是宿主私有实现细节。因此**服务域的 SQL
//! 住在服务自己的模块里**（本文件），`crate::migrations::MIGRATIONS` 只做版本登记
//! （`up: tracker::migrations::v3_...`），不放任何服务域 SQL。兄弟票（#14/#15/#18…）
//! 跟随同一先例：各自 `farm-core/src/<service>/migrations.rs`，版本号在集成分支上
//! 协调分配（#11 = v1 `host_meta`；#13 = v3 tracker；v2 仍空置）。

use rusqlite::Connection;

/// v3：票快照 + blocking 边 + 票状态历史（ADR-0005 §2 持久项）。
///
/// 三条表一套：`tracker_issues` 是归一化票快照（含 claim 的 assignee 列），
/// `tracker_blocking_edges` 是原生依赖边（blocker → blocked，一条边一行），
/// `tracker_issue_state_history` 是状态变迁史（只追加）。
pub fn v3_tracker_issue_snapshot(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS tracker_issues (
             issue_id    TEXT PRIMARY KEY,
             title       TEXT    NOT NULL,
             state       TEXT    NOT NULL,
             assignee    TEXT,
             labels      TEXT    NOT NULL DEFAULT '[]',
             provider    TEXT    NOT NULL,
             updated_at  TEXT    NOT NULL
         );

         CREATE TABLE IF NOT EXISTS tracker_blocking_edges (
             blocker_id  TEXT    NOT NULL,
             blocked_id  TEXT    NOT NULL,
             PRIMARY KEY (blocker_id, blocked_id)
         );

         CREATE INDEX IF NOT EXISTS tracker_blocking_edges_blocked
             ON tracker_blocking_edges (blocked_id);

         CREATE TABLE IF NOT EXISTS tracker_issue_state_history (
             seq          INTEGER PRIMARY KEY AUTOINCREMENT,
             issue_id     TEXT    NOT NULL,
             from_state   TEXT,
             to_state     TEXT    NOT NULL,
             assignee     TEXT,
             actor        TEXT    NOT NULL,
             occurred_at  TEXT    NOT NULL,
             note         TEXT    NOT NULL DEFAULT ''
         );

         CREATE INDEX IF NOT EXISTS tracker_issue_state_history_issue
             ON tracker_issue_state_history (issue_id, seq);",
    )
}
