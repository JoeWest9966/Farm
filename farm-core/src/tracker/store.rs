//! 票快照 / blocking 边 / 状态历史的宿主私有存储访问（ADR-0005 §5）。
//!
//! 一切读写经 [`crate::db::Database::with_conn`]：宿主内部串行访问同一连接，
//! 因此「读当前持有者 → CAS 写」在一个闭包里是原子的。CAS 的硬保证仍然是
//! `UPDATE ... WHERE assignee IS NULL` 本身——即使将来换成多连接，也只有一家赢。

use rusqlite::{Connection, OptionalExtension};

use crate::db::Database;
use crate::error::CoreError;
use crate::tracker::model::{parse_state, state_to_str, BlockingEdge, Issue, StateTransition};
use farm_sdk::services::tracker::IssueState;

/// 原子 claim 的结局（GLOSSARY：失败即已被他人抢走）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// 抢到了（`assignee IS NULL` → 本 claimant）。
    Acquired,
    /// 同一 assignee 重复 claim：幂等，状态不变、不写新历史。
    AlreadyHeldByClaimant,
    /// 已被他人持有。
    HeldByOther { holder: String },
    /// 票已经 `done`：完成信号（merge commit）是不可逆事实，不得被 claim 拉回流内。
    AlreadyDone,
    /// 票不存在于归一化视图。
    UnknownIssue,
}

/// release 的结局（幂等：无人持有时释放是空操作）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReleaseOutcome {
    /// 持有者本人释放成功。
    Released,
    /// 无人持有：幂等空操作（重复 release 的第二次）。
    NotClaimed,
    /// 持着票的是别人：非持有者不得释放。
    HeldByOther { holder: String },
    /// 票不存在于归一化视图。
    UnknownIssue,
}

#[derive(Clone, Debug)]
pub struct TrackerStore {
    db: Database,
}

impl TrackerStore {
    pub fn new(db: &Database) -> Self {
        Self { db: db.clone() }
    }

