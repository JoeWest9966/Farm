//! attempts 的宿主私有存储访问（ticket #16；ADR-0005 §5 schema 不外泄）。
//!
//! 面板与编排器读 attempts 时间线走契约 API，**不直连库**；本模块只服务宿主内部
//! （agent-runner 自己的监督循环、宿主管理面、崩溃恢复对账）。
//!
//! 持久态闭集 `running / succeeded / failed / interrupted` 与契约态
//! [`farm_sdk::services::agent_runner::AttemptState`] 的映射见
//! [`state_to_str`]/[`parse_state`]。

use rusqlite::{Connection, OptionalExtension};

use farm_sdk::services::agent_runner::AttemptState;

use crate::agent_runner::accounting::TokenSnapshot;
use crate::agent_runner::migrations::ATTEMPTS_TABLE;
use crate::db::Database;
use crate::error::CoreError;
use crate::now_rfc3339;

/// 持久态闭集（`state` 列的权威清单；SQL 的 `CHECK` 由它生成语义）。
pub const ATTEMPT_STATES: [&str; 4] = ["running", "succeeded", "failed", "interrupted"];

/// 契约态 → 持久态。
///
/// `Aborted` 落 `failed`：四态闭集里没有 `aborted`（GLOSSARY 只认四种终态），
/// abort 收尾的**原因**进 `note` 列，宿主私有记录仍可区分。
pub fn state_to_str(state: AttemptState) -> &'static str {
    match state {
        AttemptState::Running => "running",
        AttemptState::Settled => "succeeded",
        AttemptState::Failed | AttemptState::Aborted => "failed",
        AttemptState::Interrupted => "interrupted",
    }
}

/// 持久态 → 契约态（未知值 = 数据损坏，返回 `None`）。
pub fn parse_state(text: &str) -> Option<AttemptState> {
    match text {
        "running" => Some(AttemptState::Running),
        "succeeded" => Some(AttemptState::Settled),
        "failed" => Some(AttemptState::Failed),
        "interrupted" => Some(AttemptState::Interrupted),
        _ => None,
    }
}

