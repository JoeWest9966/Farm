//! token 三维归集（ADR-0002 §9：绝对累计法，按全局/每票/每天归集）。
//!
//! 面板**不重算** token：宿主在终态事件（`agent_settled` / `attempt_finished` /
//! `interrupted`）的 payload 里带上 attempt 的 token 快照，这里只做归集。
//!
//! **防重**是这一模块的核心不变式：同一个 attempt 可能被投递多份快照（流式对账 +
//! WS 重发），绝对累计法要求**后帧覆盖前帧**，绝不累加——否则一次重发就把账面翻倍。

use std::collections::BTreeMap;

use farm_sdk::events::EventEnvelope;
use farm_sdk::EventKind;

/// 一条 attempt 的 token 快照（契约 payload `tokens` 的投影）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TokenPayload {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub total: u64,
    /// 微美元（host-private 记账口径；单价表归配置，面板只展示）。
    pub cost_micros: u64,
    /// 记账被截断（中断态：宿主重启时 running attempt 的最后快照）。
    pub truncated: bool,
}

impl TokenPayload {
    /// 从事件 payload 的 `tokens` 字段解出快照；缺字段/类型不对 → `None`。
    ///
    /// 宁少报不虚报：解不出来就不入账（GLOSSARY「绝对累计法」的纪律）。
    pub fn parse(value: &serde_json::Value) -> Option<Self> {
        let tokens = value.get("tokens")?;
        if !tokens.is_object() {
            return None;
        }
        let number = |key: &str| tokens.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
        Some(Self {
            input: number("input"),
            output: number("output"),
            cache_read: number("cacheRead"),
            cache_write: number("cacheWrite"),
            // `total` 缺省时用四项之和兜底（老快照可能没有这个字段）。
            total: tokens
                .get("total")
                .and_then(|v| v.as_u64())
                .unwrap_or_else(|| {
                    number("input") + number("output") + number("cacheRead") + number("cacheWrite")
                }),
            cost_micros: number("costMicros"),
            truncated: tokens
                .get("truncated")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        })
    }

    /// 事件顶层自带记账截断标记（payload 的 `accountingTruncated`）。
    pub fn truncated_flag(envelope: &EventEnvelope) -> bool {
        envelope
            .payload
            .get("accountingTruncated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }
}

/// 一组 attempt 的 token 合计（全局 / 每票 / 每天共用同一形状）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TokenTotals {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub total: u64,
    pub cost_micros: u64,
    /// 计入的 attempt 数。
    pub attempts: usize,
    /// 其中记账被截断的 attempt 数（中断态）。
    pub truncated_attempts: usize,
}

impl TokenTotals {
    /// 人读的一行（面板展示用）：`12,345 tokens · $0.42 · 3 attempts`。
    pub fn summary(&self) -> String {
        let dollars = self.cost_micros as f64 / 1_000_000.0;
        format!(
            "{} tokens · ${:.2} · {} attempt{}",
            self.total,
            dollars,
            self.attempts,
            if self.attempts == 1 { "" } else { "s" }
        )
    }
}

/// attempt 维度的入账记录（`occurred_at` 用来判帧的新旧）。
#[derive(Clone, Debug, PartialEq, Eq)]
struct AttemptTokens {
    ticket_id: String,
    occurred_at: String,
    payload: TokenPayload,
}

/// token 账本 + 三维归集视图（全局 / 每票 / 每天）。
///
/// 账目按 **attempt** 存一份最后快照，三维视图都从账本现算——所以「后帧覆盖前帧」
/// 不需要做减法，重发天然无害。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TokenAggregate {
    attempts: BTreeMap<String, AttemptTokens>,
}

/// 一天之内的日期键（RFC3339 前 10 位；认不出来归 `unknown`）。
pub fn day_key(occurred_at: &str) -> String {
    if occurred_at.len() >= 10 && occurred_at.as_bytes().get(4) == Some(&b'-') {
        occurred_at[..10].to_string()
    } else {
        "unknown".to_string()
    }
}

impl TokenAggregate {
    pub fn new() -> Self {
        Self::default()
    }

