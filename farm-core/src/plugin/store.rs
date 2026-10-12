//! 插件监督状态表（ticket #19，migration v8 `plugin_state`）。
//!
//! schema 是宿主私有实现细节（ADR-0005 §5）：面板与插件都不直连，一切读经宿主管理 API
//! （#23）投影。本模块只被 [`super::PluginHost`] 与宿主自己的启动对账用。

use std::sync::Arc;

use rusqlite::{OptionalExtension, Row};

use crate::db::Database;
use crate::error::CoreError;
use farm_sdk::admin::PluginState;

use super::PluginStatus;

/// 插件监督状态表名（migration v8；单一源在本模块）。
pub const PLUGIN_TABLE: &str = "plugin_state";

/// 插件状态行的宿主私有存储（`Clone` 廉价：内部共享同一个句柄）。
#[derive(Clone, Debug)]
pub struct PluginStore {
    db: Arc<Database>,
}

impl PluginStore {
    pub fn new(db: &Database) -> Self {
        Self {
            db: Arc::new(db.clone()),
        }
    }

    /// 底层存储句柄（宿主内部用；schema 不外泄，ADR-0005 §5）。
    pub fn database(&self) -> &Database {
        &self.db
    }

    /// write-ahead 落一行（launch 先落账再 spawn：崩在 spawn 半路也有账可查）。
    pub fn upsert(&self, status: &PluginStatus) -> Result<(), CoreError> {
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO plugin_state
                    (plugin_id, name, program, state, restarts, pid, last_error,
                     next_restart_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(plugin_id) DO UPDATE SET
                    name = excluded.name,
                    program = excluded.program,
                    state = excluded.state,
                    restarts = excluded.restarts,
                    pid = excluded.pid,
                    last_error = excluded.last_error,
                    next_restart_at = excluded.next_restart_at,
                    updated_at = excluded.updated_at",
                rusqlite::params![
                    status.plugin_id,
                    status.name,
                    status.program,
                    status.state.as_str(),
                    status.restarts,
                    status.pid,
                    status.last_error,
                    status.next_restart_at,
                    status.updated_at,
                ],
            )?;
            Ok(())
        })
    }

    /// 单行插件状态（不存在 → `None`）。
    pub fn get(&self, plugin_id: &str) -> Result<Option<PluginStatus>, CoreError> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT plugin_id, name, program, state, restarts, pid, last_error,
                        next_restart_at, updated_at
                 FROM plugin_state WHERE plugin_id = ?1",
                rusqlite::params![plugin_id],
                row_to_status,
            )
            .optional()
        })
    }

    /// 全部插件状态（按 plugin_id 排序，面板读源稳定）。
    pub fn list(&self) -> Result<Vec<PluginStatus>, CoreError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT plugin_id, name, program, state, restarts, pid, last_error,
                        next_restart_at, updated_at
                 FROM plugin_state ORDER BY plugin_id",
            )?;
            let rows = stmt.query_map([], row_to_status)?;
            let mut statuses = Vec::new();
            for row in rows {
                statuses.push(row?);
            }
            Ok(statuses)
        })
    }

    /// 改契约面状态（`false` = 这个插件没落过账，不偷偷建行）。
    pub fn set_state(
        &self,
        plugin_id: &str,
        state: PluginState,
        at: &str,
    ) -> Result<bool, CoreError> {
        Ok(self
            .db
            .with_conn(|conn| {
                conn.execute(
                    "UPDATE plugin_state SET state = ?2, updated_at = ?3 WHERE plugin_id = ?1",
                    rusqlite::params![plugin_id, state.as_str(), at],
                )
            })? > 0)
    }

    /// 记一次崩溃：`restarts + 1` + 崩溃原因 + 清掉 pid。回新的计数。
    pub fn note_crash(&self, plugin_id: &str, error: &str, at: &str) -> Result<u32, CoreError> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE plugin_state
                 SET restarts = restarts + 1, pid = NULL, last_error = ?2, updated_at = ?3
                 WHERE plugin_id = ?1",
                rusqlite::params![plugin_id, error, at],
            )?;
            Ok(conn.query_row(
                "SELECT restarts FROM plugin_state WHERE plugin_id = ?1",
                rusqlite::params![plugin_id],
                |row| row.get::<_, u32>(0),
            )?)
        })
    }

    /// 恢复 / 自愈成功：重试预算归零、崩溃原因清掉、记上新子进程 pid。
    pub fn reset(&self, plugin_id: &str, pid: Option<u32>, at: &str) -> Result<(), CoreError> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE plugin_state
                 SET state = 'running', restarts = 0, pid = ?2, last_error = NULL,
                     next_restart_at = NULL, updated_at = ?3
                 WHERE plugin_id = ?1",
                rusqlite::params![plugin_id, pid, at],
            )?;
            Ok(())
        })
    }

    /// 记 / 清退避窗口（`None` = 没有待定的重启）。
    pub fn set_next_restart(
        &self,
        plugin_id: &str,
        next_restart_at: Option<&str>,
        at: &str,
    ) -> Result<(), CoreError> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE plugin_state
                 SET next_restart_at = ?2, updated_at = ?3 WHERE plugin_id = ?1",
                rusqlite::params![plugin_id, next_restart_at, at],
            )?;
            Ok(())
        })
    }
}

fn row_to_status(row: &Row<'_>) -> rusqlite::Result<PluginStatus> {
    Ok(PluginStatus {
        plugin_id: row.get("plugin_id")?,
        name: row.get("name")?,
        program: row.get("program")?,
        state: parse_state(&row.get::<_, String>("state")?).unwrap_or(PluginState::Stopped),
        restarts: row.get("restarts")?,
        pid: row.get("pid")?,
        last_error: row.get("last_error")?,
        next_restart_at: row.get("next_restart_at")?,
        updated_at: row.get("updated_at")?,
    })
}

/// 存储文本 → 闭集成员（认不出的状态按 `stopped`：宿主绝不把来路不明的东西当 running）。
pub fn parse_state(text: &str) -> Option<PluginState> {
    PluginState::ALL
        .into_iter()
        .find(|state| state.as_str() == text)
}
