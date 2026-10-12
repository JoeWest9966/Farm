//! Merge 服务（ticket #18）公开接缝测试。
//!
//! GLOSSARY「Merge 服务」：agent 只能**提议**合并；宿主机械校验（worktree 干净、
//! FF 可行、测试过）后执行**真正的 merge**。**完成信号 = 集成分支上出现该票的
//! merge commit**，是宿主可验证的机械事实，永不采信 agent 自述。
//!
//! 测试只用公开 API（`AppState` + `MergeService`），不触及内部 SQLite 形状。
//! worktree 的 git 操作对临时 scratch 仓库跑**真实 git CLI**（ADR：Farm 不用 libgit2）；
//! 测试命令也真实起子进程（`./run-tests.sh`），只是脚本内容由夹具决定过不过。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use farm_core::event_log::EventLogFilter;
use farm_core::merge::{MergeConfig, MergeService};
use farm_core::workspace::EstablishRequest;
use farm_core::{AppState, HostConfig};
use farm_sdk::events::{EventEnvelope, EventKind};
use farm_sdk::services::merge::{MergeProposal, MergeProposalState};
use farm_sdk::ContractError;

const TICKET: &str = "18";
const INTEGRATION: &str = "implement/spec-10";
const BRANCH: &str = "farm/t18";

// ---------------------------------------------------------------------------
// 夹具：一个真 scratch 仓库 + 一条已建立的 ticket worktree（映射 built）
// ---------------------------------------------------------------------------

/// 跑一条 git 子命令并回传 stdout（非零退出即 panic：夹具自己要先立住）。
fn git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// git 子命令的退出码（不 panic；用于「这事失败了」的负向断言）。
fn git_status(cwd: &Path, args: &[&str]) -> Option<i32> {
    Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .expect("spawn git")
        .status
        .code()
}

struct Fixture {
    _repo: tempfile::TempDir,
    _wt_parent: tempfile::TempDir,
    repo: PathBuf,
    worktree: PathBuf,
    state: AppState,
}

/// 建夹具：scratch 仓库（main + 集成分支 + 可执行的 `./run-tests.sh`）、
/// 经 Workspace 服务 write-ahead 建立 ticket worktree、再模拟 agent 在 worktree 里
/// 提交一个改动。`tests_exit` 决定 `run-tests.sh` 的退出码（0 = 测试过）。
fn fixture(tests_exit: i32) -> Fixture {
    let repo_dir = tempfile::tempdir().expect("tempdir");
    git(repo_dir.path(), &["init", "--initial-branch=main"]);
    git(repo_dir.path(), &["config", "user.email", "test@farm.local"]);
    git(repo_dir.path(), &["config", "user.name", "Farm Test"]);
    std::fs::write(repo_dir.path().join("README.md"), "scratch\n").expect("write readme");
    write_test_script(repo_dir.path(), tests_exit);
    git(repo_dir.path(), &["add", "."]);
    git(repo_dir.path(), &["commit", "-m", "init"]);
    // 集成分支：spec 的集成分支先落在初始提交上。
    git(repo_dir.path(), &["branch", INTEGRATION]);

    let wt_parent = tempfile::tempdir().expect("worktree parent");
    let worktree = wt_parent.path().join("t18");

    let mut state = AppState::new_in_memory(HostConfig {
        bind: "127.0.0.1:3721".parse().expect("bind"),
        op_token: "unit-op-token".into(),
        admin_token: None,
        db_path: std::env::temp_dir().join("farm-core-merge-unit.db"),
    })
    .expect("in-memory state");
    // 测试命令走 worktree 里的脚本（真子进程 + 真超时），不跑 cargo test。
    let db = state.db.clone();
    state = state.with_merge(MergeService::with_config(
        db,
        MergeConfig {
            test_command: vec!["./run-tests.sh".into()],
            test_timeout: Duration::from_secs(30),
        },
    ));
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

    // agent 的活：在 worktree 里落一个提交（分支因此领先集成分支 → FF 可行）。
    std::fs::write(worktree.join("feature.txt"), "ticket 18 work\n").expect("write feature");
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-m", "feat: ticket 18 work"]);

    let repo_path = repo_dir.path().to_path_buf();
    Fixture {
        _repo: repo_dir,
        _wt_parent: wt_parent,
        repo: repo_path,
        worktree,
        state,
    }
}

