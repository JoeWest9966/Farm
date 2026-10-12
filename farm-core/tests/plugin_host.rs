//! 插件宿主接缝测试（ticket #19，ADR-0004 决策 4）。
//!
//! 全部经**真实子进程**打接缝：宿主拉起 `fake-plugin` 替身（`CARGO_BIN_EXE_fake-plugin`），
//! 走真的 stdin/stdout 管道。命令路径可注入（`PluginSpec::program`）正是这条接缝能跑在
//! 替身之上的原因——与 #16 的 fake-pi / #20 的 fake-ledger 同宗。
//!
//! 覆盖验收：① 插件被宿主拉起、握手；② 关 stdin ⇒ 有序停机（插件自己退，不是被强杀）；
//! ③ 崩溃按分级退避重启、3 次后 quarantined + `needs_attention`；④ 隔离后经恢复接口
//! 重启并自愈；⑤ 宿主收尾后无孤儿插件进程。

use std::path::{Path, PathBuf};
use std::time::Duration;

use farm_core::plugin::{
    PluginError, PluginHost, PluginSpec, PluginState, PluginStatus, RetryPolicy,
};
use farm_core::{AppState, HostConfig};
use farm_sdk::admin::PluginSummary;
use farm_sdk::events::EventKind;
use serde_json::Value;

/// fake plugin 替身的绝对路径（cargo 为集成测试提供）。
const FAKE_PLUGIN: &str = env!("CARGO_BIN_EXE_fake-plugin");

/// 快档退避（1s/10s/1min 的等比例缩小）：机制一样，测试不必真睡 71 秒。
/// 规范档位本身由 `plugin::retry` 的已知字面量单测钉死。
fn fast_policy() -> RetryPolicy {
    RetryPolicy::with_schedule(vec![40, 80, 120], 3)
}

fn test_state() -> (AppState, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir").keep();
    let db_path = dir.join("farm.db");
    let mut state = AppState::new_in_memory(HostConfig {
        bind: "127.0.0.1:3791".parse().expect("valid bind"),
        op_token: "plugin-test-token".into(),
        admin_token: None,
        db_path: db_path.clone(),
    })
    .expect("in-memory state");
    let host = PluginHost::with_policy(&state.db, fast_policy());
    state = state.with_plugin(host);
    (state, dir)
}

/// 一个 fake-plugin 计划：场景 + 生命周期日志路径。
fn spec(scenario: &str, log: &Path) -> PluginSpec {
    PluginSpec::new("fake-plugin", "fake plugin", FAKE_PLUGIN)
        .with_args(vec![])
        .with_env("FAKE_PLUGIN_SCENARIO", scenario)
        .with_env("FAKE_PLUGIN_LOG", log.display().to_string())
        .with_handshake_timeout(Duration::from_millis(400))
        .with_shutdown_grace(Duration::from_millis(500))
}

/// 读替身生命周期日志（一行一个 JSON）。
fn log_events(log: &Path) -> Vec<Value> {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("json log line"))
        .collect()
}

fn count(log: &Path, kind: &str) -> usize {
    log_events(log)
        .into_iter()
        .filter(|event| event.get("t").and_then(|t| t.as_str()) == Some(kind))
        .count()
}

/// 进程是否还活着（Linux `/proc`；测试专用探针）。
fn pid_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

fn status_of(state: &AppState, plugin_id: &str) -> PluginStatus {
    state
        .plugin
        .status(plugin_id)
        .expect("status query")
        .expect("the plugin must have a row")
}

