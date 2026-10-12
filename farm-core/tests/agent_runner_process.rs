//! pi 子进程接缝测试（ticket #16 slice 1：spawn + 版本探测 + 读循环 + 有序停机）。
//!
//! 全部经**真实子进程**打接缝：spawn `fake-pi` 替身、按 `\n` 读写 JSONL。
//! 命令路径可注入（`PiSpawnPlan::program`）正是这条接缝能跑在替身之上的原因。

use std::time::Duration;

use farm_core::agent_runner::process::{probe_version, PiProcess, PiSpawnPlan};
use farm_core::agent_runner::rpc::{event_type, parse_frame, Frame};
use serde_json::{json, Value};

/// fake pi 替身的绝对路径（cargo 为集成测试提供）。
const FAKE_PI: &str = env!("CARGO_BIN_EXE_fake-pi");

fn plan(scenario: &str) -> PiSpawnPlan {
    PiSpawnPlan::new(
        FAKE_PI,
        vec!["--mode".to_string(), "rpc".to_string()],
    )
    .with_env("FAKE_PI_SCENARIO", scenario)
}

/// 读到某个事件类型为止（跳过中间的帧），返回该帧。
async fn next_of_type(
    frames: &mut tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    event_type: &str,
) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let frame = tokio::time::timeout_at(deadline, frames.recv())
            .await
            .expect("frame must arrive before the deadline")
            .expect("read loop must still be alive");
        let value: Value = serde_json::from_slice(&frame).expect("json frame");
        if value.get("type").and_then(|t| t.as_str()) == Some(event_type) {
            return value;
        }
    }
}

#[tokio::test]
async fn spawning_the_stand_in_and_probing_gets_the_pinned_wire_version() {
    let mut process = PiProcess::spawn(plan("normal")).await.expect("spawn");
    let mut parts = process.into_parts();

    let (probe, leftovers) =
        probe_version(&mut parts, "1.0.2", Duration::from_secs(5)).await;

    assert_eq!(probe.expected, "1.0.2");
    assert_eq!(probe.reported.as_deref(), Some("1.0.2"), "替身按 1.0.2 自描述");
    assert!(probe.is_aligned(), "替身说的就是规格钉的版本");
    assert!(probe.session_id.is_some(), "探测要拿到 session id（fork 找回用）");
    assert!(
        probe.deviations.is_empty(),
        "版本一致时不得记偏差: {:?}",
        probe.deviations
    );
    assert!(leftovers.is_empty(), "探测之前不该有别的帧");

    process = PiProcess::from_parts(parts);
    process.close_stdin().await;
}

#[tokio::test]
async fn a_child_without_a_version_field_records_a_deviation_instead_of_failing() {
    let mut plan = plan("normal");
    plan.env.push(("FAKE_PI_MISSING_VERSION".into(), "1".into()));
    let mut process = PiProcess::spawn(plan).await.expect("spawn");
    let mut parts = process.into_parts();

    let (probe, _leftovers) =
        probe_version(&mut parts, "1.0.2", Duration::from_secs(5)).await;

    assert_eq!(probe.reported, None, "替身这一轮真的不带版本字段");
    assert!(!probe.is_aligned());
    assert_eq!(probe.deviations.len(), 1, "缺版本 = 一条偏差记录");
    assert!(
        probe.deviations[0].contains("did not report a version"),
        "{}",
        probe.deviations[0]
    );

    process = PiProcess::from_parts(parts);
    process.close_stdin().await;
}

#[tokio::test]
async fn a_child_reporting_another_version_records_the_deviation_and_keeps_running() {
    let mut plan = plan("normal");
    plan.env.push(("FAKE_PI_VERSION".into(), "0.84.3".into()));
    let mut process = PiProcess::spawn(plan).await.expect("spawn");
    let mut parts = process.into_parts();

    let (probe, _leftovers) =
        probe_version(&mut parts, "1.0.2", Duration::from_secs(5)).await;

    assert_eq!(probe.reported.as_deref(), Some("0.84.3"));
    assert!(!probe.is_aligned());
    assert!(
        probe.deviations.iter().any(|d| d.contains("0.84.3")),
        "版本偏差必须点名实测版本: {:?}",
        probe.deviations
    );

    process = PiProcess::from_parts(parts);
    process.close_stdin().await;
}

