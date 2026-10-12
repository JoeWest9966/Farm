//! 覆盖层（Overlay）的契约形状（ticket #24）。
//!
//! GLOSSARY「覆盖层」：面板对 workflow 配置基线的**临时运行时覆盖**——显式标记、
//! 重启后提示、不静默污染仓库文件。ADR-0002 决策 2（配置双层：仓库文件是基线，
//! 面板可施加覆盖层）+ ADR-0005 §6（覆盖层落 Farm SQLite，per-仓库 keyed，
//! **不落被编排仓库**）。
//!
//! 这些类型经宿主管理 API 暴露（`GET /admin/workflow/overlays/{repo_id}` 等），
//! 所以住 `farm-sdk`；读写行为（SQLite、事件回流、重启提示）住
//! `farm-core::overlay`（ADR-0005 §5：宿主私有实现细节不外泄）。
//!
//! ## 为什么键是闭集
//! 覆盖层改的是 workflow 配置的**六要素**（GLOSSARY）：dispatchable 判定标签、
//! 并发配额、重试退避、stall 阈值、派发 prompt 模板、投影开关。闭集让「哪一格
//! 能被临时改」可机械审查，也让值校验有单一源（[`validate_value`]）——自由键会把
//! 覆盖层变成第二个配置文件，那就分不清哪句是基线哪句是覆盖了。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::workflow::WorkflowConfig;

/// 覆盖层的临时性标记（ADR-0002 §2：**显式标记**「这是临时覆盖」）。
///
/// 单一源：宿主写库时带上，面板渲染时原文照抄——标记不是暗示，是文案。
pub const OVERLAY_TEMPORARY_MARKER: &str =
    "这是临时覆盖（temporary overlay）：运行时值，不写回仓库文件，重启后提示恢复基线";

/// 覆盖层键闭集：workflow 配置六要素的可覆盖字段。
///
/// wire 字符串与 [`OverlayKey::as_str`] 一致（`retry_backoff.` / `projection.`
/// 两级点号路径，直接对着配置字段念）。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema)]
pub enum OverlayKey {
    #[serde(rename = "max_concurrency")]
    MaxConcurrency,
    #[serde(rename = "stall_threshold_secs")]
    StallThresholdSecs,
    #[serde(rename = "dispatch_labels")]
    DispatchLabels,
    #[serde(rename = "prompt_template")]
    PromptTemplate,
    #[serde(rename = "retry_backoff.max_retries")]
    RetryMaxRetries,
    #[serde(rename = "retry_backoff.base_delay_ms")]
    RetryBaseDelayMs,
    #[serde(rename = "retry_backoff.backoff_multiplier")]
    RetryBackoffMultiplier,
    #[serde(rename = "projection.close_issue")]
    ProjectionCloseIssue,
    #[serde(rename = "projection.open_pr")]
    ProjectionOpenPr,
    #[serde(rename = "projection.comment_merge_commit")]
    ProjectionCommentMergeCommit,
    #[serde(rename = "projection.add_label")]
    ProjectionAddLabel,
}

impl OverlayKey {
    /// 闭集的权威清单（加键 = 加法契约变更）。
    pub const ALL: [OverlayKey; 11] = [
        OverlayKey::MaxConcurrency,
        OverlayKey::StallThresholdSecs,
        OverlayKey::DispatchLabels,
        OverlayKey::PromptTemplate,
        OverlayKey::RetryMaxRetries,
        OverlayKey::RetryBaseDelayMs,
        OverlayKey::RetryBackoffMultiplier,
        OverlayKey::ProjectionCloseIssue,
        OverlayKey::ProjectionOpenPr,
        OverlayKey::ProjectionCommentMergeCommit,
        OverlayKey::ProjectionAddLabel,
    ];

