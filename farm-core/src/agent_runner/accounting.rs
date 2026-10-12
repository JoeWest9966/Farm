//! token 流式记账（GLOSSARY：agent-runner 的 token 记账）。
//!
//! 口径（源自 Slaveholder 判据学 + `docs/research/pi-runtime-interface.md` §2）：
//!
//! - **绝对累计**：按权威的「每条消息一次」累加，不按流式增量累加。
//! - **宁少报不虚报**：`message_update` 的 `usage` 是**累积快照**，绝不能当增量加；
//!   只有权威来源算数——`message_end.message.usage`、`agent_end.messages[].usage`、
//!   `tool_execution_end.result.usage`（可带 `parentToolCallId` 的**嵌套用量**）、
//!   `compaction_end.result.usage`。缺字段的来源整条跳过（少报，不猜）。
//! - **去重**：同一条消息可能既走 `message_end` 又出现在 `agent_end.messages` 里，
//!   按 message id 去重；1.0.2 的 `agent_end` 还**多一条 system 消息**（无用量），
//!   不得被当成 assistant 消息。
//! - **权威对账**：settle 时用 `get_session_stats` 的聚合对账；我方累计超过权威值时
//!   报权威值（少报）并记偏差。

use std::collections::HashSet;

use serde_json::Value;

/// 一次用量（四料 + 金额）。`total` 由四料相加，不信任何自报 total。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// 金额微美元（`cost.total` 四舍五入；缺省 0）。
    pub cost_micros: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    /// 从 pi 的 usage JSON 解析；一个数字字段都没有时返回 `None`（整条跳过）。
    pub fn from_pi_json(value: &Value) -> Option<Self> {
        let number = |key: &str| -> u64 {
            value.get(key).and_then(|v| v.as_u64()).unwrap_or(0)
        };
        let input = number("input");
        let output = number("output");
        let cache_read = number("cacheRead");
        let cache_write = number("cacheWrite");
        if input == 0 && output == 0 && cache_read == 0 && cache_write == 0 {
            // 可能是「没有用量可报」（system 消息 / 未知形状）→ 不当成 0 用量硬记。
            let has_any_numeric = ["input", "output", "cacheRead", "cacheWrite"]
                .iter()
                .any(|key| value.get(*key).map(|v| v.is_number()).unwrap_or(false));
            if !has_any_numeric {
                return None;
            }
        }
        let cost_micros = value
            .get("cost")
            .and_then(|cost| cost.get("total"))
            .and_then(|total| total.as_f64())
            .map(|total| (total * 1_000_000.0).round() as u64)
            .unwrap_or(0);
        Some(Self {
            input,
            output,
            cache_read,
            cache_write,
            cost_micros,
        })
    }

    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "input": self.input,
            "output": self.output,
            "cacheRead": self.cache_read,
            "cacheWrite": self.cache_write,
            "total": self.total(),
            "costMicros": self.cost_micros,
        })
    }
}

/// token 快照（持久化到 attempts 表；GLOSSARY「中断态」以最后快照为准并标 truncated）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TokenSnapshot {
    /// 上报值（权威优先，其次我方累计）。
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub total: u64,
    pub cost_micros: u64,
    /// 我方流式累计（只含权威来源）。
    pub streamed_total: u64,
    /// pi `get_session_stats` 的权威聚合（对账用）。
    pub authoritative_total: Option<u64>,
    /// 带 `parentToolCallId` 的嵌套用量条数。
    pub nested_usage_calls: u32,
    /// 去重后计入的权威消息条数。
    pub counted_messages: u32,
    /// 记账被截断（中断态：宿主重启时 running attempt 的最后快照）。
    pub truncated: bool,
    /// 记账偏差说明（对账不一致 / 来源缺失等）。
    pub note: Option<String>,
}

impl TokenSnapshot {
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "input": self.input,
            "output": self.output,
            "cacheRead": self.cache_read,
            "cacheWrite": self.cache_write,
            "total": self.total,
            "costMicros": self.cost_micros,
            "streamedTotal": self.streamed_total,
            "authoritativeTotal": self.authoritative_total,
            "nestedUsageCalls": self.nested_usage_calls,
            "countedMessages": self.counted_messages,
            "truncated": self.truncated,
            "note": self.note,
        })
    }

    pub fn from_json(value: &Value) -> Self {
        let number = |key: &str| value.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
        Self {
            input: number("input"),
            output: number("output"),
            cache_read: number("cacheRead"),
            cache_write: number("cacheWrite"),
            total: number("total"),
            cost_micros: number("costMicros"),
            streamed_total: number("streamedTotal"),
            authoritative_total: value
                .get("authoritativeTotal")
                .and_then(|v| v.as_u64()),
            nested_usage_calls: value
                .get("nestedUsageCalls")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            counted_messages: value
                .get("countedMessages")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            truncated: value
                .get("truncated")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            note: value
                .get("note")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        }
    }
}

