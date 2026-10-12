//! agent-runner 凭据过路财神端到端接缝测试（ticket #17）。
//!
//! 全部经**真实子进程**打接缝：`AppState` 的 agent-runner 句柄指向 fake pi 替身，凭据源是
//! 指向 fake nw 替身的 [`WarehouseCourier`]。spawn 窗口内，宿主子进程调 `nw key get`，
//! 把 Warehouse 说的 env 对注入 pi 子进程——断言的是**行为**（fake nw 被以预期参数调用、
//! fake pi 子进程收到凭据、Farm 不缓存不落盘、失败走 needs_attention、凭据不外溢到插件 env），
//! 不是内部结构。GLOSSARY「过路财神」/ ADR-0004 决策 3。
//!
//! fake pi 与 fake nw 的日志落在同一个 `logs` tempdir；no-disk 测试另用一块 `db` tempdir
//! 只放 Farm 的 SQLite 文件，从而把「Farm 写的盘」与「子进程写的盘」分开扫描。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use farm_core::agent_runner::credentials::{
    credentials_leaked_into, CredentialCourier, CredentialRequest, WarehouseCourier,
};
use farm_core::agent_runner::service::{AgentRunner, RunnerConfig};
use farm_core::{AppState, EventLogFilter, HostConfig};
use farm_sdk::events::{EventEnvelope, EventKind};
use farm_sdk::services::agent_runner::{AttemptState, DispatchRequest};
use serde_json::{json, Value};
use tempfile::TempDir;

const FAKE_PI: &str = env!("CARGO_BIN_EXE_fake-pi");
const FAKE_NW: &str = env!("CARGO_BIN_EXE_fake-nw");

/// fake nw 认的 key、它会说的 env 变量名、以及它返回的机密值。
const NW_KEY: &str = "github-token";
const CRED_ENV: &str = "FARM_GH_TOKEN";
const CRED_VALUE: &str = "ghp_passthrough_17_only_pi_child_9f3a";
/// 数据仓位置（Warehouse 两仓取货；`--repo` 由 courier 传给 `nw`）。
const NW_REPO: &str = "/opt/Data-001";

struct Harness {
    state: AppState,
    _logs: TempDir,
    _db: Option<TempDir>,
    pi_log: PathBuf,
    nw_log: PathBuf,
}

impl Harness {
    /// 装配一个宿主：fake pi 替身 + 指向 fake nw 的 [`WarehouseCourier`]。
    ///
    /// - `on_disk`：Farm 用文件型 SQLite（no-disk 测试要扫真盘）；否则内存库。
    /// - `fail_key`：让 fake nw 对这个 key 失败（ok=false, exit 3）。
    fn build(scenario: &str, on_disk: bool, fail_key: bool) -> Self {
        let logs = tempfile::tempdir().expect("logs tempdir");
        let pi_log = logs.path().join("fake-pi.log");
        let nw_log = logs.path().join("fake-nw.log");

        let keys_json = json!({ NW_KEY: {"env": CRED_ENV, "value": CRED_VALUE} }).to_string();
        let mut courier = WarehouseCourier::new(FAKE_NW, vec![NW_KEY.to_string()])
            .with_repo(NW_REPO)
            .with_env("FAKE_NW_KEYS", keys_json)
            .with_env("FAKE_NW_LOG", nw_log.to_string_lossy().to_string());
        if fail_key {
            courier = courier.with_env("FAKE_NW_FAIL_KEYS", json!([NW_KEY]).to_string());
        }

        let mut config = RunnerConfig::for_fake_pi(FAKE_PI);
        config.scenario_env = vec![
            ("FAKE_PI_SCENARIO".to_string(), scenario.to_string()),
            ("FAKE_PI_LOG".to_string(), pi_log.to_string_lossy().to_string()),
        ];

        let host_config = || HostConfig {
            bind: "127.0.0.1:3721".parse().expect("bind"),
            op_token: "test-op-token".into(),
            admin_token: None,
            db_path: std::env::temp_dir().join("farm-credential-test-ignored.db"),
        };

        let db_dir_opt;
        let mut state = if on_disk {
            let db_dir = tempfile::tempdir().expect("db tempdir");
            let mut cfg = host_config();
            cfg.db_path = db_dir.path().join("farm.db");
            let state = AppState::new(cfg).expect("on-disk state");
            db_dir_opt = Some(db_dir);
            state
        } else {
            db_dir_opt = None;
            AppState::new_in_memory(host_config()).expect("in-memory state")
        };

        let db = state.db.clone();
        state = state.with_agent_runner(
            AgentRunner::new(db)
                .with_config(config)
                .with_credentials(Arc::new(courier)),
        );

        Self {
            state,
            _logs: logs,
            _db: db_dir_opt,
            pi_log,
            nw_log,
        }
    }

    fn normal() -> Self {
        Self::build("normal", false, false)
    }

