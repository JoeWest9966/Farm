//! pi RPC wire protocol：JSONL 帧定界 + 命令/响应/事件的容错形状。
//!
//! 规格来源：`docs/research/pi-runtime-interface.md`（**文档化 1.0.2**；本机安装的是
//! 0.84.3，故本模块按文档建模，版本偏差由 [`crate::agent_runner::process`] 的启动探测
//! 记录而不是硬失败）。
//!
//! **帧定界是硬约束**：只按 `\n` 切分，允许剥掉尾随 `\r`；**不能**用会把 Unicode 行
//! 分隔符 `U+2028`/`U+2029` 当换行的通用 line reader（Node `readline` 就因此不合规）。
//! Rust 侧用 [`FrameReader`]（`BufRead::read_until(b'\n')`）或纯函数 [`drain_frames`]。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncBufRead;

pub use farm_sdk::services::agent_runner::Disposition;

/// 宿主 → pi 的命令（行分隔 JSON）。`id` 可选，用于请求/响应配对。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RpcCommand {
    pub id: String,
    #[serde(flatten)]
    pub kind: CommandKind,
}

/// 命令清单（34 个里的本票用到的子集；其余按需加法追加）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CommandKind {
    Prompt {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        streaming_behavior: Option<String>,
    },
    Steer {
        message: String,
    },
    FollowUp {
        message: String,
    },
    Abort,
    ClearQueue,
    Fork {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entry_id: Option<String>,
    },
    GetState,
    GetEntries {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<String>,
    },
    GetSessionStats,
}

impl CommandKind {
    /// 协议里的命令名（与 `type` 字段一致）。
    pub fn name(&self) -> &'static str {
        match self {
            CommandKind::Prompt { .. } => "prompt",
            CommandKind::Steer { .. } => "steer",
            CommandKind::FollowUp { .. } => "follow_up",
            CommandKind::Abort => "abort",
            CommandKind::ClearQueue => "clear_queue",
            CommandKind::Fork { .. } => "fork",
            CommandKind::GetState => "get_state",
            CommandKind::GetEntries { .. } => "get_entries",
            CommandKind::GetSessionStats => "get_session_stats",
        }
    }
}

/// pi → 宿主的响应：`{"type":"response","command":...,"success":bool,"id":...}`。
///
/// `disposition` 是 1.0.2 给 `prompt`/`steer`/`follow_up` 的受理裁决；0.84.3 没有这个
/// 字段，故 `Option`（缺失 = 版本偏差，记录不断失败）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RpcResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub command: String,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition: Option<Disposition>,
}

/// 一帧线内容：要么是响应，要么是事件（事件不带 `type:"response"`）。
#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    Response(RpcResponse),
    Event(Value),
    /// 解析不了的帧（协议偏差：记录，不致命）。
    Unparsable(String),
}

/// 把一行 JSONL 解成 [`Frame`]。未知字段一律忽略（版本偏斜下的前向兼容）。
pub fn parse_frame(line: &[u8]) -> Frame {
    let text = match std::str::from_utf8(line) {
        Ok(t) => t,
        Err(_) => return Frame::Unparsable(String::from_utf8_lossy(line).into_owned()),
    };
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return Frame::Unparsable(text.to_string()),
    };
    let is_response = value
        .get("type")
        .and_then(|t| t.as_str())
        .map(|t| t == "response")
        .unwrap_or(false);
    if is_response {
        match serde_json::from_value::<RpcResponse>(value.clone()) {
            Ok(response) => Frame::Response(response),
            Err(_) => Frame::Unparsable(text.to_string()),
        }
    } else {
        Frame::Event(value)
    }
}

/// 帧事件类型（`type` 字段），未知事件原样交给调用方。
pub fn event_type(event: &Value) -> Option<&str> {
    event.get("type").and_then(|t| t.as_str())
}

// ---------------------------------------------------------------------------
// 帧定界：只按 \n
// ---------------------------------------------------------------------------

/// 从缓冲区里切出所有完整帧（**只按 `\n`**），剩余的不完整字节留在 `buf`。
///
/// 纯函数：定界规则在这里单一源定义，异步读循环与单元测试共用。
/// `U+2028`/`U+2029` 是多字节 UTF-8 序列、永不含 `0x0A`，因此落在 JSON 字符串里时
/// 不会被切开——这正是「禁用智能 line reader」的含义。
pub fn drain_frames(buf: &mut Vec<u8>, out: &mut Vec<Vec<u8>>) {
    while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
        let mut line: Vec<u8> = buf.drain(..=pos).collect();
        line.pop(); // 去掉 \n
        if line.last() == Some(&b'\r') {
            line.pop(); // 允许 \r\n
        }
        if !line.is_empty() {
            out.push(line);
        }
    }
}

