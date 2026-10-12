//! # farm-core `workflow` 模块 — Workflow 配置装载 + last-known-good（LKG）。
//!
//! GLOSSARY「Workflow 配置」：住在被编排仓库里的机器可读调度策略，消费者是编排器的
//! 确定性循环。宿主（本模块）负责**读取、校验、保 LKG、告警**：
//!
//! - [`config`]：读取 + 校验（合法 → `WorkflowConfig`，非法 → `ConfigError`）。宿主私有，不进契约。
//! - （LKG 落库与服务句柄在后续切片追加。）
//!
//! 域**类型**（`WorkflowConfig` / `WorkflowConfigStatus`）在 `farm-sdk`（查询 API 暴露它们）；
//! 本模块只放宿主私有行为。

pub mod config;

pub use config::{load_from_repo, parse_and_validate, validate, ConfigError};