    /// 裁决并记入一帧。
    ///
    /// - 新 attempt → 入账；
    /// - 同一 attempt 的**更晚**快照 → 覆盖前帧（后帧胜，绝对累计）；
    /// - 同一 attempt 的**更早或同时**快照 → 忽略（乱序旧帧 / 重发）。
    ///
    /// 返回 `false` 表示账面没变（调用方无需刷新）。
    pub fn record(
        &mut self,
        attempt_id: &str,
        ticket_id: &str,
        occurred_at: &str,
        payload: TokenPayload,
    ) -> bool {
        if let Some(existing) = self.attempts.get(attempt_id) {
            if occurred_at < existing.occurred_at.as_str() {
                return false; // 乱序旧帧：不回滚账面
            }
            if existing.payload == payload && existing.occurred_at == occurred_at {
                return false; // 同一帧重发：账面不动
            }
        }
        self.attempts.insert(
            attempt_id.to_string(),
            AttemptTokens {
                ticket_id: ticket_id.to_string(),
                occurred_at: occurred_at.to_string(),
                payload,
            },
        );
        true
    }

    /// 已入账的 attempt 数。
    pub fn attempts(&self) -> usize {
        self.attempts.len()
    }

    /// 已归集到的票数。
    pub fn tickets(&self) -> usize {
        self.attempts
            .values()
            .map(|entry| entry.ticket_id.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    }

    /// 某 attempt 的最后快照（下钻页展示）。
    pub fn snapshot(&self, attempt_id: &str) -> Option<TokenPayload> {
        self.attempts.get(attempt_id).map(|e| e.payload)
    }

    /// 全局合计。
    pub fn global(&self) -> TokenTotals {
        let mut totals = TokenTotals::default();
        for entry in self.attempts.values() {
            totals.input += entry.payload.input;
            totals.output += entry.payload.output;
            totals.cache_read += entry.payload.cache_read;
            totals.cache_write += entry.payload.cache_write;
            totals.total += entry.payload.total;
            totals.cost_micros += entry.payload.cost_micros;
            totals.attempts += 1;
            if entry.payload.truncated {
                totals.truncated_attempts += 1;
            }
        }
        totals
    }

    /// 每票合计（下钻页用）。
    pub fn for_ticket(&self, ticket_id: &str) -> TokenTotals {
        let mut totals = TokenTotals::default();
        for entry in self.attempts.values() {
            if entry.ticket_id != ticket_id {
                continue;
            }
            totals.input += entry.payload.input;
            totals.output += entry.payload.output;
            totals.cache_read += entry.payload.cache_read;
            totals.cache_write += entry.payload.cache_write;
            totals.total += entry.payload.total;
            totals.cost_micros += entry.payload.cost_micros;
            totals.attempts += 1;
            if entry.payload.truncated {
                totals.truncated_attempts += 1;
            }
        }
        totals
    }

    /// 每天合计（按日期键排序；ADR-0002 §9 的第三维）。
    pub fn per_day(&self) -> Vec<(String, TokenTotals)> {
        let mut by_day: BTreeMap<String, TokenTotals> = BTreeMap::new();
        for entry in self.attempts.values() {
            let totals = by_day.entry(day_key(&entry.occurred_at)).or_default();
            totals.input += entry.payload.input;
            totals.output += entry.payload.output;
            totals.cache_read += entry.payload.cache_read;
            totals.cache_write += entry.payload.cache_write;
            totals.total += entry.payload.total;
            totals.cost_micros += entry.payload.cost_micros;
            totals.attempts += 1;
            if entry.payload.truncated {
                totals.truncated_attempts += 1;
            }
        }
        by_day.into_iter().collect()
    }
}

/// 从一条事件里解出「这一帧要不要入账」以及它的 token 快照。
///
/// 只有终态事件带权威 token 快照（`agent_settled` / `attempt_finished` / `interrupted`）；
/// 其余事件没有 `tokens` 字段，直接忽略。
pub fn tokens_from_event(envelope: &EventEnvelope) -> Option<TokenPayload> {
    if !matches!(
        envelope.kind,
        EventKind::AgentSettled | EventKind::AttemptFinished | EventKind::Interrupted
    ) {
        return None;
    }
    let mut payload = TokenPayload::parse(&envelope.payload)?;
    if TokenPayload::truncated_flag(envelope) {
        payload.truncated = true;
    }
    Some(payload)
}

/// 事件的人读 token 摘要（没有快照就给一句「宿主未上报」）。
pub fn tokens_summary(envelope: &EventEnvelope) -> String {
    match tokens_from_event(envelope) {
        Some(payload) => format!(
            "{} tokens（in {} / out {} / cache r{} w{}）",
            payload.total, payload.input, payload.output, payload.cache_read, payload.cache_write
        ),
        None => "宿主未上报 token 快照".to_string(),
    }
}