// ---------------------------------------------------------------------------
// 拉起 + 握手（验收①）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_host_launches_a_plugin_and_gets_its_handshake() {
    let (state, dir) = test_state();
    let log = dir.join("fake-plugin.log");
    std::fs::write(&log, "").expect("empty log");

    let hello = state
        .plugin
        .launch(&state, &spec("normal", &log))
        .await
        .expect("a well-behaved plugin must handshake");

    assert_eq!(hello.plugin, "fake-plugin");
    assert_eq!(hello.contract_version, "1.0.0", "握手自描述契约版本");
    assert!(
        hello.is_compatible(),
        "同主版本的契约版本必须判为兼容（与编排器握手同一规则）"
    );

    let status = status_of(&state, "fake-plugin");
    assert_eq!(status.state, PluginState::Running);
    assert_eq!(status.restarts, 0, "握手成功 = 自愈，重试预算归零");
    assert!(status.pid.is_some(), "子进程 pid 要落账（无孤儿断言的地基）");
    assert_eq!(
        status.to_summary(),
        PluginSummary {
            plugin_id: "fake-plugin".into(),
            name: "fake plugin".into(),
            state: PluginState::Running,
            restarts: 0,
        },
        "宿主管理 API（#23）的投影形状"
    );
    assert_eq!(count(&log, "hello"), 1, "一次拉起一次握手");

    state.plugin.shutdown_all().await;
}

#[tokio::test]
async fn a_plugin_that_speaks_an_incompatible_contract_version_is_refused() {
    let (state, dir) = test_state();
    let log = dir.join("fake-plugin.log");
    std::fs::write(&log, "").expect("empty log");

    let mut plan = spec("normal", &log);
    plan.env
        .push(("FAKE_PLUGIN_CONTRACT_VERSION".into(), "2.0.0".into()));
    let err = state
        .plugin
        .launch(&state, &plan)
        .await
        .expect_err("契约大版本不同必须被拒（闭集 contract_version_unsupported 的同一条规则）");

    assert!(
        matches!(err, PluginError::HandshakeRejected { .. }),
        "收到 {err:?}"
    );
    state.plugin.shutdown_all().await;
}

#[tokio::test]
async fn a_plugin_that_never_handshakes_fails_the_launch_instead_of_hanging() {
    let (state, dir) = test_state();
    let log = dir.join("fake-plugin.log");
    std::fs::write(&log, "").expect("empty log");

    let err = state
        .plugin
        .launch(&state, &spec("silent", &log))
        .await
        .expect_err("不说话插件必须让 launch 失败，而不是挂住宿主");

    assert!(
        matches!(err, PluginError::Handshake { .. }),
        "收到 {err:?}"
    );
    // 起不来也是一种崩溃：监督循环接手重试，不是拉倒一次就放弃。
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        count(&log, "start") >= 2,
        "silent 插件该被重试，日志只有 {:?}",
        log_events(&log)
    );
    state.plugin.shutdown_all().await;
}

// ---------------------------------------------------------------------------
// 有序停机（验收①后半）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn closing_the_plugins_stdin_shuts_it_down_in_order() {
    let (state, dir) = test_state();
    let log = dir.join("fake-plugin.log");
    std::fs::write(&log, "").expect("empty log");

    let hello = state
        .plugin
        .launch(&state, &spec("normal", &log))
        .await
        .expect("handshake");
    assert_eq!(hello.plugin, "fake-plugin");
    let pid = status_of(&state, "fake-plugin").pid.expect("pid");

    state
        .plugin
        .shutdown("fake-plugin")
        .await
        .expect("orderly shutdown");

    let events = log_events(&log);
    assert_eq!(count(&log, "hello"), 1, "有序停机不触发重试");
    assert!(
        events.iter().any(|event| {
            event.get("t").and_then(|t| t.as_str()) == Some("bye")
                && event.get("reason").and_then(|r| r.as_str()) == Some("stdin_eof")
        }),
        "插件必须是自己见到 stdin EOF 才退的：{events:?}"
    );
    assert_eq!(count(&log, "exit"), 1, "一次退出，没有被强杀重来的第二次");

    let status = status_of(&state, "fake-plugin");
    assert_eq!(status.state, PluginState::Stopped, "停机后落 stopped");
    assert_eq!(status.pid, None, "进程没了，pid 不再占账");
    assert!(
        state.plugin.supervised().await.is_empty(),
        "监督任务收尾后不该还挂在表上"
    );
}

