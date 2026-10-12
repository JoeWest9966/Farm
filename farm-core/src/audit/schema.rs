//! Kron 账本记录的**形状映射**（ADR-0004 决策 2：serde + `deny_unknown_fields`）。
//!
//! Kron 的 `internal/schema` 不在本仓（Go，跨仓）。本模块按
//! `docs/research/report-nous-family.md` §3.2 文档化的形状镜像：
//!
//! - 全封闭 struct 树、**拒未知字段**（`deny_unknown_fields`）——上游扩位/我们漏抄都会
//!   响亮失败，绝不静默丢字段；
//! - 无 `any` / `map[string]any`：`details` 是封闭 struct，只放标识字段；
//! - 写入方**不得**填账本自留字段（`cursor` / `prev` / `hash`）——所以本模块连这些
//!   字段都不声明，真二进制接入时若字段名有偏，`ledger record` 会以 `INPUT_INVALID`
//!   逐项隔离，Farm 侧只告警不阻塞；
//! - **红线「禁入」**：消息正文、文件内容、system prompt、凭据一律不进账。事件 payload
//!   原样留在宿主私有存储（面板读源），[`KronRecord`] 只带标识字段。
//!
//! 事件类型映射（Farm 审计闭集 → Kron 10 类事件）：
//!
//! | Farm `EventKind` | Kron `event` | 依据 |
//! | --- | --- | --- |
//! | `dispatch` | `run_boundary` | 一次 run 的起点 |
//! | `merge` | `run_boundary` | 一次 run 的交付边界（完成信号落账） |
//! | `approval` | `approval` | Kron 审批状态机的外部裁决 |
//! | `stall_kill` | `escalation` | 两口钟判杀 = 异常上报 |
//! | `host_restarted` | `escalation` | ADR-0004 决策 4：崩溃/重启进 Kron 账（escalation，`actor=farm`） |

use serde::{Deserialize, Serialize};

use farm_sdk::events::{EventEnvelope, EventKind};

/// Kron 10 类事件闭集（report-nous-family §3.2）。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum KronEventType {
    Token,
    Approval,
    Escalation,
    Read,
    RunBoundary,
    SessionBoundary,
    ContextTransition,
    ToolOutcome,
    Destruction,
    Integrity,
}

impl KronEventType {
    /// 闭集的权威清单。
    pub const ALL: [KronEventType; 10] = [
        KronEventType::Token,
        KronEventType::Approval,
        KronEventType::Escalation,
        KronEventType::Read,
        KronEventType::RunBoundary,
        KronEventType::SessionBoundary,
        KronEventType::ContextTransition,
        KronEventType::ToolOutcome,
        KronEventType::Destruction,
        KronEventType::Integrity,
    ];

    /// 契约里出现的字符串形式。
    pub fn as_str(&self) -> &'static str {
        match self {
            KronEventType::Token => "token",
            KronEventType::Approval => "approval",
            KronEventType::Escalation => "escalation",
            KronEventType::Read => "read",
            KronEventType::RunBoundary => "run_boundary",
            KronEventType::SessionBoundary => "session_boundary",
            KronEventType::ContextTransition => "context_transition",
            KronEventType::ToolOutcome => "tool_outcome",
            KronEventType::Destruction => "destruction",
            KronEventType::Integrity => "integrity",
        }
    }
}

impl std::fmt::Display for KronEventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Kron 写入方闭集（escalation 的 `actor`）：文档化五位 + 待跨仓扩位的 `farm`。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum KronActor {
    Sentinel,
    Whitehelmet,
    Slaveholder,
    Crossline,
    Human,
    /// Farm 宿主自己——**跨仓协调项**：Kron actor 闭集扩位加入 `farm` 未落地前，
    /// Farm 只把事件标成未入账，不往账里写（见 [`crate::audit::AuditSink`]）。
    Farm,
}

impl KronActor {
    /// 闭集的权威清单（前五位 = 文档化闭集，第六位 = 待扩位）。
    pub const ALL: [KronActor; 6] = [
        KronActor::Sentinel,
        KronActor::Whitehelmet,
        KronActor::Slaveholder,
        KronActor::Crossline,
        KronActor::Human,
        KronActor::Farm,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            KronActor::Sentinel => "sentinel",
            KronActor::Whitehelmet => "whitehelmet",
            KronActor::Slaveholder => "slaveholder",
            KronActor::Crossline => "crossline",
            KronActor::Human => "human",
            KronActor::Farm => "farm",
        }
    }

    /// 今天的 Kron 收不收这一位（`farm` = 跨仓协调未完成）。
    pub fn in_kron_closed_set_today(&self) -> bool {
        !matches!(self, KronActor::Farm)
    }
}

