//! # farm-core `notifier` 模块 — 通知通道配置（Notification Channel config，ticket #21）。
//!
//! GLOSSARY「通知通道」：关键事件（`blocked`、`needs_attention`、merge 完成、`stall`）的
//! **宿主侧外发出口**，可插拔。ADR-0002 决策 6：通知做成宿主侧可插拔出口。本模块是它的
//! **配置持久面**——per-仓库 keyed（ADR-0005 §6，落宿主私有 SQLite，不落被编排仓库），
//! 经宿主管理 API（`/admin/notifier/channel/{repo_id}`）可改、重启后生效。
//!
//! ## 宿主零推送域知识
//! 配置只有「开不开（`enabled`）+ 转哪些关键事件（过滤集）」。**没有一个** url / header /
//! body 模板 / 重试 / 限流字段——那些是插件侧自决载体（GLOSSARY「Notifier 插件」：
//! 第一方只做一个通用 HTTP notifier，推送格式全住插件侧，见 `examples/http-notifier`）。
//! 宿主只跑总线 + 插件，不懂推送。
//!
//! ## 模块分层（与 tracker / merge / overlay 同构）
//! - [`migrations`] —— v10 建表（SQL 住在服务域，同 #13 的先例）；
//! - [`store`] —— per-仓库 keyed 的哑存储；
//! - [`service`] —— 句柄（挂在 [`crate::AppState`] 上）——get（默认化）/ set。

pub mod migrations;
pub mod service;
pub mod store;

pub use migrations::{v10_notifier_channel_config, NOTIFIER_TABLE};
pub use service::NotifierService;
pub use store::{NotifierStore, StoredNotifierConfig};