#[tokio::test]
async fn a_plugin_that_ignores_stdin_eof_is_killed_after_the_grace_window() {
    // 有序停机的硬边界：宽限内不退，才强杀（父死子陪葬不是无限等待）。
    let (state, dir) = test_state();
    let log = dir.join("fake-plugin.log");
    std::fs::write(&log, "").expect("empty log");

    let hello = state
        .plugin
        .launch(&state, &spec("hang", &log))
        .await
        .expect("a hanging plugin still handshakes first");
    assert_eq!(hello.plugin, "fake-plugin");
    let pid = status_of(&state, "fake-plugin").pid.expect("pid");

    state
        .plugin
        .shutdown("fake-plugin")
        .await
        .expect("shutdown reports the outcome, it does not hang");

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!pid_alive(pid), "宽限后必须强杀，pid {pid} 还活着");
    assert_eq!(status_of(&state, "fake-plugin").state, PluginState::Stopped);
}

// ---------------------------------------------------------------------------
// 无孤儿进程（验收④）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shutting_the_host_down_leaves_no_orphan_plugin_process() {
    let (state, dir) = test_state();
    let log = dir.join("fake-plugin.log");
    std::fs::write(&log, "").expect("empty log");

    let first = state
        .plugin
        .launch(&state, &spec("normal", &log))
        .await
        .expect("first plugin");
    assert_eq!(first.plugin, "fake-plugin");
    let pid = status_of(&state, "fake-plugin").pid.expect("pid");

    state.plugin.shutdown_all().await;

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!pid_alive(pid), "宿主收尾后插件进程不得残留：pid {pid}");
    assert!(
        state.plugin.supervised().await.is_empty(),
        "shutdown_all 之后不该还有监督任务"
    );
    for status in state.plugin.statuses().expect("statuses") {
        assert_eq!(status.state, PluginState::Stopped);
        assert_eq!(status.pid, None);
    }
}

#[tokio::test]
async fn dropping_the_host_handle_also_reaps_the_plugin() {
    // `kill_on_drop` 兜底：句柄被丢掉（宿主卸载）时子进程不得活过宿主。
    let (state, dir) = test_state();
    let log = dir.join("fake-plugin.log");
    std::fs::write(&log, "").expect("empty log");

    let host = PluginHost::with_policy(&state.db, fast_policy());
    let hello = host
        .launch(&state, &spec("hang", &log))
        .await
        .expect("handshake");
    assert_eq!(hello.plugin, "fake-plugin");
    let pid = host.status("fake-plugin").expect("status").expect("row").pid.expect("pid");

    drop(host);

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!pid_alive(pid), "插件宿主句柄丢掉后子进程必须陪葬：pid {pid}");
}

// ---------------------------------------------------------------------------
// 事件投影（隔离要发 needs_attention，面板才渲染得出来）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_host_publishes_first_class_events_for_the_plugin_lifecycle() {
    let (state, dir) = test_state();
    let log = dir.join("fake-plugin.log");
    std::fs::write(&log, "").expect("empty log");

    let _ = state
        .plugin
        .launch(&state, &spec("normal", &log))
        .await
        .expect("handshake");

    let history = state
        .event_history(&farm_core::EventLogFilter {
            since: None,
            kind: None,
            ticket_id: None,
            limit: Some(200),
        })
        .expect("history");
    // 一次成功的拉起不该有噪音事件（没有重启、没有告警）。
    assert!(
        history
            .events
            .iter()
            .all(|event| event.kind != EventKind::NeedsAttention
                && event.kind != EventKind::PluginRestarted
                && event.kind != EventKind::PluginQuarantined),
        "顺利的拉起不该发事件：{:?}",
        history.events
    );

    state.plugin.shutdown_all().await;
}

// ---------------------------------------------------------------------------
// 崩溃退避 + 隔离 + 恢复（验收②③）
// ---------------------------------------------------------------------------

/// 数一数事件日志里某种事件有几条（隔离要发 needs_attention / plugin_quarantined）。
fn event_count(state: &AppState, kind: EventKind) -> usize {
    state
        .event_history(&farm_core::event_log::EventLogFilter {
            since: None,
            kind: Some(kind),
            ticket_id: None,
            limit: None,
        })
        .expect("event history")
        .events
        .len()
}