    fn on_disk() -> Self {
        Self::build("normal", true, false)
    }

    fn failing_key() -> Self {
        Self::build("normal", false, true)
    }

    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<EventEnvelope> {
        self.state.events.subscribe()
    }

    async fn dispatch(&self, issue_id: &str) -> String {
        self.state
            .agent_runner
            .dispatch(
                &self.state,
                &DispatchRequest {
                    issue_id: issue_id.into(),
                    profile: Some("implement-spec".into()),
                    prompt: Some("work the ticket".into()),
                },
            )
            .await
            .expect("dispatch must be accepted")
            .attempt_id
    }

    async fn wait_terminal(&self, attempt_id: &str) -> farm_core::agent_runner::store::AttemptRecord {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(record) = self.state.agent_runner.attempt(attempt_id).expect("read") {
                if record.state != AttemptState::Running {
                    return record;
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "attempt {attempt_id} never left running"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn log_lines(path: &Path) -> Vec<Value> {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        text.lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    fn pi_log(&self) -> Vec<Value> {
        Self::log_lines(&self.pi_log)
    }

    fn nw_log(&self) -> Vec<Value> {
        Self::log_lines(&self.nw_log)
    }

    /// pi 子进程 spawn 时的完整 env（fake pi 首行日志里的 `env` 映射）。
    fn pi_child_env(&self) -> Vec<(String, String)> {
        let spawn = self
            .pi_log()
            .into_iter()
            .find(|entry| entry["t"] == "spawn")
            .expect("fake pi must have logged a spawn line");
        spawn["env"]
            .as_object()
            .expect("spawn env must be an object")
            .into_iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
            .collect()
    }
}

/// 在一块目录里做字节子串扫描，返回命中的文件名（二进制 SQLite 文件也扫）。
fn files_containing(dir: &Path, needle: &[u8]) -> Vec<String> {
    let mut hits = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Ok(bytes) = std::fs::read(&path) {
                    if needle.len() <= bytes.len()
                        && bytes.windows(needle.len()).any(|window| window == needle)
                    {
                        hits.push(path.display().to_string());
                    }
                }
            }
        }
    }
    hits
}

async fn next_kind(
    rx: &mut tokio::sync::broadcast::Receiver<EventEnvelope>,
    kind: EventKind,
) -> EventEnvelope {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let envelope = tokio::time::timeout(remaining, rx.recv())
            .await
            .expect("event must arrive before the deadline")
            .expect("bus must be alive");
        if envelope.kind == kind {
            return envelope;
        }
    }
}

// ---------------------------------------------------------------------------
// 凭据通路：fake nw 被以预期参数调用 → 凭据以 env 出现在 pi 子进程
// ---------------------------------------------------------------------------

#[tokio::test]
async fn warehouse_courier_calls_nw_key_get_and_injects_the_credentials_into_the_pi_child() {
    let harness = Harness::normal();
    let attempt_id = harness.dispatch("17").await;
    let record = harness.wait_terminal(&attempt_id).await;
    assert_eq!(record.state, AttemptState::Settled);

    // fake nw 被以预期参数调用（`nw key get <key> --repo <repo>`）。
    let key_get = harness
        .nw_log()
        .into_iter()
        .find(|entry| entry["t"] == "key_get")
        .expect("the spawn window must run `nw key get` before spawning pi");
    assert_eq!(key_get["key"], NW_KEY);
    assert_eq!(
        key_get["args"],
        json!(["key", "get", NW_KEY, "--repo", NW_REPO]),
        "`nw` 二进制必须以预期参数被调用"
    );

    // 凭据以 env 出现在 pi 子进程（fake pi 的 spawn 日志断言它收到了）。
    let child_env = harness.pi_child_env();
    assert!(
        child_env
            .iter()
            .any(|(k, v)| k == CRED_ENV && v == CRED_VALUE),
        "凭据必须以 env 形式到达 pi 子进程: {child_env:?}"
    );
}

// ---------------------------------------------------------------------------
// 零缓存 + 零落盘
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_courier_caches_nothing_each_collect_refetches_from_warehouse() {
    // 直接调 courier.collect 两次：零缓存 = 每次都真的跑一次 nw，绝不从内存回放。
    let dir = tempfile::tempdir().expect("tempdir");
    let nw_log = dir.path().join("fake-nw.log");
    let keys_json = json!({ NW_KEY: {"env": CRED_ENV, "value": CRED_VALUE} }).to_string();
    let courier = WarehouseCourier::new(FAKE_NW, vec![NW_KEY.to_string()])
        .with_env("FAKE_NW_KEYS", keys_json)
        .with_env("FAKE_NW_LOG", nw_log.to_string_lossy().to_string());
    let request = CredentialRequest {
        ticket: "17".into(),
        attempt_id: "att-cache".into(),
        profile: None,
    };

    let first = courier.collect(&request).expect("first collect");
    let second = courier.collect(&request).expect("second collect");
    assert_eq!(first, second, "两次取到的凭据一致");
    assert_eq!(first, vec![(CRED_ENV.to_string(), CRED_VALUE.to_string())]);

    let invocations = Harness::log_lines(&nw_log)
        .into_iter()
        .filter(|entry| entry["t"] == "key_get")
        .count();
    assert_eq!(
        invocations, 2,
        "零缓存：每次 collect 都重新调 nw，不从 Farm 内存里回放"
    );
}

