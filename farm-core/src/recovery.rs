//! 崩溃恢复（ticket #20，ADR-0005 §4）：宿主重启时的状态对账。
//!
//! 重启时按序做三件事：
//!
//! 1. 所有 `running` attempt → **interrupted**（第四终态，非成功非失败；worktree 不清、
//!    保留现场；记账以最后一次流式快照为准 + `accounting_truncated`）——复用 #16 的
//!    [`AgentRunner::recover_interrupted`](crate::agent_runner)，interrupted **不**触发重试退避。
//! 2. 事件日志补一条 `host_restarted`（经持久化发布 + 审计双写 → `audited`）。
//! 3. 审计旁路批量补账：未入账的审计关键事件补进 Kron（ledger 幂等）。
//!
//! 重派与否留给编排器按 workflow 配置决定，宿主不自作主张（契约零域知识，ADR-0005 §4）。

use crate::AppState;
use crate::error::CoreError;
use farm_sdk::events::{EventKind, NewEvent};

/// 一次启动恢复的报告（宿主管理面 / 测试可读）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// 被标 `interrupted` 的 attempt id。
    pub interrupted: Vec<String>,
    /// 审计旁路补账笔数。
    pub backfilled: usize,
}

/// 宿主启动恢复：attempt 对账 → `host_restarted` 事件 → 审计补账。
///
/// 存储故障才返回 `Err`（投影为契约 `internal`/`service_unavailable`）；audit 旁路
/// 未就绪不在此报错（补账笔数为 0，事件留 SQLite 待下次）。
pub fn recover_on_startup(state: &AppState) -> Result<RecoveryReport, CoreError> {
    // 1. running → interrupted（保留现场，worktree 不清）。
    let interrupted = state.agent_runner.recover_interrupted(state)?;

    // 2. host_restarted 事件：经持久化发布路径（先落事件日志再广播）+ 审计双写入账。
    let payload = serde_json::json!({ "interrupted_attempts": interrupted });
    state.publish(NewEvent::new(EventKind::HostRestarted, "host").with_payload(payload))?;

    // 3. 审计旁路补账（幂等；旁路未就绪则 0）。
    let backfilled = state.audit.reconcile(state)?;

    Ok(RecoveryReport {
        interrupted,
        backfilled,
    })
}
