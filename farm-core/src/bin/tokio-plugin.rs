//! tokio plugin —— ticket #19 的「带真 runtime 的插件」测试替身。
//!
//! 为什么单独立一个：`fake-plugin` 是**纯 std** 的（无 async runtime），而真实插件
//! （`examples/plugin-skeleton` / `examples/http-notifier`）都跑在自己的 tokio
//! runtime 上。宿主用 `tokio::process::Command` 拉起插件，子进程自带 runtime 时
//! 在本沙箱里会**冻住**（见 `tests/plugin_host.rs` 的
//! `a_plugin_with_its_own_tokio_runtime_handshakes_and_shuts_down_in_order`）：
//! 那正是 #19「宿主收尾后无孤儿插件进程」覆盖不到的一角——现有 #19 测试全跑在无
//! runtime 的替身之上，所以从未暴露过它。
//!
//! 本替身把那一角做成可测的接缝：它说同一条 wire（stdout 第一行 `hello`，stdin
//! EOF → `bye` → 退 0），只是**带一个真 tokio runtime**。测试因此能在沙箱内记录
//! 「这里冻了」这一事实（eprintln + 早退 = 有记录的跳过），而不是挂死；换到非沙箱
//! 宿主上同一条测试会真正验证握手 + 有序停机。
//!
//! | env | 说明 |
//! | --- | --- |
//! | `FAKE_PLUGIN_ID` | 插件身份（默认 `tokio-plugin`） |
//! | `FAKE_PLUGIN_LOG` | 追加写生命周期日志的路径（测试断言用） |
//! | `FAKE_PLUGIN_HELLO_DELAY_MS` | 握手行延迟（默认 0） |
//! | `FAKE_PLUGIN_HANG_MS` | `hang` 场景装死多久（默认 60000） |

use std::fs::OpenOptions;
use std::io::Write;
use std::time::Duration;

use serde_json::json;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let plugin_id = env_or("FAKE_PLUGIN_ID", "tokio-plugin");

    log_event(&json!({
        "t": "start",
        "plugin": plugin_id,
        "pid": std::process::id(),
        "hostPid": std::env::var("FARM_PLUGIN_HOST_PID").ok(),
        "runtime": "tokio",
    }));

    let delay = env_or("FAKE_PLUGIN_HELLO_DELAY_MS", "0")
        .parse::<u64>()
        .unwrap_or(0);
    if delay > 0 {
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }

    // 握手自描述（stdout 第一行；管道下也要立刻送达，故显式 flush）。
    {
        let mut out = std::io::stdout();
        let _ = writeln!(
            out,
            "{}",
            json!({
                "t": "hello",
                "plugin": plugin_id,
                "name": env_or("FAKE_PLUGIN_NAME", "tokio plugin"),
                "contract_version": "1.0.0",
                "kind": "plugin",
            })
        );
        let _ = out.flush();
    }
    log_event(&json!({ "t": "hello", "plugin": plugin_id }));

    let scenario = env_or("FAKE_PLUGIN_SCENARIO", "normal");
    if scenario == "hang" {
        let hang = env_or("FAKE_PLUGIN_HANG_MS", "60000")
            .parse::<u64>()
            .unwrap_or(60_000);
        tokio::time::sleep(Duration::from_millis(hang)).await;
        log_event(&json!({ "t": "exit", "plugin": plugin_id, "code": 0, "why": "hang-expired" }));
        std::process::exit(0);
    }

    // 有序停机：宿主关 stdin = EOF 送到本进程（ADR-0004 决策 4）。stdin 的行读取丢到
    // blocking 池（插件侧不想为 `tokio` 开 `io-std` 特性），主任务在自己的 runtime 上
    // await 它——整条停机路径因此是真 async 的。
    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    tokio::task::spawn_blocking(move || {
        let mut reason = "stdin_eof".to_string();
        for line in std::io::stdin().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                if value.get("t").and_then(|t| t.as_str()) == Some("shutdown") {
                    reason = "shutdown".to_string();
                    break;
                }
            }
        }
        let _ = tx.send(reason);
    });
    let reason = rx.await.unwrap_or_else(|_| "stdin_eof".to_string());
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

/// 生命周期日志：一行一个 JSON（与 `fake-plugin` 同格式，测试可复用同一套读取器）。
fn log_event(event: &serde_json::Value) {
    let Some(path) = std::env::var_os("FAKE_PLUGIN_LOG") else {
        return;
    };
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{event}");
    }
}
