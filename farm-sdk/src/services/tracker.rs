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

/// release 请求：交还排他所有权（GLOSSARY：claim 是票据状态，不是技术锁）。
///
/// 与 claim 同形：`assignee` 是**交还者**。持着票的是别人 → 闭集 `conflict`
/// （409，`details.holder` 指名当前持有者）；无人持有 → 幂等空操作
/// （[`ReleaseOutcome::NotClaimed`）；票不在视图里 → 闭集 `not_found`。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReleaseRequest {
    pub issue_id: String,
    pub assignee: String,
}

/// release 的结局投影（闭集）。
///
/// 只有两种**成功**投影：真的交还了、本来就没人持有。第三种可能（持票的是别人）
/// 不是结局而是闭集 `conflict` 错误——投影与错误不混在一个枚举里。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseOutcome {
    /// 持有者本人释放：之后他人可 claim。
    Released,
    /// 无人持有：重复 release 的幂等空操作（不报错，也不写新历史）。
    NotClaimed,
}

impl ReleaseOutcome {
    /// 闭集的权威清单。
    pub const ALL: [ReleaseOutcome; 2] = [ReleaseOutcome::Released, ReleaseOutcome::NotClaimed];

    /// 契约里出现的字符串形式。
    pub fn as_str(&self) -> &'static str {
        match self {
            ReleaseOutcome::Released => "released",
            ReleaseOutcome::NotClaimed => "not_claimed",
        }
    }
}

impl std::fmt::Display for ReleaseOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// release 结果：票 + 结局投影。`holder` 不进契约——release 的目的状态就是无人持有。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReleaseResponse {
    pub issue_id: String,
    pub outcome: ReleaseOutcome,
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
