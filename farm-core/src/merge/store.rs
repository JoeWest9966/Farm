//! merge 提议的宿主私有存储（ticket #18；ADR-0005 §2/§5）。
//!
//! 一行 = 一次提议的完整轨迹。**write-ahead**：提议先落成 `proposed` 行（与
//! workspace / tracker / agent-runner 同一先例），宿主再机械校验、执行；终态
//! `merged`（带 merge commit）或 `rejected`（带闭集错误码与闸门名）。拒收行**不删**：
//! 它是 #25 审阅队列的读源，也是「永不采信自述」的审计轨迹。
//!
//! schema 是宿主私有实现细节，永不外泄为契约（ADR-0005 §5）：对外只投影到
//! farm-sdk 的 [`MergeProposal`]/[`MergeProposalResponse`]。

use farm_sdk::services::merge::MergeProposalState;

use crate::db::Database;
use crate::error::CoreError;
use crate::merge::projection::ProjectionOutcome;
use crate::now_rfc3339;

/// 提议表名（宿主私有 schema；migration v7）。
pub const PROPOSAL_TABLE: &str = "merge_proposal";

/// 提议状态在存储列里的文本形态（与契约 [`MergeProposalState`] 的 snake_case 一致）。
pub fn state_to_str(state: MergeProposalState) -> &'static str {
    match state {
        MergeProposalState::Proposed => "proposed",
        MergeProposalState::Validated => "validated",
        MergeProposalState::Merged => "merged",
        MergeProposalState::Rejected => "rejected",
    }
}

/// 文本形态解析回提议状态；无法识别即数据损坏（写入口径固定为四个规范字符串）。
pub fn parse_state(text: &str) -> Option<MergeProposalState> {
    match text {
        "proposed" => Some(MergeProposalState::Proposed),
        "validated" => Some(MergeProposalState::Validated),
        "merged" => Some(MergeProposalState::Merged),
        "rejected" => Some(MergeProposalState::Rejected),
        _ => None,
    }
}

/// 一条提议记录（宿主私有形状；#25 审阅队列与投影都读它）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeProposalRecord {
    /// 宿主分配的提议 id（`mrg-...`）。
    pub proposal_id: String,
    pub ticket_id: String,
    pub worktree_branch: String,
    pub integration_branch: String,
    /// 源仓库与 worktree 路径（来自 workspace 映射，执行时要用）。
    pub repo_path: String,
    pub worktree_path: String,
    /// agent 自报的 commit（宿主只校验、不采信）。
    pub claimed_commit: Option<String>,
    /// agent 的提议留言（进 merge commit 消息）。
    pub message: String,
    pub state: MergeProposalState,
    /// 拒收时的机械闸门名（`mapping` / `commit_claim` / `worktree_clean` / `fast_forward` / `tests`）。
    pub rejection_stage: Option<String>,
    /// 拒收时的闭集错误码（`conflict` / `not_found` / …）。
    pub rejection_code: Option<String>,
    /// 拒收原因（人话 + 机械细节）。
    pub rejection_detail: Option<String>,
    /// 完成信号：集成分支上该票的 merge commit。
    pub merge_commit: Option<String>,
    /// 投影结局（按 workflow 开关）。
    pub projection: Vec<ProjectionOutcome>,
    pub created_at: String,
    pub updated_at: String,
}

impl MergeProposalRecord {
    /// write-ahead 的初始行：`proposed`（宿主还没做任何动作）。
    pub(crate) fn proposed(
        proposal_id: String,
        ticket_id: &str,
        worktree_branch: &str,
        integration_branch: &str,
        repo_path: &str,
        worktree_path: &str,
        claimed_commit: Option<&str>,
        message: &str,
        at: &str,
    ) -> Self {
        Self {
            proposal_id,
            ticket_id: ticket_id.to_string(),
            worktree_branch: worktree_branch.to_string(),
            integration_branch: integration_branch.to_string(),
            repo_path: repo_path.to_string(),
            worktree_path: worktree_path.to_string(),
            claimed_commit: claimed_commit.map(str::to_string),
            message: message.to_string(),
            state: MergeProposalState::Proposed,
            rejection_stage: None,
            rejection_code: None,
            rejection_detail: None,
            merge_commit: None,
            projection: Vec::new(),
            created_at: at.to_string(),
            updated_at: at.to_string(),
        }
    }

    /// 机械闸门全部通过（merge 尚未执行）。
    pub(crate) fn mark_validated(&mut self, at: &str) {
        self.state = MergeProposalState::Validated;
        self.updated_at = at.to_string();
    }

