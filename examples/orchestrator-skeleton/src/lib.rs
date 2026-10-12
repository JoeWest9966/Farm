//! orchestrator-skeleton：Farm 契约的第一个客户端（ticket #11）。
//!
//! ADR-0001：编排器是契约的客户端/驱动进程，跑自己的确定性循环、每 tick 调服务。
//! 本骨架只证明「契约可达」：连上宿主、完成 `/handshake`、并收到**合法闭集错误**。
//! 全链路编排（claim→dispatch→merge→投影→通知）归 #26 的加固版骨架。
//!
//! 只 depend `farm-sdk`（纯契约类型，零运行时）——与面板、插件同路。

pub mod client;
pub mod conformance;
pub mod report;

pub use client::{run_contract_smoke, ContractClient, SmokeCheck};
pub use conformance::run_conformance;
pub use report::{Check, Report};