fn write_test_script(dir: &Path, exit: i32) {
    let path = dir.join("run-tests.sh");
    std::fs::write(&path, format!("#!/bin/sh\nexit {exit}\n")).expect("write test script");
    set_executable(&path);
}

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).expect("stat").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).expect("chmod");
}

fn proposal(commit: Option<&str>, message: Option<&str>) -> MergeProposal {
    MergeProposal {
        ticket_id: TICKET.into(),
        worktree_branch: BRANCH.into(),
        commit: commit.map(str::to_string),
        message: message.map(str::to_string),
    }
}

/// 事件历史里的 `merge` 事件（完成信号的事件面）。
fn merge_events(state: &AppState) -> Vec<EventEnvelope> {
    state
        .event_history(&EventLogFilter::default())
        .expect("event history")
        .events
        .into_iter()
        .filter(|event| event.kind == EventKind::Merge)
        .collect()
}

// ---------------------------------------------------------------------------
// S1 — 干净 worktree + FF 可行 + 测试过 → merge commit 产生（完成信号）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_clean_worktree_with_a_feasible_fast_forward_and_passing_tests_produces_the_tickets_merge_commit()
{
    let fx = fixture(0);
    let integration_before = git(&fx.repo, &["rev-parse", INTEGRATION]);

    let report = fx
        .state
        .merge
        .propose(&fx.state, &proposal(None, Some("agent-runner local backend")))
        .await
        .expect("the host must execute the real merge");

    // 提议进入 merged 态，并带回 merge commit。
    assert_eq!(report.state, MergeProposalState::Merged);
    let merge_commit = report.merge_commit.expect("merge commit sha");

    // 完成信号 = 集成分支上出现该票的 merge commit（机械事实，可 rev-parse）。
    assert_eq!(git(&fx.repo, &["rev-parse", INTEGRATION]), merge_commit);
    // 它是一个真 merge commit：父1 = 原集成分支顶点，父2 = ticket 分支顶点。
    assert_eq!(
        git(&fx.repo, &["rev-parse", &format!("{merge_commit}^1")]),
        integration_before
    );
    assert_eq!(
        git(&fx.repo, &["rev-parse", &format!("{merge_commit}^2")]),
        git(&fx.repo, &["rev-parse", BRANCH])
    );
    // 消息点名票号（与仓库里既有的 merge commit 惯例一致）。
    let subject = git(&fx.repo, &["log", "-1", "--format=%s", INTEGRATION]);
    assert!(subject.contains("ticket #18"), "{subject}");

    // 提议落库（#25 审阅队列的读源）：状态 + merge commit 都可回读。
    let stored = fx
        .state
        .merge
        .proposal(&report.proposal_id)
        .expect("query the stored proposal")
        .expect("the proposal is stored");
    assert_eq!(stored.ticket_id, TICKET);
    assert_eq!(stored.state, MergeProposalState::Merged);
    assert_eq!(stored.merge_commit.as_deref(), Some(merge_commit.as_str()));

    // merge 结果事件进总线（持久路径：也在事件日志里，可被 #26 断言）。
    let events = merge_events(&fx.state);
    assert_eq!(events.len(), 1, "一次 merge 一条 merge 事件");
    assert_eq!(events[0].ticket_id.as_deref(), Some(TICKET));
    assert_eq!(events[0].payload["merge_commit"], serde_json::json!(merge_commit));
    assert_eq!(events[0].payload["completion_signal"], "merge_commit");
    assert_eq!(events[0].payload["integration_branch"], INTEGRATION);
}

#[tokio::test]
async fn a_stored_proposal_is_queryable_for_the_review_queue() {
    let fx = fixture(0);
    let report = fx
        .state
        .merge
        .propose(&fx.state, &proposal(None, None))
        .await
        .expect("merge");

    // #25 审阅队列的读面：按票查、全量查。
    let for_ticket = fx
        .state
        .merge
        .proposals_for_ticket(TICKET)
        .expect("proposals for ticket");
    assert_eq!(for_ticket.len(), 1);
    assert_eq!(for_ticket[0].proposal_id, report.proposal_id);
    assert_eq!(fx.state.merge.proposals().expect("all proposals").len(), 1);
}

