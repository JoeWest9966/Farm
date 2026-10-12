//! # Agent-Runner 本地后端（ticket #16）
//!
//! GLOSSARY「Agent-Runner」五服务之一：以统一接口派发与管理 agent。本模块是**本地后端**——
//! 直接 spawn `pi --mode rpc` 子进程、持有全部生命周期（ADR-0004 决策 1：agent-runner
//! 自 spawn、自监督），并把判据阶梯（GLOSSARY「判据阶梯」）完整落地：
//!
//! ```text
//! 读循环健康度前置 → 两口钟（静默钟 + 总钟）→ abort 探针（30s 独立死线）→ fork 找回
//! 完成判定双条件：agent_settled 且最后一条权威 assistant 消息 stopReason === "stop"
//! ```
//!
//! **版本偏斜**：规格钉 1.0.2（ADR-0004 决策 5），本机安装的是 0.84.3。本模块按
//! `docs/research/pi-runtime-interface.md` 文档化的 **1.0.2 wire protocol** 实现，测试跑在
//! fake pi 替身（`src/bin/fake-pi.rs`）之上；真 pi 走启动探测 + 记录偏差，**不**因版本
//! 不符而失败。
//!
//! pi 子进程**不是插件**：归判据阶梯管，不进插件重试策略（ADR-0004 决策 4 末句）。
//!
//! 子模块按接缝切分：`rpc`（帧定界与线协议）→ `process`（spawn + 版本探测）→
//! `accounting`（token 绝对累计）→ `ladder`（判据阶梯）→ `store`（attempts 持久化）→
//! `service`（派发/steer/abort 与事件回流）→ `credentials`（凭据注入接缝，#17）。

pub mod accounting;
pub mod credentials;
pub mod ladder;
pub mod process;
pub mod rpc;

/// fake pi RPC 测试替身的路径（测试与 #26 一致性套件共用）。
///
/// 优先 `FARM_FAKE_PI` 环境变量；否则按 cargo 布局推断（集成测试二进制在
/// `target/<profile>/deps/`，替身二进制在 `target/<profile>/fake-pi`）。
pub fn fake_pi_path() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("FARM_FAKE_PI") {
        return std::path::PathBuf::from(path);
    }
    let mut path = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("."));
    path.pop(); // 去掉测试二进制名 → target/<profile>/deps
    if path.ends_with("deps") {
        path.pop(); // → target/<profile>
    }
    path.join("fake-pi")
}
