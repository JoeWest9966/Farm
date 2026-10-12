//! fake `nw` —— Warehouse CLI 的**测试替身**（ticket #17 凭据过路财神）。
//!
//! 为什么需要它：真机没有 `nw`（Warehouse）二进制（见 PLAN「环境」），而 #17 的凭据通路是
//! 「spawn 前子进程调 `nw key get`」。本替身按 `docs/research/report-nous-family.md` §2.4
//! 记下的 Warehouse 契约说话：stdout **只有一份 JSON envelope**，`ok == (exitCode == 0)`，
//! `errors[]` 恒为数组。命令面只实现本票用到的 `key get`。
//!
//! 场景与数据全部经**子进程环境变量**注入（宿主经 `WarehouseCourier` 的 per-subprocess env
//! 注入它们，绝不污染宿主自己的 env）：
//!
//! | env | 说明 |
//! | --- | --- |
//! | `FAKE_NW_KEYS` | JSON 对象 `{ "<key>": {"env":"<ENV>","value":"<secret>"}, ... }`：命中即成功返回 |
//! | `FAKE_NW_FAIL_KEYS` | JSON 数组 `["<key>", ...]`：命中的 key 一律失败（ok=false, exit 3） |
//! | `FAKE_NW_LOG` | 追加写调用日志的路径（测试断言「被以预期参数调用」） |
//!
//! 用法：`nw key get <key> [--repo <path>]`。

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;

use serde_json::{json, Value};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut log = std::env::var("FAKE_NW_LOG").ok().and_then(|path| {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
    });

    // 只认 `key get <key> [--repo <path>]`；别的用法 = 输入无效（exit 3，fail-closed）。
    let parsed = parse_key_get(&args);
    let Some((key, repo)) = parsed else {
        log_line(&mut log, json!({"t": "usage_error", "args": args}));
        emit(json!({
            "schema": "nw/key.v1", "ok": false,
            "errors": [{"code": "INPUT_INVALID", "message": "usage: nw key get <key> [--repo <path>]"}],
        }));
        std::process::exit(3);
    };

    log_line(
        &mut log,
        json!({"t": "key_get", "args": args, "key": key, "repo": repo}),
    );

    let fail_keys = load_string_set("FAKE_NW_FAIL_KEYS");
    if fail_keys.contains(&key) {
        emit(json!({
            "schema": "nw/key.v1", "ok": false,
            "errors": [{"code": "FORBIDDEN", "message": format!("fake nw denied key {key}")}],
        }));
        std::process::exit(3);
    }

    let keys = load_keys();
    match keys.get(&key) {
        Some(entry) => {
            emit(json!({
                "schema": "nw/key.v1", "nw": "0.1.0", "ok": true,
                "errors": [], "warnings": [],
                "data": {"env": entry.env, "value": entry.value},
            }));
            std::process::exit(0);
        }
        None => {
            emit(json!({
                "schema": "nw/key.v1", "ok": false,
                "errors": [{"code": "NOT_FOUND", "message": format!("no such key: {key}")}],
            }));
            std::process::exit(3);
        }
    }
}

struct KeyEntry {
    env: String,
    value: String,
}

/// 从 `FAKE_NW_KEYS` 读 `{ "<key>": {"env","value"} }`。
fn load_keys() -> BTreeMap<String, KeyEntry> {
    let raw = match std::env::var("FAKE_NW_KEYS") {
        Ok(raw) => raw,
        Err(_) => return BTreeMap::new(),
    };
    let value: Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(_) => return BTreeMap::new(),
    };
    let mut map = BTreeMap::new();
    if let Some(object) = value.as_object() {
        for (key, entry) in object {
            let env = entry.get("env").and_then(Value::as_str).unwrap_or_default();
            let value = entry.get("value").and_then(Value::as_str).unwrap_or_default();
            map.insert(
                key.clone(),
                KeyEntry {
                    env: env.to_string(),
                    value: value.to_string(),
                },
            );
        }
    }
    map
}

/// 从一个装着 JSON 字符串数组的 env 读集合。
fn load_string_set(name: &str) -> Vec<String> {
    std::env::var(name)
        .ok()
        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
        .unwrap_or_default()
}

/// 解析 `key get <key> [--repo <path>]`。
fn parse_key_get(args: &[String]) -> Option<(String, Option<String>)> {
    if args.first().map(String::as_str) != Some("key") || args.get(1).map(String::as_str) != Some("get")
    {
        return None;
    }
    let key = args.get(2)?.clone();
    let mut repo = None;
    if let Some(index) = args.iter().position(|arg| arg == "--repo") {
        repo = args.get(index + 1).cloned();
    }
    Some((key, repo))
}

fn emit(value: Value) {
    let line = serde_json::to_string(&value).expect("fake nw envelope must serialize");
    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn log_line(log: &mut Option<std::fs::File>, entry: Value) {
    if let Some(file) = log.as_mut() {
        let line = serde_json::to_string(&entry).expect("log entry must serialize");
        let _ = writeln!(file, "{line}");
        let _ = file.flush();
    }
}
