//! 判据阶梯（GLOSSARY「判据阶梯」；源自 Slaveholder 六篇 ADR）。
//!
//! ```text
//! 读循环健康度前置 → 两口钟（静默钟 + 总钟）→ abort 探针（30s 独立死线）→ fork 找回
//! ```
//!
//! 本模块是**纯判决内核**：只吃「时间 + 帧/事件」，不碰任何 I/O，所以四梯的每一次
//! 升级都能用已知字面量断言（不重算实现）。驱动它的异步监督循环在
//! [`crate::agent_runner::service`]。
//!
//! 两条容易搞错的语义，这里显式立住：
//!
//! 1. **两口钟不是一回事**。静默钟看「离上一帧多久」（pi 吐字间隙），总钟看「attempt
//!    活了多久」（与有没有帧无关）。流式场景下静默钟永远不到点，总钟照样到。
//! 2. **abort 探针是独立死线**。进探针相之后时钟**不因来帧而重置**——pi 可以在被杀前
//!    继续吐 `queue_update`，那不是它响应了 abort。
//!
//! ## 完成双条件
//! `agent_settled` **且**最后一条权威 assistant 消息 `stopReason === "stop"`。
//! 只满足前者不算完：`no-stop-reason` 场景里 pi 照样发 `agent_settled`，但停因是
//! `length`（被截断），attempt 必须继续受两口钟管，不能谎报成功。

use std::time::{Duration, Instant};

use serde_json::Value;

/// 两口钟 + abort 探针的配置（测试注入短值；默认值按「本地后端跑一个真 agent」估）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LadderConfig {
    /// 静默钟：多久收不到任何帧就从第一梯升级（发 abort）。
    pub silent_clock: Duration,
    /// 总钟：整个 attempt 的预算（与有没有帧无关）。
    pub total_clock: Duration,
    /// abort 探针：abort 之后等多久强杀（规格 30s；**独立死线**，来帧不重置）。
    pub abort_probe: Duration,
    /// 监督循环的轮询粒度（`next_wait` 的上界）。
    pub tick: Duration,
}

impl Default for LadderConfig {
    fn default() -> Self {
        Self {
            silent_clock: Duration::from_secs(300),
            total_clock: Duration::from_secs(3600),
            abort_probe: Duration::from_secs(30),
            tick: Duration::from_millis(50),
        }
    }
}

impl LadderConfig {
    /// 测试用的短钟（毫秒级），不碰默认值。
    pub fn fast(silent_ms: u64, total_ms: u64, abort_probe_ms: u64) -> Self {
        Self {
            silent_clock: Duration::from_millis(silent_ms),
            total_clock: Duration::from_millis(total_ms),
            abort_probe: Duration::from_millis(abort_probe_ms),
            tick: Duration::from_millis(5),
        }
    }
}

/// 阶梯当前所在相。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LadderPhase {
    /// 正常运行：两口钟都在走。
    Running,
    /// 已发 abort：只有 abort 探针的独立死线在走。
    Aborting,
}

/// 一次检查的结论：监督循环照此行动。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LadderAction {
    /// 还没到点，继续等。
    KeepWaiting,
    /// 第一梯到点：静默钟。监督循环应发 `abort` 并进探针相。
    SilentClockExpired {
        silent_for: Duration,
    },
    /// 第二梯到点：总钟。监督循环应强杀（不等 abort 回应）。
    TotalClockExpired {
        elapsed: Duration,
    },
    /// 第三梯到点：abort 探针死线。监督循环应强杀（pi 不理 abort）。
    AbortProbeExpired {
        waited: Duration,
    },
    /// 完成双条件满足。
    Settled,
}

/// 判据阶梯状态机。
#[derive(Clone, Debug)]
pub struct JudgementLadder {
    config: LadderConfig,
    started_at: Instant,
    last_frame_at: Instant,
    phase: LadderPhase,
    abort_requested_at: Option<Instant>,
    /// 见过 `agent_settled`（双条件的第一半）。
    settled_seen: bool,
    /// 最后一条**权威 assistant** 消息的 `stopReason`（双条件的第二半）。
    last_stop_reason: Option<String>,
    /// 计入过的权威 assistant 消息条数。
    authoritative_messages: u32,
    /// 读循环交出来的帧数（健康度前置的度量）。
    frames_seen: u64,
}

