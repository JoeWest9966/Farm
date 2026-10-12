//! fake plugin —— ticket #19 的插件**测试替身**（进程级 IPC 的插件侧）。
//!
//! 为什么需要它：插件监督的接缝是「宿主拉起子进程 → 握手 → 监督 → 有序停机」，测试要能
//! 注入「崩溃 / 起不来 / 握手后装死 / 乖乖退出」这些**进程行为**，所以替身与 fake-pi /
//! fake-ledger 同宗：全部行为经**子进程环境变量**注入，测试经 `CARGO_BIN_EXE_fake-plugin`
//! 拿到它的绝对路径。
//!
//! 契约（与 `examples/plugin-skeleton` 同一条 wire，只是不说 HTTP）：
//! - stdout 第一行是 `hello` 握手自描述（JSONL）；
//! - stdin 读到 EOF（或收到 `{"t":"shutdown"}`）= 宿主要我有序停机 → 打 `bye` 后退出 0；
//! - 父进程死了也要退（`FARM_PLUGIN_HOST_PID` 轮询，与示例插件的第二道保险同机制）。
//!
//! | env | 说明 |
//! | --- | --- |
//! | `FAKE_PLUGIN_SCENARIO` | `normal`(默认)/`crash-loop`/`crash-once`/`silent`/`hang` |
//! | `FAKE_PLUGIN_LOG` | 追加写生命周期日志的路径（测试断言用） |
//! | `FAKE_PLUGIN_EXIT_CODE` | 崩溃场景的退出码（默认 1） |
//! | `FAKE_PLUGIN_HELLO_DELAY_MS` | 握手行延迟（默认 0） |
//! | `FAKE_PLUGIN_COUNTER` | `crash-once` 的运行计数文件路径 |
//! | `FAKE_PLUGIN_HANG_MS` | `hang` 场景装死多久（默认 60000） |

use std::fs::OpenOptions;
use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

fn main() {
    let plugin_id = env_or("FAKE_PLUGIN_ID", "fake-plugin");
    let scenario = env_or("FAKE_PLUGIN_SCENARIO", "normal");
    let exit_code = env_or("FAKE_PLUGIN_EXIT_CODE", "1")
        .parse::<i32>()
        .unwrap_or(1);

    log_event(&json!({
        "t": "start",
        "plugin": plugin_id,
        "scenario": scenario,
        "pid": std::process::id(),
        "hostPid": std::env::var("FARM_PLUGIN_HOST_PID").ok(),
        "atMs": 0,
    }));

    if scenario == "crash-loop" {
        // 每次都崩：验证分级退避 + 重试耗尽 → quarantined。
        log_event(&json!({ "t": "exit", "plugin": plugin_id, "code": exit_code, "why": "crash-loop" }));
        std::process::exit(exit_code);
    }
    if scenario == "crash-once" {
        // 第一次崩、之后正常：验证「崩溃后重启成功 + 自愈（重试预算归零）」。
        let runs = bump_counter() + 1;
        log_event(&json!({ "t": "run", "plugin": plugin_id, "run": runs }));
        if runs == 1 {
            log_event(&json!({ "t": "exit", "plugin": plugin_id, "code": exit_code, "why": "crash-once" }));
            std::process::exit(exit_code);
        }
    }

    let delay = env_or("FAKE_PLUGIN_HELLO_DELAY_MS", "0")
        .parse::<u64>()
        .unwrap_or(0);
    if delay > 0 {
        std::thread::sleep(Duration::from_millis(delay));
    }

    if scenario != "silent" {
        // 握手自描述（stdout 第一行）。
        println!(
            "{}",
            json!({
                "t": "hello",
                "plugin": plugin_id,
                "name": env_or("FAKE_PLUGIN_NAME", "fake plugin"),
                "contract_version": "1.0.0",
                "kind": "plugin",
            })
        );
        let _ = std::io::stdout().flush();
        log_event(&json!({ "t": "hello", "plugin": plugin_id }));
    }

    if scenario == "hang" {
        // 握手后装死：既不说话也不退——验证停机宽限 + 强杀。
        let hang = env_or("FAKE_PLUGIN_HANG_MS", "60000")
            .parse::<u64>()
            .unwrap_or(60_000);
        let deadline = Instant::now() + Duration::from_millis(hang);
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        log_event(&json!({ "t": "exit", "plugin": plugin_id, "code": 0, "why": "hang-expired" }));
        std::process::exit(0);
    }

    // 正常：等宿主关 stdin（有序停机）或收到 shutdown 行。
    let stdin = std::io::stdin();
    let mut reason = "stdin_eof";
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(&line) {
            if value.get("t").and_then(|t| t.as_str()) == Some("shutdown") {
                reason = "shutdown";
                break;
            }
        }
    }
    log_event(&json!({ "t": "bye", "plugin": plugin_id, "reason": reason }));
    println!(
        "{}",
        json!({ "t": "bye", "plugin": plugin_id, "reason": reason })
    );
    let _ = std::io::stdout().flush();
    log_event(&json!({ "t": "exit", "plugin": plugin_id, "code": 0, "why": reason }));
    std::process::exit(0);
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// `crash-once` 的运行计数（跨进程：计数文件）。
fn bump_counter() -> u64 {
    let path = env_or(
        "FAKE_PLUGIN_COUNTER",
        &format!(
            "{}/fake-plugin-counter-{}.txt",
            std::env::temp_dir().display(),
            std::process::id()
        ),
    );
    let current = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let next = current + 1;
    let _ = std::fs::write(&path, next.to_string());
    next
}

/// 生命周期日志：一行一个 JSON（测试按已知字段断言重启次数 / 停机原因）。
fn log_event(event: &Value) {
    let Some(path) = std::env::var_os("FAKE_PLUGIN_LOG") else {
        return;
    };
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{event}");
    }
}