/// 绝对累计账本。
#[derive(Clone, Debug, Default)]
pub struct TokenAccounting {
    sum: Usage,
    /// 已计入的权威消息 id（去重：message_end 与 agent_end 可能重复）。
    counted_messages: HashSet<String>,
    /// 带 `parentToolCallId` 的嵌套用量条数。
    nested_usage_calls: u32,
    /// 因去重被跳过的条数（诊断用）。
    duplicates_skipped: u32,
    /// 无用量可报而被跳过的条数（诊断用：少报的证据）。
    sources_without_usage: u32,
}

impl TokenAccounting {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记一条**权威消息**（`message_end` / `agent_end.messages[]`）。返回是否新计入。
    ///
    /// `role == "system"` 的消息直接跳过：1.0.2 的 `agent_end` 多一条 system 消息，
    /// 它既不是 assistant 消息也不带用量。
    pub fn add_authoritative_message(
        &mut self,
        message_id: Option<&str>,
        role: Option<&str>,
        usage: Option<&Value>,
    ) -> bool {
        if role == Some("system") {
            return false;
        }
        let id = match message_id {
            Some(id) if !id.is_empty() => id,
            // 没有 id 的消息无法去重：宁少报不虚报 → 跳过并计数。
            _ => {
                self.sources_without_usage += 1;
                return false;
            }
        };
        if !self.counted_messages.insert(id.to_string()) {
            self.duplicates_skipped += 1;
            return false;
        }
        match usage.and_then(Usage::from_pi_json) {
            Some(usage) => {
                self.sum.input += usage.input;
                self.sum.output += usage.output;
                self.sum.cache_read += usage.cache_read;
                self.sum.cache_write += usage.cache_write;
                self.sum.cost_micros += usage.cost_micros;
                true
            }
            None => {
                // 已计入 id 但没有用量：撤回去重标记，避免「占位」污染后续对账。
                self.counted_messages.remove(id);
                self.sources_without_usage += 1;
                false
            }
        }
    }

    /// 记一条**嵌套用量**（`tool_execution_end.result.usage`，可带 `parentToolCallId`）。
    pub fn add_nested_usage(&mut self, parent_tool_call_id: Option<&str>, usage: Option<&Value>) {
        let Some(usage) = usage.and_then(Usage::from_pi_json) else {
            self.sources_without_usage += 1;
            return;
        };
        self.sum.input += usage.input;
        self.sum.output += usage.output;
        self.sum.cache_read += usage.cache_read;
        self.sum.cache_write += usage.cache_write;
        self.sum.cost_micros += usage.cost_micros;
        if parent_tool_call_id.is_some() {
            self.nested_usage_calls += 1;
        }
    }

    pub fn sum(&self) -> Usage {
        self.sum
    }

    pub fn counted_messages(&self) -> usize {
        self.counted_messages.len()
    }

    pub fn nested_usage_calls(&self) -> u32 {
        self.nested_usage_calls
    }

    pub fn duplicates_skipped(&self) -> u32 {
        self.duplicates_skipped
    }