#[tokio::test]
async fn the_read_loop_delivers_every_frame_of_a_turn_until_agent_settled() {
    let mut process = PiProcess::spawn(plan("normal")).await.expect("spawn");
    let mut parts = process.into_parts();
    let (probe, leftovers) =
        probe_version(&mut parts, "1.0.2", Duration::from_secs(5)).await;
    assert!(probe.is_aligned());

    process = PiProcess::from_parts(parts);
    process
        .send_kind(
            "req-1",
            farm_core::agent_runner::rpc::CommandKind::Prompt {
                message: "hello".into(),
                streaming_behavior: None,
            },
        )
        .await
        .expect("prompt must be written to pi stdin");
    let mut parts = process.into_parts();

    // 探测阶段可能顺带收到帧：先回放，再继续读。
    let mut seen: Vec<String> = Vec::new();
    for frame in leftovers {
        seen.push(type_of(&frame));
    }
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), parts.frames.recv())
            .await
            .expect("frame must arrive")
            .expect("read loop alive");
        let value: Value = serde_json::from_slice(&frame).expect("json frame");
        seen.push(type_of(&frame));
        if event_type(&value) == Some("agent_settled") {
            break;
        }
    }

    assert_eq!(
        seen,
        vec![
            "response:prompt",
            "agent_start",
            "turn_start",
            "message_start",
            "message_update",
            "tool_execution_start",
            "tool_execution_end",
            "message_end",
            "agent_end",
            "agent_settled",
        ],
        "读循环必须按序把一整轮的帧全部交出来"
    );

    process = PiProcess::from_parts(parts);
    process.close_stdin().await;
}

#[tokio::test]
async fn closing_stdin_ends_the_read_loop_and_the_child_exits_by_itself() {
    let mut process = PiProcess::spawn(plan("normal")).await.expect("spawn");
    let mut parts = process.into_parts();

    // 读循环健康度前置：EOF 是「子进程把自己收尾了」的信号。
    process = PiProcess::from_parts(parts);
    process.close_stdin().await;
    parts = process.into_parts();

    let eof = tokio::time::timeout(Duration::from_secs(10), parts.frames.recv())
        .await
        .expect("read loop must report EOF");
    assert!(eof.is_none(), "stdin 关闭后读循环必须结束（EOF）");

    let status = tokio::time::timeout(Duration::from_secs(10), parts.child.wait())
        .await
        .expect("child must exit")
        .expect("wait");
    assert!(status.success(), "stdin 关闭应有序退出: {status}");
}

#[tokio::test]
async fn a_frame_carrying_unicode_separators_survives_the_read_loop() {
    let mut process = PiProcess::spawn(plan("separators")).await.expect("spawn");
    let mut parts = process.into_parts();
    let (probe, _leftovers) =
        probe_version(&mut parts, "1.0.2", Duration::from_secs(5)).await;
    assert!(probe.is_aligned());

    process = PiProcess::from_parts(parts);
    process
        .send_kind(
            "req-1",
            farm_core::agent_runner::rpc::CommandKind::Prompt {
                message: "hello".into(),
                streaming_behavior: None,
            },
        )
        .await
        .expect("prompt");
    let mut parts = process.into_parts();

    let message_end = next_of_type(&mut parts.frames, "message_end").await;
    let text = message_end["message"]["content"][0]["text"]
        .as_str()
        .expect("text");
    assert!(
        text.contains('\u{2028}') && text.contains('\u{2029}'),
        "U+2028/U+2029 必须完整落在一帧里: {text:?}"
    );

    process = PiProcess::from_parts(parts);
    process.close_stdin().await;
}

fn type_of(frame: &[u8]) -> String {
    match parse_frame(frame) {
        Frame::Response(response) => format!("response:{}", response.command),
        Frame::Event(event) => event_type(&event).unwrap_or("<none>").to_string(),
        Frame::Unparsable(_) => "<unparsable>".to_string(),
    }
}

/// 命令行的形状也要能序列化（读循环写出去的帧只补 `\n`）。
#[test]
fn a_command_line_ends_with_exactly_one_newline() {
    let command = farm_core::agent_runner::rpc::RpcCommand {
        id: "req-1".into(),
        kind: farm_core::agent_runner::rpc::CommandKind::ClearQueue,
    };
    let line = serde_json::to_vec(&command).expect("serialize");
    assert!(line.ends_with(b"}"));
    assert_eq!(
        serde_json::to_value(&command).expect("value"),
        json!({"id": "req-1", "type": "clear_queue"})
    );
}