// ---------------------------------------------------------------------------
// code review P2-9：测试闸门超时必须把测试进程收掉，不是留它在机器上跑
// ---------------------------------------------------------------------------

/// 一个**永不结束**的测试命令：把自己的 pid 写进 worktree，然后空转（不起子进程，
/// 所以「pid 死没死」能干净地判定 `kill_on_drop` 有没有生效）。
fn write_hanging_test_script(dir: &Path) {
    let path = dir.join("run-tests.sh");
    std::fs::write(
        &path,
        "#!/bin/sh\necho $$ > hang.pid\ni=0\nwhile [ $i -lt 100000000 ]; do i=$((i+1)); done\n",
    )
    .expect("write hanging test script");
    set_executable(&path);
}

/// 进程是否还活着（Linux `/proc`；测试专用探针）。
///
/// 僵尸（`Z`）算「已经死了，只差收尸」——`kill_on_drop` 发的是 SIGKILL，被杀的进程
/// 在被 wait 之前一直是僵尸，`/proc/<pid>` 因此仍然存在。所以按 state 字段判，不按
/// 目录存在判。
fn pid_alive(pid: u32) -> bool {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    let Some(state) = stat.rsplit(')').next().and_then(|rest| rest.split_whitespace().next()) else {
        return false; // /proc 项没了 = 进程没了
    };
    state != "Z"
}

/// 轮询等到进程死透（`timeout` 内没死透 → `false`）。
fn wait_until_dead(pid: u32, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while pid_alive(pid) {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

#[tokio::test]
async fn a_test_gate_timeout_kills_the_test_process_instead_of_leaking_it() {
    let repo_dir = tempfile::tempdir().expect("tempdir");
    git(repo_dir.path(), &["init", "--initial-branch=main"]);
    git(repo_dir.path(), &["config", "user.email", "test@farm.local"]);
    git(repo_dir.path(), &["config", "user.name", "Farm Test"]);
    std::fs::write(repo_dir.path().join("README.md"), "scratch\n").expect("write readme");
    write_hanging_test_script(repo_dir.path());
    git(repo_dir.path(), &["add", "."]);
    git(repo_dir.path(), &["commit", "-m", "init"]);
    git(repo_dir.path(), &["branch", INTEGRATION]);

    let wt_parent = tempfile::tempdir().expect("worktree parent");
    let worktree = wt_parent.path().join("t18");

    let mut state = AppState::new_in_memory(HostConfig {
        bind: "127.0.0.1:3721".parse().expect("bind"),
        op_token: "unit-op-token".into(),
        admin_token: None,
        db_path: std::env::temp_dir().join("farm-core-merge-unit-timeout.db"),
    })
    .expect("in-memory state");
    // 闸门死线压到 300ms：足够让「超时即拒收」发生，又不拖慢测试。
    let db = state.db.clone();
    state = state.with_merge(MergeService::with_config(
        db,
        MergeConfig {
            test_command: vec!["./run-tests.sh".into()],
            test_timeout: Duration::from_millis(300),
        },
    ));
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
    std::fs::write(worktree.join("feature.txt"), "ticket 18 work\n").expect("write feature");
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-m", "feat: ticket 18 work"]);

    let err = state
        .merge
        .propose(&state, &proposal(None, None))
        .await
        .expect_err("测试闸门超时必须是拒收理由");
    assert!(
        err.to_string().contains("did not finish within"),
        "拒收理由要点名测试超时：{err}"
    );
    // 提议不得变成 merged（无完成信号）。
    let stored = state
        .merge
        .proposals_for_ticket(TICKET)
        .expect("proposals")
        .into_iter()
        .next()
        .expect("the proposal is stored");
    assert_eq!(stored.state, MergeProposalState::Rejected);
    assert!(stored.merge_commit.is_none(), "超时不产生完成信号");

    // 关键断言：超时的测试进程必须被收掉（`kill_on_drop`），不得留在机器上继续跑。
    let pid_file = worktree.join("hang.pid");
    let pid = std::fs::read_to_string(&pid_file)
        .expect("the hanging test script must have recorded its pid")
        .trim()
        .parse::<u32>()
        .expect("pid");
    assert!(
        wait_until_dead(pid, Duration::from_secs(2)),
        "测试闸门超时后测试进程不得残留（kill_on_drop）：pid {pid} 还活着"
    );
}
