//! # farm-core `control` 模块 — 编排 tick 的控制面（暂停 / 恢复）。
//!
//! GLOSSARY：编排器跑自己的确定性循环，每 tick 调用编排器契约里的服务。**tick 归编排器
//! 所有**，Farm 宿主只提供「编排器可见的控制面」——一个宿主持有的开关：
//!
//! - 宿主侧权威态在本模块（[`TickControl`]，挂在 [`crate::AppState`] 上）；
//! - 编排器侧观测面是**契约事件总线**：每次暂停/恢复都发布一条 `needs_attention`
//!   （payload `kind = "tick_control"`），编排器循环折叠它即得当前 tick 态。
//!   宿主重启后从 `GET /v1/events/history` 折叠最后一条即可恢复视图。
//!
//! ## 为什么是易失的（不落库）
//! ADR-0005 §2：调度循环瞬态是易失项。暂停是**人对当前编排节奏的临时干预**，不是配置
//! （配置走覆盖层，见 [`crate::overlay`]）。落库会让「重启后该不该继续暂停」变成一个
//! 需要解释的漂移；易失 + 事件历史可折叠，行为反而可解释。
//!
//! ## 暂停停的是什么
//! 只停**新派发**：编排器循环读到 `paused` 就跳过本 tick 的派发与回收。运行中的
//! attempt 不被打断（打断是 abort 的语义，GLOSSARY：面板 HITL 有独立的 abort 入口）。

use std::sync::{Arc, Mutex};

use farm_sdk::control::{TickControlRequest, TickControlStatus, TickState};
use farm_sdk::events::{EventKind, NewEvent};

use crate::AppState;
use crate::error::CoreError;
use crate::now_rfc3339;

/// 控制面的内部投影（进程内；重启后从事件历史折叠）。
#[derive(Clone, Debug)]
struct TickEntry {
    state: TickState,
    reason: Option<String>,
    actor: Option<String>,
    updated_at: Option<String>,
}

impl Default for TickEntry {
    fn default() -> Self {
        Self {
            state: TickState::Running,
            reason: None,
            actor: None,
            updated_at: None,
        }
    }
}

/// 宿主共享的 tick 控制面。`Clone` 廉价（内部 `Arc`），可被 axum handler 广泛共享。
#[derive(Clone, Debug, Default)]
pub struct TickControl {
    inner: Arc<Mutex<TickEntry>>,
}

impl TickControl {
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前状态（宿主权威态；编排器读事件总线上的折叠结果）。
    pub fn status(&self) -> Result<TickControlStatus, CoreError> {
        let entry = self.inner.lock().map_err(|_| CoreError::StorePoisoned)?;
        Ok(TickControlStatus {
            state: entry.state,
            reason: entry.reason.clone(),
            actor: entry.actor.clone(),
            updated_at: entry.updated_at.clone(),
        })
    }

    /// 编排器循环的判据：`true` = 本 tick 跳过派发。
    pub fn is_paused(&self) -> bool {
        self.inner
            .lock()
            .map(|entry| entry.state.is_paused())
            .unwrap_or(false)
    }

    /// 暂停 tick（面板操作）。运行中的 attempt 不受影响（见模块文档）。
    pub fn pause(
        &self,
        host: &AppState,
        request: &TickControlRequest,
        actor: &str,
    ) -> Result<TickControlStatus, CoreError> {
        let status = {
            let mut entry = self.inner.lock().map_err(|_| CoreError::StorePoisoned)?;
            entry.state = TickState::Paused;
            entry.reason = request.reason.clone();
            entry.actor = Some(actor.to_string());
            entry.updated_at = Some(now_rfc3339());
            TickControlStatus {
                state: entry.state,
                reason: entry.reason.clone(),
                actor: entry.actor.clone(),
                updated_at: entry.updated_at.clone(),
            }
        };
        self.announce(host, &status);
        Ok(status)
    }

    /// 恢复 tick。
    pub fn resume(&self, host: &AppState, actor: &str) -> Result<TickControlStatus, CoreError> {
        let status = {
            let mut entry = self.inner.lock().map_err(|_| CoreError::StorePoisoned)?;
            entry.state = TickState::Running;
            entry.reason = None;
            entry.actor = Some(actor.to_string());
            entry.updated_at = Some(now_rfc3339());
            TickControlStatus {
                state: entry.state,
                reason: entry.reason.clone(),
                actor: entry.actor.clone(),
                updated_at: entry.updated_at.clone(),
            }
        };
        self.announce(host, &status);
        Ok(status)
    }

    /// 在事件总线上发一条 tick 控制变更（编排器的观测面）。
    ///
    /// **不新增事件种类**：`events` 表的 CHECK 由 migration v5 的 `EventKind::ALL` 生成，
    /// 加种类会让已落库的插入失败。payload 用 `kind = "tick_control"` 细分，折叠幂等。
    fn announce(&self, host: &AppState, status: &TickControlStatus) {
        let published = host.publish(
            NewEvent::new(EventKind::NeedsAttention, "control").with_payload(serde_json::json!({
                "kind": "tick_control",
                "state": status.state.as_str(),
                "reason": status.reason,
                "actor": status.actor,
                "updated_at": status.updated_at,
            })),
        );
        if let Err(error) = published {
            tracing::warn!(%error, "tick control change could not be published to the event bus");
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
            db_path: std::env::temp_dir().join("farm-core-control-unit.db"),
        })
        .expect("in-memory state")
    }

    #[test]
    fn the_tick_starts_running() {
        let control = TickControl::new();
        assert!(!control.is_paused());
        assert_eq!(control.status().expect("status").state, TickState::Running);
    }

    #[test]
    fn pausing_and_resuming_flips_the_flag_the_orchestrator_reads() {
        let host = state();
        let paused = host
            .control
            .pause(&host, &TickControlRequest { reason: Some("human paused".into()) }, "panel")
            .expect("pause");
        assert!(paused.state.is_paused());
        assert!(host.control.is_paused(), "编排器循环的判据随之翻转");

        let resumed = host.control.resume(&host, "panel").expect("resume");
        assert!(!resumed.state.is_paused());
        assert!(!host.control.is_paused());
        assert_eq!(resumed.reason, None, "恢复后不留暂停理由");
    }

    #[test]
    fn the_control_surface_is_cheap_to_clone_and_shares_one_flag() {
        let control = TickControl::new();
        let cloned = control.clone();
        let host = state();
        control
            .pause(&host, &TickControlRequest { reason: None }, "panel")
            .expect("pause");
        assert!(cloned.is_paused(), "克隆共享同一个开关");
    }
}
