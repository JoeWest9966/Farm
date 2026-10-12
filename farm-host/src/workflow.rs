//! Workflow 配置查询 API（ticket #12）：`GET /admin/workflow/config/{repo_id}`。
//!
//! GLOSSARY「宿主管理 API」：查询宿主自身对某被编排仓库的**当前生效配置 + LKG 状态**。
//! 路由进 [`farm_sdk::ADMIN_ROUTES`]（单一源），宿主按表挂载；管理 token 校验机制归 #23，
//! 故本票该路由与其它管理面路由一样暂不带鉴权。数据源是 [`farm_core::AppState::workflow`]。

use axum::extract::{Path, State};
use axum::Json;
use farm_core::AppState;
use farm_sdk::workflow::WorkflowConfigStatus;

use crate::error::ApiError;

/// `GET /admin/workflow/config/{repo_id}`：返回当前生效配置 + LKG 状态。
///
/// 存储故障 → 闭集 `internal`/`service_unavailable`；配置非法**不**在此出错——
/// 那只影响 `effective`/`source`/`last_alert` 字段（回退或 needs_attention），
/// 查询本身永远可读。
pub async fn config_status(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
) -> Result<Json<WorkflowConfigStatus>, ApiError> {
    let status = state.workflow.status(&repo_id).map_err(ApiError::from)?;
    Ok(Json(status))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_router;
    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use farm_core::HostConfig;
    use farm_sdk::workflow::EffectiveSource;
    use std::fs;
    use tower::ServiceExt;

    const VALID: &str = r#"{
      "dispatch_labels": ["agent-ready"],
      "max_concurrency": 2,
      "retry_backoff": { "max_retries": 3, "base_delay_ms": 500, "backoff_multiplier": 2 },
      "stall_threshold_secs": 300,
      "prompt_template": "do {{ticket_id}}",
      "projection": { "open_pr": true, "close_issue": true }
    }"#;

    fn state() -> AppState {
        AppState::new_in_memory(HostConfig {
            bind: "127.0.0.1:3721".parse().expect("bind"),
            op_token: "unit-op-token".into(),
            admin_token: None,
            db_path: std::env::temp_dir().join("farm-host-workflow-test.db"),
        })
        .expect("in-memory state")
    }

    async fn get_status(router: axum::Router, repo_id: &str) -> (StatusCode, WorkflowConfigStatus) {
        let response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/admin/workflow/config/{repo_id}"))
                    .body(axum::body::Body::empty())
                    .expect("request"),
            )
            .await
            .expect("router serves the request");
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.expect("body");
        let parsed: WorkflowConfigStatus = serde_json::from_slice(&bytes).expect("status json");
        (status, parsed)
    }

    #[tokio::test]
    async fn the_query_api_exposes_the_effective_config_for_a_valid_repo() {
        let state = state();
        let repo = tempfile::tempdir().expect("repo");
        fs::create_dir_all(repo.path().join(".farm")).unwrap();
        fs::write(repo.path().join(".farm/workflow.json"), VALID).unwrap();
        state
            .workflow
            .load_repo("repo-a", repo.path())
            .expect("load valid");

        let (status, parsed) = get_status(build_router(state), "repo-a").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(parsed.repo_id, "repo-a");
        assert_eq!(parsed.source, Some(EffectiveSource::Fresh));
        assert_eq!(
            parsed.effective.expect("effective config").max_concurrency,
            2
        );
        assert!(parsed.last_known_good.is_some(), "合法配置成为 LKG 并落库");
    }

    #[tokio::test]
    async fn the_query_api_reports_needs_attention_for_a_first_ever_invalid_repo() {
        // 从未装载、也无 LKG 的仓库：查询不报错，但 effective 为空（拒绝编排）。
        let (status, parsed) = get_status(build_router(state()), "ghost").await;
        assert_eq!(status, StatusCode::OK, "查询永远可读，不因未配置而 500");
        assert!(parsed.effective.is_none());
        assert!(parsed.last_known_good.is_none());
    }
}
