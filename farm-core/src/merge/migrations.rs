//! merge 服务域的 migration（ticket #18）——**按五服务分域**（ADR-0005 §5）。
//!
//! 先例由 #13（tracker）/ #16（agent-runner）建立：服务域的 SQL 住在服务自己的
//! `farm-core/src/<service>/migrations.rs`，`crate::migrations::MIGRATIONS` 只做
//! 版本登记（`up: merge::migrations::v7_merge_proposal`）。
//!
//! 版本分配（集成分支上协调）：#11 = v1 host_meta；#12 workflow LKG = v2；
//! #13 tracker = v3；#14 workspace 映射 = v4；#15 事件日志 = v5；#16 attempts = v6；
//! **#18 merge 提议 = v7**。

use rusqlite::Connection;

use farm_sdk::services::merge::MergeProposalState;

use super::store::{parse_state, state_to_str};

/// v7：merge 提议表（agent 只能提议；宿主校验后执行。GLOSSARY「Merge 服务」）。
///
/// 一行 = 一次提议的完整轨迹：`proposed`（write-ahead 先落）→ `validated`
/// （机械闸门过）→ `merged`（merge commit 已产生）/ `rejected`（拒收 + 闭集错误码）。
/// 拒收行**不删**：它是 #25 审阅队列与审计轨迹的读源（ADR-0005 §3 merge 是关键事件）。
///
/// `state` 列的 `CHECK` 由 [`MergeProposalState::ALL`] **生成**——闭集的单一源仍在
/// farm-sdk（ADR-0001 决策 6），SQL 只是第二道闸。schema 是宿主私有实现细节，
/// 永不外泄为契约（ADR-0005 §5）。
pub fn v7_merge_proposal(conn: &Connection) -> rusqlite::Result<()> {
    let states = MergeProposalState::ALL
        .iter()
        .map(|state| format!("'{}'", state_to_str(*state)))
        .collect::<Vec<_>>()
        .join(", ");
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS merge_proposal (
             seq                INTEGER PRIMARY KEY AUTOINCREMENT,
             proposal_id        TEXT    NOT NULL UNIQUE,
             ticket_id          TEXT    NOT NULL,
             worktree_branch    TEXT    NOT NULL,
             integration_branch TEXT    NOT NULL,
             repo_path          TEXT    NOT NULL,
             worktree_path      TEXT    NOT NULL,
             claimed_commit     TEXT,
             message            TEXT    NOT NULL DEFAULT '',
             state              TEXT    NOT NULL CHECK (state IN ({states})),
             rejection_stage    TEXT,
             rejection_code     TEXT,
             rejection_detail   TEXT,
             merge_commit       TEXT,
             projection         TEXT    NOT NULL DEFAULT '[]',
             created_at         TEXT    NOT NULL,
             updated_at         TEXT    NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_merge_proposal_ticket
             ON merge_proposal(ticket_id, seq);
         CREATE INDEX IF NOT EXISTS idx_merge_proposal_state
             ON merge_proposal(state);"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[test]
    fn the_migration_creates_the_proposal_table_and_the_check_accepts_only_the_closed_set() {
        let db = Database::open_in_memory().expect("in-memory store");
        db.with_conn(|conn| {
            v7_merge_proposal(conn)?;
            // 闭集内的四个状态都能写。
            for state in MergeProposalState::ALL {
                conn.execute(
                    "INSERT INTO merge_proposal
                        (proposal_id, ticket_id, worktree_branch, integration_branch, repo_path,
                         worktree_path, state, created_at, updated_at)
                     VALUES (?1, '18', 'farm/t18', 'implement/spec-10', '/repo', '/wt', ?2, 'now', 'now')",
                    rusqlite::params![format!("mrg-{}", state_to_str(state)), state_to_str(state)],
                )?;
            }
            // 闭集外的字符串在存储层就进不来。
            let rejected = conn.execute(
                "INSERT INTO merge_proposal
                    (proposal_id, ticket_id, worktree_branch, integration_branch, repo_path,
                     worktree_path, state, created_at, updated_at)
                 VALUES ('mrg-x', '18', 'b', 'i', '/r', '/w', 'done', 'now', 'now')",
                [],
            );
            assert!(rejected.is_err(), "state 列的 CHECK 必须挡住闭集外的值");
            Ok(())
        })
        .expect("migration + inserts");

        // 落库的文本形态能解析回闭集成员。
        for state in MergeProposalState::ALL {
            assert_eq!(parse_state(state_to_str(state)), Some(state));
        }
        assert_eq!(parse_state("done"), None);
    }
}