    /// 真 merge 已执行：完成信号落库。
    pub(crate) fn mark_merged(&mut self, merge_commit: &str, at: &str) {
        self.state = MergeProposalState::Merged;
        self.merge_commit = Some(merge_commit.to_string());
        self.updated_at = at.to_string();
    }

    /// 拒收：闸门名 + 闭集错误码 + 原因一并落库（worktree 原样保留）。
    pub(crate) fn mark_rejected(
        &mut self,
        stage: &str,
        code: &str,
        detail: &str,
        at: &str,
    ) {
        self.state = MergeProposalState::Rejected;
        self.rejection_stage = Some(stage.to_string());
        self.rejection_code = Some(code.to_string());
        self.rejection_detail = Some(detail.to_string());
        self.updated_at = at.to_string();
    }

    pub(crate) fn set_projection(&mut self, outcomes: Vec<ProjectionOutcome>, at: &str) {
        self.projection = outcomes;
        self.updated_at = at.to_string();
    }
}

/// 提议存储句柄（`Clone` 廉价：内部共享同一个 [`Database`]）。
#[derive(Clone, Debug)]
pub struct MergeStore {
    db: Database,
}

impl MergeStore {
    pub fn new(db: &Database) -> Self {
        Self { db: db.clone() }
    }

    /// 写一行（insert 或整行更新；状态机由调用方驱动）。
    pub fn save(&self, record: &MergeProposalRecord) -> Result<(), CoreError> {
        let projection = serde_json::to_string(&record.projection)
            .map_err(|e| CoreError::Config(format!("projection outcomes are not serializable: {e}")))?;
        self.db.with_conn(|conn| {
            conn.execute(
                &format!(
                    "INSERT INTO {PROPOSAL_TABLE}
                        (proposal_id, ticket_id, worktree_branch, integration_branch, repo_path,
                         worktree_path, claimed_commit, message, state, rejection_stage,
                         rejection_code, rejection_detail, merge_commit, projection,
                         created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
                     ON CONFLICT(proposal_id) DO UPDATE SET
                        ticket_id = excluded.ticket_id,
                        worktree_branch = excluded.worktree_branch,
                        integration_branch = excluded.integration_branch,
                        repo_path = excluded.repo_path,
                        worktree_path = excluded.worktree_path,
                        claimed_commit = excluded.claimed_commit,
                        message = excluded.message,
                        state = excluded.state,
                        rejection_stage = excluded.rejection_stage,
                        rejection_code = excluded.rejection_code,
                        rejection_detail = excluded.rejection_detail,
                        merge_commit = excluded.merge_commit,
                        projection = excluded.projection,
                        updated_at = excluded.updated_at"
                ),
                rusqlite::params![
                    record.proposal_id,
                    record.ticket_id,
                    record.worktree_branch,
                    record.integration_branch,
                    record.repo_path,
                    record.worktree_path,
                    record.claimed_commit,
                    record.message,
                    state_to_str(record.state),
                    record.rejection_stage,
                    record.rejection_code,
                    record.rejection_detail,
                    record.merge_commit,
                    projection,
                    record.created_at,
                    record.updated_at,
                ],
            )?;
            Ok(())
        })
    }

    /// 按提议 id 查一行。
    pub fn find(&self, proposal_id: &str) -> Result<Option<MergeProposalRecord>, CoreError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!("{SELECT_COLS} WHERE proposal_id = ?1"))?;
            let mut rows = stmt.query_map([proposal_id], row_from_query)?;
            rows.next().transpose()
        })
    }

    /// 全部提议（最新优先；#25 审阅队列的读面）。
    pub fn list(&self) -> Result<Vec<MergeProposalRecord>, CoreError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!("{SELECT_COLS} ORDER BY seq DESC"))?;
            let rows = stmt.query_map([], row_from_query)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
    }

    /// 某票的提议时间线（最早优先；一票可被多次提议）。
    pub fn for_ticket(&self, ticket_id: &str) -> Result<Vec<MergeProposalRecord>, CoreError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!("{SELECT_COLS} WHERE ticket_id = ?1 ORDER BY seq ASC"))?;
            let rows = stmt.query_map([ticket_id], row_from_query)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
    }

    /// 某状态的提议（#25 审阅队列按状态过滤；`proposed` = 尚待人工闸门）。
    pub fn in_state(&self, state: MergeProposalState) -> Result<Vec<MergeProposalRecord>, CoreError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!("{SELECT_COLS} WHERE state = ?1 ORDER BY seq DESC"))?;
            let rows = stmt.query_map([state_to_str(state)], row_from_query)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
    }
}

