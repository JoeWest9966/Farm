//! 事件总线（Event Bus）契约：事件闭集与事件信封。
//!
//! GLOSSARY：`blocked` / `needs_attention` 是一等事件；ADR-0005 §3：关键事件
//! （派发、merge、审批、stall 判杀、host_restarted）进审计双写。闭集在此单一源定义，
//! 宿主、面板、编排器三方共用；新增事件种类是**加法**契约变更。
//!
//! ticket #19 按 ADR-0004 决策 4「崩溃/重启进 Kron 账（escalation，`actor=farm`）」
//! **加法**追加 `plugin_restarted` 并纳入审计双写闭集——插件崩溃重启因此与
//! stall 判杀、宿主重启同路进 Kron（`escalation`），未入账只标记、不阻塞。

use serde::{Deserialize, Serialize};

/// 事件种类闭集。
#[derive(
    Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// 派发：agent-runner 起了 attempt（审计双写关键事件）。
    Dispatch,
    /// attempt 生命周期占位：已启动。
    AttemptStarted,
    /// attempt 生命周期占位：判据阶梯判终态（`agent_settled`）。
    AgentSettled,
    /// attempt 生命周期占位：已结束（成功/失败/interrupted 之外的收尾）。
    AttemptFinished,
    /// 一等事件：attempt 卡住，claim 保留，人类经 steer/follow_up 回应。
    Blocked,
    /// 一等事件：需要人类注意（插件隔离、取凭据失败等）。
    NeedsAttention,
    /// steer：人类/编排器向运行中的 attempt 注入指令。
    Steer,
    /// abort：探针/人工中止 attempt。
    Abort,
    /// stall 判杀：两口钟超时后的判杀（审计双写关键事件）。
    StallKill,
    /// merge：merge 服务执行了真正的 merge（审计双写关键事件）。
    Merge,
    /// 审批：人工闸门放行 merge 提议（审计双写关键事件）。
    Approval,
    /// 中断态：宿主重启时 running attempt 被标记的记账状态（第四终态）。
    Interrupted,
    /// 宿主重启（审计双写关键事件；ADR-0005 §4 崩溃恢复）。
    HostRestarted,
    /// 原子 claim 成功（CAS 语义）。
    ClaimAcquired,
    /// 插件重试耗尽进入隔离（quarantined）。
    PluginQuarantined,
    /// 插件崩溃后按分级退避重启（ADR-0004 决策 4；进审计双写 → Kron escalation）。
    PluginRestarted,
    /// 通知通道外发完成（HITL 前半程）。
    NotificationSent,
}

impl EventKind {
    /// 闭集的权威清单。
    pub const ALL: [EventKind; 17] = [
        EventKind::Dispatch,
        EventKind::AttemptStarted,
        EventKind::AgentSettled,
        EventKind::AttemptFinished,
        EventKind::Blocked,
        EventKind::NeedsAttention,
        EventKind::Steer,
        EventKind::Abort,
        EventKind::StallKill,
        EventKind::Merge,
        EventKind::Approval,
        EventKind::Interrupted,
        EventKind::HostRestarted,
        EventKind::ClaimAcquired,
        EventKind::PluginQuarantined,
        EventKind::PluginRestarted,
        EventKind::NotificationSent,
    ];

