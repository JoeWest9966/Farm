//! Workflow 服务句柄：把「读取校验 + LKG 落库 + 事件总线」缝成宿主共享状态。
//!
//! 这是 [`crate::AppState`] 的 `workflow` 字段。行为契约（GLOSSARY + ticket #12）：
//! - 合法配置 → 生效并成为新 LKG（`LoadOutcome::Applied`）；
//! - 非法配置 + 有 LKG → 生效值回退 LKG，事件总线发 `needs_attention` 告警（`FellBack`）；
//! - 非法配置 + 无 LKG（首次即非法）→ 拒绝编排，发 `needs_attention`（`Refused`）；
//! - 非法**绝不** panic；LKG 经重启仍生效（SQLite 持久，重启后从库里引导）。

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use farm_sdk::events::{EventKind, NewEvent};
use farm_sdk::workflow::{
    EffectiveSource, LastKnownGoodInfo, WorkflowConfig, WorkflowConfigStatus,
};

use crate::db::Database;
use crate::error::CoreError;
use crate::events::EventBus;
use crate::now_rfc3339;

use super::config::load_from_repo;
use super::lkg::LkgStore;

/// 装载结果：调用方（编排器/宿主）据此决定是否继续编排该仓库。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadOutcome {
    /// 仓库当前配置合法：生效并成为新 LKG。
    Applied(WorkflowConfig),
    /// 仓库当前配置非法：回退到 LKG 生效（已发告警）。
    FellBack {
        effective: WorkflowConfig,
        reason: String,
    },
    /// 首次即非法：拒绝编排（已发 needs_attention）。
    Refused {
        reason: String,
    },
}

/// 某仓库的当前生效投影（进程内；重启后从 LKG 库引导）。
#[derive(Clone)]
struct Entry {
    /// 当前生效配置（`None` = 尚无生效配置：首次即非法）。
    config: Option<WorkflowConfig>,
    source: Option<EffectiveSource>,
    loaded_at: Option<String>,
    last_alert: Option<String>,
}

/// 宿主共享的 workflow 配置句柄。`Clone` 廉价（内部 `Arc`），可被 axum handler 广泛共享。
#[derive(Clone)]
pub struct WorkflowService {
    store: LkgStore,
    events: EventBus,
    effective: Arc<Mutex<HashMap<String, Entry>>>,
}

impl WorkflowService {
    pub fn new(db: Database, events: EventBus) -> Self {
        Self {
            store: LkgStore::new(db),
            events,
            effective: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 从被编排仓库装载配置：合法 → Applied + 落 LKG；非法 → 回退 LKG 或拒绝，并发告警。
    ///
    /// 只有宿主私有存储故障才会返回 `Err`（投影为契约 `internal`/`service_unavailable`）；
    /// 配置非法本身**不致错**，而是走 `FellBack` / `Refused` 两条明确路径。
    pub fn load_repo(&self, repo_id: &str, repo_root: &Path) -> Result<LoadOutcome, CoreError> {
        match load_from_repo(repo_root) {
            Ok(config) => {
                let now = now_rfc3339();
                self.store.put(repo_id, &config, &now)?; // 合法即成为新 LKG（落库）
                self.set_entry(
                    repo_id,
                    Some(config.clone()),
                    Some(EffectiveSource::Fresh),
                    Some(now),
                    None,
                );
                Ok(LoadOutcome::Applied(config))
            }
            Err(err) => {
                let reason = err.to_string();
                match self.store.get(repo_id)? {
                    Some(lkg) => {
                        // 有 LKG：回退生效 + 告警（降级但继续编排）。
                        self.set_entry(
                            repo_id,
                            Some(lkg.config.clone()),
                            Some(EffectiveSource::LastKnownGood),
                            Some(lkg.updated_at.clone()),
                            Some(reason.clone()),
                        );
                        self.alert(repo_id, "fallback", &reason);
                        Ok(LoadOutcome::FellBack {
                            effective: lkg.config,
                            reason,
                        })
                    }
                    None => {
                        // 无 LKG：首次即非法 → 拒绝编排 + needs_attention。
                        self.set_entry(repo_id, None, None, None, Some(reason.clone()));
                        self.alert(repo_id, "refused", &reason);
                        Ok(LoadOutcome::Refused { reason })
                    }
                }
            }
        }
    }

    /// 查询当前生效配置 + LKG 状态（查询 API 的数据源）。
    pub fn status(&self, repo_id: &str) -> Result<WorkflowConfigStatus, CoreError> {
        let lkg = self.store.get(repo_id)?;
        let last_known_good = lkg
            .as_ref()
            .map(|l| LastKnownGoodInfo {
                updated_at: l.updated_at.clone(),
            });

        let entry = self
            .effective
            .lock()
            .map_err(|_| CoreError::StorePoisoned)?
            .get(repo_id)
            .cloned();

        // 无进程内投影时（重启后/未主动装载）从 LKG 库引导：重启后 LKG 仍生效。
        let (effective, source, loaded_at, last_alert) = match entry {
            Some(e) => (e.config, e.source, e.loaded_at, e.last_alert),
            None => match &lkg {
                Some(l) => (
                    Some(l.config.clone()),
                    Some(EffectiveSource::LastKnownGood),
                    Some(l.updated_at.clone()),
                    None,
                ),
                None => (None, None, None, None),
            },
        };

        Ok(WorkflowConfigStatus {
            repo_id: repo_id.to_string(),
            effective,
            source,
            loaded_at,
            last_known_good,
            last_alert,
        })
    }

    /// 写进程内投影。锁只做一次 HashMap insert（临界区内不 panic），中毒时尽力而为
    /// ——不回退：Applied 路径已先落 LKG，status 会退到读库，不丢配置。
    fn set_entry(
        &self,
        repo_id: &str,
        config: Option<WorkflowConfig>,
        source: Option<EffectiveSource>,
        loaded_at: Option<String>,
        last_alert: Option<String>,
    ) {
        if let Ok(mut guard) = self.effective.lock() {
            guard.insert(
                repo_id.to_string(),
                Entry {
                    config,
                    source,
                    loaded_at,
                    last_alert,
                },
            );
        }
    }

    /// 在事件总线上发一条 `needs_attention` 告警（GLOSSARY：一等事件，面板必渲染）。
    /// payload 标出 `config_state`（fallback / refused）、`repo_id` 与 `reason`。
    fn alert(&self, repo_id: &str, config_state: &str, reason: &str) {
        let payload = serde_json::json!({
            "kind": "workflow_config",
            "repo_id": repo_id,
            "config_state": config_state,
            "reason": reason,
        });
        self.events.publish(
            NewEvent::new(EventKind::NeedsAttention, "workflow").with_payload(payload),
        );
    }
}
