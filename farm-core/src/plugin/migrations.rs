//! 插件服务域的 migration（ticket #19）——**按五服务分域**（ADR-0005 §5）。
//!
//! 先例由 #13（tracker）/ #16（agent-runner）/ #18（merge）建立：服务域的 SQL 住在服务自己的
//! `farm-core/src/<service>/migrations.rs`，`crate::migrations::MIGRATIONS` 只做版本登记
//! （`up: plugin::migrations::v8_plugin_state`）。
//!
//! 版本分配（集成分支上协调）：#11 = v1 host_meta；#12 workflow LKG = v2；
//! #13 tracker = v3；#14 workspace 映射 = v4；#15 事件日志 = v5；#16 attempts = v6；
//! #18 merge 提议 = v7；**#19 插件监督状态 = v8**。只准追加。

use rusqlite::Connection;

use farm_sdk::admin::PluginState;

use super::store::PLUGIN_TABLE;

/// v8：插件监督状态表（GLOSSARY「隔离 quarantined」的持久面）。
///
/// 一行 = 一个被宿主监督的插件进程：契约面状态（[`PluginState`] 三态闭集）+ 宿主私有的
/// 重试账面（`restarts` 分级退避计数、`next_restart_at` 退避窗口、`last_error` 崩溃原因、
/// `pid` 当前子进程）。`restarts` 与隔离态是**持久项**：宿主重启后不清零、不悄悄自愈
/// （ADR-0004 决策 4：恢复是人的裁决）。
///
/// `state` 列的 `CHECK` 由 [`PluginState::ALL`] **生成**——闭集的单一源仍在 farm-sdk
/// （ADR-0001 决策 6），SQL 只是第二道闸。schema 是宿主私有实现细节，永不外泄为契约
/// （ADR-0005 §5）：面板读它经宿主管理 API（#23）的投影，插件自身不直连 DB。
pub fn v8_plugin_state(conn: &Connection) -> rusqlite::Result<()> {
    let states = PluginState::ALL
        .iter()
        .map(|state| format!("'{}'", state.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS {table} (
             plugin_id        TEXT PRIMARY KEY,
             name             TEXT    NOT NULL,
             program          TEXT    NOT NULL,
             state            TEXT    NOT NULL CHECK (state IN ({states})),
             restarts         INTEGER NOT NULL DEFAULT 0,
             pid              INTEGER,
             last_error       TEXT,
             next_restart_at  TEXT,
             updated_at       TEXT    NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_plugin_state_state
             ON {table}(state);",
        table = PLUGIN_TABLE
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[test]
    fn the_migration_creates_the_plugin_state_table_and_the_check_accepts_only_the_closed_set() {
        let db = Database::open_in_memory().expect("in-memory store");
        db.with_conn(|conn| {
            v8_plugin_state(conn)?;
            // 闭集内的三个状态都能写。
            for state in PluginState::ALL {
                conn.execute(
                    "INSERT INTO plugin_state
                        (plugin_id, name, program, state, restarts, updated_at)
                     VALUES (?1, 'notifier', '/bin/true', ?2, 0, 'now')",
                    rusqlite::params![format!("pl-{}", state.as_str()), state.as_str()],
                )?;
            }
            // 闭集外的字符串在存储层就进不来。
            let rejected = conn.execute(
                "INSERT INTO plugin_state
                    (plugin_id, name, program, state, restarts, updated_at)
                 VALUES ('pl-x', 'notifier', '/bin/true', 'dead', 0, 'now')",
                [],
            );
            assert!(rejected.is_err(), "state 列的 CHECK 必须挡住闭集外的值");
            Ok(())
        })
        .expect("migration + inserts");
    }
}
