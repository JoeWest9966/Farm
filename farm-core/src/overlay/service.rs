//! 覆盖层服务句柄：把「per-仓库存储 + 生效配置 + 事件回流 + 重启提示」缝成宿主共享状态。
//!
//! 这是 [`crate::AppState`] 的 `overlay` 字段。行为契约（GLOSSARY + ADR-0002 §2 +
//! ADR-0005 §6）：
//!
//! - **生效**：`effective = baseline + overlay`，基线 = workflow 当前配置 / LKG
//!   （[`crate::workflow::WorkflowService`] 是唯一读源，本服务不复制它）；
//! - **显式临时**：每条覆盖层带 `temporary` 标记与单一源文案
//!   （[`farm_sdk::overlay::OVERLAY_TEMPORARY_MARKER`]）；
//! - **不写回仓库**：本服务只写宿主私有 SQLite，**没有一个**写被编排仓库的调用点；
//! - **可观测**：施加/清除都经 [`crate::AppState::publish`]（持久化路径：先落事件日志
//!   再广播）回流，payload 带标记与**生效配置**——编排器折叠它即得当前生效视图；
//! - **重启提示**：[`OverlayService::prompt_restore`] 逐仓库发 `needs_attention`，
//!   问人类要不要恢复基线（ADR-0002 §2：重启后提示）。
//!
//! ## 为什么发布面由调用方把 `&AppState` 传进来
//! 与 [`crate::agent_runner::AgentRunner`] 同一理由：`AppState` 持有本句柄，句柄不能再
//! 反向持有一个 `AppState`（构造期循环）。发布面作为参数传入即可。

use farm_sdk::events::{EventKind, NewEvent};
use farm_sdk::overlay::{
    validate_value, OverlayClearRequest, OverlayEntry, OverlayKey, OverlaySetRequest, OverlayStatus,
    OVERLAY_TEMPORARY_MARKER,
};
use farm_sdk::workflow::WorkflowConfig;

use crate::AppState;
use crate::db::Database;
use crate::error::CoreError;

use super::store::{OverlayStore, StoredOverlay};

/// 覆盖层服务自己的错误（宿主内部形状；投影到契约闭集见
/// [`OverlayError::to_contract_error`]）。
#[derive(Debug, thiserror::Error)]
pub enum OverlayError {
    /// 值不符合该键声明的种类（闭集 `invalid_request`）。
    #[error("overlay {key} rejected the value {value}: {reason}")]
    InvalidValue {
        key: OverlayKey,
        value: String,
        reason: String,
    },

    /// 宿主私有存储 / 事件总线故障（投影回 [`CoreError`] 的闭集形状）。
    #[error("host-private store or event bus failure: {0}")]
    Store(#[from] CoreError),
}

impl OverlayError {
    /// 投影到契约闭集错误（ADR-0001 决策 6：错误码是闭集）。
    pub fn to_contract_error(&self) -> farm_sdk::ContractError {
        use farm_sdk::ContractError;
        match self {
            OverlayError::InvalidValue { key, value, reason } => {
                ContractError::invalid_request(format!("overlay {key} rejected the value {value}: {reason}"))
            }
            OverlayError::Store(error) => error.to_contract_error(),
        }
    }
}

impl From<OverlayError> for farm_sdk::ContractError {
    fn from(err: OverlayError) -> Self {
        err.to_contract_error()
    }
}

/// 宿主共享的覆盖层句柄。`Clone` 廉价（内部 `Arc`），可被 axum handler 广泛共享。
#[derive(Clone)]
pub struct OverlayService {
    store: OverlayStore,
}

impl OverlayService {
    pub fn new(db: Database) -> Self {
        Self {
            store: OverlayStore::new(db),
        }
    }

    /// 底层存储句柄（宿主内部用；schema 不外泄，ADR-0005 §5）。
    pub fn store(&self) -> &OverlayStore {
        &self.store
    }