impl JudgementLadder {
    pub fn new(config: LadderConfig) -> Self {
        let now = Instant::now();
        Self {
            config,
            started_at: now,
            last_frame_at: now,
            phase: LadderPhase::Running,
            abort_requested_at: None,
            settled_seen: false,
            last_stop_reason: None,
            authoritative_messages: 0,
            frames_seen: 0,
        }
    }

    pub fn config(&self) -> &LadderConfig {
        &self.config
    }

    pub fn phase(&self) -> LadderPhase {
        self.phase
    }

    /// 读循环健康度前置：收到一帧即「这一瞬还活着」，静默钟从这里重新起算。
    pub fn note_frame(&mut self, now: Instant) {
        self.frames_seen += 1;
        self.last_frame_at = now;
    }

    /// 读循环交出来的帧总数（0 = 一个帧都没见到过：启动即坏）。
    pub fn frames_seen(&self) -> u64 {
        self.frames_seen
    }

    /// 记一条**权威** assistant 消息（`message_end` / `agent_end.messages[]`）。
    ///
    /// `role == "system"` 直接跳过：1.0.2 的 `agent_end` 多一条 system 消息且放在**最后**，
    /// 「取最后一条消息」的懒判据会把它当成 assistant 消息，从而把双条件的第二半判丢。
    pub fn note_authoritative_message(&mut self, role: Option<&str>, stop_reason: Option<&str>) {
        if role == Some("system") {
            return;
        }
        self.authoritative_messages += 1;
        self.last_stop_reason = stop_reason.map(str::to_string);
    }

    /// 见过 `agent_settled`。
    pub fn note_settled(&mut self) {
        self.settled_seen = true;
    }

    /// 最后一条权威 assistant 消息的 `stopReason`。
    pub fn last_stop_reason(&self) -> Option<&str> {
        self.last_stop_reason.as_deref()
    }

    pub fn authoritative_messages(&self) -> u32 {
        self.authoritative_messages
    }

    /// **完成双条件**：`agent_settled` 且最后一条权威 assistant 消息 `stopReason === "stop"`。
    pub fn is_settled(&self) -> bool {
        self.settled_seen && self.last_stop_reason.as_deref() == Some("stop")
    }