/// 一条 attempt 行（宿主私有形状；投影到契约归路由层）。
#[derive(Clone, Debug, PartialEq)]
pub struct AttemptRecord {
    pub attempt_id: String,
    pub ticket_id: String,
    pub state: AttemptState,
    /// 派内容物装配标识（profile）。
    pub profile: Option<String>,
    /// pi 会话 id（`get_state` 探测拿到；fork 找回的输入）。
    pub session_id: Option<String>,
    pub pid: Option<u32>,
    /// 最后一份 token 快照（中断态以它为准）。
    pub tokens: TokenSnapshot,
    /// 记账被截断（中断态标记位）。
    pub accounting_truncated: bool,
    /// `prompt/steer/follow_up` 响应给出的 disposition（最后一条）。
    pub disposition: Option<String>,
    /// 判杀原因 / abort 理由 / 版本偏差等人读说明。
    pub note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl AttemptRecord {
    /// 新建一条 `running` attempt（write-ahead：进程还没起来，行已在）。
    pub fn running(
        attempt_id: impl Into<String>,
        ticket_id: impl Into<String>,
        profile: Option<String>,
    ) -> Self {
        let now = now_rfc3339();
        Self {
            attempt_id: attempt_id.into(),
            ticket_id: ticket_id.into(),
            state: AttemptState::Running,
            profile,
            session_id: None,
            pid: None,
            tokens: TokenSnapshot::default(),
            accounting_truncated: false,
            disposition: None,
            note: None,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    /// 收尾：改状态 + 记原因 + 落最后一份快照。
    pub fn finish(&mut self, state: AttemptState, note: impl Into<String>) {
        self.state = state;
        self.note = Some(note.into());
        self.updated_at = now_rfc3339();
    }
}

/// attempts 存储句柄（`Clone` 廉价：内部共享同一个 `Database`）。
#[derive(Clone, Debug)]
pub struct AttemptStore {
    db: Database,
}

impl AttemptStore {
    pub fn new(db: &Database) -> Self {
        Self { db: db.clone() }
    }

    /// 落一条 attempt（insert or replace by attempt_id）。
    pub fn upsert(&self, record: &AttemptRecord) -> Result<(), CoreError> {
        let tokens_json = serde_json::to_string(&record.tokens.to_json())
            .map_err(|e| CoreError::Config(format!("token snapshot must serialize: {e}")))?;
        self.db.with_conn(|conn| {
            conn.execute(
                &format!(
                    "INSERT OR REPLACE INTO {ATTEMPTS_TABLE}
                        (attempt_id, ticket_id, state, profile, session_id, pid,
                         tokens_json, accounting_truncated, disposition, note,
                         created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"
                ),
                rusqlite::params![
                    record.attempt_id,
                    record.ticket_id,
                    state_to_str(record.state),
                    record.profile,
                    record.session_id,
                    record.pid,
                    tokens_json,
                    i64::from(record.accounting_truncated),
                    record.disposition,
                    record.note,
                    record.created_at,
                    record.updated_at,
                ],
            )?;
            Ok(())
        })
    }

    pub fn find(&self, attempt_id: &str) -> Result<Option<AttemptRecord>, CoreError> {
        self.db.with_conn(|conn| {
            select_record(conn, &format!("WHERE attempt_id = ?1"), &[&attempt_id])
        })
    }

    /// 全部 attempts（按创建次序；面板时间线与调试查询用）。
    pub fn list(&self) -> Result<Vec<AttemptRecord>, CoreError> {
        self.db.with_conn(|conn| {
            select_records(conn, "ORDER BY created_at ASC, rowid ASC", &[])
        })
    }

    /// 某票的 attempts（按创建次序）。
    pub fn list_for_ticket(&self, ticket_id: &str) -> Result<Vec<AttemptRecord>, CoreError> {
        self.db.with_conn(|conn| {
            select_records(
                conn,
                "WHERE ticket_id = ?1 ORDER BY created_at ASC, rowid ASC",
                &[&ticket_id],
            )
        })
    }

    /// 某票当前是否还有 `running` attempt（双跑防护的宿主侧读数）。
    pub fn has_running_for_ticket(&self, ticket_id: &str) -> Result<bool, CoreError> {
        self.db.with_conn(|conn| {
            let count: i64 = conn.query_row(
                &format!(
                    "SELECT COUNT(*) FROM {ATTEMPTS_TABLE}
                      WHERE ticket_id = ?1 AND state = 'running'"
                ),
                [ticket_id],
                |row| row.get(0),
            )?;
            Ok(count > 0)
        })
    }

    /// 崩溃恢复对账：把所有 `running` attempt 标成**第四终态** `interrupted`。
    ///
    /// GLOSSARY「中断态」：宿主重启时 running attempt 被标记的记账状态——非成功非失败，
    /// worktree 不清（保留现场），token 记账以**最后快照**为准并标 `accounting_truncated`。
    /// 是否重派由编排器按 workflow 配置裁决，宿主不自作主张（所以这里只标、不重派）。
    ///
    /// 返回被标记的 attempt id（调用方据此发 `interrupted` 事件）。
    pub fn mark_running_as_interrupted(&self, at: &str) -> Result<Vec<String>, CoreError> {
        let running = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT attempt_id, tokens_json FROM {ATTEMPTS_TABLE} WHERE state = 'running'"
            ))?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })?;

        let mut interrupted = Vec::with_capacity(running.len());
        for (attempt_id, tokens_json) in running {
            // 最后快照 + 截断标记：快照原样保留，只把标记立起来。
            let tokens_json = if tokens_json.trim().is_empty() {
                "{}".to_string()
            } else {
                tokens_json
            };
            self.db.with_conn(|conn| {
                conn.execute(
                    &format!(
                        "UPDATE {ATTEMPTS_TABLE}
                            SET state = 'interrupted',
                                accounting_truncated = 1,
                                tokens_json = ?2,
                                note = COALESCE(note, 'host restarted while the attempt was running; accounting truncated to the last snapshot'),
                                updated_at = ?3
                          WHERE attempt_id = ?1 AND state = 'running'"
                    ),
                    rusqlite::params![attempt_id, tokens_json, at],
                )?;
                Ok(())
            })?;
            interrupted.push(attempt_id);
        }
        Ok(interrupted)
    }
}

