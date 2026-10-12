//! 测试共用夹具：真实 router + 内存宿主私有存储。
//!
//! 集成测试（`tests/*.rs`）各自是独立 crate，共用逻辑放这里：用不到的夹具在
//! 单个测试 crate 里就是死代码，所以这里统一放行（不是缺陷，是分工）。
#![allow(dead_code)]

use farm_core::{AppState, HostConfig};
use farm_host::build_router;
use axum::Router;

/// 测试用 op token（静态单 token 鉴权的已知值）。
pub const OP_TOKEN: &str = "test-op-token";

/// 测试用管理 token（ticket #23）：宿主管理 API 的**独立**凭据，与 op token 不是同一个
/// （spec 新增裁决：管理 token ≠ 操作 token；交叉访问被拒）。
pub const ADMIN_TOKEN: &str = "test-admin-token";

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

/// 管理面启用的测试配置：`admin_token = Some(ADMIN_TOKEN)`，与 op token 相异。
pub fn admin_config() -> HostConfig {
    HostConfig {
        bind: "127.0.0.1:3721".parse().expect("valid bind"),
        op_token: OP_TOKEN.into(),
        admin_token: Some(ADMIN_TOKEN.into()),
        db_path: std::env::temp_dir().join("farm-host-admin-tests.db"),
    }
}

/// 管理面启用的测试状态（ticket #23 的 admin face 测试用）。
pub fn admin_state() -> AppState {
    AppState::new_in_memory(admin_config()).expect("in-memory state")
}

/// 装配好的真实 router（与 `main` 用的是同一个装配点）。
pub fn app() -> Router {
    build_router(state())
}

/// 管理面启用的真实 router（admin token 校验机制已落地，#23）。
pub fn admin_app() -> Router {
    build_router(admin_state())
}
