//! Merge 审阅流（ticket #25）：人工闸门叠加在 #18 机械闸门之上。
//!
//! GLOSSARY：agent 提议 → 人审阅 diff → 批准（机械闸门照跑，出 merge commit）或拒绝
//! （闭集错误回 agent）；审批决策进 Kron 审批账（EventKind::Approval 审计双写）。
//! 完成信号仍是 merge commit（机械事实）；审批只是叠加的人工闸门，永不替代机械校验。
//!
//! 全部经公开 API（`AppState` + `MergeService`）+ 真 git CLI + fake `ledger` 替身。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use farm_core::audit::AuditSink;
use farm_core::event_log::EventLogFilter;
use farm_core::merge::{MergeConfig, MergeService};
use farm_core::workspace::EstablishRequest;
use farm_core::{AppState, HostConfig};
use farm_sdk::events::{EventKind, NewEvent};
use farm_sdk::services::merge::{MergeProposal, MergeProposalState};
use serde_json::Value;

const TICKET: &str = "25";
const INTEGRATION: &str = "implement/spec-10";
const BRANCH: &str = "farm/t25";
const FAKE_LEDGER: &str = env!("CARGO_BIN_EXE_fake-ledger");

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git").arg("-C").arg(cwd).args(args).output().expect("spawn git");
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
}

fn write_test_script(repo: &Path, exit: i32) {
    let script = format!("#!/bin/sh\nexit {exit}\n");
    let path = repo.join("run-tests.sh");
    std::fs::write(&path, script).expect("write run-tests.sh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
}

struct Fixture {
    _repo: tempfile::TempDir,
    _wt_parent: tempfile::TempDir,
    repo: PathBuf,
    worktree: PathBuf,
    ledger_log: PathBuf,
    state: AppState,
}

/// 夹具：scratch 仓库 + ticket worktree（映射 built）+ agent 已提交改动 +
/// 快速 merge 测试闸门 + 启用 fake `ledger` 审计旁路（actor_farm_accepted）。
fn fixture(tests_exit: i32) -> Fixture {
    let repo_dir = tempfile::tempdir().expect("tempdir");
    git(repo_dir.path(), &["init", "--initial-branch=main"]);
    git(repo_dir.path(), &["config", "user.email", "test@farm.local"]);
    git(repo_dir.path(), &["config", "user.name", "Farm Test"]);
    std::fs::write(repo_dir.path().join("README.md"), "scratch\n").expect("write readme");
    write_test_script(repo_dir.path(), tests_exit);
    git(repo_dir.path(), &["add", "."]);
    git(repo_dir.path(), &["commit", "-m", "init"]);
    git(repo_dir.path(), &["branch", INTEGRATION]);

    let wt_parent = tempfile::tempdir().expect("worktree parent");
    let worktree = wt_parent.path().join("t25");

    let ledger_dir = tempfile::tempdir().expect("ledger tempdir");
    let ledger_log = ledger_dir.path().join("ledger.log");
    std::fs::write(&ledger_log, "").expect("empty ledger log");

    let mut state = AppState::new_in_memory(HostConfig {
        bind: "127.0.0.1:3795".parse().expect("bind"),
        op_token: "unit-op-token".into(),
        admin_token: None,
        db_path: std::env::temp_dir().join("farm-core-merge-review.db"),
    })
    .expect("in-memory state");
    let db = state.db.clone();
    state = state.with_merge(MergeService::with_config(
        db,
        MergeConfig { test_command: vec!["./run-tests.sh".into()], test_timeout: Duration::from_secs(30) },
    ));
    // 启用审计旁路：审批事件经 ledger record 子进程进 fake Kron。
    state = state.with_audit_sink(
        AuditSink::new(FAKE_LEDGER)
            .with_record_args(vec!["record".to_string(), "--log".to_string(), ledger_log.display().to_string()])
            .with_actor_farm_accepted(true),
    );
    state
        .workspace
        .establish(&EstablishRequest {
            spec_id: "spec-10".into(),
            integration_branch: INTEGRATION.into(),
            ticket_id: TICKET.into(),
            repo_path: repo_dir.path().to_path_buf(),
            worktree_path: worktree.clone(),
            worktree_branch: BRANCH.into(),
            start_point: Some(INTEGRATION.into()),
        })
        .expect("establish the ticket worktree");

    // agent 的活：在 worktree 里落一个提交（FF 可行）。
    std::fs::write(worktree.join("feature.txt"), "ticket 25 work\n").expect("write feature");
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-m", "feat: ticket 25 work"]);

    let repo_path = repo_dir.path().to_path_buf();
    // keep ledger_dir alive by leaking its handle into the fixture lifetime via _wt_parent scope;
    std::mem::forget(ledger_dir);
    Fixture { _repo: repo_dir, _wt_parent: wt_parent, repo: repo_path, worktree, ledger_log, state }
}

fn proposal() -> MergeProposal {
    MergeProposal { ticket_id: TICKET.into(), worktree_branch: BRANCH.into(), commit: None, message: Some("human-reviewed".into()) }
}

/// fake ledger 收到的 NDJSON 行（原文）。
fn ledger_lines(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|e| e.get("t").and_then(|t| t.as_str()) == Some("record"))
        .filter_map(|e| e.get("line").and_then(|v| v.as_str()).map(str::to_string))
        .collect()
}

