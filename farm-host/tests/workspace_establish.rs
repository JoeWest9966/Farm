//! Workspace 建立 worktree 的契约面接缝测试（ticket #26 加法接通）。
//!
//! 编排器以契约客户端身份触发 workspace 建 worktree：`POST /v1/workspace/worktrees`
//! （write-ahead 映射 + 真 `git worktree add`）→ 回 `WorktreeRef`（`ready`）；
//! 只读投影 `GET /v1/workspace/mappings` 不变。worktree 的 git 操作对临时 scratch
//! 仓库跑**真实 git CLI**（ADR：Farm 不用 libgit2），不 mock 内部协作者。

use std::path::{Path, PathBuf};
use std::process::Command;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use farm_host::build_router;
use farm_sdk::services::workspace::{MappingListResponse, WorktreeEstablishRequest, WorktreeState};
use tower::ServiceExt;

mod common;
use common::{state, OP_TOKEN};

const TICKET: &str = "26";
const SPEC: &str = "spec-10";
const INTEGRATION: &str = "implement/spec-10";
const BRANCH: &str = "farm/t26";

fn git(cwd: &Path, args: &[&str]) {
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

/// scratch 仓库：main + 集成分支（worktree 从集成分支新建）。
fn scratch_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    git(dir.path(), &["init", "--initial-branch=main"]);
    git(dir.path(), &["config", "user.email", "test@farm.local"]);
    git(dir.path(), &["config", "user.name", "Farm Test"]);
    std::fs::write(dir.path().join("README.md"), "scratch\n").expect("write");
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-m", "init"]);
    git(dir.path(), &["branch", INTEGRATION]);
    dir
}

fn json_request(uri: &str, token: Option<&str>, body: &str) -> Request<Body> {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        builder = builder.header("x-farm-token", token);
    }
    builder.body(Body::from(body.to_string())).expect("request")
}

fn get_request(uri: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method(Method::GET).uri(uri);
    if let Some(token) = token {
        builder = builder.header("x-farm-token", token);
    }
    builder.body(Body::empty()).expect("request")
}

async fn body_json<T: serde::de::DeserializeOwned>(response: axum::http::Response<Body>) -> T {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body bytes");
    serde_json::from_slice(&bytes).expect("json body")
}

#[tokio::test]
async fn a_contract_client_can_establish_a_worktree_and_it_projects_as_ready() {
    let repo = scratch_repo();
    let wt_parent = tempfile::tempdir().expect("worktree parent");
    let worktree = wt_parent.path().join("t26");
    let app: Router = build_router(state());

    let request = WorktreeEstablishRequest {
        spec_id: SPEC.into(),
        integration_branch: INTEGRATION.into(),
        ticket_id: TICKET.into(),
        repo_path: repo.path().to_string_lossy().into_owned(),
        worktree_path: worktree.to_string_lossy().into_owned(),
        worktree_branch: BRANCH.into(),
        start_point: Some(INTEGRATION.into()),
    };
    let response = app
        .clone()
        .oneshot(json_request(
            "/v1/workspace/worktrees",
            Some(OP_TOKEN),
            &serde_json::to_string(&request).expect("serialize"),
        ))
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::OK);
    let wt: farm_sdk::services::workspace::WorktreeRef = body_json(response).await;
    assert_eq!(wt.ticket_id, TICKET);
    assert_eq!(wt.branch, BRANCH);
    assert_eq!(wt.state, WorktreeState::Ready, "write-ahead + git worktree add → ready");
    // 真 worktree：目录 + .git 落盘。
    assert!(worktree.join(".git").exists(), "worktree 目录必须真的建起来");

    // 只读投影反映出这条映射（集成分支归组）。
    let response = app
        .oneshot(get_request("/v1/workspace/mappings", Some(OP_TOKEN)))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::OK);
    let mappings: MappingListResponse = body_json(response).await;
    assert_eq!(mappings.mappings.len(), 1);
    assert_eq!(mappings.mappings[0].integration_branch, INTEGRATION);
    assert_eq!(mappings.mappings[0].worktrees[0].ticket_id, TICKET);
    assert_eq!(mappings.mappings[0].worktrees[0].state, WorktreeState::Ready);
}

