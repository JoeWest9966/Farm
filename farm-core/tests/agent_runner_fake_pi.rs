//! fake pi RPC 替身 + 帧定界接缝测试（ticket #16）。
//!
//! 全部经**真实子进程**打接缝：spawn `fake-pi`、按 `\n` 读写 JSONL。替身的场景由子进程
//! env 注入（宿主把 env 只注入 pi 子进程——这条通道同时是 #17 凭据注入接缝的验证面）。

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};

/// fake pi 替身的绝对路径（cargo 为集成测试提供）。
const FAKE_PI: &str = env!("CARGO_BIN_EXE_fake-pi");

/// 一个最小 JSONL 客户端：只按 `\n` 读写（与宿主同一套定界规则）。
struct FakePiClient {
    child: Child,
    stdin: Option<std::process::ChildStdin>,
    lines: std::io::Lines<BufReader<std::process::ChildStdout>>,
}

impl FakePiClient {
    fn spawn(scenario: &str) -> Self {
        Self::spawn_with_env(scenario, &[])
    }

    fn spawn_with_env(scenario: &str, env: &[(&str, &str)]) -> Self {
        let mut command = Command::new(FAKE_PI);
        command
            .args(["--mode", "rpc"])
            .env("FAKE_PI_SCENARIO", scenario)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("fake pi must spawn");
        let stdin = Some(child.stdin.take().expect("stdin"));
        let stdout = child.stdout.take().expect("stdout");
        Self {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
        }
    }

    fn send(&mut self, command: Value) {
        let line = serde_json::to_string(&command).expect("serialize");
        let stdin = self.stdin.as_mut().expect("stdin still open");
        stdin
            .write_all(line.as_bytes())
            .expect("write to fake pi");
        stdin.write_all(b"\n").expect("write newline");
        stdin.flush().expect("flush");
    }

    /// 关掉 stdin（请求有序停机）。
    fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// 读下一帧（JSON 行）。超时即失败——替身卡住本身就是缺陷。
    fn next_event(&mut self) -> Value {
        let line = self.lines.next().expect("a line").expect("utf8 line");
        serde_json::from_str(&line).expect("json line")
    }

    /// 读到某个事件类型为止（跳过中间的帧）。
    fn next_of_type(&mut self, event_type: &str) -> Value {
        for _ in 0..64 {
            let event = self.next_event();
            if event.get("type").and_then(|t| t.as_str()) == Some(event_type) {
                return event;
            }
        }
        panic!("fake pi never emitted {event_type}");
    }