#[tokio::test]
async fn a_crash_looping_plugin_is_quarantined_after_the_retry_budget_with_a_needs_attention() {
    let (state, dir) = test_state();
    let log = dir.join("fake-plugin.log");
    std::fs::write(&log, "").expect("empty log");

    // crash-loop 每次都崩在握手前：launch 的第一次握手必失败，但监督循环在后台按
    // fast_policy（40/80/120ms，上限 3）重试；restarts 超过 3 → quarantined。
    let result = state.plugin.launch(&state, &spec("crash-loop", &log)).await;
    assert!(result.is_err(), "crash-loop 起不来，launch 报握手失败");

    let status = state
        .plugin
        .wait_for_state("fake-plugin", PluginState::Quarantined, Duration::from_secs(15))
        .await
        .expect("crash-loop 必须在重试耗尽后进隔离");
    assert_eq!(status.state, PluginState::Quarantined);
    assert!(status.restarts > 3, "重试预算已耗尽：{status:?}");

    // 隔离发一等事件：needs_attention（面板必渲染）+ plugin_quarantined（闭集成员）。
    assert!(
        event_count(&state, EventKind::NeedsAttention) >= 1,
        "隔离必须发 needs_attention 一等事件"
    );
    assert!(
        event_count(&state, EventKind::PluginQuarantined) >= 1,
        "隔离必须发 plugin_quarantined 事件"
    );
    // 隔离后停止重启：没有待定的下次重启。
    assert!(status.next_restart_at.is_none(), "隔离后不再排重启");
}

#[tokio::test]
async fn a_quarantined_plugin_self_heals_through_the_recovery_interface() {
    let dir = tempfile::tempdir().expect("tempdir").keep();
    let counter = dir.join("crash-once.counter");
    std::fs::write(&counter, "0").expect("counter starts at zero");
    let log = dir.join("fake-plugin.log");
    std::fs::write(&log, "").expect("empty log");

    // max_restarts=0：第一次崩溃即越界 → 立刻隔离（不必真睡退避）。
    let mut state = AppState::new_in_memory(HostConfig {
        bind: "127.0.0.1:3792".parse().expect("valid bind"),
        op_token: "plugin-test-token".into(),
        admin_token: None,
        db_path: dir.join("farm.db"),
    })
    .expect("in-memory state");
    let host = PluginHost::with_policy(&state.db, RetryPolicy::with_schedule(vec![10], 0));
    state = state.with_plugin(host);

    let crash_once = PluginSpec::new("fake-plugin", "fake plugin", FAKE_PLUGIN)
        .with_env("FAKE_PLUGIN_SCENARIO", "crash-once")
        .with_env("FAKE_PLUGIN_LOG", log.display().to_string())
        .with_env("FAKE_PLUGIN_COUNTER", counter.display().to_string())
        .with_handshake_timeout(Duration::from_millis(400))
        .with_shutdown_grace(Duration::from_millis(500));

    let _ = state.plugin.launch(&state, &crash_once).await; // 崩一次 → 越界 → 隔离
    state
        .plugin
        .wait_for_state("fake-plugin", PluginState::Quarantined, Duration::from_secs(10))
        .await
        .expect("max_restarts=0 时一次崩溃即隔离");

    // 恢复接口：重置预算 → 重启；crash-once 第二次运行不再崩 → 握手自愈。
    let hello = state
        .plugin
        .recover(&state, "fake-plugin")
        .await
        .expect("recovery must relaunch and self-heal");
    assert!(hello.is_compatible(), "恢复后重新握手自描述契约版本");

    let status = status_of(&state, "fake-plugin");
    assert_eq!(status.state, PluginState::Running, "恢复后回到 running");
    assert_eq!(status.restarts, 0, "恢复重置重试预算");
}

// ---------------------------------------------------------------------------
// code review P1-2：退避窗口内请求停机 → 不重启、不留孤儿
// ---------------------------------------------------------------------------

/// 慢档退避（3s）：保证 `shutdown` 落在**退避睡眠中间**（规范档 1s/10s/1min 的
/// 第三档之前那一档的等比例缩小；机制不变，只是测试不必真睡 10s）。
fn slow_backoff_policy() -> RetryPolicy {
    RetryPolicy::with_schedule(vec![3_000], 3)
}