    /// 契约里出现的字符串形式。
    pub fn as_str(&self) -> &'static str {
        match self {
            EventKind::Dispatch => "dispatch",
            EventKind::AttemptStarted => "attempt_started",
            EventKind::AgentSettled => "agent_settled",
            EventKind::AttemptFinished => "attempt_finished",
            EventKind::Blocked => "blocked",
            EventKind::NeedsAttention => "needs_attention",
            EventKind::Steer => "steer",
            EventKind::Abort => "abort",
            EventKind::StallKill => "stall_kill",
            EventKind::Merge => "merge",
            EventKind::Approval => "approval",
            EventKind::Interrupted => "interrupted",
            EventKind::HostRestarted => "host_restarted",
            EventKind::ClaimAcquired => "claim_acquired",
            EventKind::PluginQuarantined => "plugin_quarantined",
            EventKind::PluginRestarted => "plugin_restarted",
            EventKind::NotificationSent => "notification_sent",
        }
    }

    /// GLOSSARY「通知通道」的事件集闭包：这四类关键事件保证进总线且可订阅。
    pub const NOTIFICATION_CRITICAL: [EventKind; 4] = [
        EventKind::Blocked,
        EventKind::NeedsAttention,
        EventKind::Merge,
        EventKind::StallKill,
    ];

    /// 是否属于通知通道的事件集闭包。
    pub fn is_notification_critical(&self) -> bool {
        Self::NOTIFICATION_CRITICAL.contains(self)
    }

    /// ADR-0005 §3：是否进审计双写（Kron 账本）。
    ///
    /// ticket #19 按 ADR-0004 决策 4（崩溃/重启进 Kron 账，escalation，`actor=farm`）
    /// 把 `plugin_restarted` 纳入闭集：插件崩溃后的分级退避重启与 stall 判杀、
    /// 宿主重启同路入账。
    pub fn is_audit_critical(&self) -> bool {
        matches!(
            self,
            EventKind::Dispatch
                | EventKind::Merge
                | EventKind::Approval
                | EventKind::StallKill
                | EventKind::HostRestarted
                | EventKind::PluginRestarted
        )
    }
}

impl std::fmt::Display for EventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 待发布事件：id / occurred_at / audited 由宿主的事件总线盖章。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewEvent {
    pub kind: EventKind,
    /// 事件产生方（host / agent-runner / orchestrator / human / plugin…）。
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    /// 开放形状载荷；闭集只在 `kind` 上。
    #[serde(default)]
    pub payload: serde_json::Value,
}

impl NewEvent {
    pub fn new(kind: EventKind, actor: impl Into<String>) -> Self {
        Self {
            kind,
            actor: actor.into(),
            ticket_id: None,
            attempt_id: None,
            payload: serde_json::Value::Null,
        }
    }

    pub fn with_ticket(mut self, ticket_id: impl Into<String>) -> Self {
        self.ticket_id = Some(ticket_id.into());
        self
    }

    pub fn with_attempt(mut self, attempt_id: impl Into<String>) -> Self {
        self.attempt_id = Some(attempt_id.into());
        self
    }

    pub fn with_payload(mut self, payload: serde_json::Value) -> Self {
        self.payload = payload;
        self
    }
}

/// 事件信封：总线上的唯一形状，WS 文本帧与事件历史查询共用。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EventEnvelope {
    /// 宿主分配的事件 id（单调/唯一）。
    pub id: String,
    pub kind: EventKind,
    /// RFC3339 时间戳。
    pub occurred_at: String,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(default)]
    pub payload: serde_json::Value,
    /// ADR-0005 §8：是否已入 Kron 账本（未入账事件启动时批量补账）。
    #[serde(default)]
    pub audited: bool,
}

impl EventEnvelope {
    pub fn from_new(new: NewEvent, id: impl Into<String>, occurred_at: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind: new.kind,
            occurred_at: occurred_at.into(),
            actor: new.actor,
            ticket_id: new.ticket_id,
            attempt_id: new.attempt_id,
            payload: new.payload,
            audited: false,
        }
    }
}

/// WS 首帧：订阅建立后宿主立即回送的自描述（契约版本 + 事件闭集）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WsHello {
    /// 固定为 `hello`，与错误信封（有 `code`）区分。
    pub r#type: String,
    pub contract_version: String,
    pub event_kinds: Vec<EventKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_id: Option<String>,
}

impl WsHello {
    pub fn new(contract_version: impl Into<String>, event_kinds: Vec<EventKind>) -> Self {
        Self {
            r#type: "hello".to_string(),
            contract_version: contract_version.into(),
            event_kinds,
            subscription_id: None,
        }
    }
}
