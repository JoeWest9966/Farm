//! # Merge 服务（五服务之一，ticket #18）
//!
//! GLOSSARY「Merge 服务」：**宿主侧确定性的合并执行者**。agent 只能**提议**合并；
//! 宿主校验（worktree 干净、FF 可行、测试过）后执行**真正的 merge**。
//! **完成信号 = 集成分支上出现该票的 merge commit**，是宿主可验证的机械事实，
//! 永不采信 agent 自述。关票、开 PR 等都只是完成**之后**的投影
//! （[`projection`]，由 workflow 配置开关）。
//!
//! ```text
//! agent ──提议（POST /v1/merge/proposals）──▶ MergeService
//!                                              ├─ 读 workspace 映射（#14，不拥有它）
//!                                              ├─ 机械校验：干净 / FF 可行 / 测试过
//!                                              ├─ 真 merge（git merge --no-ff）
//!                                              ├─ 投影（workflow ProjectionSwitches）
//!                                              └─ merge 事件 → 事件总线（#15 持久路径）
//! ```
//!
//! 模块分层（与 tracker / agent-runner 同构）：
//!
//! - [`service`] — [`MergeService`] 句柄：提议状态机 + 机械校验 + 执行 + 事件；
//! - [`git`] — 真实 `git` CLI 原语（Farm 不用 libgit2）；
//! - [`store`] — 提议行的宿主私有存储（#25 审阅队列的读源）；
//! - [`projection`] — 完成信号之后的机械写回（开关驱动）；
//! - [`migrations`] — v7 `merge_proposal` 表（`crate::migrations` 只登记版本）。

pub mod git;
pub mod migrations;
pub mod projection;
pub mod service;
pub mod store;

pub use projection::{ProjectionOutcome, ProjectionStatus, ProjectionSwitch};
pub use service::{MergeConfig, MergeError, MergeReport, MergeService, RejectionStage};
pub use store::{MergeProposalRecord, MergeStore, PROPOSAL_TABLE};
