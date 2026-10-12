//! 五服务（ADR-0001 v1 五服务）的契约名与占位路由形状。
//!
//! 本票只落地**形状**：请求/响应信封 + 路由清单是单一源，宿主按它们挂占位路由
//! （返回闭集 `not_implemented`）。服务域字段由 #13/#14/#15/#18 加法填充；
//! 加法字段一律 `#[serde(default)]`，保证对已发布契约向后兼容。

pub mod agent_runner;
pub mod events;
pub mod merge;
pub mod tracker;
pub mod workspace;

use serde::{Deserialize, Serialize};

/// v1 五服务（闭集）。
#[derive(
    Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ServiceName {
    /// Tracker 服务：归一化 Issue 模型 + 原子 claim。
    Tracker,
    /// Agent-Runner：统一接口派发与管理 agent。
    AgentRunner,
    /// Merge 服务：宿主侧确定性合并执行者。
    Merge,
    /// Workspace 服务：spec ↔ 集成分支 ↔ worktrees 映射。
    Workspace,
    /// 事件总线：统一事件回流面。
    Events,
}

impl ServiceName {
    pub const ALL: [ServiceName; 5] = [
        ServiceName::Tracker,
        ServiceName::AgentRunner,
        ServiceName::Merge,
        ServiceName::Workspace,
        ServiceName::Events,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            ServiceName::Tracker => "tracker",
            ServiceName::AgentRunner => "agent-runner",
            ServiceName::Merge => "merge",
            ServiceName::Workspace => "workspace",
            ServiceName::Events => "events",
        }
    }

    /// 该服务在契约前缀下的路径片段。
    pub fn path_segment(&self) -> &'static str {
        self.as_str()
    }
}

impl std::fmt::Display for ServiceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