    /// 契约里出现的字符串形式。
    pub fn as_str(&self) -> &'static str {
        match self {
            OverlayKey::MaxConcurrency => "max_concurrency",
            OverlayKey::StallThresholdSecs => "stall_threshold_secs",
            OverlayKey::DispatchLabels => "dispatch_labels",
            OverlayKey::PromptTemplate => "prompt_template",
            OverlayKey::RetryMaxRetries => "retry_backoff.max_retries",
            OverlayKey::RetryBaseDelayMs => "retry_backoff.base_delay_ms",
            OverlayKey::RetryBackoffMultiplier => "retry_backoff.backoff_multiplier",
            OverlayKey::ProjectionCloseIssue => "projection.close_issue",
            OverlayKey::ProjectionOpenPr => "projection.open_pr",
            OverlayKey::ProjectionCommentMergeCommit => "projection.comment_merge_commit",
            OverlayKey::ProjectionAddLabel => "projection.add_label",
        }
    }

    /// 从 wire 字符串解析（未知键返回 `None`：不猜）。
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|key| key.as_str() == text)
    }

    /// 这个键接受的值种类（面板据此给输入框，宿主据此校验）。
    pub fn value_kind(&self) -> OverlayValueKind {
        match self {
            OverlayKey::MaxConcurrency | OverlayKey::RetryMaxRetries => OverlayValueKind::Count,
            OverlayKey::StallThresholdSecs | OverlayKey::RetryBaseDelayMs => {
                OverlayValueKind::Seconds
            }
            OverlayKey::RetryBackoffMultiplier => OverlayValueKind::Count,
            OverlayKey::DispatchLabels => OverlayValueKind::Labels,
            OverlayKey::PromptTemplate => OverlayValueKind::Text,
            OverlayKey::ProjectionCloseIssue
            | OverlayKey::ProjectionOpenPr
            | OverlayKey::ProjectionCommentMergeCommit
            | OverlayKey::ProjectionAddLabel => OverlayValueKind::Flag,
        }
    }

    /// 人读的一句话（面板标签 / 错误消息）。
    pub fn label(&self) -> &'static str {
        match self {
            OverlayKey::MaxConcurrency => "并发配额",
            OverlayKey::StallThresholdSecs => "stall 阈值（秒）",
            OverlayKey::DispatchLabels => "dispatchable 判定标签",
            OverlayKey::PromptTemplate => "派发 prompt 模板",
            OverlayKey::RetryMaxRetries => "最大重试次数",
            OverlayKey::RetryBaseDelayMs => "重试基础延迟（毫秒）",
            OverlayKey::RetryBackoffMultiplier => "退避倍率",
            OverlayKey::ProjectionCloseIssue => "投影：完成后关票",
            OverlayKey::ProjectionOpenPr => "投影：完成后开 PR",
            OverlayKey::ProjectionCommentMergeCommit => "投影：完成后评论 merge commit",
            OverlayKey::ProjectionAddLabel => "投影：完成后打标签",
        }
    }
}

impl std::fmt::Display for OverlayKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 覆盖层值的种类（闭集）：决定 JSON 形状与取值范围。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OverlayValueKind {
    /// 计数（>= 1 的整数）。
    Count,
    /// 秒数（>= 1 的整数；`retry_backoff.base_delay_ms` 是毫秒，同形状）。
    Seconds,
    /// 标签清单（至少一个非空字符串）。
    Labels,
    /// 文本（非空白）。
    Text,
    /// 开关（布尔）。
    Flag,
}

impl OverlayValueKind {
    /// 契约里出现的字符串形式。
    pub fn as_str(&self) -> &'static str {
        match self {
            OverlayValueKind::Count => "count",
            OverlayValueKind::Seconds => "seconds",
            OverlayValueKind::Labels => "labels",
            OverlayValueKind::Text => "text",
            OverlayValueKind::Flag => "flag",
        }
    }
}

