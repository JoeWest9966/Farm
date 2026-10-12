//! Workflow 配置的契约形状（ticket #12）。
//!
//! GLOSSARY「Workflow 配置」：住在被编排仓库里的机器可读调度策略，消费者是编排器
//! 的确定性循环。宿主读取/校验/保 LKG（ADR-0002 配置双层：仓库是基线，面板覆盖层
//! 叠加，#24）。**这些类型经宿主查询 API 暴露**（`GET /admin/workflow/config/{repo_id}`），
//! 所以住 `farm-sdk`；读取/校验/LKG 的宿主私有行为住 `farm-core::workflow`（ADR-0005 §5）。
//!
//! 六要素（GLOSSARY）：dispatchable 判定标签、并发配额、重试退避、stall 阈值、
//! 派发 prompt 模板、投影开关。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// 被编排仓库的 workflow 配置（`.farm/workflow.json` 解析后的形状）。
///
/// 前向兼容：未知字段被忽略（不因仓库用了更新版字段名而判非法）；但四个核心字段
/// （dispatch_labels / max_concurrency / stall_threshold_secs / prompt_template）缺失即解析
/// 失败。合法性由 `farm-core` 的语义校验判定（缺一即回退 LKG，绝不 panic）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct WorkflowConfig {
    /// dispatchable 判定标签：至少一个（否则没有票可派发）。
    pub dispatch_labels: Vec<String>,
    /// 并发配额：同时派发的 attempt 上限（>= 1）。
    pub max_concurrency: u32,
    /// stall 阈值（秒）：attempt 静默超过它即判杀（>= 1）。
    pub stall_threshold_secs: u64,
    /// 派发 prompt 模板（占位符由编排器展开，如 `{{ticket_id}}`）。
    pub prompt_template: String,
    /// 重试退避策略（可省略，用默认值）。
    #[serde(default)]
    pub retry_backoff: RetryBackoff,
    /// 投影开关：attempt 完成后宿主机械执行的投影动作（归 #18 merge 服务；可省略）。
    #[serde(default)]
    pub projection: ProjectionSwitches,
}

/// 分级指数退避（编排器重试策略；整数倍以保持 `Eq`）。缺字段用默认值（前向兼容）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(default)]
pub struct RetryBackoff {
    /// 最大重试次数（0 = 不重试）。
    pub max_retries: u32,
    /// 基础延迟（毫秒；启用重试时 >= 1）。
    pub base_delay_ms: u64,
    /// 退避倍率（>= 1；2 = 每次翻倍）。
    pub backoff_multiplier: u32,
}

impl Default for RetryBackoff {
    fn default() -> Self {
        Self {
            max_retries: 0,
            base_delay_ms: 0,
            backoff_multiplier: 1,
        }
    }
}

/// 投影开关：宿主在 attempt 完成后机械执行的投影动作（ADR-0001 决策 2，归 #18）。
/// 全关 = 只产 merge commit，不关票/不开 PR/不评论/不打标签。缺字段默认 false（前向兼容）。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema)]
#[serde(default)]
pub struct ProjectionSwitches {
    /// 完成后关票。
    pub close_issue: bool,
    /// 完成后开 PR。
    pub open_pr: bool,
    /// 完成后评论 merge commit 链接。
    pub comment_merge_commit: bool,
    /// 完成后打标签。
    pub add_label: bool,
}

impl Default for ProjectionSwitches {
    fn default() -> Self {
        Self {
            close_issue: false,
            open_pr: false,
            comment_merge_commit: false,
            add_label: false,
        }
    }
}

/// 当前生效配置的来源。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveSource {
    /// 仓库当前配置合法，直接生效。
    Fresh,
    /// 仓库当前配置非法，回退到 last-known-good 生效。
    LastKnownGood,
}

/// last-known-good 的落库信息（per-仓库，ADR-0005 §2/§6）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LastKnownGoodInfo {
    /// 最近一次写入/更新时间（RFC3339）。
    pub updated_at: String,
}

/// 查询 API 的响应：某被编排仓库的当前生效配置 + LKG 状态。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkflowConfigStatus {
    pub repo_id: String,
    /// 当前生效配置；`None` = 尚无生效配置（首次即非法，拒绝编排）。
    pub effective: Option<WorkflowConfig>,
    /// 生效配置来源。
    pub source: Option<EffectiveSource>,
    /// 本次生效配置的装载时间（RFC3339）。
    pub loaded_at: Option<String>,
    /// 落库的 last-known-good 信息（重启后仍可引导）。
    pub last_known_good: Option<LastKnownGoodInfo>,
    /// 最近一次配置告警的原因（非法回退/拒绝时）。
    pub last_alert: Option<String>,
}
