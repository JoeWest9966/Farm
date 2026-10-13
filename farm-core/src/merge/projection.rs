//! 投影（GLOSSARY「投影」）：完成信号触发后由**宿主机械执行**的确定性写回动作。
//!
//! ADR-0001 决策 2 + GLOSSARY：关票、评论 merge commit 链接、开 PR、打标签都由宿主
//! 机械执行，由 workflow 配置开关（[`farm_sdk::workflow::ProjectionSwitches`]）；
//! **agent 的职责在 merge 提议时结束**。全关 = 只产 merge commit，不做任何写回。
//!
//! v1 的执行面（诚实边界）：
//! - `close_issue` —— 真执行：tracker 关票（host-private 存储，`done` 不可逆）。
//! - `open_pr` / `comment_merge_commit` / `add_label` —— **记 `skipped` + 理由**：
//!   v1 只有 native provider（只读，ADR-0001 决策 3：provider 写回归 adapter），
//!   没有 GitHub/Gogs 写回面。merge commit 链接随 `merge` 事件载荷进总线，
//!   通知通道（#21）与面板（#25）读它即可，不必假装执行。
//!
//! 投影失败**不影响**完成信号：merge commit 已经在集成分支上，投影只是其后的写回。
//! 失败只记进提议行与 `merge` 事件载荷（可观测、可重放）。

use serde::{Deserialize, Serialize};
use farm_sdk::workflow::ProjectionSwitches;

use crate::AppState;

/// 四个投影开关（与 [`ProjectionSwitches`] 的字段一一对应；按声明序执行）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionSwitch {
    /// 关票。
    CloseIssue,
    /// 开 PR。
    OpenPr,
    /// 评论 merge commit 链接。
    CommentMergeCommit,
    /// 打标签。
    AddLabel,
}

impl ProjectionSwitch {
    /// 声明序（投影报告按这个次序）。
    pub const ALL: [ProjectionSwitch; 4] = [
        ProjectionSwitch::CloseIssue,
        ProjectionSwitch::OpenPr,
        ProjectionSwitch::CommentMergeCommit,
        ProjectionSwitch::AddLabel,
    ];

    /// workflow 配置里的开关位。
    pub fn is_enabled(&self, switches: &ProjectionSwitches) -> bool {
        match self {
            ProjectionSwitch::CloseIssue => switches.close_issue,
            ProjectionSwitch::OpenPr => switches.open_pr,
            ProjectionSwitch::CommentMergeCommit => switches.comment_merge_commit,
            ProjectionSwitch::AddLabel => switches.add_label,
        }
    }
}

/// 一个投影动作的结局。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionStatus {
    /// 真执行了。
    Executed,
    /// 配置开着但本宿主/provider 执行不了（带理由）。
    Skipped,
    /// 执行了但失败（完成信号不受影响）。
    Failed,
}

/// 一个投影动作的结局记录（进提议行 + `merge` 事件载荷）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionOutcome {
    pub switch: ProjectionSwitch,
    pub status: ProjectionStatus,
    pub detail: String,
}

impl ProjectionOutcome {
    fn new(switch: ProjectionSwitch, status: ProjectionStatus, detail: impl Into<String>) -> Self {
        Self {
            switch,
            status,
            detail: detail.into(),
        }
    }

    /// JSON 形状（事件载荷 / 提议行同一形状）。
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

/// 某被编排仓库当前生效的投影开关（workflow 配置是基线；读不到就**全关**）。
///
/// 全关是安全默认：没有生效配置时宿主不做任何写回，只留 merge commit。
pub fn switches_for(host: &AppState, repo_id: &str) -> ProjectionSwitches {
    match host.workflow.status(repo_id) {
        Ok(status) => status
            .effective
            .map(|config| config.projection)
            .unwrap_or_default(),
        // 存储故障时保「全关」：宁可少投影，也不在未知配置下写回。
        Err(_) => ProjectionSwitches::default(),
    }
}

/// 机械执行投影，回传每个**开着**的开关的结局（按声明序）。
///
/// 全关时回空清单——只产 merge commit，不做任何写回。
pub fn run(host: &AppState, repo_id: &str, ticket_id: &str, merge_commit: &str) -> Vec<ProjectionOutcome> {
    let switches = switches_for(host, repo_id);
    ProjectionSwitch::ALL
        .into_iter()
        .filter(|switch| switch.is_enabled(&switches))
        .map(|switch| match switch {
            ProjectionSwitch::CloseIssue => close_issue(host, ticket_id, merge_commit),
            ProjectionSwitch::OpenPr => ProjectionOutcome::new(
                ProjectionSwitch::OpenPr,
                ProjectionStatus::Skipped,
                "v1 has no provider PR write-back (native provider is read-only); \
                 the merge commit is carried on the merge event",
            ),
            ProjectionSwitch::CommentMergeCommit => ProjectionOutcome::new(
                ProjectionSwitch::CommentMergeCommit,
                ProjectionStatus::Skipped,
                format!(
                    "v1 has no provider comment write-back (native provider is read-only); \
                     merge commit {merge_commit} is carried on the merge event"
                ),
            ),
            ProjectionSwitch::AddLabel => ProjectionOutcome::new(
                ProjectionSwitch::AddLabel,
                ProjectionStatus::Skipped,
                "v1 has no provider label write-back (native provider is read-only); \
                 the merge event payload is the projection record",
            ),
        })
        .collect()
}

/// 关票投影：把归一化票标成 `done`（host-private；`done` 不可逆）。
fn close_issue(host: &AppState, ticket_id: &str, merge_commit: &str) -> ProjectionOutcome {
    match host.tracker.close(ticket_id, "merge") {
        Ok(Some(issue)) => ProjectionOutcome::new(
            ProjectionSwitch::CloseIssue,
            ProjectionStatus::Executed,
            format!(
                "ticket {} is {} after merge commit {merge_commit}",
                issue.issue_id,
                crate::tracker::model::state_to_str(issue.state)
            ),
        ),
        Ok(None) => ProjectionOutcome::new(
            ProjectionSwitch::CloseIssue,
            ProjectionStatus::Failed,
            format!("ticket {ticket_id} is not in the normalized view; nothing to close"),
        ),
        Err(err) => ProjectionOutcome::new(
            ProjectionSwitch::CloseIssue,
            ProjectionStatus::Failed,
            format!("closing ticket {ticket_id} failed: {err}"),
        ),
    }
}
