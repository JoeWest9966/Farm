//! 归一化 Issue 模型（GLOSSARY：Tracker 服务的归一化 Issue 模型 + 原生 blocking 边）。
//!
//! 形状与契约投影（[`IssueView`]）一一对应，但**不是**契约类型：`provider` 是
//! 宿主私有列，永不进契约（ADR-0005 §5：SQLite schema 永不外泄）。

use farm_sdk::services::tracker::{IssueState, IssueView};

/// 归一化状态在宿主私有存储里的文本形态（与契约 `IssueState` 的 snake_case 一致）。
pub fn state_to_str(state: IssueState) -> &'static str {
    match state {
        IssueState::Open => "open",
        IssueState::Claimed => "claimed",
        IssueState::InProgress => "in_progress",
        IssueState::Blocked => "blocked",
        IssueState::Done => "done",
    }
}

/// 文本形态解析回归一化状态；无法识别即 provider 给了契约外的值。
pub fn parse_state(text: &str) -> Option<IssueState> {
    match text {
        "open" => Some(IssueState::Open),
        "claimed" => Some(IssueState::Claimed),
        "in_progress" => Some(IssueState::InProgress),
        "blocked" => Some(IssueState::Blocked),
        "done" => Some(IssueState::Done),
        _ => None,
    }
}

/// 一条 blocking 边：`blocker` 未竟则 `blocked` 不可派发。
///
/// 边是**原语**：dispatchable / frontier 的计算归编排器（ADR-0001 决策 3），
/// 服务只陈述「谁堵谁」，不替编排器下结论。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockingEdge {
    pub blocker: String,
    pub blocked: String,
}

/// 归一化票（host-private 形状；经 [`Issue::to_view`] 投影成契约视图）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub issue_id: String,
    pub title: String,
    pub state: IssueState,
    pub labels: Vec<String>,
    /// 排他所有权声明（CAS 语义，assignee 即 claim）。`None` = 无人持有。
    pub assignee: Option<String>,
    /// 堵住本票的票（provider 原生依赖的声明方向）。
    pub blocked_by: Vec<String>,
    /// 被本票堵住的票（反向，从全量边集推导）。
    pub blocking: Vec<String>,
    /// provider 名（host-private；不进契约）。
    pub provider: String,
    /// 内容上一次发生变化的时间（RFC3339）；无变化不推进。
    pub updated_at: String,
}

impl Issue {
    /// 投影到契约视图：丢掉 `provider`（ADR-0005 §5），只留归一化事实。
    pub fn to_view(&self) -> IssueView {
        IssueView {
            issue_id: self.issue_id.clone(),
            title: self.title.clone(),
            state: self.state,
            labels: self.labels.clone(),
            assignee: self.assignee.clone(),
            updated_at: Some(self.updated_at.clone()),
            blocked_by: self.blocked_by.clone(),
            blocking: self.blocking.clone(),
        }
    }
}

/// 一条状态历史（票快照的状态变迁；ADR-0005 §2 持久项）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateTransition {
    /// 宿主分配的单调序号（同一票内有序）。
    pub seq: i64,
    pub issue_id: String,
    pub from_state: Option<IssueState>,
    pub to_state: IssueState,
    /// 变迁后的持有者（claim / release 都记录）。
    pub assignee: Option<String>,
    /// 谁造成的变迁（claimant / provider 名 / 人类）。
    pub actor: String,
    pub occurred_at: String,
    /// 一句话原因（provider sync / claim / release / …）。
    pub note: String,
}
