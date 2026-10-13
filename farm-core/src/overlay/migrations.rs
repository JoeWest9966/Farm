//! 覆盖层服务域的 migration（ticket #24）——**按五服务分域**（先例由 #13 建立）。
//!
//! ADR-0005 §5：表按五服务分域，SQLite 是宿主私有实现细节。因此服务域的 SQL 住在
//! 服务自己的模块里（本文件），`crate::migrations::MIGRATIONS` 只做版本登记
//! （`up: overlay::migrations::v9_...`）。版本号在集成分支上协调分配：
//! #11 = v1 `host_meta`；#12 = v2 workflow LKG；#13 = v3 tracker；#14 = v4 workspace；
//! #15 = v5 事件日志；#16 = v6 attempts；#18 = v7 merge 提议；**#24 = v9 覆盖层**
//! （v8 留给 #19/#20/#23 之间的协调空档，本票不占）。
//!
//! ## 为什么 `temporary` 是列而不是约定
//! ADR-0002 §2：覆盖层必须**显式标记**「这是临时覆盖」。标记写成代码注释或 UI 文案
//! 都只是暗示；做成列 + `CHECK (temporary IN (0,1))` 之后，「没标记的覆盖层」在存储层
//! 就写不进来——与 attempts 的 `state` 闭集、事件的 `kind` 闭集同一道纪律。

use rusqlite::Connection;

use farm_sdk::overlay::OverlayKey;

/// 覆盖层表名（表名的单一源；migration 与 store 共用）。
pub const OVERLAY_TABLE: &str = "workflow_overlay";

/// v9：覆盖层表（ADR-0005 §6：落宿主私有 SQLite，per-仓库 keyed，不落被编排仓库）。
///
/// - 主键 `(repo_id, key)`：一仓库一格，同键再写即覆盖（`INSERT .. ON CONFLICT`）。
/// - `key` 的 `CHECK` 由 [`OverlayKey::ALL`] **生成**（闭集单一源在 farm-sdk，
///   SQL 只是第二道闸：自由键在存储层就进不来）。
/// - `value_json`：该格的运行时值（类型由键决定，校验见 `farm_sdk::overlay::validate_value`）。
/// - `note`：人读理由（谁为什么临时改这一格）。
/// - `temporary`：临时标记，`NOT NULL DEFAULT 1 CHECK (temporary = 1)`——**只有 1 写得进来**，
///   「没标记的覆盖层」在存储层就存在不了（见模块文档）。
pub fn v9_workflow_overlay(conn: &Connection) -> rusqlite::Result<()> {
    let keys = OverlayKey::ALL
        .iter()
        .map(|key| format!("'{}'", key.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS {table} (
             repo_id     TEXT    NOT NULL,
             key         TEXT    NOT NULL CHECK (key IN ({keys})),
             value_json  TEXT    NOT NULL,
             note        TEXT,
             temporary   INTEGER NOT NULL DEFAULT 1 CHECK (temporary = 1),
             updated_at  TEXT    NOT NULL,
             PRIMARY KEY (repo_id, key)
         );

         CREATE INDEX IF NOT EXISTS idx_workflow_overlay_repo
             ON {table} (repo_id, key);",
        table = OVERLAY_TABLE
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::migrations::{Migrator, MIGRATIONS};

    #[test]
    fn v9_creates_the_overlay_table_per_repo_keyed() {
        let db = Database::open_in_memory().expect("memory db");
        db.migrate(&Migrator::new(MIGRATIONS)).expect("migrate");

        let columns: Vec<String> = db
            .with_conn(|conn| {
                let mut stmt = conn.prepare(&format!("PRAGMA table_info({OVERLAY_TABLE})"))?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
            })
            .expect("columns");
        for want in ["repo_id", "key", "value_json", "note", "temporary", "updated_at"] {
            assert!(columns.contains(&want.to_string()), "缺列 {want}：{columns:?}");
        }
    }

    #[test]
    fn the_overlay_table_has_no_foreign_key_into_the_orchestrated_repo() {
        // 覆盖层只住宿主库（ADR-0005 §6）：表定义里不引用任何被编排仓库的对象。
        let db = Database::open_in_memory().expect("memory db");
        db.migrate(&Migrator::new(MIGRATIONS)).expect("migrate");
        let sql: String = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
                    [OVERLAY_TABLE],
                    |row| row.get(0),
                )
            })
            .expect("table sql");
        assert!(!sql.contains("REFERENCES"), "不引用外表：{sql}");
    }
}
