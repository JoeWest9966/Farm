//! 宿主管理 API（Host Admin API）的占位契约形状。
//!
//! GLOSSARY：面板专用的宿主自身管理面（插件装卸、tracker 配置、服务启停），
//! 与编排器契约并存但边界分明。本票只留**接口位**：独立前缀 `/admin` +
//! 独立 token header；管理 token 机制本身归 #23。

use serde::{Deserialize, Serialize};

/// 插件状态（占位闭集；`quarantined` 见 GLOSSARY「隔离」）。
///
/// 宿主侧插件监督状态（ticket #19 的 `plugin_state` 表）与宿主管理 API 的投影共用
/// 同一个闭集，所以它是插件管理面的单一源：`CHECK` 约束由 [`PluginState::ALL`] 生成。
#[derive(
    Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PluginState {
    Running,
    Stopped,
    /// 重试耗尽后被隔离；宿主停止重启、发 `needs_attention`、面板可一键恢复。
    Quarantined,
}

impl PluginState {
    /// 闭集的权威清单（`plugin_state` 表 `CHECK` 的单一源）。
    pub const ALL: [PluginState; 3] = [
        PluginState::Running,
        PluginState::Stopped,
        PluginState::Quarantined,
    ];

    /// 契约 / 存储里出现的字符串形式。
    pub fn as_str(&self) -> &'static str {
        match self {
            PluginState::Running => "running",
            PluginState::Stopped => "stopped",
            PluginState::Quarantined => "quarantined",
        }
    }
}

impl std::fmt::Display for PluginState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 插件摘要。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PluginSummary {
    pub plugin_id: String,
    pub name: String,
    pub state: PluginState,
    #[serde(default)]
    pub restarts: u32,
}

/// `GET /admin/plugins` 的响应信封（占位）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminPluginListResponse {
    pub plugins: Vec<PluginSummary>,
}

/// 宿主管理 API 自描述（面板据此知道管理面在跑、契约版本是什么）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminStatusResponse {
    pub host_version: String,
    pub contract_version: String,
    /// 宿主监听地址（管理面与契约面同进程同端口，只是前缀不同）。
    pub bind_addr: String,
    pub services: Vec<crate::services::ServiceName>,
}