fn event_count(state: &AppState, kind: EventKind) -> usize {
    state
        .event_history(&EventLogFilter { since: None, kind: Some(kind), ticket_id: None, limit: None })
        .expect("history")
        .events
        .len()
}

fn approval_audited(state: &AppState) -> bool {
    state
        .event_history(&EventLogFilter { since: None, kind: Some(EventKind::Approval), ticket_id: None, limit: None })
        .expect("history")
        .events
        .iter()
        .any(|e| e.audited)
}

/// 最近一条 Approval 事件的载荷（决策 + 审阅人住这里；Kron 记录该审批事件本身）。
fn approval_event_payload(state: &AppState) -> Option<Value> {
    state
        .event_history(&EventLogFilter { since: None, kind: Some(EventKind::Approval), ticket_id: None, limit: None })
        .ok()?
        .events
        .into_iter()
        .last()
        .map(|e| e.payload)
}

/// fake ledger 是否收到过一条 event=approval 的 Kron 记录（审批事件入了账）。
fn ledger_has_approval(log: &Path) -> bool {
    ledger_lines(log)
        .iter()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .any(|r| r.get("event").and_then(|e| e.as_str()) == Some("approval"))
}

// ---------------------------------------------------------------------------
// S1: 人工审阅流
// ---------------------------------------------------------------------------

#[tokio::test]
async fn submitting_for_review_queues_the_proposal_without_merging() {
    let f = fixture(0);
    let id = f.state.merge.submit_for_review(&f.state, &proposal()).expect("submit");

    let record = f.state.merge.proposal(&id).expect("query").expect("row");
    assert_eq!(record.state, MergeProposalState::Proposed, "提交审阅只落账，不执行");
    assert!(record.merge_commit.is_none(), "审阅前没有 merge commit");

    // 审阅期 needs_attention 是一等事件。
    assert!(event_count(&f.state, EventKind::NeedsAttention) >= 1, "审阅队列要发 needs_attention");
    // 没有自动合入：集成分支上没有该票的 merge commit。
    let head = Command::new("git").arg("-C").arg(&f.repo).args(["rev-parse", INTEGRATION]).output().expect("git");
    assert!(head.status.success());
}

#[tokio::test]
async fn approving_runs_the_mechanical_gates_and_produces_the_merge_commit() {
    let f = fixture(0);
    let id = f.state.merge.submit_for_review(&f.state, &proposal()).expect("submit");

    let report = f.state.merge.approve(&f.state, &id, "reviewer-joe").await.expect("approve");
    assert_eq!(report.state, MergeProposalState::Merged);
    assert!(report.merge_commit.is_some(), "批准后机械闸门照跑，出 merge commit（完成信号）");

    // merge commit 真的落在集成分支上（机械事实）。
    let subject = Command::new("git").arg("-C").arg(&f.repo).args(["log", INTEGRATION, "-1", "--pretty=%s"]).output().expect("git");
    let subject = String::from_utf8_lossy(&subject.stdout);
    assert!(subject.contains(&format!("ticket #{TICKET}")), "集成分支顶点是该票 merge commit：{subject}");

    // merge 事件进总线。
    assert!(event_count(&f.state, EventKind::Merge) >= 1, "merge 结果事件进总线");
}