    /// 归一化视图：全部票 + 全部 blocking 边（blocker→blocked 一行一边）。
    pub fn list_issues(&self) -> Result<Vec<Issue>, CoreError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT issue_id, title, state, assignee, labels, provider, updated_at
                   FROM tracker_issues
                  ORDER BY issue_id",
            )?;
            let rows = stmt.query_map([], |row| {
                let state_text: String = row.get(2)?;
                let state = parse_state(&state_text).ok_or_else(|| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::other(format!(
                            "unknown normalized issue state: {state_text}"
                        ))),
                    )
                })?;
                Ok(Issue {
                    issue_id: row.get(0)?,
                    title: row.get(1)?,
                    state,
                    assignee: row.get(3)?,
                    labels: parse_labels(row.get::<_, String>(4)?),
                    provider: row.get(5)?,
                    updated_at: row.get(6)?,
                    blocked_by: Vec::new(),
                    blocking: Vec::new(),
                })
            })?;

            let mut issues: Vec<Issue> = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            let edges = select_edges(conn)?;
            for edge in edges {
                if let Some(blocked) = issues.iter_mut().find(|i| i.issue_id == edge.blocked) {
                    blocked.blocked_by.push(edge.blocker.clone());
                }
                if let Some(blocker) = issues.iter_mut().find(|i| i.issue_id == edge.blocker) {
                    blocker.blocking.push(edge.blocked);
                }
            }
            for issue in &mut issues {
                issue.blocked_by.sort();
                issue.blocking.sort();
            }
            Ok(issues)
        })
    }

    pub fn find_issue(&self, issue_id: &str) -> Result<Option<Issue>, CoreError> {
        Ok(self
            .list_issues()?
            .into_iter()
            .find(|issue| issue.issue_id == issue_id))
    }

    /// 把 provider 拉平的归一化票写进快照：内容变了才更新 + 记一条状态历史。
    ///
    /// 返回发生变化的票数（调用方可据此判断是否需要重读视图）。
    pub fn upsert_snapshot(
        &self,
        fetched: &[Issue],
        at: &str,
    ) -> Result<usize, CoreError> {
        self.db.with_conn(|conn| {
            let mut changed = 0usize;
            for issue in fetched {
                let existing = select_issue_row(conn, &issue.issue_id)?;
                match existing {
                    None => {
                        insert_issue(conn, issue, at)?;
                        record_transition(
                            conn,
                            &issue.issue_id,
                            None,
                            issue.state,
                            issue.assignee.as_deref(),
                            &issue.provider,
                            at,
                            "provider sync: ticket entered the normalized view",
                        )?;
                        changed += 1;
                    }
                    Some(row) => {
                        // claim 是 Farm 侧的排他声明：provider 不报 assignee 时**不清**它，
                        // 否则一次 sync 就把双跑防护悄悄抹掉。
                        // 同理，**已关的票不被降级**：`done` 是不可逆事实（#18 的关票投影
                        // 尤其依赖这一点——provider 侧的 open 回报不该把完成信号抹掉）。
                        let assignee = issue.assignee.clone().or_else(|| row.assignee.clone());
                        let state = if row.state == IssueState::Done || issue.state == IssueState::Done {
                            IssueState::Done
                        } else if assignee.is_some() {
                            IssueState::Claimed
                        } else {
                            IssueState::Open
                        };
                        if row.title == issue.title
                            && row.state == state
                            && row.assignee == assignee
                            && row.labels == issue.labels
                            && row.blocked_by == issue.blocked_by
                        {
                            continue; // 无变化：不推进 updated_at、不造历史
                        }
                        update_issue(conn, issue, state, assignee.as_deref(), at)?;
                        record_transition(
                            conn,
                            &issue.issue_id,
                            Some(row.state),
                            state,
                            assignee.as_deref(),
                            &issue.provider,
                            at,
                            "provider sync: normalized ticket changed",
                        )?;
                        changed += 1;
                    }
                }
                replace_declared_edges(conn, &issue.issue_id, &issue.blocked_by)?;
            }
            Ok(changed)
        })
    }

    /// 原子 claim（CAS）：只有一家赢。
    pub fn claim(
        &self,
        issue_id: &str,
        assignee: &str,
        at: &str,
    ) -> Result<ClaimOutcome, CoreError> {
        self.db.with_conn(|conn| {
            let Some(row) = select_issue_row(conn, issue_id)? else {
                return Ok(ClaimOutcome::UnknownIssue);
            };
            // `done` 先于 assignee 判（code review P2-8）：完成信号不可逆，claim 不得把
            // 一张已关的票推回 `claimed`。放在 assignee 判断之前，原 assignee 也一样拒。
            if row.state == IssueState::Done {
                return Ok(ClaimOutcome::AlreadyDone);
            }
            match row.assignee.as_deref() {
                // 同一 assignee 重复 claim：幂等（GLOSSARY：claim 是票据状态，不是技术锁）。
                Some(current) if current == assignee => {
                    return Ok(ClaimOutcome::AlreadyHeldByClaimant)
                }
                Some(other) => return Ok(ClaimOutcome::HeldByOther { holder: other.to_string() }),
                None => {}
            }

            let updated = conn.execute(
                "UPDATE tracker_issues
                    SET assignee = ?2, state = ?3, updated_at = ?4
                  WHERE issue_id = ?1 AND assignee IS NULL",
                rusqlite::params![
                    issue_id,
                    assignee,
                    state_to_str(IssueState::Claimed),
                    at
                ],
            )?;
            if updated != 1 {
                // 与上面的读之间被他人抢先（多连接时才可能）：重读判冲突，不谎报成功。
                let holder = select_issue_row(conn, issue_id)?
                    .and_then(|row| row.assignee)
                    .unwrap_or_default();
                return Ok(ClaimOutcome::HeldByOther { holder });
            }

            record_transition(
                conn,
                issue_id,
                Some(row.state),
                IssueState::Claimed,
                Some(assignee),
                assignee,
                at,
                "claim: exclusive ownership acquired (CAS)",
            )?;
            Ok(ClaimOutcome::Acquired)
        })
    }

    /// **关票投影**（#18 merge 服务的投影目标）：把归一化票标成 `done`。
    ///
    /// `done` 不可逆：已关的票再关是幂等空操作（不造新历史）。票不存在 → `false`。
    /// 只在状态真的变时才记一条历史（note 点名完成信号来自 merge commit）。
    pub fn close_issue(&self, issue_id: &str, actor: &str, at: &str) -> Result<bool, CoreError> {
        self.db.with_conn(|conn| {
            let Some(row) = select_issue_row(conn, issue_id)? else {
                return Ok(false);
            };
            if row.state == IssueState::Done {
                return Ok(true); // 幂等：关闭是不可逆事实
            }
            conn.execute(
                "UPDATE tracker_issues SET state = ?2, updated_at = ?3 WHERE issue_id = ?1",
                rusqlite::params![issue_id, state_to_str(IssueState::Done), at],
            )?;
            record_transition(
                conn,
                issue_id,
                Some(row.state),
                IssueState::Done,
                row.assignee.as_deref(),
                actor,
                at,
                "merge projection: completion signal observed (merge commit on the integration branch)",
            )?;
            Ok(true)
        })
    }

    /// release：只有持有者能释放；无人持有时是幂等空操作。
    pub fn release(
        &self,
        issue_id: &str,
        assignee: &str,
        at: &str,
    ) -> Result<ReleaseOutcome, CoreError> {
        self.db.with_conn(|conn| {
            let Some(row) = select_issue_row(conn, issue_id)? else {
                return Ok(ReleaseOutcome::UnknownIssue);
            };
            match row.assignee.as_deref() {
                None => return Ok(ReleaseOutcome::NotClaimed),
                Some(current) if current != assignee => {
                    return Ok(ReleaseOutcome::HeldByOther {
                        holder: current.to_string(),
                    })
                }
                Some(_) => {}
            }

            // 释放回 open；provider 侧已关闭的票保持 done（关闭是不可逆事实）。
            let next_state = if row.state == IssueState::Done {
                IssueState::Done
            } else {
                IssueState::Open
            };
            let updated = conn.execute(
                "UPDATE tracker_issues
                    SET assignee = NULL, state = ?2, updated_at = ?3
                  WHERE issue_id = ?1 AND assignee = ?4",
                rusqlite::params![issue_id, state_to_str(next_state), at, assignee],
            )?;
            if updated != 1 {
                return Ok(ReleaseOutcome::HeldByOther {
                    holder: select_issue_row(conn, issue_id)?
                        .and_then(|row| row.assignee)
                        .unwrap_or_default(),
                });
            }

            record_transition(
                conn,
                issue_id,
                Some(row.state),
                next_state,
                None,
                assignee,
                at,
                "release: exclusive ownership released",
            )?;
            Ok(ReleaseOutcome::Released)
        })
    }

    /// 单票状态历史（按 `seq` 升序；可查询 = 落库后可回读）。
    pub fn history(&self, issue_id: &str) -> Result<Vec<StateTransition>, CoreError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT seq, issue_id, from_state, to_state, assignee, actor, occurred_at, note
                   FROM tracker_issue_state_history
                  WHERE issue_id = ?1
                  ORDER BY seq ASC",
            )?;
            let rows = stmt.query_map([issue_id], |row| {
                Ok(StateTransition {
                    seq: row.get(0)?,
                    issue_id: row.get(1)?,
                    from_state: row
                        .get::<_, Option<String>>(2)?
                        .as_deref()
                        .and_then(parse_state),
                    to_state: parse_state(row.get::<_, String>(3)?.as_str()).ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            3,
                            rusqlite::types::Type::Text,
                            Box::new(std::io::Error::other("unknown normalized issue state")),
                        )
                    })?,
                    assignee: row.get(4)?,
                    actor: row.get(5)?,
                    occurred_at: row.get(6)?,
                    note: row.get(7)?,
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
    }

    /// 全部票的状态历史（面板/调试查询用；按 `seq` 升序）。
    pub fn history_all(&self) -> Result<Vec<StateTransition>, CoreError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT seq, issue_id, from_state, to_state, assignee, actor, occurred_at, note
                   FROM tracker_issue_state_history
                  ORDER BY seq ASC",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(StateTransition {
                    seq: row.get(0)?,
                    issue_id: row.get(1)?,
                    from_state: row
                        .get::<_, Option<String>>(2)?
                        .as_deref()
                        .and_then(parse_state),
                    to_state: parse_state(row.get::<_, String>(3)?.as_str()).ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            3,
                            rusqlite::types::Type::Text,
                            Box::new(std::io::Error::other("unknown normalized issue state")),
                        )
                    })?,
                    assignee: row.get(4)?,
                    actor: row.get(5)?,
                    occurred_at: row.get(6)?,
                    note: row.get(7)?,
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
    }
}