#[tokio::test]
async fn farm_persists_no_credential_to_disk_and_keeps_none_in_its_store() {
    let harness = Harness::on_disk();
    let attempt_id = harness.dispatch("17").await;
    let record = harness.wait_terminal(&attempt_id).await;
    assert_eq!(record.state, AttemptState::Settled);

    // 1) 落盘扫描：Farm 的 SQLite 文件里绝不出现凭据值。
    //    （fake pi / fake nw 的日志在另一块 tempdir，不在此扫——那是子进程自己写的盘。）
    let db_dir = harness._db.as_ref().expect("on-disk harness").path();
    let on_disk_hits = files_containing(db_dir, CRED_VALUE.as_bytes());
    assert!(
        on_disk_hits.is_empty(),
        "凭据值绝不落进 Farm 的存储文件: {on_disk_hits:?}"
    );

    // 2) 宿主私有存储 / 事件里没有凭据值（attempt 记录的可读字段 + 全部事件载荷）。
    for text in [&record.note, &record.disposition, &record.session_id]
        .into_iter()
        .flatten()
    {
        assert!(
            !text.contains(CRED_VALUE),
            "attempt 记录绝不夹带凭据值: {text}"
        );
    }
    let history = harness
        .state
        .event_history(&EventLogFilter::default())
        .expect("history");
    assert!(!history.events.is_empty(), "这一趟必须有事件");
    for event in &history.events {
        assert!(
            !event.payload.to_string().contains(CRED_VALUE),
            "事件载荷绝不夹带凭据值: {}",
            event.payload
        );
    }
}

// ---------------------------------------------------------------------------
// 取凭据失败 → spawn 失败 + needs_attention
// ---------------------------------------------------------------------------

#[tokio::test]
async fn nw_key_get_failure_fails_the_spawn_and_raises_needs_attention() {
    let harness = Harness::failing_key();
    let mut rx = harness.subscribe();
    let attempt_id = harness.dispatch("17").await;

    let record = harness.wait_terminal(&attempt_id).await;
    assert_eq!(record.state, AttemptState::Failed);
    assert!(
        record
            .note
            .as_deref()
            .unwrap_or("")
            .contains("credential courier failed"),
        "取凭据失败即 spawn 失败: {:?}",
        record.note
    );
    // 凭据在 spawn 之前取：失败时 pi 子进程根本不该被 spawn。
    assert!(
        harness
            .pi_log()
            .iter()
            .all(|entry| entry["t"] != "spawn"),
        "取凭据失败时不得 spawn pi 子进程"
    );

    let event = next_kind(&mut rx, EventKind::NeedsAttention).await;
    assert_eq!(
        event.payload["reason"], "credential courier failed inside the spawn window",
        "取凭据失败走 needs_attention，不是插件重试语义"
    );
    assert_eq!(event.attempt_id.as_deref(), Some(attempt_id.as_str()));
}

// ---------------------------------------------------------------------------
// #19 联测接缝：凭据 env 绝不出现在插件进程环境
// ---------------------------------------------------------------------------

#[tokio::test]
async fn credentials_reach_the_pi_child_but_never_a_plugin_process_environment() {
    let harness = Harness::normal();
    let attempt_id = harness.dispatch("17").await;
    let record = harness.wait_terminal(&attempt_id).await;
    assert_eq!(record.state, AttemptState::Settled);

    let credential_pairs = vec![(CRED_ENV.to_string(), CRED_VALUE.to_string())];

    // pi 子进程确实拿到了凭据（凭据在它 env 里）。
    let child_env = harness.pi_child_env();
    assert_eq!(
        credentials_leaked_into(&child_env, &credential_pairs),
        vec![CRED_ENV.to_string()],
        "凭据必须在 pi 子进程 env 里"
    );

    // 宿主进程 env（插件进程会继承的那份）没有凭据 → 插件进程环境绝无凭据。
    // #19 联测时对**真插件进程**跑同一条 credentials_leaked_into 断言。
    let host_env: Vec<(String, String)> = std::env::vars().collect();
    assert!(
        credentials_leaked_into(&host_env, &credential_pairs).is_empty(),
        "宿主 env 绝不持有凭据（凭据只单向注入 pi 子进程）→ 插件进程环境无凭据"
    );
}
