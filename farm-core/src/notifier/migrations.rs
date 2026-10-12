//! 通知通道配置服务域的 migration（ticket #21）——**按五服务分域**（ADR-0005 §5）。
//!
//! 先例由 #13/#16/#18/#19/#24 建立：服务域的 SQL 住在服务自己的
//! `farm-core/src/<service>/migrations.rs`，`crate::migrations::MIGRATIONS` 只做版本登记
//! （`up: notifier::migrations::v10_notifier_channel_config`）。版本号在集成分支上协调分配：
//! #11=v1 host_meta … #24=v9 覆盖层；**#21 = v10 通知通道配置**。只准追加。
//!
//! ## 宿主零推送域知识
//! 本表只存「开不开（`enabled`）+ 转哪些关键事件（四个 `notify_*` 开关）」，**没有一个**
//! url / header / body 模板 / 重试 / 限流列——那些是插件侧自决载体（GLOSSARY「Notifier 插件」）。
//! 与覆盖层的 `temporary` 列同一纪律：默认过滤集做成**列 + CHECK**，「没标记的开关」在存储层
//! 就写不进来（0/1 之外的值进不来）。

use rusqlite::Connection;

/// 通知通道配置表名（表名的单一源；migration 与 store 共用）。
pub const NOTIFIER_TABLE: &str = "notifier_channel_config";

/// v10：通知通道配置表（GLOSSARY「通知通道」的持久面；per-仓库 keyed，ADR-0005 §6）。
///
/// - 主键 `repo_id`：一仓库一行（`INSERT .. ON CONFLICT(repo_id)` 即覆盖）。
/// - `enabled`：通道总闸；四个 `notify_*`：过滤集（对应 [`farm_sdk::notifier::NotifierFilter`]
///   的四个开关）。全部 `CHECK (x IN (0,1))`——默认过滤集在存储层就有第二道闸。
/// - schema 是宿主私有实现细节，永不外泄为契约（ADR-0005 §5）：读只经宿主管理 API 投影。
pub fn v10_notifier_channel_config(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS {table} (
             repo_id                 TEXT    NOT NULL PRIMARY KEY,
             enabled                 INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
             notify_blocked          INTEGER NOT NULL DEFAULT 1 CHECK (notify_blocked IN (0,1)),
             notify_needs_attention  INTEGER NOT NULL DEFAULT 1 CHECK (notify_needs_attention IN (0,1)),
             notify_stall            INTEGER NOT NULL DEFAULT 1 CHECK (notify_stall IN (0,1)),
             notify_merge_complete   INTEGER NOT NULL DEFAULT 0 CHECK (notify_merge_complete IN (0,1)),
             updated_at              TEXT    NOT NULL
         );",
        table = NOTIFIER_TABLE
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::migrations::{Migrator, MIGRATIONS};

    #[test]
    fn v10_creates_the_notifier_channel_config_table_per_repo_keyed() {
        let db = Database::open_in_memory().expect("memory db");
        db.migrate(&Migrator::new(MIGRATIONS)).expect("migrate");
        let columns: Vec<String> = db
            .with_conn(|conn| {
                let mut stmt = conn.prepare(&format!("PRAGMA table_info({NOTIFIER_TABLE})"))?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
            })
            .expect("columns");
        for want in [
            "repo_id",
            "enabled",
            "notify_blocked",
            "notify_needs_attention",
            "notify_stall",
            "notify_merge_complete",
            "updated_at",
        ] {
            assert!(columns.contains(&want.to_string()), "缺列 {want}：{columns:?}");
        }
    }

    #[test]
    fn the_notifier_table_has_no_push_domain_columns() {
        // 宿主零推送域知识：表里没有 url/header/模板/重试/限流列（载体全住插件侧）。
        let db = Database::open_in_memory().expect("memory db");
        db.migrate(&Migrator::new(MIGRATIONS)).expect("migrate");
        let columns: Vec<String> = db
            .with_conn(|conn| {
                let mut stmt = conn.prepare(&format!("PRAGMA table_info({NOTIFIER_TABLE})"))?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
            })
            .expect("columns");
        for forbidden in ["url", "header", "template", "retry", "rate_limit", "endpoint", "body"] {
            assert!(
                !columns.iter().any(|c| c.contains(forbidden)),
                "通知通道配置不得含推送域列 {forbidden}：{columns:?}"
            );
        }
    }

    #[test]
    fn the_switch_columns_accept_only_zero_or_one() {
        let db = Database::open_in_memory().expect("memory db");
        db.migrate(&Migrator::new(MIGRATIONS)).expect("migrate");
        db.with_conn(|conn| {
            conn.execute(
                &format!(
                    "INSERT INTO {NOTIFIER_TABLE} (repo_id, enabled, updated_at)
                     VALUES ('repo-a', 1, 'now')"
                ),
                [],
            )?;
            // 0/1 之外的开关值在存储层就进不来（默认过滤集的第二道闸）。
            let rejected = conn.execute(
                &format!(
                    "INSERT INTO {NOTIFIER_TABLE} (repo_id, notify_blocked, updated_at)
                     VALUES ('repo-b', 7, 'now')"
                ),
                [],
            );
            assert!(rejected.is_err(), "notify_* 列的 CHECK 必须挡住 0/1 之外的值");
            Ok(())
        })
        .expect("inserts");
    }
}