const SELECT_COLS: &str = "SELECT proposal_id, ticket_id, worktree_branch, integration_branch, \
                           repo_path, worktree_path, claimed_commit, message, state, rejection_stage, \
                           rejection_code, rejection_detail, merge_commit, projection, created_at, updated_at \
                           FROM merge_proposal";

/// 从查询行构造记录（列序见 `SELECT_COLS`）。
fn row_from_query(row: &rusqlite::Row<'_>) -> rusqlite::Result<MergeProposalRecord> {
    let state_text: String = row.get(8)?;
    let state = parse_state(&state_text).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            8,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(format!(
                "unknown merge_proposal.state: {state_text}"
            ))),
        )
    })?;
    let projection_text: String = row.get(13)?;
    let projection: Vec<ProjectionOutcome> = if projection_text.trim().is_empty() {
        Vec::new()
    } else {
        serde_json::from_str(&projection_text).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                13,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::other(format!(
                    "malformed merge_proposal.projection: {e}"
                ))),
            )
        })?
    };
    Ok(MergeProposalRecord {
        proposal_id: row.get(0)?,
        ticket_id: row.get(1)?,
        worktree_branch: row.get(2)?,
        integration_branch: row.get(3)?,
        repo_path: row.get(4)?,
        worktree_path: row.get(5)?,
        claimed_commit: row.get(6)?,
        message: row.get(7)?,
        state,
        rejection_stage: row.get(9)?,
        rejection_code: row.get(10)?,
        rejection_detail: row.get(11)?,
        merge_commit: row.get(12)?,
        projection,
        created_at: row.get(14)?,
        updated_at: row.get(15)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::{Migrator, MIGRATIONS};

    fn store() -> (tempfile::TempDir, MergeStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open(dir.path().join("farm.db")).expect("open store");
        db.migrate(&Migrator::new(MIGRATIONS)).expect("migrate");
        (dir, MergeStore::new(&db))
    }

    #[test]
    fn the_proposal_state_round_trips_through_its_column_string() {
        for state in farm_sdk::services::merge::MergeProposalState::ALL {
            assert_eq!(parse_state(state_to_str(state)), Some(state));
        }
        assert_eq!(parse_state("done"), None, "done 不是提议状态");
    }

    #[test]
    fn a_proposal_row_persists_its_whole_trajectory() {
        let (_dir, store) = store();
        let mut record = MergeProposalRecord::proposed(
            "mrg-1".into(),
            "18",
            "farm/t18",
            "implement/spec-10",
            "/repo",
            "/wt/t18",
            Some("abc123"),
            "agent-runner local backend",
            "2026-01-01T00:00:00Z",
        );
        store.save(&record).expect("write-ahead insert");
        assert_eq!(
            store.find("mrg-1").expect("find").expect("row").state,
            farm_sdk::services::merge::MergeProposalState::Proposed
        );

        record.mark_validated("2026-01-01T00:00:01Z");
        record.mark_merged("deadbeef", "2026-01-01T00:00:02Z");
        store.save(&record).expect("update to merged");
        let stored = store.find("mrg-1").expect("find").expect("row");
        assert_eq!(stored.state, farm_sdk::services::merge::MergeProposalState::Merged);
        assert_eq!(stored.merge_commit.as_deref(), Some("deadbeef"));

        // 拒收轨迹同样可回读（#25 审阅队列 / 审计）。
        let mut rejected = MergeProposalRecord::proposed(
            "mrg-2".into(),
            "19",
            "farm/t19",
            "implement/spec-10",
            "/repo",
            "/wt/t19",
            None,
            "",
            "2026-01-01T00:00:03Z",
        );
        rejected.mark_rejected("worktree_clean", "conflict", "worktree is not clean: ?? a.txt", "2026-01-01T00:00:04Z");
        store.save(&rejected).expect("insert rejected");

        let list = store.list().expect("list");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].proposal_id, "mrg-2", "最新优先");
        let for_ticket = store.for_ticket("19").expect("for ticket");
        assert_eq!(for_ticket.len(), 1);
        assert_eq!(for_ticket[0].rejection_stage.as_deref(), Some("worktree_clean"));
        assert_eq!(for_ticket[0].rejection_code.as_deref(), Some("conflict"));
        assert_eq!(store.in_state(farm_sdk::services::merge::MergeProposalState::Merged).expect("in state").len(), 1);
    }
}
