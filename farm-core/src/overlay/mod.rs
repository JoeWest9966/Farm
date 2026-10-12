//! # farm-core `overlay` 模块 — 覆盖层（Overlay）：对 workflow 配置基线的临时运行时覆盖。
//!
//! GLOSSARY「覆盖层」：面板对 workflow 配置基线的**临时运行时覆盖**：显式标记、
//! 重启后提示、不静默污染仓库文件。ADR-0002 决策 2（配置双层：被编排仓库的
//! workflow 配置是基线，面板可施加覆盖层）+ ADR-0005 §6（覆盖层落 Farm SQLite，
//! per-仓库 keyed，**不落被编排仓库**——写进仓库会污染 PR diff）。
//!
//! 契约**类型**（`OverlayKey` / `OverlayEntry` / `OverlayStatus`…）在 `farm-sdk`
//! （经宿主管理 API 暴露）；本模块只放宿主私有行为：
//!
//! - [`migrations`]：v9 建表（SQL 住在服务域，同 #13 的先例）；
//! - [`store`]：per-仓库 keyed 的哑存储；
//! - [`service`]：句柄（挂在 [`crate::AppState`] 上）——set/list/clear、**生效配置
//!   = 基线 + 覆盖层**、事件回流、重启提示。
//!
//! 路由面：宿主管理 API（`/admin/workflow/overlays/{repo_id}`…），**不进编排器契约**——
//! 覆盖层是宿主自身的配置面（ADR-0002 §1 边界分明）。

pub mod migrations;
pub mod service;
pub mod store;

pub use migrations::{v9_workflow_overlay, OVERLAY_TABLE};
pub use service::{OverlayError, OverlayService};
pub use store::{OverlayStore, StoredOverlay};
