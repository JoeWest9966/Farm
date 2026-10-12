//! Tracker 服务面（GLOSSARY：归一化 Issue 模型 + 原子 claim，tracker 是总线）。
//!
//! 职责边界（ADR-0001 决策 3）：
//! - **做**：把 provider 语义拉平成归一化视图、维护原生 blocking 边、提供原子 claim
//!   （CAS 语义，assignee 即 claim）、持久化票快照与状态历史。
//! - **不做**：frontier / dispatchable 的计算（归编排器）、provider 写回（归 adapter）。
//!
//! 面板与编排器读**同一个**归一化视图（本服务），所以看板与调度看到的是同一份事实。

use std::sync::Arc;

use crate::db::Database;
use crate::tracker::adapter::TrackerAdapter;
use crate::tracker::model::{Issue, StateTransition};
use crate::tracker::store::{ClaimOutcome, ReleaseOutcome, TrackerStore};
use crate::tracker::TrackerError;
use crate::now_rfc3339;

/// 一次 provider 拉平的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncReport {
    /// provider 名（host-private）。
    pub provider: &'static str,
    /// 从 provider 读到的归一化票数。
    pub fetched: usize,
    /// 内容发生变化（含新进视图）的票数。
    pub changed: usize,
}

/// Tracker 服务（`Clone` 廉价：内部共享同一个存储句柄与 adapter）。
#[derive(Clone, Debug)]
pub struct TrackerService {
    db: Database,
    adapter: Arc<dyn TrackerAdapter>,
    store: TrackerStore,
}

impl TrackerService {
    pub fn new(db: Database, adapter: Arc<dyn TrackerAdapter>) -> Self {
        let store = TrackerStore::new(&db);
        Self { db, adapter, store }
    }

    /// 当前 provider 名（host-private；不进契约）。
    pub fn provider(&self) -> &'static str {
        self.adapter.provider()
    }

    /// 底层存储句柄（宿主内部用；schema 不外泄，ADR-0005 §5）。
    pub fn database(&self) -> &Database {
        &self.db
    }

    /// 把 provider 语义拉平进快照：内容变了才更新 + 记一条状态历史。
    pub fn sync(&self) -> Result<SyncReport, TrackerError> {
        let fetched = self.adapter.fetch()?;
        let provider = self.adapter.provider();
        let changed = self.store.upsert_snapshot(&fetched, &now_rfc3339())?;
        Ok(SyncReport {
            provider,
            fetched: fetched.len(),
            changed,
        })
    }

    /// 归一化视图（面板与编排器读同一个视图——tracker 是总线）。
    ///
    /// 先 sync 再读：视图永远反映 provider 的最新事实，快照与状态历史落在 SQLite。
    pub fn issues(&self) -> Result<Vec<Issue>, TrackerError> {
        self.sync()?;
        Ok(self.store.list_issues()?)
    }

    /// 单票归一化视图（含 blocking 边）。
    pub fn issue(&self, issue_id: &str) -> Result<Option<Issue>, TrackerError> {
        self.sync()?;
        Ok(self.store.find_issue(issue_id)?)
    }

    /// 原子 claim（CAS 语义，assignee 即 claim）。
    ///
    /// - 抢到 → 返回持有后的归一化票；
    /// - 同一 assignee 重复 claim → 幂等，同样返回（不写新历史）；
    /// - 已被他人持有 → [`TrackerError::Conflict`]（GLOSSARY：失败即已被他人抢走）。
    pub fn claim(&self, issue_id: &str, assignee: &str) -> Result<Issue, TrackerError> {
        self.claim_with_outcome(issue_id, assignee)
            .map(|(issue, _)| issue)
    }

    /// 同 [`TrackerService::claim`]，但额外给出 CAS 结局。
    ///
    /// 调用方据此区分「真抢到」与「幂等重claim」——例如事件投影只在前者时发。
    pub fn claim_with_outcome(
        &self,
        issue_id: &str,
        assignee: &str,
    ) -> Result<(Issue, ClaimOutcome), TrackerError> {
        // claim 的对象是归一化视图里的票：先拉平，provider 上存在的票才能被抢。
        self.sync()?;
        let outcome = self.store.claim(issue_id, assignee, &now_rfc3339())?;
        match outcome {
            ClaimOutcome::Acquired | ClaimOutcome::AlreadyHeldByClaimant => {
                Ok((self.require_issue(issue_id)?, outcome))
            }
            ClaimOutcome::HeldByOther { holder } => Err(TrackerError::Conflict {
                issue_id: issue_id.to_string(),
                holder,
            }),
            ClaimOutcome::UnknownIssue => Err(TrackerError::NotFound {
                issue_id: issue_id.to_string(),
            }),
        }
    }

    /// release：交还排他所有权，之后他人可 claim。
    ///
    /// - 持有者本人释放 → 成功（记一条历史）；
    /// - 无人持有 → 幂等空操作（重复 release 不报错）；
    /// - 持着票的是别人 → [`TrackerError::Conflict`]。
    pub fn release(&self, issue_id: &str, assignee: &str) -> Result<ReleaseOutcome, TrackerError> {        self.sync()?;
        let outcome = self.store.release(issue_id, assignee, &now_rfc3339())?;
        match outcome {
            ReleaseOutcome::Released | ReleaseOutcome::NotClaimed => Ok(outcome),
            ReleaseOutcome::HeldByOther { holder } => Err(TrackerError::Conflict {
                issue_id: issue_id.to_string(),
                holder,
            }),
            ReleaseOutcome::UnknownIssue => Err(TrackerError::NotFound {
                issue_id: issue_id.to_string(),
            }),
        }
    }

    /// **关票投影**（GLOSSARY「投影」；merge 服务 #18 的投影目标）。
    ///
    /// 完成信号（集成分支上的 merge commit）出现**之后**，宿主按 workflow 开关
    /// 机械把归一化票标成 `done`。`done` 是**不可逆事实**（与 `release` 对已关票
    /// 的处理同语义）：之后的 provider sync 不会把它降级回 open/claimed，
    /// 见 [`TrackerStore::upsert_snapshot`]。
    ///
    /// 票不在归一化视图里 → `Ok(None)`（投影记 `failed`，不影响完成信号）。
    pub fn close(&self, issue_id: &str, actor: &str) -> Result<Option<Issue>, TrackerError> {
        self.sync()?;
        if !self.store.close_issue(issue_id, actor, &now_rfc3339())? {
            return Ok(None);
        }
        Ok(self.store.find_issue(issue_id)?)
    }

    /// 单票状态历史（落库后可查询；按发生次序）。
    pub fn history(&self, issue_id: &str) -> Result<Vec<StateTransition>, TrackerError> {
        Ok(self.store.history(issue_id)?)
    }

    /// 全部票的状态历史（面板/调试查询）。
    pub fn history_all(&self) -> Result<Vec<StateTransition>, TrackerError> {
        Ok(self.store.history_all()?)
    }

    fn require_issue(&self, issue_id: &str) -> Result<Issue, TrackerError> {
        self.store.find_issue(issue_id)?.ok_or_else(|| TrackerError::NotFound {
            issue_id: issue_id.to_string(),
        })
    }
}
