//! agent-runner 本地后端端到端接缝测试（ticket #16）。
//!
//! 全部经**真实子进程**打接缝：`AppState` 的 agent-runner 句柄指向 fake pi 替身，
//! 场景由子进程 env 注入。断言的是**行为**（终态、token 字面量、事件回流、日志里的
//! fork 记录），不是内部结构。
//!
//! 覆盖验收清单：settle 双条件（含反例）、静默钟、总钟、abort 探针死线、fork 找回、
//! token 流式记账、中断快照、disposition 回流、settle 期注入推迟、凭据只进子进程。

use std::path::PathBuf;
use std::time::Duration;

use farm_core::agent_runner::credentials::{
    CredentialCourier, FailingCredentialCourier, StaticCredentialCourier,
};
use farm_core::agent_runner::ladder::LadderConfig;
use farm_core::agent_runner::service::{AgentRunner, RunnerConfig};
use farm_core::agent_runner::store::AttemptStore;
use farm_core::{AppState, HostConfig};
use farm_sdk::events::{EventEnvelope, EventKind};
use farm_sdk::services::agent_runner::{
    AbortRequest, AttemptState, DispatchRequest, SteerRequest,
};
use serde_json::{json, Value};
use tempfile::TempDir;

/// fake pi 替身的绝对路径（cargo 为集成测试提供）。
const FAKE_PI: &str = env!("CARGO_BIN_EXE_fake-pi");

/// 一个带 fake pi 替身的宿主（attempts 落在同一块内存库里）。
struct Harness {
    state: AppState,
    _dir: TempDir,
    log_path: PathBuf,
}

impl Harness {
    fn new(scenario: &str) -> Self {
        Self::with_ladder(scenario, LadderConfig::fast(400, 10_000, 600), &[])
    }

    fn with_ladder(scenario: &str, ladder: LadderConfig, env: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let log_path = dir.path().join("fake-pi.log");
        let mut config = RunnerConfig::for_fake_pi(FAKE_PI);
        config.ladder = ladder;
        config.scenario_env = vec![
            ("FAKE_PI_SCENARIO".to_string(), scenario.to_string()),
            ("FAKE_PI_LOG".to_string(), log_path.to_string_lossy().to_string()),
        ];
        for (key, value) in env {
            config
                .scenario_env
                .push(((*key).to_string(), (*value).to_string()));
        }
        let state = AppState::new_in_memory(HostConfig {
            bind: "127.0.0.1:3721".parse().expect("bind"),
            op_token: "test-op-token".into(),
            admin_token: None,
            db_path: std::env::temp_dir().join("farm-agent-runner-test.db"),
        })
        .expect("in-memory state");
        let db = state.db.clone();
        let state = state.with_agent_runner(AgentRunner::new(db).with_config(config));
        Self {
            state,
            _dir: dir,
            log_path,
        }
    }

    fn with_credentials(
        scenario: &str,
        courier: std::sync::Arc<dyn CredentialCourier>,
    ) -> Self {
        let mut harness = Self::new(scenario);
        let db = harness.state.db.clone();
        let config = harness.state.agent_runner.config().clone();
        let runner = AgentRunner::new(db)
            .with_config(config)
            .with_credentials(courier);
        harness.state = harness.state.with_agent_runner(runner);
        harness
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
                    prompt: Some("hello from the host".into()),
                },
            )
            .await
            .expect("dispatch must be accepted")
            .attempt_id
    }

    /// 等到 attempt 离开 `running`（判据阶梯判终态 / 判杀 / abort）。
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

    fn log(&self) -> Vec<Value> {
        let text = std::fs::read_to_string(&self.log_path).unwrap_or_default();
        text.lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
}

