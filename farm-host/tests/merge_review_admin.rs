//! Merge 审阅流的宿主管理面接缝测试（ticket #25，GLOSSARY「宿主管理 API」）。
//!
//! 全部经 axum `oneshot` 打**真实 router**。核心不变量：人工闸门经**管理 API**（管理
//! token），与编排器契约边界分明（ADR-0002 §1）；批准跑机械闸门出 merge commit，拒绝
//! 闭集错误回 agent；审阅期 needs_attention 一等；契约触达不到管理面操作。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{header, Method, Request, StatusCode};
use farm_core::merge::{MergeConfig, MergeService};
use farm_core::workspace::EstablishRequest;
use farm_core::{AppState, HostConfig};
use farm_host::build_router;
use farm_sdk::services::merge::{
    MergeProposal, MergeProposalDetail, MergeProposalListResponse, MergeReviewResponse,
    MergeProposalState,
};
use serde_json::Value;
use tower::ServiceExt;

const TICKET: &str = "25";
const INTEGRATION: &str = "implement/spec-10";
const BRANCH: &str = "farm/t25";
const OP_TOKEN: &str = "op-secret";
const ADMIN_TOKEN: &str = "admin-secret";

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git").arg("-C").arg(cwd).args(args).output().expect("spawn git");
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
}

fn write_test_script(repo: &Path, exit: i32) {
    let path = repo.join("run-tests.sh");
    std::fs::write(&path, format!("#!/bin/sh\nexit {exit}\n")).expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
}

struct Fixture {
    router: axum::Router,
    _repo: tempfile::TempDir,
    _wt_parent: tempfile::TempDir,
    repo: PathBuf,
}