/// 异步 JSONL 读循环：`BufRead::read_until(b'\n')` 取帧，交给 [`drain_frames`]。
///
/// **不用**任何「智能按行」封装（`tokio::io::Lines`/`split` 之外的自定义 reader 一律
/// 不得把 Unicode 分隔符当换行）。
pub struct FrameReader<R> {
    reader: R,
    buf: Vec<u8>,
}

impl<R> FrameReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            buf: Vec::new(),
        }
    }

    pub fn into_inner(self) -> R {
        self.reader
    }
}

impl<R: AsyncBufRead + Unpin> FrameReader<R> {
    /// 读下一帧；`Ok(None)` = EOF（读循环健康度信号，交给判据阶梯解释）。
    pub async fn next_frame(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        use tokio::io::AsyncBufReadExt;
        loop {
            let mut frames = Vec::new();
            drain_frames(&mut self.buf, &mut frames);
            if let Some(frame) = frames.into_iter().next() {
                return Ok(Some(frame));
            }
            let read = self.reader.read_until(b'\n', &mut self.buf).await?;
            if read == 0 {
                // EOF：把残余（不完整帧）丢掉，报读循环结束。
                self.buf.clear();
                return Ok(None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_splits_only_on_lf_and_keeps_unicode_separators_inside_a_string() {
        // U+2028 / U+2029 是合法 JSON 字符串内容；智能 line reader 会在这里错切。
        let mut buf = Vec::new();
        buf.extend_from_slice(
            "{\"text\":\"a\u{2028}b\u{2029}c\"}\n{\"text\":\"tail\"}".as_bytes(),
        );
        let mut frames = Vec::new();
        drain_frames(&mut buf, &mut frames);

        assert_eq!(frames.len(), 1, "第一帧必须完整，不得被 U+2028 切开");
        assert_eq!(
            String::from_utf8(frames[0].clone()).expect("utf8"),
            "{\"text\":\"a\u{2028}b\u{2029}c\"}"
        );
        assert_eq!(buf, b"{\"text\":\"tail\"}", "不完整帧留在缓冲里等下一个 \\n");
    }

    #[test]
    fn framing_accepts_crlf_and_skips_blank_lines() {
        let mut buf = b"{\"a\":1}\r\n\n{\"b\":2}\n".to_vec();
        let mut frames = Vec::new();
        drain_frames(&mut buf, &mut frames);

        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0], b"{\"a\":1}");
        assert_eq!(frames[1], b"{\"b\":2}");
        assert!(buf.is_empty());
    }

    #[test]
    fn a_response_frame_carries_the_disposition_of_the_injection() {
        let frame = parse_frame(
            br#"{"id":"req-1","type":"response","command":"steer","success":true,"disposition":"deferred"}"#,
        );
        match frame {
            Frame::Response(response) => {
                assert!(response.success);
                assert_eq!(
                    response.disposition,
                    Some(Disposition("deferred".into())),
                    "settle 期注入的 disposition 必须是 deferred"
                );
                assert!(response.disposition.as_ref().expect("disposition").is_deferred());
            }
            other => panic!("expected a response frame, got {other:?}"),
        }
    }

    #[test]
    fn an_event_frame_is_passed_through_untouched() {
        let frame = parse_frame(br#"{"type":"agent_settled"}"#);
        match frame {
            Frame::Event(event) => assert_eq!(event_type(&event), Some("agent_settled")),
            other => panic!("expected an event frame, got {other:?}"),
        }
    }

    #[test]
    fn a_frame_without_a_type_field_is_unparsable_rather_than_fatal() {
        let frame = parse_frame(br#"{"command":"prompt"}"#);
        assert!(
            matches!(frame, Frame::Event(_)),
            "无 type 的帧按事件透传（偏差记录，不致命）"
        );
    }

    #[test]
    fn an_unknown_disposition_is_a_recorded_deviation_not_a_rejection() {
        let disposition = Disposition("brand-new-value".into());
        assert!(
            !disposition.is_known(),
            "未知取值必须可被识别为版本偏差"
        );
        assert!(!disposition.is_accepted());
        assert!(!disposition.is_rejected());
        assert!(!disposition.is_deferred());
    }
}
