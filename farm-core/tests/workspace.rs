//! Workspace 服务（ticket #14）公开接缝测试。
//!
//! GLOSSARY：Workspace 服务拥有 spec ↔ 集成分支 ↔ worktree 映射并负责 worktree 的
//! 创建与清理；映射持久化在宿主私有存储（ADR-0005 §9）。测试只用公开 API
//! （`WorkspaceService`），不触及内部 SQLite 形状。worktree 的 git 操作对临时
//! scratch 仓库跑真实 `git` CLI（ADR：Farm 不用 libgit2）。

use std::path::PathBuf;
use std::process::Command;

use farm_core::db::Database;
use farm_core::workspace::{EstablishRequest, MappingStatus, WorkspaceService};
use farm_sdk::services::workspace::WorktreeState;

/// 内存库上的服务（不落盘；同 `AppState::new_in_memory` 用的存储）。
fn service() -> (tempfile::TempDir, WorkspaceService) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Database::open_in_memory().expect("in-memory store");
    // 内存库也要跑 migration（与 AppState 一致）：workspace 表由 v4 建。
    db.migrate(&farm_core::migrations::Migrator::new(
        farm_core::migrations::MIGRATIONS,
    ))
    .expect("migrate");
    (dir, WorkspaceService::new(db))
}

fn establish_request(ticket: &str, branch: &str) -> EstablishRequest {
    EstablishRequest {
        spec_id: "spec-10".into(),
        integration_branch: "implement/spec-10".into(),
        ticket_id: ticket.into(),
        repo_path: PathBuf::from("/nonexistent-repo"),
        worktree_path: PathBuf::from(format!("/tmp/farm-wt/{ticket}")),
        worktree_branch: branch.into(),
        start_point: None,
    }
}

/// 建一个带初始 commit 的 scratch git 仓库（`git worktree add` 需要有 HEAD）。
fn scratch_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    git(dir.path(), &["init", "--initial-branch=main"]);
    git(dir.path(), &["config", "user.email", "test@farm.local"]);
    git(dir.path(), &["config", "user.name", "Farm Test"]);
    std::fs::write(dir.path().join("README.md"), "scratch\n").expect("write readme");
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-m", "init"]);
    dir
}

fn git(cwd: &std::path::Path, args: &[&str]) {
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
}

// ---------------------------------------------------------------------------
// S1 — write-ahead 持久化 + 读视图
// ---------------------------------------------------------------------------

#[test]
fn reserve_is_a_write_ahead_pending_mapping_that_persists() {
    let (_dir, svc) = service();
    let row = svc
        .reserve(&establish_request("14", "farm/t14"))
        .expect("reserve writes the mapping first");

    assert_eq!(row.ticket_id, "14");
    assert_eq!(row.spec_id, "spec-10");
    assert_eq!(row.integration_branch, "implement/spec-10");
    assert_eq!(row.status, MappingStatus::Pending, "write-ahead：先落 pending 行");
    assert!(!row.created_at.is_empty());

    // 持久化可查：list / status_of 都能看到这条 pending 映射。
    let all = svc.list().expect("list");
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].ticket_id, "14");
    assert_eq!(svc.status_of("14").expect("status_of"), Some(MappingStatus::Pending));
}

#[test]
fn the_mapping_view_projects_internal_status_to_the_contract_worktree_state() {
    let (_dir, svc) = service();
    svc.reserve(&establish_request("14", "farm/t14")).expect("reserve");
    svc.reserve(&establish_request("15", "farm/t15")).expect("reserve");

    let view = svc.mapping_view().expect("mapping view");
    assert_eq!(view.len(), 1, "同一 spec + 集成分支归组为一个映射");
    let mapping = &view[0];
    assert_eq!(mapping.spec_id, "spec-10");
    assert_eq!(mapping.integration_branch, "implement/spec-10");
    assert_eq!(mapping.worktrees.len(), 2);

    // pending → Creating（契约态）；worktree 引用带票号/路径/分支。
    for wt in &mapping.worktrees {
        assert_eq!(wt.state, WorktreeState::Creating, "pending 投影为 Creating");
    }
    let t14 = mapping
        .worktrees
        .iter()
        .find(|w| w.ticket_id == "14")
        .expect("t14 worktree ref");
    assert_eq!(t14.branch, "farm/t14");
    assert_eq!(t14.path, "/tmp/farm-wt/14");
}

// ---------------------------------------------------------------------------
// S2 — 映射创建 → worktree 建立 → 清理 全链路（对 scratch git 仓库跑真实 git CLI）
// ---------------------------------------------------------------------------

#[test]
fn establish_builds_the_worktree_and_cleanup_removes_it_the_full_chain() {
    let (_dir, svc) = service();
    let repo = scratch_repo();
    let wt_parent = tempfile::tempdir().expect("worktree parent dir");
    let worktree_path = wt_parent.path().join("t14");
    let req = EstablishRequest {
        spec_id: "spec-10".into(),
        integration_branch: "implement/spec-10".into(),
        ticket_id: "14".into(),
        repo_path: repo.path().to_path_buf(),
        worktree_path: worktree_path.clone(),
        worktree_branch: "farm/t14".into(),
        start_point: Some("HEAD".into()),
    };

    let row = svc.establish(&req).expect("establish: reserve + git worktree add");
    assert_eq!(row.status, MappingStatus::Built, "worktree 建成后转 built");
    assert!(worktree_path.join(".git").exists(), "linked worktree 带 .git 文件");
    assert!(worktree_path.join("README.md").exists(), "起点内容在工作树里");
    assert_eq!(svc.status_of("14").expect("status"), Some(MappingStatus::Built));

    svc.cleanup("14").expect("cleanup removes worktree + row");
    assert!(!worktree_path.exists(), "清理后 worktree 目录没了");
    assert_eq!(svc.find("14").expect("find"), None, "清理后映射行删掉");
    assert!(svc.list().expect("list").is_empty());
}