fn fixture(tests_exit: i32) -> Fixture {
    let repo_dir = tempfile::tempdir().expect("tempdir");
    git(repo_dir.path(), &["init", "--initial-branch=main"]);
    git(repo_dir.path(), &["config", "user.email", "test@farm.local"]);
    git(repo_dir.path(), &["config", "user.name", "Farm Test"]);
    std::fs::write(repo_dir.path().join("README.md"), "scratch\n").expect("write");
    write_test_script(repo_dir.path(), tests_exit);
    git(repo_dir.path(), &["add", "."]);
    git(repo_dir.path(), &["commit", "-m", "init"]);
    git(repo_dir.path(), &["branch", INTEGRATION]);

    let wt_parent = tempfile::tempdir().expect("wt parent");
    let worktree = wt_parent.path().join("t25");

    let mut state = AppState::new_in_memory(HostConfig {
        bind: "127.0.0.1:3796".parse().expect("bind"),
        op_token: OP_TOKEN.into(),
        admin_token: Some(ADMIN_TOKEN.into()),
        db_path: std::env::temp_dir().join("farm-host-merge-review.db"),
    })
    .expect("state");
    let db = state.db.clone();
    state = state.with_merge(MergeService::with_config(
        db,
        MergeConfig { test_command: vec!["./run-tests.sh".into()], test_timeout: Duration::from_secs(30) },
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
        .expect("establish");
    std::fs::write(worktree.join("feature.txt"), "ticket 25 work\n").expect("write");
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-m", "feat: ticket 25 work"]);

    let repo = repo_dir.path().to_path_buf();
    let router = build_router(state);
    Fixture { router, _repo: repo_dir, _wt_parent: wt_parent, repo }
}

fn req(method: Method, uri: &str, op: Option<&str>, admin: Option<&str>, body: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(t) = op { b = b.header("x-farm-token", t); }
    if let Some(t) = admin { b = b.header("x-farm-admin-token", t); }
    if let Some(body) = body { b = b.header(header::CONTENT_TYPE, "application/json"); }
    b.body(Body::from(body.unwrap_or("").to_string())).expect("request")
}

async fn json<T: serde::de::DeserializeOwned>(resp: axum::http::Response<Body>) -> (StatusCode, T) {
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("body");
    (status, serde_json::from_slice(&bytes).expect("json"))
}

/// agent 经契约路由提交审阅 → proposal_id。
async fn submit_for_review(router: &axum::Router) -> String {
    let proposal = MergeProposal { ticket_id: TICKET.into(), worktree_branch: BRANCH.into(), commit: None, message: Some("review me".into()) };
    let body = serde_json::to_string(&proposal).expect("ser");
    let resp = router.clone().oneshot(req(Method::POST, "/v1/merge/reviews", Some(OP_TOKEN), None, Some(&body))).await.expect("oneshot");
    let (status, out): (_, Value) = json(resp).await;
    assert_eq!(status, StatusCode::OK, "agent 提交审阅应 200");
    out["proposal_id"].as_str().expect("proposal_id").to_string()
}

#[tokio::test]
async fn the_review_queue_shows_the_diff_and_approval_produces_the_merge_commit() {
    let f = fixture(0);
    let id = submit_for_review(&f.router).await;

    // 审阅队列（管理 token）呈现 diff。
    let resp = f.router.clone().oneshot(req(Method::GET, "/admin/merge/proposals", None, Some(ADMIN_TOKEN), None)).await.expect("oneshot");
    let (status, queue): (_, MergeProposalListResponse) = json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(queue.proposals.len(), 1, "队列里有一条待审提议");
    assert_eq!(queue.proposals[0].state, MergeProposalState::Proposed);
    assert!(queue.proposals[0].diff.contains("feature.txt"), "队列呈现 diff");

    // 单个详情 + diff。
    let resp = f.router.clone().oneshot(req(Method::GET, &format!("/admin/merge/proposals/{id}"), None, Some(ADMIN_TOKEN), None)).await.expect("oneshot");
    let (status, detail): (_, MergeProposalDetail) = json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert!(detail.diff.contains("ticket 25 work"), "详情呈现 diff 内容");

    // 批准（管理 token）→ 机械闸门照跑 → merge commit。
    let resp = f.router.clone().oneshot(req(Method::POST, &format!("/admin/merge/proposals/{id}/approve"), None, Some(ADMIN_TOKEN), Some(r#"{"reviewer":"joe"}"#))).await.expect("oneshot");
    let (status, out): (_, MergeReviewResponse) = json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out.state, MergeProposalState::Merged, "批准 → 合入");
    assert!(out.merge_commit.is_some(), "完成信号 = merge commit");

    // 集成分支上真的有该票 merge commit（机械事实）。
    let subject = Command::new("git").arg("-C").arg(&f.repo).args(["log", INTEGRATION, "-1", "--pretty=%s"]).output().expect("git");
    assert!(String::from_utf8_lossy(&subject.stdout).contains(&format!("ticket #{TICKET}")));
}

#[tokio::test]
async fn rejection_sends_a_closed_set_signal_and_merges_nothing() {
    let f = fixture(0);
    let id = submit_for_review(&f.router).await;

    let resp = f.router.clone().oneshot(req(Method::POST, &format!("/admin/merge/proposals/{id}/reject"), None, Some(ADMIN_TOKEN), Some(r#"{"reviewer":"joe","reason":"needs more tests"}"#))).await.expect("oneshot");
    let (status, out): (_, MergeReviewResponse) = json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out.state, MergeProposalState::Rejected, "拒绝 → rejected");
    assert!(out.merge_commit.is_none(), "拒绝无完成信号");

    // 集成分支没有该票的 merge commit。
    let log = Command::new("git").arg("-C").arg(&f.repo).args(["log", INTEGRATION, "--pretty=%s"]).output().expect("git");
    assert!(!String::from_utf8_lossy(&log.stdout).contains(&format!("ticket #{TICKET}")), "拒绝不产生 merge commit");
}

#[tokio::test]
async fn the_admin_face_requires_the_admin_token_and_the_contract_cannot_reach_it() {
    let f = fixture(0);
    let id = submit_for_review(&f.router).await;

    // 无 token → unauthorized（管理面闭集码：缺管理 token = admin_unauthorized）。
    let resp = f.router.clone().oneshot(req(Method::GET, "/admin/merge/proposals", None, None, None)).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "管理面无 token → unauthorized");

    // op token 递到管理面 → forbidden（管理 token ≠ 操作 token）。
    let resp = f.router.clone().oneshot(req(Method::GET, "/admin/merge/proposals", Some(OP_TOKEN), None, None)).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "op token 不能触达管理面");

    // 契约面只认 op token：管理 token 递到契约 → unauthorized（不触达管理操作）。
    let resp = f.router.clone().oneshot(req(Method::POST, "/v1/merge/reviews", None, Some(ADMIN_TOKEN), Some("{}"))).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "管理 token 不能当 op token 用");

    // approve 走管理 token 才通（证明操作确实经管理 API）。
    let resp = f.router.clone().oneshot(req(Method::POST, &format!("/admin/merge/proposals/{id}/approve"), None, Some(ADMIN_TOKEN), Some(r#"{"reviewer":"joe"}"#))).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK, "管理 token 下批准通过");
}