    /// 施加一条覆盖层，返回**施加后的生效视图**（一次往返给全：清单 + 生效 + 提示）。
    ///
    /// 两道闸：值先过 [`validate_value`]（单字段形状），再合并出**预览生效配置**过
    /// [`crate::workflow::config::validate`]（跨字段语义，如「启用重试时基础延迟
    /// 必须 >= 1」）。任一不过即拒、什么都不落——生效配置永远是合法配置。
    pub fn set(
        &self,
        host: &AppState,
        repo_id: &str,
        request: &OverlaySetRequest,
    ) -> Result<OverlayStatus, OverlayError> {
        validate_value(request.key, &request.value).map_err(|reason| OverlayError::InvalidValue {
            key: request.key,
            value: request.value.to_string(),
            reason,
        })?;
        if let Some(merged) = self.merge_config(host, repo_id, Some((request.key, &request.value)))? {
            crate::workflow::config::validate(&merged).map_err(|error| {
                OverlayError::InvalidValue {
                    key: request.key,
                    value: request.value.to_string(),
                    reason: error.to_string(),
                }
            })?;
        }
        self.store.put(&StoredOverlay {
            repo_id: repo_id.to_string(),
            key: request.key,
            value: request.value.clone(),
            note: request.note.clone(),
            temporary: true,
            updated_at: crate::now_rfc3339(),
        })?;
        let status = self.status(host, repo_id)?;
        self.announce(host, "applied", &[request.key], &status);
        Ok(status)
    }

    /// 清除覆盖层：给 `key` 清一条，不给清该仓库全部。返回清除后的生效视图。
    pub fn clear(
        &self,
        host: &AppState,
        repo_id: &str,
        request: &OverlayClearRequest,
    ) -> Result<OverlayStatus, OverlayError> {
        let cleared = match request.key {
            Some(key) => {
                if self.store.delete(repo_id, key)? {
                    vec![key]
                } else {
                    Vec::new()
                }
            }
            None => {
                let existing = self.store.list(repo_id)?;
                self.store.clear_repo(repo_id)?;
                existing.into_iter().map(|entry| entry.key).collect()
            }
        };
        let status = self.status(host, repo_id)?;
        self.announce(host, "cleared", &cleared, &status);
        Ok(status)
    }

    /// 生效视图：基线 + 覆盖层 + 临时标记 + 恢复提示（查询 API 的数据源）。
    pub fn status(&self, host: &AppState, repo_id: &str) -> Result<OverlayStatus, CoreError> {
        let baseline = host.workflow.status(repo_id)?.effective;
        let stored = self.store.list(repo_id)?;
        let overlays: Vec<OverlayEntry> = stored.iter().map(project).collect();
        let effective = self.merge_config(host, repo_id, None)?;
        // 「生效」的判据：有基线可覆盖，且确实有覆盖层叠在上面。
        let overlay_applied = baseline.is_some() && !stored.is_empty();

        Ok(OverlayStatus {
            repo_id: repo_id.to_string(),
            baseline,
            effective,
            overlay_applied,
            overlays,
            restore_prompt: restore_prompt(repo_id, &stored),
        })
    }

    /// 生效计算的单一处：基线 + 已落库的覆盖层（外加一条**尚未写入**的预览值）。
    ///
    /// `pending` 给 `set` 做写入前预览用（同键覆盖已存值）。无基线（从未装载也无
    /// LKG）→ `None`：没有基线就没有生效值，覆盖层仍落库等着基线落地。
    fn merge_config(
        &self,
        host: &AppState,
        repo_id: &str,
        pending: Option<(OverlayKey, &serde_json::Value)>,
    ) -> Result<Option<WorkflowConfig>, CoreError> {
        let Some(mut merged) = host.workflow.status(repo_id)?.effective else {
            return Ok(None);
        };
        let mut stored = self.store.list(repo_id)?;
        if let Some((key, value)) = pending {
            stored.retain(|entry| entry.key != key);
            stored.push(StoredOverlay {
                repo_id: repo_id.to_string(),
                key,
                value: value.clone(),
                note: None,
                temporary: true,
                updated_at: crate::now_rfc3339(),
            });
        }
        // 确定序：按键的 wire 字符串升序施加（list 已排序，预览值按同一把尺子归位）。
        stored.sort_by_key(|entry| entry.key.as_str());
        for entry in &stored {
            apply_override(&mut merged, entry.key, &entry.value)?;
        }
        Ok(Some(merged))
    }

    /// 某仓库的覆盖层清单（契约形状；不带生效计算）。
    pub fn entries(&self, repo_id: &str) -> Result<Vec<OverlayEntry>, CoreError> {
        Ok(self.store.list(repo_id)?.iter().map(project).collect())
    }