/// 从总线里收事件直到出现某个 kind（跳过别的）。
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
// 完成双条件
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_normal_turn_settles_on_the_double_condition_and_accounts_the_known_literals() {
    let harness = Harness::new("normal");
    let mut rx = harness.subscribe();
    let attempt_id = harness.dispatch("16").await;

    let record = harness.wait_terminal(&attempt_id).await;
    assert_eq!(record.state, AttemptState::Settled, "双条件满足才判终态");
    assert_eq!(
        record.disposition.as_deref(),
        Some("accepted"),
        "prompt 的 disposition 必须被记下来"
    );
    assert!(record.session_id.is_some(), "探测要拿到 session id");

    // 绝对累计：assistant 100/10 + 嵌套 tool 40/5（宁少报不虚报，message_update 不入账）。
    assert_eq!(record.tokens.input, 140);
    assert_eq!(record.tokens.output, 15);
    assert_eq!(record.tokens.total, 155);
    assert_eq!(record.tokens.nested_usage_calls, 1, "parentToolCallId 嵌套用量");
    assert_eq!(record.tokens.counted_messages, 1, "同一条消息只记一次");
    assert!(!record.accounting_truncated, "settle 不是中断态");
    assert!(record.tokens.note.is_none(), "对账一致时不记记账偏差");

    // 回流：dispatch → attempt_started（带 disposition）→ agent_settled。
    let dispatch = next_kind(&mut rx, EventKind::Dispatch).await;
    assert_eq!(dispatch.attempt_id.as_deref(), Some(attempt_id.as_str()));
    assert_eq!(dispatch.ticket_id.as_deref(), Some("16"));
    let started = next_kind(&mut rx, EventKind::AttemptStarted).await;
    assert_eq!(started.payload["disposition"], "accepted");
    let settled = next_kind(&mut rx, EventKind::AgentSettled).await;
    assert_eq!(settled.payload["state"], "settled");
    assert_eq!(settled.payload["tokens"]["total"], 155);
    assert_eq!(
        settled.payload["versionProbe"]["reported"], "1.0.2",
        "探测结果要随终态一起回流"
    );
    assert_eq!(
        settled.payload["versionProbe"]["deviations"],
        json!([]),
        "版本一致时不得记偏差"
    );

    // 持久化路径：事件必须也在事件日志里（面板读的是同一份事实）。
    let history = harness
        .state
        .event_history(&farm_core::EventLogFilter::default())
        .expect("history");
    assert!(
        history
            .events
            .iter()
            .any(|event| event.kind == EventKind::AgentSettled
                && event.attempt_id.as_deref() == Some(attempt_id.as_str())),
        "settle 事件必须落事件日志，否则面板查询看不到"
    );
}

#[tokio::test]
async fn an_agent_that_settles_without_a_stop_reason_never_reports_success() {
    let harness = Harness::new("no-stop-reason");
    let attempt_id = harness.dispatch("16").await;

    let record = harness.wait_terminal(&attempt_id).await;
    assert_ne!(
        record.state,
        AttemptState::Settled,
        "只有 agent_settled 不算完：stopReason=length 是截断"
    );
    // 这一轮的用量仍然要入账（少报也不行，但要报）。
    assert_eq!(record.tokens.input, 100);
    assert_eq!(record.tokens.output, 10);
    assert_eq!(record.tokens.total, 110);
}

// ---------------------------------------------------------------------------
// 两口钟 + abort 探针
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_silent_clock_escalates_to_the_abort_probe_and_the_child_obeys() {
    let harness = Harness::new("silence");
    let mut rx = harness.subscribe();
    let attempt_id = harness.dispatch("16").await;

    let record = harness.wait_terminal(&attempt_id).await;
    // 静默钟到点 → 发 abort → 子进程有序退出。持久态闭集里没有 aborted（四态），
    // 故落 failed，但**事件**仍按 abort 回流、note 指名是 abort 收尾。
    assert_eq!(record.state, AttemptState::Failed);
    assert!(
        record.note.as_deref().unwrap_or("").contains("abort acknowledged"),
        "{}",
        record.note.as_deref().unwrap_or("")
    );
    let stall = next_kind(&mut rx, EventKind::StallKill).await;
    assert_eq!(stall.payload["rung"], "silent clock", "判杀要指名是哪一梯");
    assert!(stall.payload["silentClockMs"].as_u64().is_some());
}

#[tokio::test]
async fn the_abort_probe_deadline_kills_a_child_that_ignores_abort() {
    let harness = Harness::new("abort-probe");
    let attempt_id = harness.dispatch("16").await;

    let record = harness.wait_terminal(&attempt_id).await;
    assert_eq!(record.state, AttemptState::Failed);
    assert!(
        record.note.as_deref().unwrap_or("").contains("abort probe deadline expired"),
        "第三梯：pi 不理 abort，独立死线到点强杀: {}",
        record.note.as_deref().unwrap_or("")
    );
}

#[tokio::test]
async fn the_total_clock_expires_while_events_keep_streaming() {
    // 静默钟给 10s（永远不到点），总钟 300ms：流得再勤也照杀。
    let harness = Harness::with_ladder(
        "total-clock",
        LadderConfig::fast(10_000, 300, 600),
        &[("FAKE_PI_EVENT_INTERVAL_MS", "10")],
    );
    let attempt_id = harness.dispatch("16").await;

    let record = harness.wait_terminal(&attempt_id).await;
    assert_eq!(record.state, AttemptState::Failed);
    assert!(
        record.note.as_deref().unwrap_or("").contains("total clock expired"),
        "第二梯：总钟与有没有帧无关: {}",
        record.note.as_deref().unwrap_or("")
    );
}