#[tokio::test]
async fn establishing_a_worktree_without_a_token_is_a_closed_set_unauthorized() {
    let repo = scratch_repo();
    let wt_parent = tempfile::tempdir().expect("worktree parent");
    let request = WorktreeEstablishRequest {
        spec_id: SPEC.into(),
        integration_branch: INTEGRATION.into(),
        ticket_id: TICKET.into(),
        repo_path: repo.path().to_string_lossy().into_owned(),
        worktree_path: wt_parent.path().join("t26").to_string_lossy().into_owned(),
        worktree_branch: BRANCH.into(),
        start_point: None,
    };
    let app: Router = build_router(state());
    let response = app
        .oneshot(json_request(
            "/v1/workspace/worktrees",
            None,
            &serde_json::to_string(&request).expect("serialize"),
        ))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_worktree_request_missing_a_required_field_is_a_closed_set_invalid_request() {
    let app: Router = build_router(state());
    // 缺 worktree_branch（deny_unknown_fields + 必填）→ invalid_request。
    let response = app
        .oneshot(json_request(
            "/v1/workspace/worktrees",
            Some(OP_TOKEN),
            r#"{"spec_id":"spec-10","integration_branch":"implement/spec-10","ticket_id":"26","repo_path":"/x","worktree_path":"/y"}"#,
        ))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let err: farm_sdk::ContractError = body_json(response).await;
    assert_eq!(err.code, farm_sdk::ErrorCode::InvalidRequest);
}

#[tokio::test]
async fn establishing_a_worktree_over_a_broken_repo_is_a_closed_set_internal() {
    // 源仓库路径不存在 → git worktree add 失败 → 宿主故障（闭集 internal）。
    let wt_parent = tempfile::tempdir().expect("worktree parent");
    let request = WorktreeEstablishRequest {
        spec_id: SPEC.into(),
        integration_branch: INTEGRATION.into(),
        ticket_id: TICKET.into(),
        repo_path: "/nonexistent/repo/for/ticket-26".into(),
        worktree_path: wt_parent.path().join("t26").to_string_lossy().into_owned(),
        worktree_branch: BRANCH.into(),
        start_point: None,
    };
    let app: Router = build_router(state());
    let response = app
        .oneshot(json_request(
            "/v1/workspace/worktrees",
            Some(OP_TOKEN),
            &serde_json::to_string(&request).expect("serialize"),
        ))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let err: farm_sdk::ContractError = body_json(response).await;
    assert_eq!(err.code, farm_sdk::ErrorCode::Internal);
}

    // 建立 worktree 时装载仓库的 workflow 配置（管理面可读回）。
    #[tokio::test]
    async fn establishing_a_worktree_loads_the_repos_workflow_config_for_the_admin_face() {
        use common::{admin_state, ADMIN_TOKEN};
        let repo = scratch_repo();
        std::fs::create_dir_all(repo.path().join(".farm")).expect("mkdir .farm");
        std::fs::write(
            repo.path().join(".farm/workflow.json"),
            r#"{"dispatch_labels":["dispatch"],"max_concurrency":3,"stall_threshold_secs":60,"prompt_template":"work {{ticket_id}}","projection":{"close_issue":true}}"#,
        )
        .expect("write workflow.json");
        let wt_parent = tempfile::tempdir().expect("worktree parent");
        let request = WorktreeEstablishRequest {
            spec_id: SPEC.into(),
            integration_branch: INTEGRATION.into(),
            ticket_id: TICKET.into(),
            repo_path: repo.path().to_string_lossy().into_owned(),
            worktree_path: wt_parent.path().join("t26").to_string_lossy().into_owned(),
            worktree_branch: BRANCH.into(),
            start_point: Some(INTEGRATION.into()),
        };
        let app: Router = build_router(admin_state());
        let response = app
            .clone()
            .oneshot(json_request(
                "/v1/workspace/worktrees",
                Some(OP_TOKEN),
                &serde_json::to_string(&request).expect("serialize"),
            ))
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);

        // 配置被装载：管理面 GET /admin/workflow/config/{spec_id} 反映 Fresh + 投影开关。
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/admin/workflow/config/{SPEC}"))
                    .header("x-farm-admin-token", ADMIN_TOKEN)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let status: farm_sdk::WorkflowConfigStatus = body_json(response).await;
        let effective = status.effective.expect("the workflow config was loaded on establish");
        assert_eq!(effective.max_concurrency, 3);
        assert!(effective.projection.close_issue, "投影关票开关随配置生效");
    }
