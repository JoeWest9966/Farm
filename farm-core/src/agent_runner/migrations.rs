//! agent-runner 服务域的 migration（ticket #16）——**按五服务分域**（先例由 #13 建立）。
//!
//! ADR-0005 §5：表按五服务分域，SQLite 是宿主私有实现细节。因此服务域的 SQL 住在
//! 服务自己的模块里（本文件），`crate::migrations::MIGRATIONS` 只做版本登记
//! （`up: agent_runner::migrations::v6_...`）。版本号在集成分支上协调分配：
//! #11 = v1 `host_meta`；#12 = v2 workflow LKG；#13 = v3 tracker；#14 = v4 workspace；
//! #15 = v5 事件日志；**#16 = v6 attempts**。

use rusqlite::Connection;

/// attempts 表名（表名的单一源；store 与 migration 共用）。
pub const ATTEMPTS_TABLE: &str = "agent_runner_attempts";

/// v6：attempts 时间线（GLOSSARY「attempt 终态」四态 + token 快照 + 记账截断标记）。
///
/// - `state` 闭集 `running / succeeded / failed / interrupted`：
///   - `running`：判据阶梯还在走；
///   - `succeeded`：完成双条件判终态；
///   - `failed`：阶梯判杀（两口钟 / abort 探针）或 abort 收尾；
///   - `interrupted`：**第四终态**（GLOSSARY）：宿主重启时所有 `running` 被标记的记账状态，
///     非成功非失败，worktree 不清，是否重派由编排器裁决，宿主不自作主张。
/// - `tokens_json`：最后一份 token 快照（`accounting::TokenSnapshot` 的 JSON）。
/// - `accounting_truncated`：记账被截断（中断态以最后快照为准并标此位）。
/// - `CHECK` 约束是第二道闸（自由字符串在存储层就进不来）；权威闭集判据在
///   [`super::store::ATTEMPT_STATES`]。
pub fn v6_agent_runner_attempts(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS {table} (
             attempt_id            TEXT PRIMARY KEY,
             ticket_id             TEXT    NOT NULL,
             state                 TEXT    NOT NULL CHECK (state IN ('running', 'succeeded', 'failed', 'interrupted')),
             profile               TEXT,
             session_id            TEXT,
             pid                   INTEGER,
             tokens_json           TEXT    NOT NULL DEFAULT '{{}}',
             accounting_truncated  INTEGER NOT NULL DEFAULT 0,
             disposition           TEXT,
             note                  TEXT,
             created_at            TEXT    NOT NULL,
             updated_at            TEXT    NOT NULL
         );

         CREATE INDEX IF NOT EXISTS idx_agent_runner_attempts_ticket
             ON {table} (ticket_id, created_at);

         CREATE INDEX IF NOT EXISTS idx_agent_runner_attempts_state
             ON {table} (state);",
        table = ATTEMPTS_TABLE
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::migrations::{Migrator, MIGRATIONS};

    #[test]
    fn v6_creates_the_attempts_table_with_the_four_terminal_states() {
        let db = Database::open_in_memory().expect("memory db");
        db.migrate(&Migrator::new(MIGRATIONS)).expect("migrate");

        let states: Vec<String> = db
            .with_conn(|conn| {
                let mut stmt = conn.prepare(&format!(
                    "SELECT DISTINCT state FROM {ATTEMPTS_TABLE}"
                ))?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
            })
            .expect("query states");
        assert!(states.is_empty(), "新表还没有行");

        // CHECK 是第二道闸：四态之外的字符串在存储层就进不来。
        let rejected = db.with_conn(|conn| {
            conn.execute(
                &format!("INSERT INTO {ATTEMPTS_TABLE} (attempt_id, ticket_id, state, created_at, updated_at) VALUES ('a','1','aborted','t','t')"),
                [],
            )
        });
        assert!(rejected.is_err(), "state 闭集由 CHECK 约束把门");
    }

    #[test]
    fn the_migration_set_registers_v6_for_the_agent_runner_domain() {
        let v6 = MIGRATIONS
            .iter()
            .find(|migration| migration.version == 6)
            .expect("v6 must be registered");
        assert_eq!(v6.name, "agent_runner_attempts");
        assert_eq!(
            v6.up as usize, v6_agent_runner_attempts as usize,
            "登记表的 up 必须指向本文件的 SQL"
        );
    }
}
