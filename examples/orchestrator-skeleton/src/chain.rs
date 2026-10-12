//! 端到端 tracer（ticket #26）：以**契约客户端**身份对**真实 farm-host 二进制**跑通整链，
//! 并在中途杀宿主、重启对账、恢复续跑。每一步都有**机械断言**（merge commit、事件序、账本形状）。
//!
//! ## 整链（GLOSSARY 词汇）
//! 读 workflow 配置 → claim 票（原子）→ workspace 建 worktree → 派发（fake pi）→
//! 凭据过路注入（fake nw）→ merge 提议 → 机械校验 → merge commit（完成信号）→
//! 投影关票 → 通知外发（fake HTTP，经 notifier 插件）→ **杀宿主** → 重启对账
//! （running→interrupted + 审计补账）→ 恢复续跑至完成。
//!
//! ## 编排器只是契约客户端
//! 全程只过 farm-sdk 类型 + 契约/管理 HTTP API + WS，无私有后门。fake 替身
//! （fake-pi/fake-nw/fake-ledger/http-notifier）由宿主按 env 装配——宿主仍是服务端。
//!
//! ## 关于「agent 的 git 工作」
//! fake-pi 是非干活的替身（只发 RPC 事件，不写代码/提交）。故 tracer 在 worktree 里
//! 代行那一次提交——worktree 路径是经 `GET /v1/workspace/mappings`（契约）得知的目录，
//! 这不是后门，是 tracer 对「agent 完成工作」这一环的诚实模拟；宿主随后的机械校验
//! （worktree 干净 / FF 可行 / 测试过）与 merge 才是被断言的对象。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use farm_sdk::events::EventKind;
use farm_sdk::notifier::{NotificationChannelUpdate, NotifierFilter};
use farm_sdk::services::merge::MergeProposal;
use farm_sdk::services::tracker::IssueState;
use farm_sdk::services::workspace::WorktreeEstablishRequest;

use crate::client::ContractClient;
use crate::harness::{FarmHost, FakeEndpoint};
use crate::report::{Check, Report};

const TICKET: &str = "26";
const SPEC: &str = "spec-10";
const INTEGRATION: &str = "implement/spec-10";
const WORKTREE_BRANCH: &str = "farm/t26-e2e";
const ASSIGNEE: &str = "orchestrator-tracer";
const CREDENTIAL_SECRET: &str = "ghp_passthrough_secret_t26";

/// 整链需要的 fake 替身二进制路径。
#[derive(Clone, Debug)]
pub struct ChainBinaries {
    pub fake_pi: PathBuf,
    pub fake_nw: PathBuf,
    pub fake_ledger: PathBuf,
    pub notifier: PathBuf,
}