// ---------------------------------------------------------------------------
// fork 找回（第四梯）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_crashed_child_is_recovered_by_forking_from_the_last_user_message() {
    let harness = Harness::new("fork-recovery");
    let mut rx = harness.subscribe();
    let attempt_id = harness.dispatch("16").await;

    let record = harness.wait_terminal(&attempt_id).await;
    assert_eq!(
        record.state,
        AttemptState::Settled,
        "读循环死了但会话可续：fork 找回之后要能跑完: {}",
        record.note.as_deref().unwrap_or("")
    );

    // 替身日志里必须真有「fork」这一下，且第二个子进程带上了 --session。
    let log = harness.log();
    let spawns = log
        .iter()
        .filter(|entry| entry["t"] == "spawn")
        .collect::<Vec<_>>();
    assert_eq!(spawns.len(), 2, "fork 找回要重 spawn 一个子进程: {spawns:?}");
    assert!(
        spawns[1]["args"]
            .as_array()
            .map(|args| args.iter().any(|arg| arg == "--session"))
            .unwrap_or(false),
        "重 spawn 必须续上崩溃前那个会话: {:?}",
        spawns[1]["args"]
    );
    assert!(
        log.iter().any(|entry| entry["t"] == "fork"),
        "必须真的发出 fork 命令: {log:?}"
    );

    let blocked = next_kind(&mut rx, EventKind::Blocked).await;
    assert_eq!(blocked.payload["reason"], "read loop died; recovering by fork");
}

#[tokio::test]
async fn a_child_that_dies_before_any_frame_is_not_fork_recovered() {
    // 一个立刻退出的「pi」：读循环健康度前置直接判失败，没有会话可续。
    let mut harness = Harness::new("normal");
    let dir = tempfile::tempdir().expect("tempdir");
    let dead = dir.path().join("dead-pi.sh");
    std::fs::write(&dead, "#!/bin/sh\nexit 3\n").expect("write stub");
    let mut permissions = std::fs::metadata(&dead).expect("metadata").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&dead, permissions).expect("chmod");

    let mut config = RunnerConfig::for_fake_pi(dead.to_string_lossy().to_string());
    config.scenario_env = Vec::new();
    let db = harness.state.db.clone();
    harness.state = harness
        .state
        .with_agent_runner(AgentRunner::new(db).with_config(config));

    let attempt_id = harness.dispatch("16").await;
    let record = harness.wait_terminal(&attempt_id).await;
    assert_eq!(record.state, AttemptState::Failed);
    assert!(
        record.note.as_deref().unwrap_or("").contains("before any frame"),
        "一个帧都没见到 = 启动即坏，不进 fork 找回: {}",
        record.note.as_deref().unwrap_or("")
    );
}

// ---------------------------------------------------------------------------
// token 流式记账
// ---------------------------------------------------------------------------

#[tokio::test]
async fn token_streaming_accounts_only_authoritative_usage_once() {
    let harness = Harness::new("token-streaming");
    let attempt_id = harness.dispatch("16").await;

    let record = harness.wait_terminal(&attempt_id).await;
    assert_eq!(record.state, AttemptState::Settled);
    // 5 次累积快照 + 1 次权威 message_end（60/6）+ 1 次嵌套 tool（25/4）。
    assert_eq!(record.tokens.input, 85, "累积快照不得当增量累加");
    assert_eq!(record.tokens.output, 10);
    assert_eq!(record.tokens.total, 95);
    assert_eq!(record.tokens.streamed_total, 95, "我方累计与权威值一致");
    assert_eq!(record.tokens.nested_usage_calls, 1);
    assert_eq!(record.tokens.counted_messages, 1, "message_end 与 agent_end 重复只记一次");
    assert!(record.tokens.note.is_none(), "对账一致时不记偏差");
}

