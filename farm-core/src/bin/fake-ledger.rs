//! fake `ledger` 替身（ticket #20 测试专用，非交付物）。
//!
//! 替身在测试里顶替 Kron 的 `ledger record` 子进程：把**收到的 NDJSON 原文**与
//! **调用 argv** 记进日志文件，测试按已知字面量断言 wire 形状。
//!
//! - 日志路径经 `--log <path>` argv 传入（避免进程级 env 在并行测试间竞争）；
//! - 退出码可经 `FAKE_LEDGER_EXIT` 覆盖（默认 0 = 账本认下；`interpret` 见 exit==0 → ok）。

use std::io::{BufRead, Write};

fn main() {
    let argv: Vec<String> = std::env::args().collect();

    // 侧车读（`read --agg`）：无需 --log，直接回显 argv + canned 聚合体。
    if argv.iter().any(|a| a == "read") {
        println!(
            "{}",
            serde_json::json!({ "argv": argv, "agg": { "actor": "farm", "events": 0 } })
        );
        std::process::exit(exit_code());
    }

    // 解析 `--log <path>`（record 路径需要）。
    let log_path = argv
        .iter()
        .position(|a| a == "--log")
        .and_then(|i| argv.get(i + 1))
        .cloned();
    let Some(log_path) = log_path else {
        std::process::exit(exit_code());
    };

    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .expect("open fake ledger log");

    // spawn 记录：argv 是「调用形状」的可断言面。
    let _ = writeln!(log, "{}", spawn_record(&argv));

    // 读 stdin 的 NDJSON，逐行原文记进日志（宿主怎么写我们就怎么记）。
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let record = serde_json::json!({ "t": "record", "line": line });
        let _ = writeln!(log, "{record}");
    }

    std::process::exit(exit_code());
}

fn spawn_record(argv: &[String]) -> serde_json::Value {
    serde_json::json!({ "t": "spawn", "argv": argv })
}

fn exit_code() -> i32 {
    std::env::var("FAKE_LEDGER_EXIT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}