// ---------------------------------------------------------------------------
// 行形状与 SQL 细节（host-private）
// ---------------------------------------------------------------------------

/// 快照行（含声明方向的 blocking 边）。
#[derive(Clone, Debug, PartialEq, Eq)]
struct IssueRow {
    title: String,
    state: IssueState,
    assignee: Option<String>,
    labels: Vec<String>,
    provider: String,
    updated_at: String,
    blocked_by: Vec<String>,
}

fn select_issue_row(conn: &Connection, issue_id: &str) -> rusqlite::Result<Option<IssueRow>> {
    let mut stmt = conn.prepare(
        "SELECT title, state, assignee, labels, provider, updated_at
           FROM tracker_issues WHERE issue_id = ?1",
    )?;
    let row = stmt
        .query_row([issue_id], |row| {
            let state_text: String = row.get(1)?;
            let state = parse_state(&state_text).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::other(format!(
                        "unknown normalized issue state: {state_text}"
                    ))),
                )
            })?;
            Ok(IssueRow {
                title: row.get(0)?,
                state,
                assignee: row.get(2)?,
                labels: parse_labels(row.get::<_, String>(3)?),
                provider: row.get(4)?,
                updated_at: row.get(5)?,
                blocked_by: Vec::new(),
            })
        })
        .optional()?;
    let Some(mut row) = row else { return Ok(None) };
    row.blocked_by = select_edges(conn)?
        .into_iter()
        .filter(|edge| edge.blocked == issue_id)
        .map(|edge| edge.blocker)
        .collect();
    row.blocked_by.sort();
    Ok(Some(row))
}

