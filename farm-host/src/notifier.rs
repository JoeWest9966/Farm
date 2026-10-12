//! 通知通道配置的宿主管理 API（ticket #21，GLOSSARY「宿主管理 API / 通知通道」）。
//!
//! 路由进 [`farm_sdk::ADMIN_ROUTES`]（单一源），宿主按表挂载；管理 token 校验机制归 #23
//! （[`crate::admin::require_admin_token`]），故本文件与其它管理面路由一样只写业务 handler。
//! 数据源是 [`farm_core::AppState::notifier`]（migration v10 的持久面）。
//!
//! **宿主零推送域知识**：本面只搬 `enabled` + 过滤集（[`farm_sdk::notifier`]），没有一个
//! url / header / body 模板 / 重试 / 限流字段——那些是插件侧自决载体（GLOSSARY「Notifier 插件」，
//! 见 `examples/http-notifier`）。宿主只跑总线 + 插件，推送格式全住插件侧。

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::Json;
use farm_core::AppState;
use farm_sdk::notifier::{NotificationChannelConfig, NotificationChannelUpdate};
use farm_sdk::ContractError;

use crate::error::ApiError;

/// `GET /admin/notifier/channel/{repo_id}`：查询某仓库的通知通道配置。
///
/// 没配过 → 默认过滤集（blocked/needs_attention/stall 开、merge 完成关）+ 总闸开，
/// `updated_at` 为空串（面板据此区分「默认生效」与「显式配置」）。存储故障 → 闭集
/// `service_unavailable`。
pub async fn channel_config(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
) -> Result<Json<NotificationChannelConfig>, ApiError> {
    let config = state.notifier.get(&repo_id).map_err(ApiError::from)?;
    Ok(Json(config))
}

/// `POST /admin/notifier/channel/{repo_id}`：设置某仓库的通知通道配置，重启后生效。
///
/// 请求体是 [`NotificationChannelUpdate`]（`enabled` + `filter`）。体解析失败（缺字段 /
/// 形状不符）→ 闭集 `invalid_request`（400）——与其它契约面同一信封形状（手动解析而非
/// 依赖 axum `Json` 提取器的默认 422/纯文本拒绝，保持「客户端永远看到同一个 JSON 形状」）。
pub async fn set_channel_config(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
    body: Bytes,
) -> Result<Json<NotificationChannelConfig>, ApiError> {
    let update: NotificationChannelUpdate = serde_json::from_slice(&body).map_err(|e| {
        ApiError(ContractError::invalid_request(format!(
            "malformed notification channel update: {e}"
        )))
    })?;
    let config = state.notifier.set(&repo_id, &update).map_err(ApiError::from)?;
    Ok(Json(config))
}