    /// 与 pi 权威聚会对账，产出快照。
    ///
    /// 我方累计**超过**权威值时报权威值（宁少报不虚报）并记偏差。
    pub fn snapshot(&self, authoritative: Option<&Value>, truncated: bool) -> TokenSnapshot {
        let authoritative_usage = authoritative
            .and_then(|stats| stats.get("tokens"))
            .and_then(Usage::from_pi_json);
        let streamed = self.sum;
        let (reported, authoritative_total, note) = match authoritative_usage {
            Some(authoritative) => {
                let note = if streamed.total() > authoritative.total() {
                    Some(format!(
                        "streamed accumulation ({}) exceeded pi authoritative aggregate ({}); reporting the smaller authoritative value (宁少报不虚报)",
                        streamed.total(),
                        authoritative.total()
                    ))
                } else if streamed.total() < authoritative.total() {
                    Some(format!(
                        "streamed accumulation ({}) is below pi authoritative aggregate ({}); some usage was not observable on the wire",
                        streamed.total(),
                        authoritative.total()
                    ))
                } else {
                    None
                };
                (authoritative, Some(authoritative.total()), note)
            }
            None => (streamed, None, None),
        };
        TokenSnapshot {
            input: reported.input,
            output: reported.output,
            cache_read: reported.cache_read,
            cache_write: reported.cache_write,
            total: reported.total(),
            cost_micros: reported.cost_micros,
            streamed_total: streamed.total(),
            authoritative_total,
            nested_usage_calls: self.nested_usage_calls,
            counted_messages: self.counted_messages.len() as u32,
            truncated,
            note,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn usage_json(input: u64, output: u64) -> Value {
        json!({"input": input, "output": output, "cacheRead": 0, "cacheWrite": 0})
    }

    #[test]
    fn cumulative_streaming_snapshots_are_never_accumulated_as_deltas() {
        // message_update 的 usage 是累积快照：累加它就会虚报 5 倍。
        let mut accounting = TokenAccounting::new();
        let snapshot = usage_json(60, 6);
        for _ in 0..5 {
            // 流式快照根本不进账本（只有权威来源进）。
            assert!(Usage::from_pi_json(&snapshot).is_some());
        }
        assert!(accounting.add_authoritative_message(
            Some("msg-1"),
            Some("assistant"),
            Some(&snapshot)
        ));

        assert_eq!(accounting.sum().input, 60, "同一条消息只记一次");
        assert_eq!(accounting.sum().output, 6);
    }

    #[test]
    fn the_same_message_seen_in_message_end_and_agent_end_is_counted_once() {
        let mut accounting = TokenAccounting::new();
        let usage = usage_json(100, 10);

        assert!(accounting.add_authoritative_message(Some("msg-1"), Some("assistant"), Some(&usage)));
        assert!(
            !accounting.add_authoritative_message(Some("msg-1"), Some("assistant"), Some(&usage)),
            "agent_end 里重复出现的同一条消息不得二次入账"
        );

        assert_eq!(accounting.sum().input, 100);
        assert_eq!(accounting.sum().output, 10);
        assert_eq!(accounting.duplicates_skipped(), 1);
    }

    #[test]
    fn the_extra_system_message_in_agent_end_is_not_an_assistant_message() {
        let mut accounting = TokenAccounting::new();
        let assistant = usage_json(100, 10);

        assert!(accounting.add_authoritative_message(Some("msg-1"), Some("assistant"), Some(&assistant)));
        assert!(
            !accounting.add_authoritative_message(
                Some("system-1"),
                Some("system"),
                Some(&usage_json(999, 999))
            ),
            "1.0.2 的 agent_end 多一条 system 消息：既不当 assistant 也不入账"
        );

        assert_eq!(accounting.sum().input, 100);
        assert_eq!(accounting.sum().output, 10);
    }

    #[test]
    fn nested_usage_with_a_parent_tool_call_id_is_accumulated() {
        let mut accounting = TokenAccounting::new();
        accounting.add_authoritative_message(Some("msg-1"), Some("assistant"), Some(&usage_json(100, 10)));

        let mut nested = usage_json(40, 5);
        nested["parentToolCallId"] = json!("call-parent-1");
        accounting.add_nested_usage(Some("call-parent-1"), Some(&nested));

        assert_eq!(accounting.sum().input, 140, "嵌套用量必须进绝对累计");
        assert_eq!(accounting.sum().output, 15);
        assert_eq!(accounting.sum().total(), 155);
        assert_eq!(accounting.nested_usage_calls(), 1);
    }

    #[test]
    fn a_source_without_usage_is_skipped_rather_than_guessed() {
        let mut accounting = TokenAccounting::new();

        assert!(!accounting.add_authoritative_message(Some("msg-1"), Some("assistant"), None));
        assert!(!accounting.add_authoritative_message(
            Some("msg-2"),
            Some("assistant"),
            Some(&json!({"content": []}))
        ));
        accounting.add_nested_usage(None, None);

        assert_eq!(accounting.sum().total(), 0, "宁少报不虚报：缺用量就不记");
        assert_eq!(accounting.counted_messages(), 0);
    }

    #[test]
    fn an_authoritative_aggregate_smaller_than_our_accumulation_is_reported_instead() {
        let mut accounting = TokenAccounting::new();
        accounting.add_authoritative_message(Some("msg-1"), Some("assistant"), Some(&usage_json(100, 10)));
        accounting.add_authoritative_message(Some("msg-2"), Some("assistant"), Some(&usage_json(100, 10)));

        let stats = json!({"tokens": {"input": 150, "output": 15, "cacheRead": 0, "cacheWrite": 0}});
        let snapshot = accounting.snapshot(Some(&stats), false);

        assert_eq!(snapshot.total, 165, "对账后报权威值（少报）");
        assert_eq!(snapshot.streamed_total, 220);
        assert_eq!(snapshot.authoritative_total, Some(165));
        assert!(snapshot.note.as_deref().is_some_and(|note| note.contains("宁少报不虚报")));
    }

    #[test]
    fn a_survives_a_round_trip_through_json() {
        let mut accounting = TokenAccounting::new();
        accounting.add_authoritative_message(Some("msg-1"), Some("assistant"), Some(&usage_json(100, 10)));
        let snapshot = accounting.snapshot(None, true);

        let restored = TokenSnapshot::from_json(&snapshot.to_json());
        assert_eq!(restored, snapshot);
        assert!(restored.truncated, "中断态快照必须带 truncated");
    }
}