// ---------------------------------------------------------------------------
// steer / follow_up / abort / clear_queue + disposition 回流
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_steer_round_trips_and_its_disposition_reaches_the_event_bus() {
    let harness = Harness::new("silence");
    let mut rx = harness.subscribe();
    let attempt_id = harness.dispatch("16").await;

    let response = harness
        .state
        .agent_runner
        .steer(
            &SteerRequest {
                attempt_id: attempt_id.clone(),
                message: "focus on the failing test".into(),
            },
        )
        .await
        .expect("steer must round-trip");
    assert_eq!(response.disposition.as_ref().map(|d| d.as_str()), Some("queued"));
    assert_eq!(response.state, AttemptState::Running);

    let event = next_kind(&mut rx, EventKind::Steer).await;
    assert_eq!(event.payload["channel"], "steer");
    assert_eq!(event.payload["disposition"], "queued");
    assert_eq!(event.payload["message"], "focus on the failing test");
    assert_eq!(event.attempt_id.as_deref(), Some(attempt_id.as_str()));

    // clear_queue 往返：清掉刚排上的那条。
    let cleared = harness
        .state
        .agent_runner
        .clear_queue(&attempt_id)
        .await
        .expect("clear_queue must round-trip");
    assert_eq!(cleared, 1, "刚排上的 steer 要被清掉");

    // 之后 abort：探针相里子进程有序退出。
    let _accepted = harness
        .state
        .agent_runner
        .abort(
            &harness.state,
            &AbortRequest {
                attempt_id: attempt_id.clone(),
                reason: Some("human said stop".into()),
            },
        )
        .await
        .expect("abort must be accepted");
    assert_eq!(_accepted.state, AttemptState::Running);
    let record = harness.wait_terminal(&attempt_id).await;
    assert_eq!(record.state, AttemptState::Failed);
    assert!(
        record.note.as_deref().unwrap_or("").contains("abort acknowledged"),
        "{}",
        record.note.as_deref().unwrap_or("")
    );
    let abort_event = next_kind(&mut rx, EventKind::Abort).await;
    assert_eq!(abort_event.payload["reason"], "human said stop");
}

#[tokio::test]
async fn a_follow_up_is_a_separate_channel_from_a_steer() {
    let harness = Harness::new("silence");
    let mut rx = harness.subscribe();
    let attempt_id = harness.dispatch("16").await;

    let response = harness
        .state
        .agent_runner
        .follow_up(&attempt_id, "also run the linter")
        .await
        .expect("follow_up must round-trip");
    assert_eq!(
        response.disposition.as_ref().map(|d| d.as_str()),
        Some("queued")
    );
    let event = next_kind(&mut rx, EventKind::Steer).await;
    assert_eq!(event.payload["channel"], "follow_up");

    let cleared = harness
        .state
        .agent_runner
        .clear_queue(&attempt_id)
        .await
        .expect("clear_queue");
    assert_eq!(cleared, 1, "follow_up 排的是另一条队列");
}

#[tokio::test]
async fn a_rejected_steer_is_a_disposition_not_a_host_error() {
    let harness = Harness::new("reject-steer");
    let mut rx = harness.subscribe();
    let attempt_id = harness.dispatch("16").await;

    let response = harness
        .state
        .agent_runner
        .steer(
            &SteerRequest {
                attempt_id: attempt_id.clone(),
                message: "turn left".into(),
            },
        )
        .await
        .expect("pi 拒绝注入不是宿主错误");
    assert_eq!(
        response.disposition.as_ref().map(|d| d.as_str()),
        Some("rejected"),
        "disposition=rejected 必须原样回到调用方"
    );

    let event = next_kind(&mut rx, EventKind::Steer).await;
    assert_eq!(event.payload["disposition"], "rejected");
    assert_eq!(event.payload["accepted"], false);
}

// ---------------------------------------------------------------------------
// settle 期注入推迟
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_injection_during_the_settle_window_is_deferred_until_after_settle() {
    // settle 窗口 800ms：静默钟给 3s，免得窗口被两口钟打断。
    let harness = Harness::with_ladder(
        "deferred-settle",
        LadderConfig::fast(3_000, 10_000, 600),
        &[("FAKE_PI_SETTLE_DELAY_MS", "800")],
    );
    let mut rx = harness.subscribe();
    let attempt_id = harness.dispatch("16").await;

    // 等第一轮跑完、进入 settle 窗口再注。
    tokio::time::sleep(Duration::from_millis(250)).await;
    let response = harness
        .state
        .agent_runner
        .steer(
            &SteerRequest {
                attempt_id: attempt_id.clone(),
                message: "after you settle, do this".into(),
            },
        )
        .await
        .expect("settle 期注入必须被受理（只是推迟）");
    assert_eq!(
        response.disposition.as_ref().map(|d| d.as_str()),
        Some("deferred"),
        "settle 期注入的 disposition 必须是 deferred"
    );
    let event = next_kind(&mut rx, EventKind::Steer).await;
    assert_eq!(event.payload["disposition"], "deferred");

    // 推迟的注入在 settle 之后才执行（替身日志里留下 deferred_exec）。
    let record = harness.wait_terminal(&attempt_id).await;
    assert_eq!(record.state, AttemptState::Settled);
    let log = harness.log();
    assert!(
        log.iter()
            .any(|entry| entry["t"] == "deferred_exec" && entry["message"] == "after you settle, do this"),
        "推迟的注入必须在 settle 后执行: {log:?}"
    );
}

