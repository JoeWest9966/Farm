//! # farm-core `service_control` 模块 — 五服务的启停控制面（ticket #23）。
//!
//! GLOSSARY「宿主管理 API」：宿主自身的「服务启停」归面板经管理面操作（`/admin/services`）。
//! 这是**宿主持有的易失开关**（ADR-0005 §2：调度/运行瞬态不落库）——每个五服务
//! （ADR-0001）一个 running/stopped 开关，被人经管理面切换。
//!
//! ## 与编排 tick 控制面（[`crate::control`]）的关系
//! 同形但不同对象：[`crate::control::TickControl`] 停的是**编排器的 tick**（归编排器所有），
//! 本模块停的是**宿主自身的服务**（归宿主所有）。两者都易失、都在切换时发 `needs_attention`
//! 让编排器/面板观测，都不新增事件种类（`events` 表 CHECK 由 `EventKind::ALL` 生成，
//! 加种类会让已落库的插入失败；payload 用 `kind` 细分）。
//!
//! ## 停机停的是什么（v1 边界）
//! v1 只落**宿主持有开关 + 事件观测**：停机是一个管理面记录的状态，编排器/宿主可据它
//! 决策（如跳过对该服务的调用）。把开关**接进契约路由**（让已停服务的 `/v1/...` 回
//! `service_unavailable`）属后续票——那要改 #13/#16 的契约 handler，会模糊管理面与契约的
//! 边界（ADR-0002 §1：边界分明，不是私有后门），本票不越界。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use farm_sdk::admin::{ServiceControlStatus, ServiceControlStatusEntry, ServiceRunState};
use farm_sdk::events::{EventKind, NewEvent};
use farm_sdk::services::ServiceName;

use crate::error::CoreError;
use crate::now_rfc3339;
use crate::AppState;

/// 单个服务的内部记账（进程内；重启后从事件历史折叠或回默认 running）。
#[derive(Clone, Debug)]
struct Entry {
    state: ServiceRunState,
    updated_at: Option<String>,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            state: ServiceRunState::Running,
            updated_at: None,
        }
    }
}

/// 宿主共享的服务启停控制面。`Clone` 廉价（内部 `Arc`），可被 axum handler 广泛共享。
#[derive(Clone, Debug, Default)]
pub struct ServiceControl {
    inner: Arc<Mutex<HashMap<ServiceName, Entry>>>,
}

impl ServiceControl {
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前全部五服务的启停状态（按 [`ServiceName::ALL`] 次序；未显式设过 → running）。
    pub fn status(&self) -> Result<ServiceControlStatus, CoreError> {
        let guard = self.inner.lock().map_err(|_| CoreError::StorePoisoned)?;
        let services = ServiceName::ALL
            .iter()
            .map(|&service| {
                let entry = guard.get(&service);
                ServiceControlStatusEntry {
                    service,
                    state: entry.map(|e| e.state).unwrap_or(ServiceRunState::Running),
                    updated_at: entry.and_then(|e| e.updated_at.clone()),
                }
            })
            .collect();
        Ok(ServiceControlStatus { services })
    }

    /// 停机一个服务（管理面操作）：记 stopped + 发 `needs_attention`。
    pub fn stop(
        &self,
        host: &AppState,
        service: ServiceName,
        actor: &str,
    ) -> Result<ServiceControlStatusEntry, CoreError> {
        self.set(host, service, ServiceRunState::Stopped, actor)
    }

    /// 启动一个服务（管理面操作）：记 running + 发 `needs_attention`。
    pub fn start(
        &self,
        host: &AppState,
        service: ServiceName,
        actor: &str,
    ) -> Result<ServiceControlStatusEntry, CoreError> {
        self.set(host, service, ServiceRunState::Running, actor)
    }