// ---------------------------------------------------------------------------
// 行形状与 SQL 细节（host-private）
// ---------------------------------------------------------------------------

fn select_records(
    conn: &Connection,
    suffix: &str,
    params: &[&dyn rusqlite::ToSql],
) -> rusqlite::Result<Vec<AttemptRecord>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT attempt_id, ticket_id, state, profile, session_id, pid,
                tokens_json, accounting_truncated, disposition, note, created_at, updated_at
           FROM {ATTEMPTS_TABLE} {suffix}"
    ))?;
    let rows = stmt.query_map(params, row_to_record)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
}

fn select_record(
    conn: &Connection,
    suffix: &str,
    params: &[&dyn rusqlite::ToSql],
) -> rusqlite::Result<Option<AttemptRecord>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT attempt_id, ticket_id, state, profile, session_id, pid,
                tokens_json, accounting_truncated, disposition, note, created_at, updated_at
           FROM {ATTEMPTS_TABLE} {suffix}"
    ))?;
    stmt.query_row(params, row_to_record).optional()
}

fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<AttemptRecord> {
    let state_text: String = row.get(2)?;
    let state = parse_state(&state_text).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(format!(
                "unknown attempt state: {state_text}"
            ))),
        )
    })?;
    let tokens_text: String = row.get(6)?;
    let tokens = serde_json::from_str::<serde_json::Value>(&tokens_text)
        .map(|value| TokenSnapshot::from_json(&value))
        .unwrap_or_default();
    Ok(AttemptRecord {
        attempt_id: row.get(0)?,
        ticket_id: row.get(1)?,
        state,
        profile: row.get(3)?,
        session_id: row.get(4)?,
        pid: row.get::<_, Option<i64>>(5)?.map(|pid| pid as u32),
        tokens,
        accounting_truncated: row.get::<_, i64>(7)? != 0,
        disposition: row.get(8)?,
        note: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::{Migrator, MIGRATIONS};
    use serde_json::json;

    fn store() -> AttemptStore {
        let db = Database::open_in_memory().expect("memory db");
        db.migrate(&Migrator::new(MIGRATIONS)).expect("migrate");
        AttemptStore::new(&db)
    }

    #[test]
    fn an_attempt_round_trips_through_the_host_private_store() {
        let store = store();
        let mut record = AttemptRecord::running("att-1", "16", Some("implement-spec".into()));
        record.session_id = Some("fake-session-1".into());
        record.pid = Some(4242);
        record.disposition = Some("accepted".into());
        store.upsert(&record).expect("insert");

        let found = store.find("att-1").expect("find").expect("row");
        assert_eq!(found, record);
        assert_eq!(found.state, AttemptState::Running);
        assert_eq!(found.ticket_id, "16");
    }

    #[test]
    fn a_token_snapshot_survives_the_round_trip_with_its_truncated_flag() {
        let store = store();
        let mut record = AttemptRecord::running("att-1", "16", None);
        record.tokens = TokenSnapshot::from_json(&json!({
            "input": 100, "output": 10, "cacheRead": 0, "cacheWrite": 0,
            "total": 110, "costMicros": 0, "streamedTotal": 110,
            "nestedUsageCalls": 1, "countedMessages": 1, "truncated": true,
            "note": "host restarted",
        }));
        record.accounting_truncated = true;
        store.upsert(&record).expect("insert");

        let found = store.find("att-1").expect("find").expect("row");
        assert_eq!(found.tokens.total, 110);
        assert_eq!(found.tokens.nested_usage_calls, 1);
        assert!(found.accounting_truncated, "中断态必须带截断标记");
        assert!(found.tokens.truncated);
    }

    #[test]
    fn every_contract_state_maps_into_the_four_persistent_states() {
        assert_eq!(state_to_str(AttemptState::Running), "running");
        assert_eq!(state_to_str(AttemptState::Settled), "succeeded");
        assert_eq!(state_to_str(AttemptState::Failed), "failed");
        assert_eq!(state_to_str(AttemptState::Interrupted), "interrupted");
        assert_eq!(state_to_str(AttemptState::Aborted), "failed");
        for state in ATTEMPT_STATES {
            assert!(parse_state(state).is_some(), "{state} 必须能解析回来");
        }
        assert!(parse_state("aborted").is_none(), "持久态闭集只有四态");
    }

    #[test]
    fn attempts_of_one_ticket_are_listed_in_creation_order() {
        let store = store();
        for id in ["att-2", "att-1", "att-3"] {
            store
                .upsert(&AttemptRecord::running(id, "16", None))
                .expect("insert");
        }
        let listed = store.list_for_ticket("16").expect("list");
        assert_eq!(
            listed.iter().map(|r| r.attempt_id.as_str()).collect::<Vec<_>>(),
            vec!["att-2", "att-1", "att-3"],
            "同一时刻创建的按写入次序"
        );
    }

    #[test]
    fn a_running_attempt_is_visible_to_the_double_dispatch_guard() {
        let store = store();
        store
            .upsert(&AttemptRecord::running("att-1", "16", None))
            .expect("insert");
        assert!(store.has_running_for_ticket("16").expect("guard"));

        let mut record = store.find("att-1").expect("find").expect("row");
        record.finish(AttemptState::Settled, "settled on the double condition");
        store.upsert(&record).expect("update");
        assert!(!store.has_running_for_ticket("16").expect("guard"));
    }

    #[test]
    fn a_host_restart_marks_running_attempts_interrupted_and_truncated() {
        let store = store();
        let mut running = AttemptRecord::running("att-running", "16", None);
        running.tokens = TokenSnapshot::from_json(&json!({"input": 7, "total": 7}));
        store.upsert(&running).expect("insert running");
        let mut done = AttemptRecord::running("att-done", "16", None);
        done.finish(AttemptState::Settled, "settled");
        store.upsert(&done).expect("insert terminal");

        let interrupted = store
            .mark_running_as_interrupted(&now_rfc3339())
            .expect("recover");
        assert_eq!(interrupted, vec!["att-running".to_string()], "只动 running");

        let row = store.find("att-running").expect("find").expect("row");
        assert_eq!(row.state, AttemptState::Interrupted, "第四终态");
        assert!(row.accounting_truncated, "记账以最后快照为准并标截断");
        assert_eq!(row.tokens.total, 7, "最后快照不许丢");
        assert!(row.note.as_deref().is_some_and(|n| n.contains("host restarted")));

        let terminal = store.find("att-done").expect("find").expect("row");
        assert_eq!(terminal.state, AttemptState::Settled, "终态 attempt 不动");

        // 幂等：第二次恢复没有可标的行。
        assert!(store
            .mark_running_as_interrupted(&now_rfc3339())
            .expect("recover again")
            .is_empty());
    }

    #[test]
    fn a_state_outside_the_closed_set_cannot_even_be_written() {
        let db = Database::open_in_memory().expect("memory db");
        db.migrate(&Migrator::new(MIGRATIONS)).expect("migrate");
        // 闭集由 CHECK 在存储层把门：写不进去，也就没有「假 attempt」可读。
        let rejected = db.with_conn(|conn| {
            conn.execute(
                &format!(
                    "INSERT INTO {ATTEMPTS_TABLE} (attempt_id, ticket_id, state, created_at, updated_at)
                     VALUES ('bad', '16', 'running ', 't', 't')"
                ),
                [],
            )
        });
        assert!(
            rejected.is_err(),
            "state 列只收四态；宿主绝不把未知值静默降级成一个假 attempt"
        );
    }
}