/// 跑完整链路 + 崩溃恢复演练；返回逐条机械断言的报告。
pub async fn run_full_chain(binaries: &ChainBinaries) -> Report {
    let mut report = Report::new("farm end-to-end tracer (full chain + crash-recovery drill)");

    let scratch = tempfile::tempdir().expect("scratch dir");
    let repo = scratch.path().join("repo");
    let worktree = scratch.path().join("worktree");
    let run_tests = scratch.path().join("run-tests.sh");
    let tickets = scratch.path().join("tickets.json");
    let pi_log = scratch.path().join("fake-pi.log");
    let ledger_log = scratch.path().join("fake-ledger.log");

    scaffold_repo(&repo, &run_tests, &tickets);

    let endpoint = FakeEndpoint::start().await;
    let addr = crate::harness::ephemeral_addr();
    // 崩溃恢复的关键：杀宿主后重启用**同一个库**——attempt / 事件 / 映射跨重启共享。
    let db_path = scratch.path().join("farm.db");

    // 共享 env（除 scenario / ledger 外，所有宿主实例一致；notifier 经继承拿到连接信息）。
    let base_env = |scenario: &str| -> Vec<(String, String)> {
        vec![
            ("FARM_AGENT_PROGRAM".into(), binaries.fake_pi.to_string_lossy().into_owned()),
            ("FARM_CREDENTIAL_PROGRAM".into(), binaries.fake_nw.to_string_lossy().into_owned()),
            ("FARM_CREDENTIAL_KEYS".into(), "github-token".into()),
            ("FARM_MERGE_TEST_COMMAND".into(), run_tests.to_string_lossy().into_owned()),
            ("FARM_NATIVE_TICKETS".into(), tickets.to_string_lossy().into_owned()),
            ("FARM_HOST".into(), addr.to_string()),
            ("FARM_NOTIFIER_URL".into(), format!("http://{}/notify", endpoint.addr)),
            ("FARM_NOTIFIER_REPO".into(), SPEC.into()),
            ("FARM_NOTIFIER_POLL_MS".into(), "150".into()),
            ("FAKE_PI_SCENARIO".into(), scenario.into()),
            ("FAKE_PI_LOG".into(), pi_log.to_string_lossy().into_owned()),
            ("FAKE_NW_KEYS".into(), format!(r#"{{"github-token":{{"env":"FARM_GH_TOKEN","value":"{CREDENTIAL_SECRET}"}}}}"#)),
        ]
        .into_iter()
        .map(|(k, v)| (k, v))
        .collect()
    };

    // =====================================================================
    // Phase 1 — 宿主 #1（silence：attempt 挂住不 settle；审计旁路**禁用**，dispatch 留未入账）
    // =====================================================================
    let mut host1 = FarmHost::spawn_at(addr, &db_path, &owned(base_env("silence")), true).await;
    let client = host1.client();

    // 读 workflow 配置（管理面）。
    match client.workflow_config(SPEC).await {
        Ok(_) => report.push(Check::pass("read the workflow config (host admin API)", "config status reachable")),
        Err(e) => report.push(Check::fail("read the workflow config (host admin API)", format!("{e}"))),
    }

    // claim 票（原子）。
    match client.claim(TICKET, ASSIGNEE).await {
        Ok(c) => report.record("claim the ticket (atomic claim)", c.claimed, format!("issue {TICKET} claimed by {ASSIGNEE}")),
        Err(e) => report.push(Check::fail("claim the ticket (atomic claim)", format!("{e}"))),
    }

    // workspace 建 worktree（契约原语）。
    let establish = WorktreeEstablishRequest {
        spec_id: SPEC.into(),
        integration_branch: INTEGRATION.into(),
        ticket_id: TICKET.into(),
        repo_path: repo.to_string_lossy().into_owned(),
        worktree_path: worktree.to_string_lossy().into_owned(),
        worktree_branch: WORKTREE_BRANCH.into(),
        start_point: Some(INTEGRATION.into()),
    };
    match client.establish_worktree(&establish).await {
        Ok(wt) => report.record(
            "the workspace service builds the worktree (contract client)",
            wt.state == farm_sdk::services::workspace::WorktreeState::Ready && worktree.join(".git").exists(),
            format!("branch {} state {:?}", wt.branch, wt.state),
        ),
        Err(e) => report.push(Check::fail("the workspace service builds the worktree (contract client)", format!("{e}"))),
    }

    // 派发 attempt #1（fake pi silence → 保持 running）。
    match client.dispatch(TICKET, "work on the capstone ticket").await {
        Ok(d) => report.record(
            "dispatch an attempt (agent-runner spawns fake pi)",
            d.state == farm_sdk::services::agent_runner::AttemptState::Running,
            format!("attempt {} running", d.attempt_id),
        ),
        Err(e) => report.push(Check::fail("dispatch an attempt (agent-runner spawns fake pi)", format!("{e}"))),
    }

    // 凭据过路注入：fake nw 取的 secret 必须经 spawn 窗口进 fake-pi 子进程 env（零落盘除外，日志即证）。
    let cred_ok = wait_for_pi_credential(&pi_log, CREDENTIAL_SECRET, Duration::from_secs(8)).await;
    report.record(
        "the credential passes through (fake nw → spawn window → fake pi child env)",
        cred_ok,
        format!("FARM_GH_TOKEN secret observed in the fake-pi child env log: {cred_ok}"),
    );

    // 派发事件已进事件历史（事件回流面）。
    let dispatch_event_id = event_id_of(&client, EventKind::Dispatch, TICKET).await;
    report.record(
        "the dispatch event lands on the event bus (event reflow face)",
        dispatch_event_id.is_some(),
        format!("dispatch event id = {dispatch_event_id:?}"),
    );

    // ---- 杀宿主（崩溃，非有序停机）----
    host1.kill();
    report.push(Check::pass("kill the host mid-run (crash, not an orderly shutdown)", "host #1 SIGKILLed while the attempt was running"));

    // =====================================================================
    // Phase 2 — 宿主 #2（同库同址；normal；审计旁路**启用** → 启动补账那条未入账的 dispatch）
    // =====================================================================
    let mut env2 = owned(base_env("normal"));
    env2.push(("FARM_LEDGER".into(), binaries.fake_ledger.to_string_lossy().into_owned()));
    env2.push(("FARM_LEDGER_ACTOR_FARM".into(), "accepted".into()));
    env2.push(("FAKE_LEDGER_LOG".into(), ledger_log.to_string_lossy().into_owned()));
    let host2 = FarmHost::spawn_at(addr, &db_path, &env2, true).await;
    let client2 = host2.client();

    // 重启对账：running attempt #1 → interrupted（第四终态）。
    let interrupted = has_event(&client2, EventKind::Interrupted, Some(TICKET)).await;
    let restarted = has_event(&client2, EventKind::HostRestarted, None).await;
    report.record(
        "on restart the running attempt is marked interrupted (the fourth terminal state)",
        interrupted,
        "an `interrupted` event for the ticket is in the event history",
    );
    report.record(
        "on restart a host_restarted event is emitted (crash recovery)",
        restarted,
        "a `host_restarted` event is in the event history",
    );

    // 审计补账：未入账的 dispatch 被补进 Kron（ledger 幂等）；账本形状可机械断言。
    let records = ledger_records(&ledger_log);
    let dispatch_backfilled = records.iter().any(|r| {
        r.get("event").and_then(|e| e.as_str()) == Some("run_boundary")
            && r.get("actor").and_then(|a| a.as_str()) == Some("farm")
    });
    let host_restart_backfilled = records.iter().any(|r| {
        r.get("event").and_then(|e| e.as_str()) == Some("escalation")
            && r.get("actor").and_then(|a| a.as_str()) == Some("farm")
    });
    let shape_ok = records.iter().all(|r| {
        r.get("event").and_then(|e| e.as_str()).is_some()
            && r.get("actor").and_then(|a| a.as_str()) == Some("farm")
            && r.get("idempotency_key").and_then(|k| k.as_str()).is_some()
            && r.get("ts").and_then(|t| t.as_str()).is_some()
            && r.get("details").and_then(|d| d.get("event_id")).is_some()
    });
    report.record(
        "the unposted dispatch is backfilled into the Kron ledger on restart (audit double-write)",
        dispatch_backfilled,
        format!("{} ledger record(s); a run_boundary/farm record present: {dispatch_backfilled}", records.len()),
    );
    report.record(
        "host_restarted is audited as a Kron escalation (actor=farm)",
        host_restart_backfilled,
        "an escalation/farm record present",
    );
    report.record(
        "every ledger record carries the Kron shape (event/actor/ts/idempotency_key/details)",
        shape_ok && !records.is_empty(),
        format!("{} records, all well-shaped: {shape_ok}", records.len()),
    );

    let dispatch_idem = dispatch_event_id.as_ref().map(|id| format!("farm:{id}"));
    let backfill_count = dispatch_idem
        .as_ref()
        .map(|key| records.iter().filter(|r| r.get("idempotency_key").and_then(|k| k.as_str()) == Some(key.as_str())).count())
        .unwrap_or(0);
    report.record(
        "the backfilled dispatch carries the stable idempotency key farm:<event id>",
        backfill_count >= 1,
        format!("idempotency_key {dispatch_idem:?} appears {backfill_count}x"),
    );

    drop(host2); // 停宿主 #2，为幂等重启让出地址

    // =====================================================================
    // Phase 2b — 宿主 #2b（同库同址，审计仍启用）：启动补账应**无账可补**（幂等）
    // =====================================================================
    let host2b = FarmHost::spawn_at(addr, &db_path, &env2, true).await;
    let records_after = ledger_records(&ledger_log);
    let backfill_count_again = dispatch_idem
        .as_ref()
        .map(|key| records_after.iter().filter(|r| r.get("idempotency_key").and_then(|k| k.as_str()) == Some(key.as_str())).count())
        .unwrap_or(0);
    report.record(
        "a second restart backfills nothing new (idempotent: the audited event is not re-posted)",
        backfill_count_again == backfill_count,
        format!("dispatch idempotency_key count stayed {backfill_count} → {backfill_count_again} across restarts"),
    );

    // =====================================================================
    // Phase 3 — 恢复续跑至完成（宿主 #2b 上）
    // =====================================================================
    let client = host2b.client();

    // 打开 merge 完成的通知外发（管理面：per-仓库通道配置，宿主零推送域知识）。
    let channel = NotificationChannelUpdate {
        enabled: true,
        filter: NotifierFilter { blocked: true, needs_attention: true, stall: true, merge_complete: true },
    };
    match client.set_channel(SPEC, &channel).await {
        Ok(_) => report.push(Check::pass("turn on merge-complete notification (host admin channel config)", "channel updated")),
        Err(e) => report.push(Check::fail("turn on merge-complete notification (host admin channel config)", format!("{e}"))),
    }

    // 恢复：重派 attempt #2（normal → settle）。
    match client.dispatch(TICKET, "resume and finish the capstone ticket").await {
        Ok(d) => report.push(Check::pass("resume by re-dispatching the attempt (orchestrator resumes after the crash)", format!("attempt {} dispatched", d.attempt_id))),
        Err(e) => report.push(Check::fail("resume by re-dispatching the attempt", format!("{e}"))),
    }
    let settled = wait_for_event(&client, EventKind::AgentSettled, TICKET, Duration::from_secs(10)).await;
    report.record(
        "the resumed attempt settles (agent_settled, judgement ladder)",
        settled,
        "an `agent_settled` event for the ticket is in the event history",
    );

    // tracer 代行 agent 的 git 工作：在 worktree 里落一个提交（分支因此领先集成分支）。
    let integration_before = git(&repo, &["rev-parse", INTEGRATION]);
    make_work_commit(&worktree);

    // merge 提议 → 宿主机械校验 + 执行真 merge。
    let proposal = MergeProposal {
        ticket_id: TICKET.into(),
        worktree_branch: WORKTREE_BRANCH.into(),
        commit: None,
        message: Some("capstone end-to-end tracer".into()),
    };
    let merge_commit = match client.propose_merge(&proposal).await {
        Ok(resp) => {
            report.record(
                "the host mechanically validates and executes the real merge (merge proposal → merge commit)",
                resp.state == farm_sdk::services::merge::MergeProposalState::Merged,
                format!("proposal {} → {:?}", resp.proposal_id, resp.state),
            );
            // 完成信号 = 集成分支上出现该票的 merge commit（机械事实，rev-parse 可查）。
            let commit = event_payload_field(&client, EventKind::Merge, TICKET, "merge_commit").await;
            let on_branch = commit.as_ref().map(|c| git(&repo, &["rev-parse", INTEGRATION]) == *c).unwrap_or(false);
            report.record(
                "the completion signal is the ticket's merge commit on the integration branch",
                on_branch,
                format!("merge commit {:?} == integration tip: {on_branch}", commit),
            );
            commit
        }
        Err(e) => {
            report.push(Check::fail("the host executes the real merge", format!("{e}")));
            None
        }
    };
    let _ = integration_before;

    // merge 事件序：dispatch → … → merge（事件按 seq 升序，merge 在 dispatch 之后）。
    let order_ok = event_order_holds(&client, TICKET).await;
    report.record(
        "event ordering holds (dispatch precedes merge on the ticket's timeline)",
        order_ok,
        "dispatch … merge in ascending seq order",
    );

    // 投影关票：完成信号之后宿主机械把票标 done。
    let closed = match client.issues().await {
        Ok(list) => list.issues.iter().any(|i| i.issue_id == TICKET && i.state == IssueState::Done),
        Err(_) => false,
    };
    report.record(
        "the projection closes the ticket after the completion signal (done, irreversible)",
        closed,
        format!("ticket {TICKET} state == done: {closed}"),
    );

    // 通知外发：notifier 插件（契约客户端）订阅宿主事件总线、按通道配置把 merge 完成
    // POST 到 fake HTTP 端点。本沙箱里宿主经 tokio::process 拉起的、带自身 tokio runtime
    // 的子进程会冻住（#19 插件监督走 tokio::process）；故 e2e 由链路经 std::process 直接
    // 拉起 notifier 打真实宿主——notifier 是契约客户端，连宿主 HTTP 面，与「宿主拉起插件」
    // 机制（#19，fake-plugin 无 runtime 不受影响）解耦。
    let mut notifier = spawn_notifier(
        &binaries.notifier,
        addr,
        &host2b.op_token,
        host2b.admin_token.as_deref(),
        &endpoint,
    );
    let notified = wait_for_notification(&endpoint, "merge", Duration::from_secs(8)).await;
    let _ = notifier.kill();
    let _ = notifier.wait();
    report.record(
        "the notifier plugin forwards the merge-complete event to the external HTTP endpoint",
        notified,
        format!("fake endpoint received a merge notification: {notified}"),
    );

    report.record(
        "the whole chain reaches its completion signal end to end",
        merge_commit.is_some() && closed && notified,
        format!("merge_commit={:?} closed={closed} notified={notified}", merge_commit),
    );

    report
}

// ---------------------------------------------------------------------------
// 脚手架：scratch 仓库 + 集成分支 + 快测试脚本 + 原生票
// ---------------------------------------------------------------------------

/// 经 `std::process` 直接拉起 notifier 插件（契约客户端）打真实宿主。
///
/// 宿主经 `tokio::process` 拉起带自身 tokio runtime 的子进程在本沙箱会冻住，故 e2e 走
/// `std::process`（与 harness 拉起 farm-host 同路）；notifier 连的是宿主 HTTP 面，行为不变。
fn spawn_notifier(
    bin: &Path,
    host: std::net::SocketAddr,
    op_token: &str,
    admin_token: Option<&str>,
    endpoint: &FakeEndpoint,
) -> std::process::Child {
    let mut cmd = Command::new(bin);
    cmd.env("FARM_HOST", host.to_string())
        .env("FARM_TOKEN", op_token)
        .env("FARM_NOTIFIER_URL", format!("http://{}/notify", endpoint.addr))
        .env("FARM_NOTIFIER_REPO", SPEC)
        .env("FARM_NOTIFIER_POLL_MS", "150")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());
    if let Some(admin) = admin_token {
        cmd.env("FARM_ADMIN_TOKEN", admin);
    }
    cmd.spawn().expect("spawn the notifier plugin")
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(cwd).args(args).output().expect("spawn git");
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn scaffold_repo(repo: &Path, run_tests: &Path, tickets: &Path) {
    std::fs::create_dir_all(repo).expect("create repo dir");
    git(repo, &["init", "--initial-branch=main"]);
    git(repo, &["config", "user.email", "tracer@farm.local"]);
    git(repo, &["config", "user.name", "Farm Tracer"]);
    std::fs::write(repo.join("README.md"), "capstone e2e scratch\n").expect("write readme");
    git(repo, &["add", "."]);
    git(repo, &["commit", "-m", "init"]);
    git(repo, &["branch", INTEGRATION]);

    std::fs::write(run_tests, "#!/bin/sh\nexit 0\n").expect("write run-tests.sh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(run_tests).expect("stat").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(run_tests, perms).expect("chmod");
    }

    std::fs::write(
        tickets,
        format!(r#"[{{"number":{TICKET},"title":"capstone tracer","state":"open","labels":["dispatch"],"blocked_by":[]}}]"#),
    )
    .expect("write tickets.json");

    // 仓库里的 workflow 配置（GLOSSARY）：开着 close_issue 投影，让宿主在完成信号后机械关票。
    std::fs::create_dir_all(repo.join(".farm")).expect("mkdir .farm");
    std::fs::write(
        repo.join(".farm/workflow.json"),
        r#"{"dispatch_labels":["dispatch"],"max_concurrency":1,"stall_threshold_secs":3600,"prompt_template":"work on {{ticket_id}}","projection":{"close_issue":true,"open_pr":false,"comment_merge_commit":false,"add_label":false}}"#,
    )
    .expect("write workflow.json");
}

/// tracer 代行 agent 的 git 工作：在 worktree 里落一个提交。
fn make_work_commit(worktree: &Path) {
    std::fs::write(worktree.join("capstone.txt"), "end-to-end tracer work\n").expect("write work file");
    git(worktree, &["add", "."]);
    git(worktree, &["commit", "-m", "feat: capstone tracer work"]);
}

// ---------------------------------------------------------------------------
// 观测助手（全部经契约/文件系统，不碰宿主内部）
// ---------------------------------------------------------------------------

fn owned(env: Vec<(String, String)>) -> Vec<(String, String)> {
    env
}

async fn wait_for_pi_credential(log_path: &Path, secret: &str, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if let Ok(content) = std::fs::read_to_string(log_path) {
            if content.contains(secret) {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

async fn event_id_of(client: &ContractClient, kind: EventKind, ticket: &str) -> Option<String> {
    let history = client.event_history(1000).await.ok()?;
    history.events.into_iter().find(|e| e.kind == kind && e.ticket_id.as_deref() == Some(ticket)).map(|e| e.id)
}

async fn has_event(client: &ContractClient, kind: EventKind, ticket: Option<&str>) -> bool {
    match client.event_history(1000).await {
        Ok(history) => history.events.iter().any(|e| {
            e.kind == kind && ticket.map(|t| e.ticket_id.as_deref() == Some(t)).unwrap_or(true)
        }),
        Err(_) => false,
    }
}

async fn wait_for_event(client: &ContractClient, kind: EventKind, ticket: &str, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if has_event(client, kind, Some(ticket)).await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    false
}

async fn event_payload_field(client: &ContractClient, kind: EventKind, ticket: &str, field: &str) -> Option<String> {
    let history = client.event_history(1000).await.ok()?;
    history.events.into_iter().find(|e| e.kind == kind && e.ticket_id.as_deref() == Some(ticket))
        .and_then(|e| e.payload.get(field).and_then(|v| v.as_str()).map(str::to_string))
}

/// 事件序：该票的 dispatch 事件必须出现在 merge 事件之前（按返回的升序 index）。
async fn event_order_holds(client: &ContractClient, ticket: &str) -> bool {
    let history = match client.event_history(1000).await {
        Ok(h) => h.events,
        Err(_) => return false,
    };
    let dispatch_idx = history.iter().position(|e| e.kind == EventKind::Dispatch && e.ticket_id.as_deref() == Some(ticket));
    let merge_idx = history.iter().position(|e| e.kind == EventKind::Merge && e.ticket_id.as_deref() == Some(ticket));
    match (dispatch_idx, merge_idx) {
        (Some(d), Some(m)) => d < m,
        _ => false,
    }
}

async fn wait_for_notification(endpoint: &FakeEndpoint, kind: &str, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        let hit = endpoint.posts().into_iter().any(|p| p.body.contains(&format!("\"kind\":\"{kind}\"")));
        if hit {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    false
}

/// 解析 fake-ledger 日志里的 Kron 记录（每笔是 `{"t":"record","line":"<ndjson>"}`）。
fn ledger_records(log_path: &Path) -> Vec<serde_json::Value> {
    let content = std::fs::read_to_string(log_path).unwrap_or_default();
    content
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v.get("t").and_then(|t| t.as_str()) == Some("record"))
        .filter_map(|v| v.get("line").and_then(|l| l.as_str()).and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()))
        .collect()
}
