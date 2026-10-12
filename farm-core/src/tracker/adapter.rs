//! provider adapter 层：把 provider 语义（GitHub/Gogs/native）关进 adapter。
//!
//! ADR-0001 决策 3 + GLOSSARY「Tracker 服务」：服务只认归一化模型，provider 的
//! 原生形状（GitHub 的 `issue_dependencies_summary.blocked_by`、assignee、label 名）
//! 由各自的 adapter 翻译。v1 只做 **native adapter**；换 provider = 加一个
//! [`TrackerAdapter`] 实现，服务与契约不动。

use std::path::Path;

use farm_sdk::services::tracker::IssueState;
use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::tracker::model::Issue;

/// provider 名（host-private；不进契约，ADR-0005 §5）。
pub const NATIVE_PROVIDER: &str = "native";

/// provider 原生的开关状态（GitHub 的 open/closed）。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum NativeTicketState {
    #[default]
    Open,
    Closed,
}

/// native provider 的原生票记录（adapter 的输入形状）。
///
/// `blocked_by` 用 provider 的号（GitHub 依赖边的方向：本票被这些票堵住），
/// 由 adapter 翻译成归一化边。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct NativeTicket {
    /// provider 号（GitHub 的 `#13` → 归一化 `issue_id` `\"13\"`）。
    pub number: u32,
    pub title: String,
    #[serde(default)]
    pub state: NativeTicketState,
    #[serde(default)]
    pub labels: Vec<String>,
    /// provider 侧已指派的 assignee（claim 即 assignee）。
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub blocked_by: Vec<u32>,
}

/// provider adapter：拉取原生记录并翻译成归一化模型。
///
/// provider 语义**止于此层**：服务与契约看不到任何 provider 特有形状。
pub trait TrackerAdapter: Send + Sync + std::fmt::Debug {
    /// provider 名。
    fn provider(&self) -> &'static str;

    /// 拉取并翻译成归一化票（含反向 blocking 边）。
    fn fetch(&self) -> Result<Vec<Issue>, CoreError>;
}

/// native provider 的 adapter。
///
/// native = 宿主自己 authored 的票（不经 GitHub/Gogs）。v1 的票源是一份原生记录集
/// （内存或 JSON 文本）；宿主启动时为空集，记录由宿主管理面（#23）或测试夹具注入。
#[derive(Clone, Debug, Default)]
pub struct NativeAdapter {
    tickets: Vec<NativeTicket>,
}

impl NativeAdapter {
    /// 空 native provider（宿主默认：还没有 authored 票）。
    pub fn empty() -> Self {
        Self { tickets: Vec::new() }
    }

    pub fn from_tickets(tickets: Vec<NativeTicket>) -> Self {
        Self { tickets }
    }

    /// 从 provider 原生 JSON 读记录集（`[NativeTicket, ...]`）。
    pub fn from_json(text: &str) -> Result<Self, CoreError> {
        let tickets: Vec<NativeTicket> = serde_json::from_str(text)
            .map_err(|e| CoreError::Config(format!("malformed native ticket json: {e}")))?;
        Ok(Self { tickets })
    }

    /// 从文件读原生记录集。
    pub fn from_json_path(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| {
            CoreError::Config(format!(
                "cannot read native ticket source {}: {e}",
                path.display()
            ))
        })?;
        Self::from_json(&text)
    }

    /// 原生记录集（只读；测试与宿主管理面可查）。
    pub fn tickets(&self) -> &[NativeTicket] {
        &self.tickets
    }
}

impl TrackerAdapter for NativeAdapter {
    fn provider(&self) -> &'static str {
        NATIVE_PROVIDER
    }

    fn fetch(&self) -> Result<Vec<Issue>, CoreError> {
        // 号 → 归一化 id：native provider 用裸号（未来 github adapter 可用 `owner/repo#13`）。
        let id_of = |number: u32| number.to_string();

        // 先把每条原生记录翻成归一化票；反向 blocking 边随后从全量集合推导。
        let mut issues: Vec<Issue> = self
            .tickets
            .iter()
            .map(|ticket| {
                let state = match ticket.state {
                    NativeTicketState::Closed => IssueState::Done,
                    NativeTicketState::Open => match ticket.assignee {
                        // provider 侧已指派的票：归一化状态就是 claimed（assignee 即 claim）。
                        Some(_) => IssueState::Claimed,
                        None => IssueState::Open,
                    },
                };
                Issue {
                    issue_id: id_of(ticket.number),
                    title: ticket.title.clone(),
                    state,
                    labels: ticket.labels.clone(),
                    assignee: ticket.assignee.clone(),
                    blocked_by: sorted_ids(ticket.blocked_by.iter().copied().map(id_of)),
                    blocking: Vec::new(),
                    provider: NATIVE_PROVIDER.to_string(),
                    updated_at: String::new(),
                }
            })
            .collect();

        // 反向边：被本票堵住的票（次序确定：按 issue_id 排序）。
        for index in 0..issues.len() {
            let (blocked, blockers) = {
                let issue = &issues[index];
                (issue.issue_id.clone(), issue.blocked_by.clone())
            };
            for blocker in blockers {
                if let Some(target) = issues.iter_mut().find(|i| i.issue_id == blocker) {
                    target.blocking.push(blocked.clone());
                }
            }
        }
        for issue in &mut issues {
            issue.blocking.sort();
            issue.blocking.dedup();
        }

        Ok(issues)
    }
}

/// 排序去重后的 id 列表（归一化视图的次序必须确定）。
fn sorted_ids(ids: impl Iterator<Item = String>) -> Vec<String> {
    let mut ids: Vec<String> = ids.collect();
    ids.sort();
    ids.dedup();
    ids
}