    /// 重启提示（ADR-0002 §2）：有覆盖层的仓库逐条发 `needs_attention`，问人类要不要
    /// 恢复基线。返回被提示的仓库清单（空 = 无需提示）。
    ///
    /// 宿主启动路径调用它（`farm-host/src/main.rs`），与崩溃恢复并列——恢复管 attempt，
    /// 本函数管配置：重启后漂移必须**可解释**（纯内存覆盖被否的正是这个理由）。
    pub fn prompt_restore(&self, host: &AppState) -> Result<Vec<String>, CoreError> {
        let repos = self.store.repos_with_overlays()?;
        for repo_id in &repos {
            let entries = self.entries(repo_id)?;
            host.publish(
                NewEvent::new(EventKind::NeedsAttention, "overlay")
                    .with_payload(serde_json::json!({
                        "kind": "overlay_restore_prompt",
                        "repo_id": repo_id,
                        "overlays": entries.iter().map(|entry| entry.key.as_str()).collect::<Vec<_>>(),
                        "temporary": true,
                        "marker": OVERLAY_TEMPORARY_MARKER,
                        "prompt": restore_prompt(repo_id, &self.store.list(repo_id)?),
                    })),
            )?;
        }
        Ok(repos)
    }

    /// 在事件总线上发一条覆盖层变更（`needs_attention` 一等事件，payload 用 `kind` 细分）。
    ///
    /// **不新增事件种类**：`events` 表的 CHECK 由 migration v5 的 `EventKind::ALL` 生成，
    /// 加种类会让已落库的插入失败（v5 已跑过的库不会重建表）。payload 带生效配置，
    /// 所以编排器不必另开读路口就能更新自己的生效视图。
    fn announce(&self, host: &AppState, action: &str, keys: &[OverlayKey], status: &OverlayStatus) {
        let published = host.publish(
            NewEvent::new(EventKind::NeedsAttention, "overlay").with_payload(serde_json::json!({
                "kind": "overlay",
                "action": action,
                "repo_id": status.repo_id,
                "keys": keys.iter().map(|key| key.as_str()).collect::<Vec<_>>(),
                "temporary": true,
                "marker": OVERLAY_TEMPORARY_MARKER,
                "effective": status.effective,
                "restore_prompt": status.restore_prompt,
            })),
        );
        if let Err(error) = published {
            // 覆盖层已落库；事件回流失败只告警，不回滚已生效的覆盖（ADR-0005 §3 同款纪律：
            // 旁路失败不阻塞主流程）。
            tracing::warn!(%error, action, "overlay change could not be published to the event bus");
        }
    }
}

/// 存储形状 → 契约形状（临时标记在这一步显式落到 `temporary`）。
fn project(entry: &StoredOverlay) -> OverlayEntry {
    OverlayEntry {
        repo_id: entry.repo_id.clone(),
        key: entry.key,
        value: entry.value.clone(),
        note: entry.note.clone(),
        temporary: entry.temporary,
        updated_at: entry.updated_at.clone(),
    }
}

/// 恢复提示文案（有覆盖层才有；`None` = 无需提示）。
fn restore_prompt(repo_id: &str, entries: &[StoredOverlay]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    let keys = entries
        .iter()
        .map(|entry| entry.key.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "{repo_id} 有 {count} 条临时覆盖（{keys}）：{marker}。是否恢复基线？",
        count = entries.len(),
        marker = OVERLAY_TEMPORARY_MARKER
    ))
}