/// 值校验（单一源：宿主写库前校验，面板输入框也按它预检）。
///
/// 返回 `Err` 时带人读原因（进闭集 `invalid_request` 的消息）。**不静默强转**：
/// 把 `"2"` 塞进计数键是拒绝，不是当成 2——覆盖层是显式操作，猜口径比报错更糟。
///
/// 逐键判据（与 [`crate::workflow`] 的语义校验同源，见 `farm-core` 的
/// `workflow::config::validate`）：并发/stall/退避倍率至少 1；重试次数与基础延迟
/// 可以是 0（0 = 不重试，`RetryBackoff::default()` 就是这样）；跨字段约束
/// （启用重试时基础延迟必须 >= 1）由宿主在**合并后**校验生效配置时把门。
pub fn validate_value(key: OverlayKey, value: &Value) -> Result<(), String> {
    match key {
        OverlayKey::MaxConcurrency | OverlayKey::StallThresholdSecs => {
            integer_at_least(key, value, 1)
        }
        OverlayKey::RetryBackoffMultiplier => integer_at_least(key, value, 1),
        OverlayKey::RetryMaxRetries | OverlayKey::RetryBaseDelayMs => {
            integer_at_least(key, value, 0)
        }
        OverlayKey::DispatchLabels => match value.as_array() {
            Some(labels) if !labels.is_empty() => {
                let all_text = labels
                    .iter()
                    .all(|label| label.as_str().is_some_and(|text| !text.trim().is_empty()));
                if all_text {
                    Ok(())
                } else {
                    Err(format!("{key} 的每一项都是非空字符串，收到 {value}"))
                }
            }
            _ => Err(format!("{key} 需要至少一个标签，收到 {value}")),
        },
        OverlayKey::PromptTemplate => match value.as_str() {
            Some(text) if !text.trim().is_empty() => Ok(()),
            _ => Err(format!("{key} 需要非空白文本，收到 {value}")),
        },
        OverlayKey::ProjectionCloseIssue
        | OverlayKey::ProjectionOpenPr
        | OverlayKey::ProjectionCommentMergeCommit
        | OverlayKey::ProjectionAddLabel => match value.as_bool() {
            Some(_) => Ok(()),
            None => Err(format!("{key} 需要布尔值，收到 {value}")),
        },
    }
}

/// 整数下界校验（计数/秒数共用）。
fn integer_at_least(key: OverlayKey, value: &Value, min: u64) -> Result<(), String> {
    match value.as_u64() {
        Some(n) if n >= min => Ok(()),
        Some(_) => Err(format!("{key} 至少为 {min}")),
        None => Err(format!("{key} 需要一个整数，收到 {value}")),
    }
}

/// 一条覆盖层（查询 API 的清单项）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OverlayEntry {
    pub repo_id: String,
    pub key: OverlayKey,
    pub value: Value,
    /// 人读理由（谁为什么临时改这一格）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// 显式标记：**这是临时覆盖**（ADR-0002 §2）。必填——标记不允许缺席。
    pub temporary: bool,
    /// 最近一次写入/更新时间（RFC3339）。
    pub updated_at: String,
}

/// 设置一条覆盖层。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OverlaySetRequest {
    pub key: OverlayKey,
    pub value: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// 清除覆盖层：给 `key` 清一条，不给清该仓库全部。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OverlayClearRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<OverlayKey>,
}

/// 清除结果（回报真清了哪几条）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OverlayClearResponse {
    pub repo_id: String,
    pub cleared: Vec<OverlayKey>,
}

/// 覆盖层生效视图（查询 API 的响应）：基线 + 覆盖层 + 生效值。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OverlayStatus {
    pub repo_id: String,
    /// 被编排仓库的基线配置（workflow 当前配置 / LKG；从未装载则为 `None`）。
    pub baseline: Option<WorkflowConfig>,
    /// 生效配置 = 基线 + 覆盖层。
    pub effective: Option<WorkflowConfig>,
    /// 是否施加了覆盖层（`false` = 生效值就是基线）。
    pub overlay_applied: bool,
    /// 覆盖层清单（每条都带临时标记）。
    pub overlays: Vec<OverlayEntry>,
    /// 重启后提示恢复基线（有覆盖层时就有这句话；`None` = 无需提示）。
    pub restore_prompt: Option<String>,
}