impl std::fmt::Display for KronActor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Farm 审计闭集 → Kron 事件类型；非关键事件回 `None`（不入账）。
pub fn kron_event_type(kind: EventKind) -> Option<KronEventType> {
    match kind {
        EventKind::Dispatch | EventKind::Merge => Some(KronEventType::RunBoundary),
        EventKind::Approval => Some(KronEventType::Approval),
        EventKind::StallKill | EventKind::HostRestarted => Some(KronEventType::Escalation),
        _ => None,
    }
}

/// 结构化细节：**只放标识字段**（Kron 红线：正文禁入）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KronDetails {
    /// 宿主盖章的事件 id（补账的去重锚）。
    pub event_id: String,
    /// Farm 事件种类（审计闭集五成员之一）。
    pub kind: EventKind,
    /// 事件发生时刻（宿主事件信封上的 RFC3339）。
    pub occurred_at: String,
    /// 事件产生方（host / agent-runner / orchestrator / human / plugin）。
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
}

/// `ledger record` 的一行 NDJSON（Kron 账本的一笔）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KronRecord {
    /// Kron 事件类型（10 类闭集）。
    pub event: KronEventType,
    /// 写入方。Farm 恒为 `farm`（跨仓扩位见 [`KronActor::Farm`]）。
    pub actor: KronActor,
    /// RFC3339 时间戳（取事件发生时刻，不是补账时刻）。
    pub ts: String,
    /// 这笔账关于什么（Kron 审批状态机的 subject 概念）。
    pub subject_id: String,
    /// 幂等自然键：同键同摘要 → 账本回 `duplicate`，不回显载荷。
    pub idempotency_key: String,
    /// 结构化细节（封闭 struct；正文禁入）。
    pub details: KronDetails,
}

impl KronRecord {
    /// 把一条宿主事件映射成 Kron 的一笔；非审计闭集事件回 `None`。
    pub fn from_event(envelope: &EventEnvelope) -> Option<Self> {
        let event = kron_event_type(envelope.kind)?;
        Some(Self {
            event,
            actor: KronActor::Farm,
            ts: envelope.occurred_at.clone(),
            subject_id: subject_id(envelope),
            idempotency_key: format!("farm:{}", envelope.id),
            details: KronDetails {
                event_id: envelope.id.clone(),
                kind: envelope.kind,
                occurred_at: envelope.occurred_at.clone(),
                actor: envelope.actor.clone(),
                ticket_id: envelope.ticket_id.clone(),
                attempt_id: envelope.attempt_id.clone(),
            },
        })
    }

    /// 这一笔的 NDJSON 行（`ledger record` 的 stdin 单位，不带换行）。
    pub fn to_ndjson(&self) -> String {
        serde_json::to_string(self).expect("a KronRecord always serializes")
    }
}

/// subject 归属：attempt → ticket → 事件本身（绝不造一个假 subject）。
fn subject_id(envelope: &EventEnvelope) -> String {
    if let Some(attempt_id) = &envelope.attempt_id {
        format!("attempt:{attempt_id}")
    } else if let Some(ticket_id) = &envelope.ticket_id {
        format!("ticket:{ticket_id}")
    } else {
        format!("event:{}", envelope.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_audit_critical_kind_has_a_kron_event_type() {
        for kind in EventKind::ALL.iter().filter(|k| k.is_audit_critical()) {
            assert!(
                kron_event_type(*kind).is_some(),
                "{kind} 是关键事件，必须能映射"
            );
        }
        assert!(kron_event_type(EventKind::Blocked).is_none());
    }

    #[test]
    fn a_record_round_trips_through_json_without_losing_a_field() {
        let envelope = EventEnvelope::from_new(
            NewEvent_for_test(),
            "evt-1",
            "2026-01-01T00:00:00Z",
        );
        let record = KronRecord::from_event(&envelope).expect("dispatch maps");
        let json = record.to_ndjson();
        let back: KronRecord = serde_json::from_str(&json).expect("round trip");
        assert_eq!(back, record);
    }

    fn NewEvent_for_test() -> farm_sdk::events::NewEvent {
        farm_sdk::events::NewEvent::new(EventKind::Dispatch, "agent-runner").with_ticket("20")
    }
}
