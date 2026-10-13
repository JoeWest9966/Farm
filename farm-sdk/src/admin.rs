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

/// tracker provider / adapter 配置（宿主管理面视图，ticket #23）。
///
/// GLOSSARY「Tracker 服务」：provider 语义（GitHub/Gogs/native）关进 adapter，服务与契约不动。
/// provider 名是 **host-private**（ADR-0005 §5）——它进宿主管理 API 的投影，**不进契约路由**。
/// v1 唯一 adapter 是 `native`；换 provider = 加一个 `TrackerAdapter` 实现。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TrackerProviderConfig {
    /// 当前选中的 provider / adapter 名（未设置时默认 `native`）。
    pub provider: String,
    /// 可选 adapter 清单（面板据此渲染选择器；host-private）。
    pub known_providers: Vec<String>,
}

/// 设置 tracker provider / adapter 的请求体（ticket #23）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TrackerProviderSelection {
    /// 要切换到的 provider / adapter 名（未知名 → 闭集 `invalid_request`）。
    pub provider: String,
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

/// 服务的运行态（宿主管理面「服务启停」，ticket #23）：二值闭集。
///
/// 与 [`PluginState`] 三态闭集**不同**：服务是宿主自身的五服务（ADR-0001），没有
/// quarantined 语义（那是插件进程崩溃重试耗尽的状态，GLOSSARY「隔离」）。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ServiceRunState {
    /// 运行中（默认；宿主启动即五服务俱在）。
    Running,
    /// 被人经管理面停机（易失宿主持有开关，重启后回到 running）。
    Stopped,
}

impl ServiceRunState {
    /// 闭集的权威清单。
    pub const ALL: [ServiceRunState; 2] = [ServiceRunState::Running, ServiceRunState::Stopped];

    /// 契约 / 存储里出现的字符串形式。
    pub fn as_str(&self) -> &'static str {
        match self {
            ServiceRunState::Running => "running",
            ServiceRunState::Stopped => "stopped",
        }
    }

    /// 从 wire 字符串解析（未知态返回 `None`：不猜）。
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.as_str() == text)
    }
}

impl std::fmt::Display for ServiceRunState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 单个服务的启停状态（宿主管理面投影，ticket #23）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServiceControlStatusEntry {
    pub service: crate::services::ServiceName,
    pub state: ServiceRunState,
    /// 最近一次启停切换时间（RFC3339；从未切过则为 `None`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// 服务启停控制面的当前状态（`GET /admin/services` 响应；五服务俱全）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServiceControlStatus {
    pub services: Vec<ServiceControlStatusEntry>,
}