/// 等插件进入「崩过一次、正在退避窗口里睡」这个中间态。
async fn wait_in_backoff(state: &AppState, plugin_id: &str) -> PluginStatus {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let status = status_of(state, plugin_id);
        if status.restarts >= 1 && status.next_restart_at.is_some() {
            return status;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "插件从未进入退避窗口: {status:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn shutting_down_during_the_backoff_window_leaves_no_orphan_and_no_respawn() {
    let dir = tempfile::tempdir().expect("tempdir").keep();
    let log = dir.join("fake-plugin.log");
    std::fs::write(&log, "").expect("empty log");

    let mut state = AppState::new_in_memory(HostConfig {
        bind: "127.0.0.1:3793".parse().expect("valid bind"),
        op_token: "plugin-test-token".into(),
        admin_token: None,
        db_path: dir.join("farm.db"),
    })
    .expect("in-memory state");
    let host = PluginHost::with_policy(&state.db, slow_backoff_policy());
    state = state.with_plugin(host);

    // crash-loop：每次都在握手前崩 → 监督循环进退避，窗口内**没有**子进程。
    let result = state.plugin.launch(&state, &spec("crash-loop", &log)).await;
    assert!(result.is_err(), "crash-loop 起不来，launch 报握手失败");
    let backing_off = wait_in_backoff(&state, "fake-plugin").await;
    assert_eq!(backing_off.state, PluginState::Running, "退避窗口内仍是 running");

    let spawns_before_stop = count(&log, "start");
    let pids_before_stop = log_events(&log)
        .into_iter()
        .filter_map(|event| {
            event
                .get("pid")
                .and_then(|pid| pid.as_u64())
                .map(|pid| pid as u32)
        })
        .collect::<Vec<_>>();
    assert!(
        !pids_before_stop.is_empty(),
        "退避窗口前至少拉起过一次（日志里有 pid）: {:?}",
        log_events(&log)
    );

    // 在退避睡眠中间请求停机。
    let started = tokio::time::Instant::now();
    state
        .plugin
        .shutdown("fake-plugin")
        .await
        .expect("shutdown must report, not hang");
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(2),
        "停机不该等完整个退避窗口（{:?}）：stop 信号必须能打断退避睡眠",
        elapsed
    );
    assert!(
        state.plugin.supervised().await.is_empty(),
        "停机后监督任务必须收尾，不能 detach 后继续跑"
    );

    // 退避窗口（3s）整个过去：不得再有新的 spawn，也不得有活着的残留进程。
    tokio::time::sleep(Duration::from_millis(3_600)).await;
    assert_eq!(
        count(&log, "start"),
        spawns_before_stop,
        "stop 之后不得重启插件（孤儿复活的来源）: {:?}",
        log_events(&log)
    );
    for pid in pids_before_stop {
        assert!(!pid_alive(pid), "停机后插件进程不得残留：pid {pid}");
    }

    // 账面也要收尾：不该留一个「running + 有待定重启」的假象。
    let status = status_of(&state, "fake-plugin");
    assert_eq!(status.state, PluginState::Stopped, "退避中被停机：落 stopped");
    assert!(status.next_restart_at.is_none(), "不再排重启");
}

#[tokio::test]
async fn the_host_gives_the_plugin_a_null_stderr_not_the_hosts_own() {
    // code review P2-11：插件 stderr 原先 `inherit()`——插件直接写宿主的 stderr，
    // 64KB 管道一写满，多话的插件就卡在自己的 stderr 上（表现为握手超时/「卡住」），
    // 宿主的日志也被插件的噪音污染。插件自己的日志归插件自己管。
    let (state, dir) = test_state();
    let log = dir.join("fake-plugin.log");
    std::fs::write(&log, "").expect("empty log");

    state
        .plugin
        .launch(&state, &spec("normal", &log))
        .await
        .expect("handshake");

    let start = log_events(&log)
        .into_iter()
        .find(|event| event.get("t").and_then(|t| t.as_str()) == Some("start"))
        .expect("fake plugin must have logged a start line");
    assert_eq!(
        start["stderr"].as_str(),
        Some("/dev/null"),
        "插件的 stderr 必须是 /dev/null（与 agent-runner 的 pi 子进程同一纪律）: {start:?}"
    );

    state.plugin.shutdown_all().await;
}
