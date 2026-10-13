//! 通知通道（Notification Channel）的契约形状（ticket #21，GLOSSARY「通知通道 / Notifier 插件」）。
//!
//! GLOSSARY：关键事件（`blocked`、`needs_attention`、merge 完成、`stall`）的**宿主侧外发出口**，
//! 可插拔。事件集闭包 = 上述四类关键事件保证进事件总线且可订阅；默认过滤集 =
//! blocked / needs_attention / stall 开、merge 完成关（常态流量，票看板已是其投影）。
//!
//! 这些类型经宿主管理 API 暴露（`GET/POST /admin/notifier/channel/{repo_id}`），所以住
//! `farm-sdk`；读写行为（SQLite、默认化）住 `farm-core::notifier`（ADR-0005 §5：宿主私有
//! 实现细节不外泄）。**宿主零推送域知识**：通道配置只有「开不开 + 转哪些关键事件」，
//! 没有一个 URL / header / body 模板 / 重试 / 限流字段——那些是插件侧自决载体
//! （GLOSSARY「Notifier 插件」：第一方只做一个通用 HTTP notifier，推送格式全住插件侧）。
//!
//! ## 为什么过滤集是四个显式开关
//! 与覆盖层键闭集同一纪律：哪几类关键事件外发是**可机械审查**的策略，做成四个具名开关
//! （而非一个自由列表）让「默认转发什么」有单一源（[`NotifierFilter::default`]），也让
//! 存储层的 `CHECK` 约束有据可依（farm-core 的 migration v10）。

use serde::{Deserialize, Serialize};

use crate::events::EventKind;

/// 通知通道的过滤集：四类关键事件各一个显式开关（GLOSSARY「通知通道」默认过滤集）。
///
/// wire 字符串为 snake_case，与 [`EventKind`] 的关键成员一一对应（`stall` → `stall_kill`、
/// `merge_complete` → `merge`）。四个开关都是**必填**：缺席即形状不成立（与
/// [`crate::overlay::OverlayEntry::temporary`] 同纪律，不猜默认）。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NotifierFilter {
    /// `blocked`：attempt 卡住、claim 保留（一等事件）。
    pub blocked: bool,
    /// `needs_attention`：需要人类注意（一等事件）。
    pub needs_attention: bool,
    /// `stall`：两口钟超时判杀（[`EventKind::StallKill`]）。
    pub stall: bool,
    /// `merge_complete`：merge 服务执行了真正的 merge（[`EventKind::Merge`]；默认关）。
    pub merge_complete: bool,
}

impl NotifierFilter {
    /// 默认过滤集（GLOSSARY）：blocked / needs_attention / stall 开、merge 完成关。
    pub fn default_filter() -> Self {
        Self {
            blocked: true,
            needs_attention: true,
            stall: true,
            merge_complete: false,
        }
    }

    /// 某事件种类是否被这个过滤集放行。
    ///
    /// 四类关键事件各映射到一个开关；**闭包之外的种类一律 `false`**——通知通道只关心
    /// 关键事件，别的回流（派发、claim、插件重启…）不外发。
    pub fn enabled(&self, kind: EventKind) -> bool {
        match kind {
            EventKind::Blocked => self.blocked,
            EventKind::NeedsAttention => self.needs_attention,
            EventKind::StallKill => self.stall,
            EventKind::Merge => self.merge_complete,
            _ => false,
        }
    }
}

impl Default for NotifierFilter {
    fn default() -> Self {
        Self::default_filter()
    }
}

/// 通知通道配置（per-仓库；宿主管理 API 的投影形状，migration v10 的存储面）。
///
/// **宿主零推送域知识**：只有 `enabled`（总闸）+ `filter`（转哪些关键事件）+ `repo_id`
/// + `updated_at`。载体（URL/headers/body 模板/重试/限流）不住这里——那是插件侧自决
/// （见模块文档）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NotificationChannelConfig {
    /// 被编排仓库 id（per-仓库 keyed，ADR-0005 §6）。
    pub repo_id: String,
    /// 通道总闸：关掉后插件不外发任何事件（即便过滤集放行）。
    pub enabled: bool,
    /// 过滤集：四类关键事件各一个开关。
    pub filter: NotifierFilter,
    /// 最近一次写入/更新时间（RFC3339；宿主张章）。
    pub updated_at: String,
}

/// 设置通知通道配置的请求体（宿主管理 API `POST /admin/notifier/channel/{repo_id}`）。
///
/// 只带 `enabled` + `filter`：`repo_id` 在路径上，`updated_at` 由宿主张章（调用方不能
/// 自定时间戳）。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NotificationChannelUpdate {
    pub enabled: bool,
    pub filter: NotifierFilter,
}
