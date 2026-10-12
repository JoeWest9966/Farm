//! 宿主配置（#11 骨架）：绑定地址、操作 token、管理 token 接口位、DB 路径。
//!
//! 安全与权限模型（spec 新增裁决）：**默认绑回环**、**单 token 静态鉴权** +
//! **独立管理 token**。本票只落地操作 token 与管理 token 的接口位；管理 token
//! 的校验机制归 #23（宿主管理 API）。插件 v1 信任安装者、不沙箱；agent 无沙箱、
//! 仅 worktree 物理边界；Farm 永不自带公网暴露。

use std::net::SocketAddr;
use std::path::PathBuf;

use crate::error::CoreError;

/// 宿主运行时配置。
#[derive(Clone, Debug)]
pub struct HostConfig {
    /// HTTP/WS 绑定地址。默认 `127.0.0.1:3721`（回环，不对外）。
    pub bind: SocketAddr,
    /// 操作 token：契约 API 与 WS 握手的单一静态鉴权凭据（必填，无默认值）。
    pub op_token: String,
    /// 管理 token：宿主管理 API 的独立鉴权凭据。本票仅留接口位（`Option`）；
    /// 独立校验归 #23。`None` 表示管理面本票未启用。
    pub admin_token: Option<String>,
    /// 宿主私有存储路径（ADR-0005 §7 全局单库）。
    pub db_path: PathBuf,
}

impl HostConfig {
    /// 从环境变量装载：
    /// - `FARM_BIND`（默认 `127.0.0.1:3721`）
    /// - `FARM_TOKEN`（操作 token，**必填**——无默认值，避免不安全的默认鉴权）
    /// - `FARM_ADMIN_TOKEN`（管理 token，可选；接口位）
    /// - `FARM_DB`（默认 `$XDG_DATA_HOME/farm/farm.db`）
    pub fn from_env() -> Result<Self, CoreError> {
        let bind = std::env::var("FARM_BIND")
            .ok()
            .and_then(|s| s.parse::<SocketAddr>().ok())
            .unwrap_or_else(|| "127.0.0.1:3721".parse().expect("default bind is valid"));

        let op_token = match std::env::var("FARM_TOKEN") {
            Ok(t) if !t.is_empty() => t,
            _ => {
                return Err(CoreError::Config(
                    "FARM_TOKEN is required (single-token static auth; no insecure default)".into(),
                ))
            }
        };

        let admin_token = std::env::var("FARM_ADMIN_TOKEN").ok().filter(|t| !t.is_empty());

        let db_path = match std::env::var("FARM_DB") {
            Ok(p) if !p.is_empty() => PathBuf::from(p),
            _ => default_db_path(),
        };

        Ok(Self {
            bind,
            op_token,
            admin_token,
            db_path,
        })
    }
}

/// 默认 DB 路径：`$XDG_DATA_HOME/farm/farm.db`，回退 `$HOME/.local/share/farm/farm.db`。
pub fn default_db_path() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("farm").join("farm.db");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join(".local").join("share").join("farm").join("farm.db");
        }
    }
    PathBuf::from("farm.db")
}
