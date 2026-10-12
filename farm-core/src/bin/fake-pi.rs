//! fake pi —— ticket #16 的 pi RPC **测试替身**（说文档化 1.0.2 wire protocol 的 JSONL）。
//!
//! 为什么需要它：本机安装的 `pi` 是 **0.84.3**，而规格钉 **1.0.2**（ADR-0004 决策 5）。
//! agent-runner 按**文档化的 1.0.2 协议**实现，因此测试跑在这个替身之上；真 pi 的冒烟
//! 测试另走「启动探测 + 记录偏差、不断言失败」的路径（见 `tests/agent_runner_real_pi.rs`）。
//!
//! 替身覆盖 1.0.2 文档面里本票用到的全部形状：
//! - 帧只按 `\n` 定界（会在文本里故意放 `U+2028`/`U+2029`，见 `separators` 场景）；
//! - `prompt`/`steer`/`follow_up` 响应带 **`disposition`**；
//! - `agent_end` 比 0.84.3 **多一条 system 消息**（放在最后，专治「取最后一条消息」的懒判据）；
//! - tool 用量带 **`parentToolCallId`**（嵌套用量，必须进绝对累计）；
//! - **settle 期注入**进 `_deferredSettledActions`，**推迟到 settle 后执行**；
//! - `clear_queue` 往返、`fork` 找回、`get_state`/`get_session_stats`/`get_entries`。
//!
//! 场景与参数全部经**子进程环境变量**注入（宿主把 env 只注入 pi 子进程——这同时是
//! 票 #17 凭据注入接缝的验证通道）：
//!
//! | env | 说明 |
//! | --- | --- |
//! | `FAKE_PI_SCENARIO` | `normal`(默认)/`silence`/`total-clock`/`abort-probe`/`fork-recovery`/`token-streaming`/`no-stop-reason`/`deferred-settle`/`reject-steer`/`separators`/`quiet` |
//! | `FAKE_PI_VERSION` | 探测到的版本串（默认 `1.0.2`） |
//! | `FAKE_PI_MISSING_VERSION` | `1` = `get_state` 不带版本字段（版本偏差） |
//! | `FAKE_PI_LOG` | 追加写命令/env 日志的路径（测试断言用） |
//! | `FAKE_PI_SETTLE_DELAY_MS` | settle 窗口长度（settle 期注入的推迟窗口） |
//! | `FAKE_PI_EVENT_INTERVAL_MS` | 流式事件间隔（`total-clock` 场景） |
//! | `FAKE_PI_CRASH_AFTER_MS` | `fork-recovery` 场景：首个子进程撑多久后崩溃 |
//! | `FAKE_PI_SECRET_ECHO` | 回显指定 env 的值（凭据只进子进程的断言） |

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{BufRead, Write};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut fake = FakePi::new(&args);
    fake.run();
}

/// 用量（绝对累计的输入项；`totalTokens` 由四料相加，与 1.0.2 文档一致）。
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct Usage {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
}

impl Usage {
    fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    fn add(&mut self, other: Usage) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
    }

    fn to_json(&self) -> Value {
        json!({
            "input": self.input,
            "output": self.output,
            "cacheRead": self.cache_read,
            "cacheWrite": self.cache_write,
            "totalTokens": self.total(),
            "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0},
        })
    }
}

/// 场景常量：确定性用量数字（测试按**已知字面量**断言，不重算实现）。
const NORMAL_ASSISTANT_USAGE: Usage = Usage {
    input: 100,
    output: 10,
    cache_read: 0,
    cache_write: 0,
};
const NORMAL_NESTED_USAGE: Usage = Usage {
    input: 40,
    output: 5,
    cache_read: 0,
    cache_write: 0,
};
const STREAM_ASSISTANT_USAGE: Usage = Usage {
    input: 60,
    output: 6,
    cache_read: 0,
    cache_write: 0,
};
const STREAM_NESTED_USAGE: Usage = Usage {
    input: 25,
    output: 4,
    cache_read: 0,
    cache_write: 0,
};
const PARENT_TOOL_CALL_ID: &str = "call-parent-1";

