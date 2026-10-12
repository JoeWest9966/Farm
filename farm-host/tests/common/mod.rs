//! 测试共用夹具：真实 router + 内存宿主私有存储。
//!
//! 集成测试（`tests/*.rs`）各自是独立 crate，共用逻辑放这里。

use farm_core::{AppState, HostConfig};
use farm_host::build_router;
use axum::Router;

/// 测试用 op token（静态单 token 鉴权的已知值）。
pub const OP_TOKEN: &str = "test-op-token";

/// 测试用宿主配置：回环地址 + 已知 token + 内存库（不落盘）。
pub fn config() -> HostConfig {
    HostConfig {
        bind: "127.0.0.1:3721".parse().expect("valid bind"),
        op_token: OP_TOKEN.into(),
        admin_token: None,
        db_path: std::env::temp_dir().join("farm-host-tests.db"),
    }
}

/// 测试用宿主状态（内存事件总线 + 内存 SQLite）。
pub fn state() -> AppState {
    AppState::new_in_memory(config()).expect("in-memory state")
}

/// 装配好的真实 router（与 `main` 用的是同一个装配点）。
pub fn app() -> Router {
    build_router(state())
}
