//! Tracker 服务的占位契约形状（GLOSSARY：归一化 Issue 模型 + 原子 claim）。
//!
//! #13 负责 provider adapter 与 claim 的 CAS 存储；此处只固定路由形状与信封。

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
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimResponse {
    pub issue_id: String,
    /// true = claim 成功；false = 已被他人持有（调用方收到 `conflict` 语义）。
    pub claimed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<String>,
}