struct FakePi {
    scenario: String,
    /// `--session <id>`：带上它就是「续会话/被 fork 出来的子进程」。
    session: Option<String>,
    session_id: String,
    version: String,
    missing_version: bool,
    out: std::io::Stdout,
    log: Option<std::fs::File>,
    steering: Vec<String>,
    follow_up: Vec<String>,
    /// `_deferredSettledActions`：settle 期注入，推迟到 settle 后执行。
    deferred: Vec<(String, String)>,
    counted: Usage,
    nested_calls: u64,
    next_message: u64,
    /// settle 窗口截止时刻（`deferred-settle` 场景）。
    settle_window_until: Option<Instant>,
    /// 流式事件的下一次时刻（`total-clock` 场景）。
    stream_next: Option<Instant>,
    stream_interval: Duration,
    prompt_message: String,
    crashed: bool,
}

impl FakePi {
    fn new(args: &[String]) -> Self {
        let scenario = std::env::var("FAKE_PI_SCENARIO").unwrap_or_else(|_| "normal".into());
        let version = std::env::var("FAKE_PI_VERSION").unwrap_or_else(|_| "1.0.2".into());
        let missing_version = std::env::var("FAKE_PI_MISSING_VERSION").map(|v| v == "1").unwrap_or(false);
        let session = flag_value(args, "--session");
        let session_id = session
            .clone()
            .unwrap_or_else(|| format!("fake-session-{}", std::process::id()));
        let log = std::env::var("FAKE_PI_LOG").ok().and_then(|path| {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok()
        });
        let stream_interval = Duration::from_millis(
            std::env::var("FAKE_PI_EVENT_INTERVAL_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(20),
        );
        Self {
            scenario,
            session,
            session_id,
            version,
            missing_version,
            out: std::io::stdout(),
            log,
            steering: Vec::new(),
            follow_up: Vec::new(),
            deferred: Vec::new(),
            counted: Usage::default(),
            nested_calls: 0,
            next_message: 1,
            settle_window_until: None,
            stream_next: None,
            stream_interval,
            prompt_message: String::new(),
            crashed: false,
        }
    }

    // -- 输出 / 日志 -------------------------------------------------------

    fn emit(&mut self, value: Value) {
        let line = serde_json::to_string(&value).expect("fake pi event must serialize");
        let _ = self.out.write_all(line.as_bytes());
        let _ = self.out.write_all(b"\n");
        let _ = self.out.flush();
    }

    fn log(&mut self, entry: Value) {
        if let Some(file) = self.log.as_mut() {
            let line = serde_json::to_string(&entry).expect("log entry must serialize");
            let _ = writeln!(file, "{line}");
            let _ = file.flush();
        }
    }

    fn respond(
        &mut self,
        id: Option<&str>,
        command: &str,
        success: bool,
        data: Option<Value>,
        disposition: Option<&str>,
    ) {
        let mut value = json!({"type": "response", "command": command, "success": success});
        if let Some(id) = id {
            value["id"] = json!(id);
        }
        if let Some(disposition) = disposition {
            value["disposition"] = json!(disposition);
        }
        if let Some(data) = data {
            value["data"] = data;
        }
        self.emit(value);
    }

    fn ok(&mut self, id: Option<&str>, command: &str, data: Option<Value>) {
        self.respond(id, command, true, data, None);
    }

    fn unknown_command(&mut self, id: Option<&str>, command: &str) {
        self.respond(
            id,
            command,
            false,
            None,
            Some(&format!("unknown command in fake pi: {command}")),
        );
    }

    // -- 主循环 -----------------------------------------------------------

    fn run(&mut self) {
        self.log(json!({
            "t": "spawn",
            "args": std::env::args().skip(1).collect::<Vec<_>>(),
            "session": self.session,
            "scenario": self.scenario,
            "version": self.version,
            "env": std::env::vars().collect::<BTreeMap<_, _>>(),
        }));

        // stdin 由独立线程按行读入（std 的 `lines()` 也只按 \n 切分），主线程掌管时序：
        // settle 窗口、流式节奏都必须在**不收命令**的情况下继续走。
        let (tx, rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                match line {
                    Ok(line) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        loop {
            let timeout = self.next_tick();
            match rx.recv_timeout(timeout) {
                Ok(line) => self.handle(&line),
                Err(RecvTimeoutError::Timeout) => self.tick(),
                Err(RecvTimeoutError::Disconnected) => break, // stdin 关闭：有序停机
            }
        }
    }

    /// 下一次定时动作的等待时长（没有定时动作时给一个较大的轮询间隔）。
    fn next_tick(&self) -> Duration {
        let now = Instant::now();
        let mut next = Duration::from_millis(250);
        if let Some(until) = self.settle_window_until {
            next = next.min(until.saturating_duration_since(now).max(Duration::from_millis(1)));
        }
        if let Some(at) = self.stream_next {
            next = next.min(at.saturating_duration_since(now).max(Duration::from_millis(1)));
        }
        next
    }

    fn tick(&mut self) {
        let now = Instant::now();
        if let Some(until) = self.settle_window_until {
            if now >= until {
                self.settle_window_until = None;
                self.emit(json!({"type": "agent_settled"}));
                // settle 完成后执行被推迟的注入（_deferredSettledActions）。
                let deferred = std::mem::take(&mut self.deferred);
                for (channel, message) in deferred {
                    self.log(json!({"t": "deferred_exec", "channel": channel, "message": message}));
                    self.run_turn();
                }
                self.stream_next = None;
            }
        }
        if let Some(at) = self.stream_next {
            if now >= at {
                self.stream_next = Some(now + self.stream_interval);
                let usage = self.current_cumulative_usage();
                self.emit(json!({
                    "type": "message_update",
                    "usage": usage.to_json(),
                    "assistantMessageEvent": {"type": "text_delta", "contentIndex": 0, "delta": "…"},
                }));
            }
        }
    }

    // -- 命令分发 ---------------------------------------------------------

    fn handle(&mut self, line: &str) {
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => return,
        };
        let command = value
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string();
        let id = value
            .get("id")
            .and_then(|i| i.as_str())
            .map(str::to_string);
        self.log(json!({"t": "cmd", "type": command, "id": id}));

        match command.as_str() {
            "prompt" => self.on_prompt(&value, id),
            "steer" => self.on_inject(&value, id, "steer"),
            "follow_up" => self.on_inject(&value, id, "follow_up"),
            "abort" => self.on_abort(id),
            "clear_queue" => self.on_clear_queue(id),
            "get_state" => self.on_get_state(id),
            "get_messages" => self.ok(id.as_deref(), "get_messages", Some(json!({"messages": []}))),
            "get_session_stats" => self.on_get_session_stats(id),
            "get_entries" => self.on_get_entries(id),
            "fork" => self.on_fork(&value, id),
            "set_steering_mode" | "set_follow_up_mode" | "new_session" | "compact"
            | "set_auto_retry" | "set_auto_compaction" => {
                self.ok(id.as_deref(), &command, Some(json!({"cancelled": false})))
            }
            other => self.unknown_command(id.as_deref(), other),
        }
    }

    fn on_prompt(&mut self, value: &Value, id: Option<String>) {
        let message = value
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or_default()
            .to_string();
        self.prompt_message = message.clone();

        if self.scenario == "fork-recovery" && self.session.is_none() && !self.crashed {
            // 崩溃：受理后立刻死掉，不 settle——读循环健康度前置 → fork 找回。
            self.respond(id.as_deref(), "prompt", true, None, Some("accepted"));
            self.emit(json!({"type": "agent_start"}));
            let _ = self.out.flush();
            self.log(json!({"t": "crash", "after_ms": crash_after_ms().as_millis()}));
            std::thread::sleep(crash_after_ms());
            self.crashed = true;
            let _ = self.out.flush();
            std::process::exit(7);
        }

        self.respond(id.as_deref(), "prompt", true, None, Some("accepted"));
        self.emit(json!({"type": "agent_start"}));

        match self.scenario.as_str() {
            // 之后一个字都不吐：静默钟 / abort 探针 / 以及「活着但拒绝 steer」的场景
            // （reject-steer 必须让 attempt 继续活着，宿主才注得进去）。
            "silence" | "abort-probe" | "reject-steer" => {}
            "total-clock" => {
                self.stream_next = Some(Instant::now());
            }
            "quiet" => {
                self.emit_assistant_turn(NORMAL_ASSISTANT_USAGE, Some(NORMAL_NESTED_USAGE));
            }
            "token-streaming" => self.on_token_streaming(),
            "no-stop-reason" => self.on_no_stop_reason(),
            "deferred-settle" => self.on_deferred_settle(),
            "separators" => self.on_separators(),
            _ => self.run_turn(),
        }
    }

    /// 一个完整轮次：message_start → message_update → tool_execution_* → message_end
    /// → agent_end（**多一条 system 消息**）→ agent_settled。
    fn run_turn(&mut self) {
        let assistant_usage = match self.scenario.as_str() {
            "token-streaming" => STREAM_ASSISTANT_USAGE,
            _ => NORMAL_ASSISTANT_USAGE,
        };
        let nested_usage = match self.scenario.as_str() {
            "token-streaming" => Some(STREAM_NESTED_USAGE),
            _ => Some(NORMAL_NESTED_USAGE),
        };
        self.emit_assistant_turn(assistant_usage, nested_usage);
        self.emit(json!({"type": "agent_settled"}));
    }

    fn emit_assistant_turn(&mut self, assistant_usage: Usage, nested: Option<Usage>) {
        let message_id = format!("msg-{}", self.next_message);
        self.next_message += 1;

        self.emit(json!({"type": "turn_start"}));
        self.emit(json!({
            "type": "message_start",
            "message": {"role": "assistant", "id": message_id},
        }));

        // 流式快照：**累积**口径。宿主不得把它当增量累加（宁少报不虚报）。
        let cumulative = Usage {
            input: assistant_usage.input,
            output: assistant_usage.output,
            cache_read: assistant_usage.cache_read,
            cache_write: assistant_usage.cache_write,
        };
        self.emit(json!({
            "type": "message_update",
            "usage": cumulative.to_json(),
            "assistantMessageEvent": {"type": "text_delta", "contentIndex": 0, "delta": "fake pi says hi"},
        }));

        // 嵌套 tool 调用：usage 带 parentToolCallId（必须进绝对累计）。
        if let Some(nested) = nested {
            self.emit(json!({
                "type": "tool_execution_start",
                "toolCallId": "call-1",
                "toolName": "bash",
                "args": {"command": "true"},
                "parentToolCallId": PARENT_TOOL_CALL_ID,
            }));
            let mut nested_json = nested.to_json();
            nested_json["parentToolCallId"] = json!(PARENT_TOOL_CALL_ID);
            self.emit(json!({
                "type": "tool_execution_end",
                "toolCallId": "call-1",
                "toolName": "bash",
                "isError": false,
                "result": {"content": [{"type": "text", "text": "ok"}], "usage": nested_json},
            }));
        }

        let mut message_json = json!({
            "role": "assistant",
            "id": message_id,
            "content": [{"type": "text", "text": "fake pi says hi"}],
            "provider": "fake",
            "model": "fake/model",
            "usage": assistant_usage.to_json(),
            "stopReason": "stop",
        });
        if self.scenario == "separators" {
            message_json["content"] = json!([{"type": "text", "text": "line\u{2028}sep\u{2029}end"}]);
        }
        self.emit(json!({"type": "message_end", "message": message_json}));

        self.counted.add(assistant_usage);
        if let Some(nested) = nested {
            self.counted.add(nested);
            self.nested_calls += 1;
        }

        // 1.0.2：agent_end 比 0.84.3 多一条 system 消息（放在最后）。
        self.emit(json!({
            "type": "agent_end",
            "willRetry": false,
            "messages": [
                {"role": "assistant", "id": message_id, "usage": assistant_usage.to_json(), "stopReason": "stop"},
                {"role": "system", "id": "system-1", "content": [{"type": "text", "text": "extra system message"}]},
            ],
        }));
    }

    fn on_token_streaming(&mut self) {
        // 5 次累积快照 + 1 次权威 message_end：宿主只能记一份。
        for _ in 0..5 {
            self.emit(json!({
                "type": "message_update",
                "usage": STREAM_ASSISTANT_USAGE.to_json(),
                "assistantMessageEvent": {"type": "text_delta", "contentIndex": 0, "delta": "x"},
            }));
        }
        self.emit_assistant_turn(STREAM_ASSISTANT_USAGE, Some(STREAM_NESTED_USAGE));
        self.emit(json!({"type": "agent_settled"}));
    }

    fn on_no_stop_reason(&mut self) {
        let message_id = "msg-no-stop";
        self.emit(json!({"type": "turn_start"}));
        self.emit(json!({
            "type": "message_end",
            "message": {
                "role": "assistant", "id": message_id,
                "usage": NORMAL_ASSISTANT_USAGE.to_json(),
                "stopReason": "length",
            },
        }));
        self.emit(json!({
            "type": "agent_end",
            "willRetry": false,
            "messages": [{"role": "assistant", "id": message_id, "stopReason": "length"}],
        }));
        self.counted.add(NORMAL_ASSISTANT_USAGE);
        self.emit(json!({"type": "agent_settled"}));
    }

    fn on_deferred_settle(&mut self) {
        let delay = Duration::from_millis(
            std::env::var("FAKE_PI_SETTLE_DELAY_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(400),
        );
        self.emit_assistant_turn(NORMAL_ASSISTANT_USAGE, Some(NORMAL_NESTED_USAGE));
        // 进入 settle 窗口：这期间到的 steer/follow_up 一律 deferred。
        self.settle_window_until = Some(Instant::now() + delay);
    }

    fn on_separators(&mut self) {
        self.scenario = "separators".into();
        self.run_turn();
    }

    fn on_inject(&mut self, value: &Value, id: Option<String>, channel: &str) {
        let message = value
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or_default()
            .to_string();

        if self.scenario == "reject-steer" && channel == "steer" {
            self.respond(id.as_deref(), channel, false, None, Some("rejected"));
            return;
        }

        // settle 期注入：推迟到 settle 后执行（_deferredSettledActions）。
        if self.settle_window_until.is_some() {
            self.deferred.push((channel.to_string(), message.clone()));
            self.respond(id.as_deref(), channel, true, None, Some("deferred"));
            self.emit_queue_update();
            return;
        }

        match channel {
            "steer" => self.steering.push(message),
            _ => self.follow_up.push(message),
        }
        self.respond(id.as_deref(), channel, true, None, Some("queued"));
        self.emit_queue_update();
    }

    fn on_abort(&mut self, id: Option<String>) {
        if self.scenario == "abort-probe" {
            // 故意不退出：宿主必须走 abort 探针的独立死线强杀。
            self.log(json!({"t": "abort_ignored"}));
            return;
        }
        self.respond(id.as_deref(), "abort", true, None, None);
        let _ = self.out.flush();
        std::process::exit(0);
    }

    fn on_clear_queue(&mut self, id: Option<String>) {
        let cleared = self.steering.len() + self.follow_up.len() + self.deferred.len();
        self.steering.clear();
        self.follow_up.clear();
        self.deferred.clear();
        self.ok(
            id.as_deref(),
            "clear_queue",
            Some(json!({"cleared": cleared})),
        );
        self.emit_queue_update();
    }

    fn on_get_state(&mut self, id: Option<String>) {
        let mut data = json!({
            "model": {"id": "fake/model", "provider": "fake"},
            "thinkingLevel": "medium",
            "isStreaming": false,
            "steeringMode": "all",
            "followUpMode": "one-at-a-time",
            "sessionFile": format!("/tmp/fake-{}.jsonl", self.session_id),
            "sessionId": self.session_id,
            "sessionName": "fake-pi",
            "autoCompactionEnabled": true,
            "messageCount": self.next_message,
            "pendingMessageCount": self.steering.len() + self.follow_up.len() + self.deferred.len(),
            "deferredSettledActions": self
                .deferred
                .iter()
                .map(|(channel, message)| json!({"channel": channel, "message": message}))
                .collect::<Vec<_>>(),
        });
        if !self.missing_version {
            data["version"] = json!(self.version);
        }
        self.ok(id.as_deref(), "get_state", Some(data));
    }

    fn on_get_session_stats(&mut self, id: Option<String>) {
        self.ok(
            id.as_deref(),
            "get_session_stats",
            Some(json!({
                "sessionFile": format!("/tmp/fake-{}.jsonl", self.session_id),
                "sessionId": self.session_id,
                "userMessages": 1,
                "assistantMessages": self.next_message.saturating_sub(1),
                "toolCalls": self.nested_calls,
                "toolResults": self.nested_calls,
                "totalMessages": self.next_message,
                "tokens": {
                    "input": self.counted.input,
                    "output": self.counted.output,
                    "cacheRead": self.counted.cache_read,
                    "cacheWrite": self.counted.cache_write,
                    "total": self.counted.total(),
                },
                "cost": 0.0,
                "contextUsage": {"tokens": self.counted.total(), "contextWindow": 200000, "percent": 1},
            })),
        );
    }

    fn on_get_entries(&mut self, id: Option<String>) {
        let prompt = if self.prompt_message.is_empty() {
            "fake prompt"
        } else {
            self.prompt_message.as_str()
        };
        self.ok(
            id.as_deref(),
            "get_entries",
            Some(json!({
                "entries": [
                    {"type": "message", "id": "entry-1", "parentId": null, "timestamp": "2025-01-01T00:00:00.000Z",
                     "message": {"role": "user", "content": [{"type": "text", "text": prompt}]}},
                    {"type": "message", "id": "entry-2", "parentId": "entry-1", "timestamp": "2025-01-01T00:00:01.000Z",
                     "message": {"role": "assistant", "content": [{"type": "text", "text": "ok"}], "stopReason": "stop"}},
                ],
                "leafId": "entry-2",
            })),
        );
    }

    fn on_fork(&mut self, value: &Value, id: Option<String>) {
        let entry_id = value
            .get("entryId")
            .and_then(|e| e.as_str())
            .unwrap_or("entry-1")
            .to_string();
        let text = if self.prompt_message.is_empty() {
            "fake prompt".to_string()
        } else {
            self.prompt_message.clone()
        };
        // fork 之后进入「被 fork 出来的会话」：从该 user message 重新跑。
        self.session = Some(format!("{}-fork-{entry_id}", self.session_id));
        self.session_id = self.session.clone().expect("fork session");
        self.crashed = false;
        self.log(json!({"t": "fork", "entryId": entry_id}));
        self.ok(
            id.as_deref(),
            "fork",
            Some(json!({"text": text, "cancelled": false})),
        );
    }

    fn emit_queue_update(&mut self) {
        self.emit(json!({
            "type": "queue_update",
            "steering": self.steering.clone(),
            "followUp": self.follow_up.clone(),
            "deferredSettledActions": self
                .deferred
                .iter()
                .map(|(channel, message)| json!({"channel": channel, "message": message}))
                .collect::<Vec<_>>(),
        }));
    }

    fn current_cumulative_usage(&self) -> Usage {
        self.counted
    }
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn crash_after_ms() -> Duration {
    Duration::from_millis(
        std::env::var("FAKE_PI_CRASH_AFTER_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30),
    )
}