// ---------------------------------------------------------------------------
// 凭据：只进 pi 子进程
// ---------------------------------------------------------------------------

#[tokio::test]
async fn credentials_reach_the_pi_child_and_never_the_host_process() {
    let harness = Harness::with_credentials(
        "normal",
        std::sync::Arc::new(StaticCredentialCourier::new(vec![(
            "FARM_TEST_SECRET".to_string(),
            "only-the-pi-child-sees-this".to_string(),
        )])),
    );
    let attempt_id = harness.dispatch("16").await;
    let record = harness.wait_terminal(&attempt_id).await;
    assert_eq!(record.state, AttemptState::Settled);

    let log = harness.log();
    let spawn = log.first().expect("spawn line");
    assert_eq!(
        spawn["env"]["FARM_TEST_SECRET"], "only-the-pi-child-sees-this",
        "凭据必须到达 pi 子进程"
    );
    assert!(
        std::env::var("FARM_TEST_SECRET").is_err(),
        "宿主进程自己不持有这份凭据（env 注入是单向的）"
    );
}

#[tokio::test]
async fn a_failing_credential_courier_fails_the_spawn_and_raises_needs_attention() {
    let harness = Harness::with_credentials(
        "normal",
        std::sync::Arc::new(FailingCredentialCourier),
    );
    let mut rx = harness.subscribe();
    let attempt_id = harness.dispatch("16").await;

    let record = harness.wait_terminal(&attempt_id).await;
    assert_eq!(record.state, AttemptState::Failed);
    assert!(
        record.note.as_deref().unwrap_or("").contains("credential courier failed"),
        "{}",
        record.note.as_deref().unwrap_or("")
    );
    let event = next_kind(&mut rx, EventKind::NeedsAttention).await;
    assert_eq!(event.payload["reason"], "credential courier failed inside the spawn window");
}

// ---------------------------------------------------------------------------
// 中断态（第四终态）+ 双跑防护
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_host_restart_marks_running_attempts_interrupted_with_a_truncated_snapshot() {
    let harness = Harness::new("normal");
    // 手写一行 running attempt（宿主重启时监督任务已经不在了）。
    let mut record = farm_core::agent_runner::store::AttemptRecord::running("att-16", "16", None);
    record.tokens = farm_core::agent_runner::accounting::TokenSnapshot::from_json(&json!({
        "input": 42, "output": 4, "total": 46,
    }));
    let store = AttemptStore::new(&harness.state.db);
    store.upsert(&record).expect("seed a running attempt");

    let mut rx = harness.subscribe();
    let interrupted = harness
        .state
        .agent_runner
        .recover_interrupted(&harness.state)
        .expect("recover");
    assert_eq!(interrupted, vec!["att-16".to_string()]);

    let found = store.find("att-16").expect("read").expect("row");
    assert_eq!(found.state, AttemptState::Interrupted, "第四终态");
    assert!(found.accounting_truncated, "记账以最后快照为准并标截断");
    assert_eq!(found.tokens.total, 46, "最后快照不许丢");

    let event = next_kind(&mut rx, EventKind::Interrupted).await;
    assert_eq!(event.payload["accountingTruncated"], true);
    assert_eq!(event.payload["tokens"]["total"], 46);
}

#[tokio::test]
async fn a_second_dispatch_on_a_ticket_with_a_running_attempt_is_rejected() {
    let harness = Harness::new("silence");
    let first = harness.dispatch("16").await;

    let error = harness
        .state
        .agent_runner
        .dispatch(
            &harness.state,
            &DispatchRequest {
                issue_id: "16".into(),
                profile: None,
                prompt: Some("again".into()),
            },
        )
        .await
        .expect_err("同一票不能同时跑两个 attempt");
    let contract = error.to_contract_error();
    assert_eq!(contract.code, farm_sdk::ErrorCode::Conflict);
    assert!(
        contract.message.contains(&first),
        "冲突消息要点名当前持有的 attempt: {}",
        contract.message
    );
}

#[tokio::test]
async fn steering_an_unknown_attempt_is_a_closed_set_not_found() {
    let harness = Harness::new("normal");
    let error = harness
        .state
        .agent_runner
        .steer(
            &SteerRequest {
                attempt_id: "att-does-not-exist".into(),
                message: "hello?".into(),
            },
        )
        .await
        .expect_err("未知 attempt");
    assert_eq!(error.to_contract_error().code, farm_sdk::ErrorCode::NotFound);
}