    fn finish(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for FakePiClient {
    fn drop(&mut self) {
        self.finish();
    }
}

// ---------------------------------------------------------------------------
// 帧定界：只按 \n（U+2028 / U+2029 不得被切开）
// ---------------------------------------------------------------------------

#[test]
fn frames_are_split_only_on_lf_so_unicode_separators_survive_inside_a_string() {
    let mut client = FakePiClient::spawn("separators");

    client.send(json!({"id": "req-1", "type": "prompt", "message": "hello"}));
    let update = client.next_of_type("message_update");
    let message_end = client.next_of_type("message_end");

    // U+2028 / U+2029 完整落在一帧里：智能 line reader 会在这里错切。
    let text = message_end["message"]["content"][0]["text"]
        .as_str()
        .expect("text");
    assert!(
        text.contains('\u{2028}') && text.contains('\u{2029}'),
        "authoritative message text must keep the unicode separators: {text:?}"
    );
    assert_eq!(
        update["assistantMessageEvent"]["delta"].as_str(),
        Some("fake pi says hi")
    );
}

// ---------------------------------------------------------------------------
// 版本探测：替身按 1.0.2 自描述；缺版本字段时记录偏差而不是失败
// ---------------------------------------------------------------------------

#[test]
fn get_state_reports_the_pinned_protocol_version() {
    let mut client = FakePiClient::spawn("normal");

    client.send(json!({"id": "req-1", "type": "get_state"}));
    let response = client.next_of_type("response");

    assert_eq!(response["command"], "get_state");
    assert_eq!(response["success"], true);
    assert_eq!(response["data"]["version"], "1.0.2");
    assert!(response["data"]["sessionId"].is_string());
}

#[test]
fn a_child_without_a_version_field_still_answers_get_state() {
    // 版本偏斜：探测不到版本只记录偏差，不得让协议面失败。
    let mut client = FakePiClient::spawn_with_env("normal", &[("FAKE_PI_MISSING_VERSION", "1")]);

    client.send(json!({"id": "req-1", "type": "get_state"}));
    let response = client.next_of_type("response");

    assert_eq!(response["success"], true);
    assert!(
        response["data"].get("version").is_none(),
        "替身必须真的不带版本字段，否则这条测试什么都没验"
    );
}

// ---------------------------------------------------------------------------
// clear_queue 往返
// ---------------------------------------------------------------------------

#[test]
fn clear_queue_round_trips_and_empties_the_pending_queues() {
    let mut client = FakePiClient::spawn("normal");

    client.send(json!({"id": "s-1", "type": "steer", "message": "focus"}));
    let steer = client.next_of_type("response");
    assert_eq!(steer["command"], "steer");
    assert_eq!(steer["disposition"], "queued");

    let queue = client.next_of_type("queue_update");
    assert_eq!(queue["steering"], json!(["focus"]));

    client.send(json!({"id": "c-1", "type": "clear_queue"}));
    let cleared = client.next_of_type("response");
    assert_eq!(cleared["command"], "clear_queue");
    assert_eq!(cleared["success"], true);
    assert_eq!(cleared["data"]["cleared"], 1);

    let queue = client.next_of_type("queue_update");
    assert_eq!(queue["steering"], json!([]));
    assert_eq!(queue["followUp"], json!([]));
}

// ---------------------------------------------------------------------------
// env 只进 pi 子进程（#17 凭据注入接缝的验证面）
// ---------------------------------------------------------------------------

#[test]
fn the_child_process_sees_only_the_environment_the_runner_injected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log_path = dir.path().join("fake-pi.log");
    let mut client = FakePiClient::spawn_with_env(
        "normal",
        &[
            ("FAKE_PI_LOG", log_path.to_str().expect("utf8 path")),
            ("FARM_TEST_SECRET", "only-the-pi-child-sees-this"),
        ],
    );

    client.send(json!({"id": "req-1", "type": "get_state"}));
    let _ = client.next_of_type("response");
    client.finish();

    let log = std::fs::read_to_string(&log_path).expect("fake pi log");
    let spawn: Value = serde_json::from_str(log.lines().next().expect("spawn line"))
        .expect("spawn line is json");
    assert_eq!(
        spawn["env"]["FARM_TEST_SECRET"], "only-the-pi-child-sees-this",
        "注入的 env 必须到达 pi 子进程"
    );
    assert_eq!(spawn["env"]["FAKE_PI_SCENARIO"], "normal");
    // 宿主进程自己不得持有这份凭据（env 注入是单向的：只进子进程）。
    assert!(std::env::var("FARM_TEST_SECRET").is_err());
}

// ---------------------------------------------------------------------------
// 替身健壮性：stdin 关闭即有序停机
// ---------------------------------------------------------------------------

#[test]
fn the_stand_in_shuts_down_when_stdin_closes() {
    let mut client = FakePiClient::spawn("normal");
    client.send(json!({"id": "req-1", "type": "get_state"}));
    let _ = client.next_of_type("response");

    // 关掉 stdin：替身应自行有序退出（不靠 kill）。
    client.close_stdin();
    for _ in 0..100 {
        if let Some(status) = client.child.try_wait().expect("try_wait") {
            assert!(status.success(), "stdin 关闭应有序退出: {status}");
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("fake pi did not exit after stdin closed");
}