    /// 离上一帧多久了（静默钟的读数）。
    pub fn silent_for(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.last_frame_at)
    }

    /// attempt 活了多久（总钟的读数）。
    pub fn elapsed(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.started_at)
    }

    /// 进探针相（监督循环发了 abort 之后调）。
    pub fn request_abort(&mut self, now: Instant) {
        if self.phase == LadderPhase::Running {
            self.phase = LadderPhase::Aborting;
            self.abort_requested_at = Some(now);
        }
    }

    /// 探针已经等了多久（`None` = 还没进探针相）。
    pub fn abort_waited(&self, now: Instant) -> Option<Duration> {
        self.abort_requested_at.map(|at| now.saturating_duration_since(at))
    }

    /// 纯判断：此刻该往哪一梯走。副作用只在 [`JudgementLadder::request_abort`]。
    pub fn check(&self, now: Instant) -> LadderAction {
        if self.is_settled() {
            return LadderAction::Settled;
        }
        // 总钟优先于静默钟：预算烧完了就是烧完了，与有没有帧无关。
        let elapsed = self.elapsed(now);
        if elapsed >= self.config.total_clock {
            return LadderAction::TotalClockExpired { elapsed };
        }
        match self.phase {
            LadderPhase::Running => {
                let silent_for = self.silent_for(now);
                if silent_for >= self.config.silent_clock {
                    return LadderAction::SilentClockExpired { silent_for };
                }
                LadderAction::KeepWaiting
            }
            LadderPhase::Aborting => {
                let waited = self
                    .abort_waited(now)
                    .unwrap_or_else(|| self.config.abort_probe);
                if waited >= self.config.abort_probe {
                    return LadderAction::AbortProbeExpired { waited };
                }
                LadderAction::KeepWaiting
            }
        }
    }

    /// 下一次该醒来的等待时长（两口钟 + 探针里最近的那个，再被 `tick` 截顶）。
    pub fn next_wait(&self, now: Instant) -> Duration {
        let mut wait = self.config.tick;
        let elapsed = self.elapsed(now);
        if let Some(remaining) = self.config.total_clock.checked_sub(elapsed) {
            wait = wait.min(remaining);
        }
        match self.phase {
            LadderPhase::Running => {
                if let Some(remaining) = self.config.silent_clock.checked_sub(self.silent_for(now)) {
                    wait = wait.min(remaining);
                }
            }
            LadderPhase::Aborting => {
                if let Some(waited) = self.abort_waited(now) {
                    if let Some(remaining) = self.config.abort_probe.checked_sub(waited) {
                        wait = wait.min(remaining);
                    }
                }
            }
        }
        wait.max(Duration::from_millis(1))
    }

    /// 从一帧事件里抽出判据与记账需要的信号（监督循环每帧都过这里）。
    ///
    /// 返回这条帧是不是权威来源（调用方据此决定是否入账）。
    pub fn observe_event<'a>(
        &mut self,
        event: &'a Value,
        now: Instant,
    ) -> EventSignals<'a> {
        self.note_frame(now);
        let kind = event.get("type").and_then(|t| t.as_str()).unwrap_or_default();
        match kind {
            "agent_settled" => {
                self.note_settled();
                EventSignals::default()
            }
            // message_end 是权威消息（GLOSSARY：最后一条权威 assistant 消息）。
            "message_end" => {
                let message = event.get("message");
                let role = message.and_then(|m| m.get("role")).and_then(|r| r.as_str());
                let stop_reason = message
                    .and_then(|m| m.get("stopReason"))
                    .and_then(|r| r.as_str());
                self.note_authoritative_message(role, stop_reason);
                EventSignals {
                    message_end: Some(MessageUsage {
                        id: message.and_then(|m| m.get("id")).and_then(|i| i.as_str()),
                        role,
                        usage: message.and_then(|m| m.get("usage")),
                    }),
                    ..Default::default()
                }
            }
            // agent_end 的 messages[] 同样是权威来源（且可能重复 message_end 见过的那条）。
            "agent_end" => {
                let mut signals = EventSignals::default();
                if let Some(messages) = event.get("messages").and_then(|m| m.as_array()) {
                    for message in messages {
                        let role = message.get("role").and_then(|r| r.as_str());
                        let stop_reason =
                            message.get("stopReason").and_then(|r| r.as_str());
                        self.note_authoritative_message(role, stop_reason);
                        if role != Some("system") {
                            signals.agent_end_messages.push(MessageUsage {
                                id: message.get("id").and_then(|i| i.as_str()),
                                role,
                                usage: message.get("usage"),
                            });
                        }
                    }
                }
                signals
            }
            // message_update 的 usage 是**累积快照**：绝不入账（宁少报不虚报）。
            "message_update" => EventSignals::default(),
            // tool_execution_end.result.usage 可带 parentToolCallId：嵌套用量，必须进绝对累计。
            // 注意 parentToolCallId 住在 **usage 对象里**（文档 §2：ToolResultMessage.usage），
            // 事件顶层那份只是 tool_execution_start 的回显——两者都认，usage 里的优先。
            "tool_execution_end" => {
                let usage = event.get("result").and_then(|r| r.get("usage"));
                EventSignals {
                    parent_tool_call_id: usage
                        .and_then(|u| u.get("parentToolCallId"))
                        .or_else(|| event.get("parentToolCallId"))
                        .and_then(|id| id.as_str()),
                    nested_usage: usage,
                    ..Default::default()
                }
            }
            // compaction_end.result.usage 也是权威来源。
            "compaction_end" => EventSignals {
                nested_usage: event.get("result").and_then(|r| r.get("usage")),
                ..Default::default()
            },
            _ => EventSignals::default(),
        }
    }
}

/// 一条权威消息的记账输入（`id` 用于跨来源去重）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessageUsage<'a> {
    pub id: Option<&'a str>,
    pub role: Option<&'a str>,
    pub usage: Option<&'a Value>,
}