/// 把一格覆盖层叠到基线配置上（生效计算的单一处）。
///
/// 值在写入前已过 [`validate_value`]，所以这里的取值失败只能是存储被外部改坏——
/// 一律报错，不静默跳过（GLOSSARY：非法绝不 panic，也不静默降级）。
fn apply_override(
    config: &mut WorkflowConfig,
    key: OverlayKey,
    value: &serde_json::Value,
) -> Result<(), CoreError> {
    let invalid = || {
        CoreError::Config(format!(
            "stored overlay {key} holds {value}, which is not a valid value for that key"
        ))
    };
    match key {
        OverlayKey::MaxConcurrency => {
            config.max_concurrency = value.as_u64().ok_or_else(invalid)? as u32;
        }
        OverlayKey::StallThresholdSecs => {
            config.stall_threshold_secs = value.as_u64().ok_or_else(invalid)?;
        }
        OverlayKey::DispatchLabels => {
            let labels = value.as_array().ok_or_else(invalid)?;
            config.dispatch_labels = labels
                .iter()
                .map(|label| label.as_str().map(str::to_string).ok_or_else(invalid))
                .collect::<Result<Vec<_>, _>>()?;
        }
        OverlayKey::PromptTemplate => {
            config.prompt_template = value.as_str().ok_or_else(invalid)?.to_string();
        }
        OverlayKey::RetryMaxRetries => {
            config.retry_backoff.max_retries = value.as_u64().ok_or_else(invalid)? as u32;
        }
        OverlayKey::RetryBaseDelayMs => {
            config.retry_backoff.base_delay_ms = value.as_u64().ok_or_else(invalid)?;
        }
        OverlayKey::RetryBackoffMultiplier => {
            config.retry_backoff.backoff_multiplier = value.as_u64().ok_or_else(invalid)? as u32;
        }
        OverlayKey::ProjectionCloseIssue => {
            config.projection.close_issue = value.as_bool().ok_or_else(invalid)?;
        }
        OverlayKey::ProjectionOpenPr => {
            config.projection.open_pr = value.as_bool().ok_or_else(invalid)?;
        }
        OverlayKey::ProjectionCommentMergeCommit => {
            config.projection.comment_merge_commit = value.as_bool().ok_or_else(invalid)?;
        }
        OverlayKey::ProjectionAddLabel => {
            config.projection.add_label = value.as_bool().ok_or_else(invalid)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HostConfig;

    fn state() -> AppState {
        AppState::new_in_memory(HostConfig {
            bind: "127.0.0.1:3721".parse().expect("bind"),
            op_token: "unit-op-token".into(),
            admin_token: None,
            db_path: std::env::temp_dir().join("farm-core-overlay-unit.db"),
        })
        .expect("in-memory state")
    }

    #[test]
    fn the_restore_prompt_names_the_repo_the_count_and_the_marker() {
        let entries = vec![StoredOverlay {
            repo_id: "repo-a".into(),
            key: OverlayKey::MaxConcurrency,
            value: serde_json::json!(1),
            note: None,
            temporary: true,
            updated_at: "t".into(),
        }];
        let prompt = restore_prompt("repo-a", &entries).expect("prompt");
        assert!(prompt.contains("repo-a"), "{prompt}");
        assert!(prompt.contains('1'), "{prompt}");
        assert!(prompt.contains("max_concurrency"), "{prompt}");
        assert!(prompt.contains("恢复基线"), "{prompt}");
        assert!(prompt.contains(OVERLAY_TEMPORARY_MARKER), "{prompt}");
        assert_eq!(restore_prompt("repo-a", &[]), None, "无覆盖无需提示");
    }

    #[test]
    fn an_overlay_value_that_no_longer_matches_its_key_is_an_error_not_a_silent_skip() {
        // 存储被外部改坏的情形：apply 必须报错（绝不静默跳过那一格）。
        let mut config = WorkflowConfig {
            dispatch_labels: vec!["agent-ready".into()],
            max_concurrency: 2,
            retry_backoff: Default::default(),
            stall_threshold_secs: 300,
            prompt_template: "do {{ticket_id}}".into(),
            projection: Default::default(),
        };
        let err = apply_override(&mut config, OverlayKey::MaxConcurrency, &serde_json::json!("2"))
            .expect_err("类型不符必须报错");
        assert!(err.to_string().contains("not a valid value"), "{err}");
        assert_eq!(config.max_concurrency, 2, "报错时配置不被改花");
    }

    #[test]
    fn the_overlay_error_projects_into_the_closed_set() {
        let err = OverlayError::InvalidValue {
            key: OverlayKey::MaxConcurrency,
            value: "\"2\"".into(),
            reason: "需要一个整数".into(),
        };
        assert_eq!(
            err.to_contract_error().code,
            farm_sdk::ErrorCode::InvalidRequest
        );
    }

    #[test]
    fn an_overlay_service_handle_is_cheap_to_clone() {
        let db = crate::db::Database::open_in_memory().expect("db");
        db.migrate(&crate::migrations::Migrator::new(crate::migrations::MIGRATIONS))
            .expect("migrate");
        let service = OverlayService::new(db);
        let cloned = service.clone();
        assert!(
            cloned.store().list("repo-a").expect("list").is_empty(),
            "克隆共享同一个存储"
        );
    }
}