fn select_edges(conn: &Connection) -> rusqlite::Result<Vec<BlockingEdge>> {
    let mut stmt =
        conn.prepare("SELECT blocker_id, blocked_id FROM tracker_blocking_edges ORDER BY blocker_id, blocked_id")?;
    let rows = stmt.query_map([], |row| {
        Ok(BlockingEdge {
            blocker: row.get(0)?,
            blocked: row.get(1)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
}

fn insert_issue(conn: &Connection, issue: &Issue, at: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO tracker_issues (issue_id, title, state, assignee, labels, provider, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            issue.issue_id,
            issue.title,
            state_to_str(issue.state),
            issue.assignee,
            encode_labels(&issue.labels),
            issue.provider,
            at,
        ],
    )?;
    Ok(())
}

fn update_issue(
    conn: &Connection,
    issue: &Issue,
    state: IssueState,
    assignee: Option<&str>,
    at: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE tracker_issues
            SET title = ?2, state = ?3, assignee = ?4, labels = ?5, updated_at = ?6
          WHERE issue_id = ?1",
        rusqlite::params![
            issue.issue_id,
            issue.title,
            state_to_str(state),
            assignee,
            encode_labels(&issue.labels),
            at,
        ],
    )?;
    Ok(())
}

/// 重写某票**声明**的 blocking 边（幂等：先删后插，反向边不入库）。
fn replace_declared_edges(
    conn: &Connection,
    issue_id: &str,
    blocked_by: &[String],
) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM tracker_blocking_edges WHERE blocked_id = ?1",
        [issue_id],
    )?;
    for blocker in blocked_by {
        conn.execute(
            "INSERT OR IGNORE INTO tracker_blocking_edges (blocker_id, blocked_id) VALUES (?1, ?2)",
            rusqlite::params![blocker, issue_id],
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn record_transition(
    conn: &Connection,
    issue_id: &str,
    from_state: Option<IssueState>,
    to_state: IssueState,
    assignee: Option<&str>,
    actor: &str,
    occurred_at: &str,
    note: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO tracker_issue_state_history
             (issue_id, from_state, to_state, assignee, actor, occurred_at, note)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            issue_id,
            from_state.map(state_to_str),
            state_to_str(to_state),
            assignee,
            actor,
            occurred_at,
            note,
        ],
    )?;
    Ok(())
}

fn encode_labels(labels: &[String]) -> String {
    serde_json::to_string(labels).unwrap_or_else(|_| "[]".to_string())
}

fn parse_labels(text: String) -> Vec<String> {
    serde_json::from_str(&text).unwrap_or_default()
}
