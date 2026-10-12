//! Tracker 服务的契约形状（GLOSSARY：归一化 Issue 模型 + 原子 claim）。
//!
//! #13 落地 v1：provider adapter 与 claim 的 CAS 存储在 `farm-core::tracker`；
//! 此处是契约投影——`IssueView` 带 blocking 边（原语），`POST /v1/tracker/claims`
//! 未抢到时走闭集 `conflict`（信封 `details.holder` 指名当前持有者）。

use serde::{Deserialize, Serialize};

/// Issue 归一化状态（占位闭集，provider 语义关进 adapter 后收敛到这些值）。
#[derive(
    Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum IssueState {
    Open,
    /// 已被排他 claim（assignee 即 claim）。
    Claimed,
    InProgress,
    Blocked,
    Done,
}

/// 归一化后的票视图（面板与编排器读同一个视图——tracker 是总线）。
///
/// blocking 边是**原语**（ADR-0001 决策 3）：服务只陈述「谁堵谁」，
/// dispatchable / frontier 的计算归编排器。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IssueView {
    pub issue_id: String,
    pub title: String,
    pub state: IssueState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// 堵住本票的票（provider 原生依赖的声明方向）。加法字段，老客户端忽略即可。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
    /// 被本票堵住的票（反向，从全量边集推导）。加法字段。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocking: Vec<String>,
}

/// `GET /v1/tracker/issues` 的响应信封（占位）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IssueListResponse {
    pub issues: Vec<IssueView>,
}

/// 原子 claim 请求（CAS 语义；失败即已被他人抢走）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimRequest {
    pub issue_id: String,
    pub assignee: String,
}

/// 原子 claim 结果。
///
/// `claimed` 是**成功投影**：抢到（含同一 assignee 的幂等重claim）恒为 `true`。
/// 未抢到的调用方收到的是闭集 `conflict` 错误信封（409），`details.holder`
/// 是当前持有者——GLOSSARY：失败即已被他人抢走。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimResponse {
    pub issue_id: String,
    /// true = claim 成功；false = 已被他人持有（调用方收到 `conflict` 语义）。
    pub claimed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<String>,
}