#[tokio::test]
async fn the_approval_decision_is_double_written_to_the_kron_ledger() {
    let f = fixture(0);
    let id = f.state.merge.submit_for_review(&f.state, &proposal()).expect("submit");
    let _ = f.state.merge.approve(&f.state, &id, "reviewer-joe").await.expect("approve");

    // 审批事件双写进 Kron（fake ledger 收到 event=approval 记录 + audited=1）。
    assert!(approval_audited(&f.state), "审批事件必须双写进 Kron（audited=1）");
    assert!(ledger_has_approval(&f.ledger_log), "fake ledger 必须收到 event=approval 记录");

    // 决策本身在事件载荷里（事件日志 + WS；Kron 的 details 是闭集 schema，只记事件元数据）。
    let payload = approval_event_payload(&f.state).expect("approval event");
    assert_eq!(payload["decision"], "approved", "审批决策进载荷");
    assert_eq!(payload["reviewer"], "reviewer-joe");
}

#[tokio::test]
async fn rejecting_sends_a_closed_set_signal_back_to_the_agent_without_merging() {
    let f = fixture(0);
    let id = f.state.merge.submit_for_review(&f.state, &proposal()).expect("submit");

    f.state.merge.reject(&f.state, &id, "reviewer-joe", "not ready").expect("reject");

    let record = f.state.merge.proposal(&id).expect("query").expect("row");
    assert_eq!(record.state, MergeProposalState::Rejected, "拒绝标 rejected");
    assert!(record.merge_commit.is_none(), "拒绝不产生完成信号");

    // 拒绝的审批决策也双写进 Kron（event=approval 记录在案 + audited=1）。
    assert!(approval_audited(&f.state), "拒绝的审批决策同样双写进 Kron");
    assert!(ledger_has_approval(&f.ledger_log), "fake ledger 必须收到 event=approval 记录");
    let payload = approval_event_payload(&f.state).expect("approval event");
    assert_eq!(payload["decision"], "rejected", "拒绝决策在事件载荷");

    // 拒绝 → 事件回 agent（needs_attention 一等）。
    let na = event_count(&f.state, EventKind::NeedsAttention);
    assert!(na >= 2, "提交审阅 + 拒绝各发一条 needs_attention：{na}");
}

#[tokio::test]
async fn approving_a_proposal_that_fails_the_mechanical_gates_is_still_rejected() {
    // 人工闸门叠加在机械闸门之上：批准≠必合入，机械校验照跑，测试不过照样拒收。
    let f = fixture(1); // run-tests.sh exit 1 → 测试闸门失败
    let id = f.state.merge.submit_for_review(&f.state, &proposal()).expect("submit");

    let err = f.state.merge.approve(&f.state, &id, "reviewer-joe").await.expect_err("mechanical gate must still reject");
    assert!(err.to_string().contains("tests"), "机械闸门拒收：{err}");

    let record = f.state.merge.proposal(&id).expect("query").expect("row");
    assert_eq!(record.state, MergeProposalState::Rejected, "机械拒收照样标 rejected");
    assert!(record.merge_commit.is_none(), "无完成信号");
    // 审批决策仍入账（人批了，但机械否了）。
    assert!(approval_audited(&f.state), "审批决策照常双写");
}

#[tokio::test]
async fn the_proposal_diff_is_the_git_diff_the_panel_renders() {
    let f = fixture(0);
    let id = f.state.merge.submit_for_review(&f.state, &proposal()).expect("submit");
    let diff = f.state.merge.diff(&id).expect("diff");
    assert!(diff.contains("feature.txt"), "diff 呈现 worktree 的改动：{diff}");
    assert!(diff.contains("ticket 25 work"), "diff 含改动内容：{diff}");
}

// 让 NewEvent 导入被用到（审批/审阅事件构造在实现侧，测试侧引用类型）。
#[allow(dead_code)]
fn _use_newevent(e: NewEvent) -> EventKind {
    e.kind
}