    /// 判据：某服务是否被人停了（宿主/编排器可据它跳过对该服务的调用）。
    pub fn is_stopped(&self, service: ServiceName) -> bool {
        self.inner
            .lock()
            .ok()
            .and_then(|guard| guard.get(&service).map(|e| e.state))
            .map(|state| matches!(state, ServiceRunState::Stopped))
            .unwrap_or(false)
    }

    fn set(
        &self,
        host: &AppState,
        service: ServiceName,
        state: ServiceRunState,
        actor: &str,
    ) -> Result<ServiceControlStatusEntry, CoreError> {
        let updated_at = now_rfc3339();
        let entry = {
            let mut guard = self.inner.lock().map_err(|_| CoreError::StorePoisoned)?;
            let slot = guard.entry(service).or_default();
            slot.state = state;
            slot.updated_at = Some(updated_at);
            slot.clone()
        };
        self.announce(host, service, &entry, actor);
        Ok(ServiceControlStatusEntry {
            service,
            state: entry.state,
            updated_at: entry.updated_at,
        })
    }

    /// 在事件总线上发一条服务启停变更（编排器/面板的观测面）。
    ///
    /// **不新增事件种类**：用 `needs_attention` + payload `kind = "service_control"` 细分，
    /// 与 [`crate::control::TickControl`] 同策略（events 表 CHECK 由 `EventKind::ALL` 生成）。
    fn announce(&self, host: &AppState, service: ServiceName, entry: &Entry, actor: &str) {
        let published = host.publish(
            NewEvent::new(EventKind::NeedsAttention, actor).with_payload(serde_json::json!({
                "kind": "service_control",
                "service": service.as_str(),
                "state": entry.state.as_str(),
                "updated_at": entry.updated_at,
            })),
        );
        if let Err(error) = published {
            tracing::warn!(%error, "service control change could not be published to the event bus");
        }
    }
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
            db_path: std::env::temp_dir().join("farm-core-service-control-unit.db"),
        })
        .expect("in-memory state")
    }

    #[test]
    fn all_five_services_start_running() {
        let control = ServiceControl::new();
        let status = control.status().expect("status");
        assert_eq!(status.services.len(), 5, "ADR-0001 五服务");
        assert!(
            status
                .services
                .iter()
                .all(|entry| entry.state == ServiceRunState::Running && entry.updated_at.is_none()),
            "初始俱 running、从未切换"
        );
    }

    #[test]
    fn stopping_a_service_flips_its_state_and_is_shared_across_clones() {
        let host = state();
        let control = ServiceControl::new();
        let cloned = control.clone();

        let entry = control
            .stop(&host, ServiceName::Tracker, "panel")
            .expect("stop");
        assert_eq!(entry.state, ServiceRunState::Stopped);
        assert!(entry.updated_at.is_some(), "切换要记时间");
        assert!(control.is_stopped(ServiceName::Tracker));
        assert!(cloned.is_stopped(ServiceName::Tracker), "克隆共享同一个开关");
        assert!(!control.is_stopped(ServiceName::Merge), "只停被点的那个");

        let resumed = control
            .start(&host, ServiceName::Tracker, "panel")
            .expect("start");
        assert_eq!(resumed.state, ServiceRunState::Running);
        assert!(!control.is_stopped(ServiceName::Tracker));
    }

    #[tokio::test]
    async fn stopping_a_service_publishes_a_needs_attention_the_panel_can_fold() {
        let host = state();
        host.service_control
            .stop(&host, ServiceName::Events, "panel")
            .expect("stop");
        let history = host
            .event_history(&crate::EventLogFilter {
                since: None,
                kind: Some(EventKind::NeedsAttention),
                ticket_id: None,
                limit: None,
            })
            .expect("history");
        assert!(
            history.events.iter().any(|event| {
                event.payload.get("kind").and_then(|k| k.as_str()) == Some("service_control")
                    && event.payload.get("service").and_then(|s| s.as_str()) == Some("events")
            }),
            "服务启停须发可观测的 needs_attention（kind=service_control）：{:?}",
            history.events
        );
    }
}
