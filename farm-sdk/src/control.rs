//! 编排 tick 的控制面（ticket #24）：暂停 / 恢复。
//!
//! GLOSSARY：编排器跑自己的确定性循环，每 tick 调用编排器契约里的服务。tick 归
//! 编排器所有，Farm 宿主只提供**编排器可见的控制面**——一个宿主持有的开关，
//! 编排器循环每 tick 读它，`paused` 就跳过本 tick 的派发（ADR-0002 §5：改调度走
//! 覆盖层/配置，不走写干预）。
//!
//! 路由进宿主管理 API（`/admin/control/tick`，面板操作）；**编排器的观测面是契约
//! 事件总线**：每次暂停/恢复都发布一条 `needs_attention`（payload 用 `kind` 细分，
//! 不新增事件种类——`events` 表的 CHECK 由 migration v5 的 `EventKind::ALL` 生成，
//! 加种类会让已落库的插入失败）。编排器循环折叠这些事件即得当前 tick 态；宿主重启后
//! 从 `GET /v1/events/history` 折叠最后一条即可恢复视图（控制面本身易失，
//! ADR-0005 §2：调度瞬态不落库）。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// tick 状态（闭集：二值，没有第三种「半暂停」）。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TickState {
    /// 正常：编排器循环每 tick 该派发就派发。
    Running,
    /// 暂停：编排器循环读到它就跳过本 tick（不派发、不回收、只等待）。
    Paused,
}

impl TickState {
    /// 闭集的权威清单。
    pub const ALL: [TickState; 2] = [TickState::Running, TickState::Paused];

    /// 契约里出现的字符串形式。
    pub fn as_str(&self) -> &'static str {
        match self {
            TickState::Running => "running",
            TickState::Paused => "paused",
        }
    }

    /// 从 wire 字符串解析（未知态返回 `None`：不猜）。
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.as_str() == text)
    }

    /// 编排器循环的判据：`true` = 本 tick 跳过派发。
    pub fn is_paused(&self) -> bool {
        matches!(self, TickState::Paused)
    }
}

impl std::fmt::Display for TickState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// tick 控制面的当前状态（查询 API 的响应）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TickControlStatus {
    pub state: TickState,
    /// 暂停理由（人读；编排器/面板可照抄进自己的日志）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 谁按的（`panel` / `orchestrator` / 人类经面板）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// 最近一次切换时间（RFC3339；从未切过则为 `None`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

impl TickControlStatus {
    /// 从未暂停过的初始态。
    pub fn running() -> Self {
        Self {
            state: TickState::Running,
            reason: None,
            actor: None,
            updated_at: None,
        }
    }
}

/// 暂停 / 恢复请求（理由可选）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TickControlRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
