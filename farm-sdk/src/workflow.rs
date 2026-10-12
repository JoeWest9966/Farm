//! Workflow 配置契约形状（GLOSSARY：Workflow 配置住在被编排仓库，非法时保 LKG）。
//!
//! 消费者是编排器的确定性循环；宿主读取并校验（见 `farm-core::workflow`），契约客户端
//! 经宿主查询 API 读**当前生效配置**与 **LKG 状态**。域类型进 farm-sdk 是因为查询 API
//! 暴露它们（ADR-0005 §5：SQLite schema 永不外泄，但契约响应形状是稳定面）。
//!
//! 六个要素（GLOSSARY「Workflow 配置」）：dispatchable 判定标签、并发配额、重试退避、
//! stall 阈值、派发 prompt 模板、投影开关。语义校验规则归 `farm-core::workflow::config`
//! （宿主私有，不进契约）；这里只固定**形状**。
//!
//! 解析从宽：不使用 `deny_unknown_fields`，仓库配置里多出的字段被忽略（前向兼容），
//! 缺必需字段或语义非法才由宿主判为非法并回退 LKG。

use serde::{Deserialize, Serialize};

/// Workflow 配置：被编排仓库里的机器可读调度策略文件。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
pub struct WorkflowConfig {
    /// dispatchable 判定标签：带这些标签的票才可派发（宿主校验非空）。
    pub dispatch_labels: Vec<String>,
    /// 并发配额：同时在跑的 attempt 上限（宿主校验 ≥ 1）。
    pub max_concurrency: u32,
    /// 重试退避策略。
    pub retry_backoff: RetryBackoff,
    /// stall 阈值（秒）：两口钟的静默钟下限（宿主校验 ≥ 1）。
    pub stall_threshold_secs: u64,
    /// 派发 prompt 模板（宿主校验非空）。
    pub prompt_template: String,
    /// 投影开关（如是否开 PR / 关票）。
    pub projection: ProjectionSwitches,
}

/// 重试退避策略。用整数倍率（非浮点）避免跨端序列化漂移。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
pub struct RetryBackoff {
    /// 最大重试次数（0 = 不重试）。
    pub max_retries: u32,
    /// 基础退避延迟（毫秒）。
    pub base_delay_ms: u64,
    /// 退避倍率（≥ 1；线性 = 1，指数 = 2）。
    pub backoff_multiplier: u32,
}

/// 投影开关（GLOSSARY「投影」：完成信号触发后的确定性写回，由宿主机械执行、workflow 配置开关）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
pub struct ProjectionSwitches {
    /// 是否在 merge 后开 PR。
    pub open_pr: bool,
    /// 是否在 merge 后关票。
    pub close_issue: bool,
}

/// 生效来源：当前生效配置是新读入的合法值，还是回退到的 last-known-good。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveSource {
    /// 仓库当前配置合法，直接生效（并成为新的 LKG）。
    Fresh,
    /// 仓库当前配置非法，回退到 last-known-good（含重启后从 SQLite 恢复的 LKG）。
    LastKnownGood,
}

/// last-known-good 落库信息（宿主私有存储，per-仓库）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LastKnownGoodInfo {
    /// LKG 最近一次写入/更新时间（RFC3339）。
    pub updated_at: String,
}

/// 当前生效配置 + LKG 状态的查询响应（宿主查询 API 的形状）。
///
/// 三种形态：
/// - 合法新读入：`effective` 有值、`source = fresh`、`last_alert` 空；
/// - 非法回退：`effective` 有值（= LKG）、`source = last_known_good`、`last_alert` 带原因；
/// - 首次即非法：`effective` 为空、`last_known_good` 为空、`last_alert` 带原因（拒绝编排）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkflowConfigStatus {
    pub repo_id: String,
    /// 当前生效配置（`None` = 尚无生效配置：首次即非法，拒绝编排 / needs_attention）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective: Option<WorkflowConfig>,
    /// 生效来源（`effective` 为 `None` 时无意义，不出现在 JSON 里）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<EffectiveSource>,
    /// 生效值装载时间（RFC3339）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loaded_at: Option<String>,
    /// LKG 落库信息（`None` = 从未有过合法配置）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_known_good: Option<LastKnownGoodInfo>,
    /// 最近一次回退/拒绝的原因（告警文案）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_alert: Option<String>,
}
