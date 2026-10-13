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
        // `FARM_BIND` 解析不了：落回安全的回环默认，并且**说一声**（code review P2-12）。
        // 原先 `.ok()` 把解析失败和「没设」混成同一条静默回退——运营者打错一个字符
        // （`0.0.0.0:372` / `localhost:3721`）时宿主照样起，只是绑在回环上，而日志里
        // 一个字都没有。安全的默认值不該是静默的默认值。
        let bind = match std::env::var("FARM_BIND") {
            Ok(raw) if !raw.trim().is_empty() => match raw.trim().parse::<SocketAddr>() {
                Ok(addr) => addr,
                Err(_) => {
                    tracing::warn!(
                        farm_bind = %raw,
                        fallback = %DEFAULT_BIND,
                        "FARM_BIND is not a valid socket address; \
                         falling back to the loopback default"
                    );
                    default_bind()
                }
            },
            _ => default_bind(),
        };

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

/// 绑定地址的默认值：回环，不对外（ADR：Farm 永不自带公网暴露）。
pub const DEFAULT_BIND: &str = "127.0.0.1:3721";

/// 默认绑定地址（`DEFAULT_BIND` 的解析形态；字面量写死，不会失败）。
fn default_bind() -> SocketAddr {
    DEFAULT_BIND.parse().expect("default bind is valid")
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// 一块测试间共享的日志缓冲（捕获 `tracing` 输出用）。
    #[derive(Clone, Default)]
    struct SharedLog(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for SharedLog {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("log lock").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// 在「日志被捕获」的前提下跑一段代码，回传 `(结果, 捕获到的日志文本)`。
    fn capture<T>(f: impl FnOnce() -> T) -> (T, String) {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_writer({
                let log = Arc::clone(&log);
                move || SharedLog(Arc::clone(&log))
            })
            .with_max_level(tracing::Level::WARN)
            .finish();
        let value = tracing::subscriber::with_default(subscriber, f);
        let written = String::from_utf8_lossy(&log.lock().expect("log lock")).into_owned();
        (value, written)
    }

    /// `FARM_BIND` 与 `FARM_TOKEN` 的整个装载契约（code review P2-12）。
    ///
    /// 三个场景写在**一个**测试里：它们都要改进程 env，而 `cargo test --lib` 的单测是
    /// 并行跑的——拆成多个 `#[test]` 就是数据竞争（互相 remove_var）。
    #[test]
    fn the_bind_and_token_env_contract() {
        // 1) 解析不了的 FARM_BIND：落回安全的回环默认，并且**说一声**。
        std::env::set_var("FARM_BIND", "localhost:3721"); // 少了 127.0.0.1 → 解析不了
        std::env::set_var("FARM_TOKEN", "config-test-token");
        let (config, written) = capture(|| HostConfig::from_env().expect("token is set"));
        assert_eq!(
            config.bind,
            default_bind(),
            "解析不了的 FARM_BIND 落回安全的回环默认"
        );
        assert_eq!(config.bind.to_string(), DEFAULT_BIND);
        assert_eq!(config.op_token, "config-test-token", "别的字段不受影响");
        assert!(
            written.contains("FARM_BIND") && written.contains(DEFAULT_BIND),
            "回退必须留一声告警（运营者的错别字不该静默）：{written}"
        );

        // 2) 合法值照用、不告警；压根没设也不告警（缺省不是错别字）。
        std::env::set_var("FARM_BIND", "0.0.0.0:4999");
        let (config, written) = capture(|| HostConfig::from_env().expect("token is set"));
        assert_eq!(config.bind.to_string(), "0.0.0.0:4999", "合法值必须被采纳");
        assert!(written.is_empty(), "合法值不该告警：{written}");

        std::env::remove_var("FARM_BIND");
        let (config, written) = capture(|| HostConfig::from_env().expect("token is set"));
        assert_eq!(config.bind, default_bind(), "没设就是缺省");
        assert!(written.is_empty(), "没设 FARM_BIND 不是错别字，不该告警：{written}");

        // 3) 无 token 仍必须报错（不安全的默认鉴权没有商量余地）。
        std::env::remove_var("FARM_TOKEN");
        let err = HostConfig::from_env().expect_err("无 token 必须报错");
        assert!(
            err.to_string().contains("FARM_TOKEN is required"),
            "{err}"
        );
        std::env::remove_var("FARM_BIND");
        std::env::remove_var("FARM_TOKEN");
    }
}