/// 一帧事件里抽出来的记账输入（`None` 字段 = 这一帧没有该来源）。
#[derive(Default, Debug, Clone)]
pub struct EventSignals<'a> {
    /// `message_end.message`（权威；`id` 供去重）。
    pub message_end: Option<MessageUsage<'a>>,
    /// `agent_end.messages[]`（权威；**system 消息已滤掉**）。
    pub agent_end_messages: Vec<MessageUsage<'a>>,
    /// `tool_execution_end.result.usage` / `compaction_end.result.usage`（权威）。
    pub nested_usage: Option<&'a Value>,
    /// `tool_execution_end` 的 `parentToolCallId`（嵌套用量标记）。
    pub parent_tool_call_id: Option<&'a str>,
}

impl EventSignals<'_> {
    /// 这一帧是否带任何权威用量来源。
    pub fn has_usage(&self) -> bool {
        self.message_end.is_some()
            || !self.agent_end_messages.is_empty()
            || self.nested_usage.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ladder() -> JudgementLadder {
        JudgementLadder::new(LadderConfig::fast(200, 5_000, 300))
    }

    fn message_end(stop_reason: &str) -> Value {
        json!({
            "type": "message_end",
            "message": {
                "role": "assistant",
                "id": "msg-1",
                "stopReason": stop_reason,
                "usage": {"input": 100, "output": 10},
            },
        })
    }

    // --- 读循环健康度前置 -------------------------------------------------

    #[test]
    fn a_frame_resets_the_silent_clock_but_never_the_total_clock() {
        let mut ladder = ladder();
        let start = Instant::now();

        // 一直有帧：静默钟永远不到点。
        for _ in 0..5 {
            std::thread::sleep(Duration::from_millis(20));
            ladder.note_frame(Instant::now());
            assert_eq!(ladder.check(Instant::now()), LadderAction::KeepWaiting);
        }
        assert!(ladder.silent_for(Instant::now()) < ladder.config().silent_clock);

        // 总钟到点：流得再勤也照杀。
        std::thread::sleep(ladder.config().total_clock);
        assert!(start.elapsed() >= ladder.config().total_clock - Duration::from_millis(1));
        assert!(
            matches!(ladder.check(Instant::now()), LadderAction::TotalClockExpired { .. }),
            "两口钟不是一回事：流式场景静默钟不到点，总钟照样到"
        );
    }

    #[test]
    fn the_read_loop_health_is_the_precondition_not_a_rung_of_its_own() {
        let mut ladder = ladder();
        assert_eq!(ladder.frames_seen(), 0, "一个帧都没见到 = 启动即坏");
        ladder.observe_event(&json!({"type": "agent_start"}), Instant::now());
        assert_eq!(ladder.frames_seen(), 1, "健康度前置由帧数度量");
    }

    // --- 两口钟 -----------------------------------------------------------

    #[test]
    fn the_silent_clock_expires_only_after_the_configured_gap() {
        let mut ladder = ladder();
        let start = Instant::now();
        ladder.note_frame(start);

        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            ladder.check(Instant::now()),
            LadderAction::KeepWaiting,
            "静默钟没到点不得升级"
        );

        std::thread::sleep(Duration::from_millis(150));
        match ladder.check(Instant::now()) {
            LadderAction::SilentClockExpired { silent_for } => {
                assert!(silent_for >= ladder.config().silent_clock, "{silent_for:?}");
            }
            other => panic!("静默钟到点必须升级，got {other:?}"),
        }
    }

    #[test]
    fn the_total_clock_expires_even_while_frames_keep_arriving() {
        let mut ladder = JudgementLadder::new(LadderConfig::fast(10_000, 120, 300));
        let deadline = Instant::now() + ladder.config().total_clock;
        while Instant::now() < deadline {
            ladder.note_frame(Instant::now());
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            matches!(ladder.check(Instant::now()), LadderAction::TotalClockExpired { .. }),
            "总钟与有没有帧无关"
        );
    }

    // --- abort 探针（独立死线）------------------------------------------

    #[test]
    fn the_abort_probe_deadline_is_independent_of_incoming_frames() {
        let mut ladder = JudgementLadder::new(LadderConfig::fast(100, 10_000, 200));
        ladder.request_abort(Instant::now());
        assert_eq!(ladder.phase(), LadderPhase::Aborting);

        // pi 被杀前还在吐帧：探针时钟不因此重置。
        let probe = ladder.config().abort_probe;
        while ladder.abort_waited(Instant::now()).unwrap_or_default() < probe - Duration::from_millis(40)
        {
            ladder.observe_event(&json!({"type": "queue_update"}), Instant::now());
            assert_eq!(
                ladder.check(Instant::now()),
                LadderAction::KeepWaiting,
                "死线没到不得强杀——来帧不是 pi 响应了 abort"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(60));
        match ladder.check(Instant::now()) {
            LadderAction::AbortProbeExpired { waited } => {
                assert!(waited >= ladder.config().abort_probe, "{waited:?}");
            }
            other => panic!("abort 探针死线到点必须强杀，got {other:?}"),
        }
    }

    #[test]
    fn requesting_abort_twice_does_not_restart_the_probe_deadline() {
        let mut ladder = JudgementLadder::new(LadderConfig::fast(100, 10_000, 200));
        ladder.request_abort(Instant::now());
        std::thread::sleep(Duration::from_millis(120));
        ladder.request_abort(Instant::now());
        assert!(
            ladder.abort_waited(Instant::now()).unwrap_or_default()
                >= ladder.config().abort_probe - Duration::from_millis(80),
            "重复发 abort 不得给探针续命"
        );
    }

    #[test]
    fn the_total_clock_still_wins_while_aborting() {
        let mut ladder = JudgementLadder::new(LadderConfig::fast(50, 150, 10_000));
        ladder.request_abort(Instant::now());
        std::thread::sleep(Duration::from_millis(180));
        assert!(
            matches!(ladder.check(Instant::now()), LadderAction::TotalClockExpired { .. }),
            "总钟是硬预算：探针相也拦不住"
        );
    }

    // --- 完成双条件 -------------------------------------------------------

    #[test]
    fn settle_needs_both_conditions_not_just_the_agent_settled_event() {
        let mut ladder = ladder();
        let now = Instant::now();
        ladder.observe_event(&json!({"type": "agent_settled"}), now);
        assert!(!ladder.is_settled(), "只有 agent_settled 不算完");

        ladder.observe_event(&message_end("length"), now);
        assert!(
            !ladder.is_settled(),
            "stopReason=length 是截断，不是正常停下"
        );
        assert_eq!(ladder.check(now), LadderAction::KeepWaiting);

        ladder.observe_event(&message_end("stop"), now);
        assert!(ladder.is_settled(), "双条件都满足才算完");
        assert_eq!(ladder.check(now), LadderAction::Settled);
    }

    #[test]
    fn the_last_authoritative_message_decides_not_the_first_one() {
        let mut ladder = ladder();
        let now = Instant::now();
        ladder.observe_event(&message_end("stop"), now);
        ladder.note_settled();
        assert!(ladder.is_settled(), "先到的 stop 让双条件成立");

        // 之后又来一条 stopReason=length 的权威消息：双条件的第二半翻案。
        ladder.observe_event(&message_end("length"), now);
        assert!(
            !ladder.is_settled(),
            "最后一条权威 assistant 消息说了算"
        );
    }

    #[test]
    fn the_extra_system_message_in_agent_end_is_not_the_last_assistant_message() {
        let mut ladder = ladder();
        let now = Instant::now();
        ladder.observe_event(&message_end("stop"), now);
        ladder.note_settled();

        // 1.0.2 的 agent_end：assistant 在前，多出来的 system 消息在**最后**。
        let agent_end = json!({
                "type": "agent_end",
                "willRetry": false,
                "messages": [
                    {"role": "assistant", "id": "msg-1", "stopReason": "stop", "usage": {"input": 1}},
            {"role": "system", "id": "system-1", "content": []},
            ],
        });
        ladder.observe_event(&agent_end, now);

        assert!(
            ladder.is_settled(),
            "system 消息不得把双条件的第二半判丢"
        );
        assert_eq!(ladder.last_stop_reason(), Some("stop"));
        assert_eq!(ladder.authoritative_messages(), 2, "assistant + system 都过了一遍筛");
    }

    #[test]
    fn a_message_without_a_stop_reason_leaves_the_second_condition_unsatisfied() {
        let mut ladder = ladder();
        let now = Instant::now();
        ladder.note_settled();
        ladder.observe_event(
            &json!({"type": "message_end", "message": {"role": "assistant", "id": "m"}}),
            now,
        );
        assert!(!ladder.is_settled(), "缺 stopReason 不是 stop");
    }

    // --- 事件信号（记账输入）--------------------------------------------

    #[test]
    fn streaming_snapshots_carry_no_authoritative_usage_source() {
        let mut ladder = ladder();
        let event = json!({"type": "message_update", "usage": {"input": 60, "output": 6}});
        let signals = ladder.observe_event(&event, Instant::now());
        assert!(
            !signals.has_usage(),
            "message_update 的累积快照不是权威来源，入账即虚报"
        );
    }

    #[test]
    fn a_message_end_frame_yields_its_usage_as_the_authoritative_source() {
        let mut ladder = ladder();
        let frame = message_end("stop");
        let signals = ladder.observe_event(&frame, Instant::now());
        assert!(signals.has_usage());
        assert_eq!(signals.message_end.expect("message_end").usage.expect("usage")["input"], 100);
        assert_eq!(signals.message_end.expect("message_end").id, Some("msg-1"));
    }

    #[test]
    fn an_agent_end_frame_yields_every_non_system_message_usage() {
        let mut ladder = ladder();
        let event = json!({
            "type": "agent_end",
            "messages": [
                {"role": "assistant", "id": "msg-1", "usage": {"input": 100}},
                {"role": "system", "id": "system-1", "usage": {"input": 999}},
            ],
        });
        let signals = ladder.observe_event(&event, Instant::now());
        assert_eq!(signals.agent_end_messages.len(), 1, "system 消息的用量不得入账");
        assert_eq!(signals.agent_end_messages[0].usage.expect("usage")["input"], 100);
    }

    #[test]
    fn a_nested_tool_usage_frame_yields_its_parent_tool_call_id() {
        let mut ladder = ladder();
        // 权威形状：parentToolCallId 住在 usage 对象里（ToolResultMessage.usage）。
        let event = json!({
            "type": "tool_execution_end",
            "toolCallId": "call-1",
            "result": {
                "content": [],
                "usage": {"input": 40, "output": 5, "parentToolCallId": "call-parent-1"},
            },
        });
        let signals = ladder.observe_event(&event, Instant::now());
        assert!(signals.has_usage());
        assert_eq!(signals.parent_tool_call_id, Some("call-parent-1"));
        assert_eq!(signals.nested_usage.unwrap()["input"], 40);
    }

    #[test]
    fn a_parent_tool_call_id_on_the_event_envelope_is_also_accepted() {
        let mut ladder = ladder();
        // 有些版本把它放在事件顶层：认，但不因此漏记用量。
        let event = json!({
            "type": "tool_execution_end",
            "toolCallId": "call-1",
            "parentToolCallId": "call-parent-1",
            "result": {"content": [], "usage": {"input": 40}},
        });
        let signals = ladder.observe_event(&event, Instant::now());
        assert_eq!(signals.parent_tool_call_id, Some("call-parent-1"));
    }

    // --- next_wait -------------------------------------------------------

    #[test]
    fn next_wait_never_exceeds_the_tick_and_never_returns_zero() {
        let ladder = JudgementLadder::new(LadderConfig::fast(10_000, 10_000, 10_000));
        let wait = ladder.next_wait(Instant::now());
        assert!(wait <= ladder.config().tick, "{wait:?}");
        assert!(wait >= Duration::from_millis(1), "{wait:?}");
    }

    #[test]
    fn next_wait_aims_at_the_nearest_deadline() {
        let ladder = JudgementLadder::new(LadderConfig::fast(80, 10_000, 10_000));
        let wait = ladder.next_wait(Instant::now());
        assert!(
            wait <= Duration::from_millis(80),
            "静默钟更近就该先醒：{wait:?}"
        );
    }
}
