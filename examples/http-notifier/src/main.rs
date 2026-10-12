//! http-notifier 二进制入口：进程级 IPC 插件（ticket #21，GLOSSARY「Notifier 插件」）。
//!
//! 宿主拉起本进程、持有其 stdin（关 stdin = 有序停机，父死子陪葬，#19）。本插件：
//! 1. 向 stdout 打一行 `hello` 握手自描述（#19 的 `PluginHost` 等这一行才认握手成功）；
//! 2. 从 env 读**自决载体**（URL/headers/body 模板/重试/限流）+ 宿主连接信息；
//! 3. 订阅事件总线快照、按宿主管理的过滤集把关键事件 POST 到端点（插件侧重试/限流）；
//! 4. 见到 stdin EOF（宿主停机）即有序退出。
//!
//! **宿主零推送域知识**：URL/模板/重试/限流全在本进程侧（env），宿主只跑总线 + 本插件。
//! 转发哪些关键事件由宿主管理的通道配置（[`http_notifier::fetch`] 走宿主管理 API）决定。

use std::io::Write;
use std::net::SocketAddr;
use std::time::Duration;

use http_notifier::{run, NotifierConfig, RateLimitConfig, RetryConfig, DEFAULT_BODY_TEMPLATE};
use tokio::io::{AsyncReadExt, BufReader};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_usize(key: &str, default: usize) -> usize {
    env_or(key, &default.to_string()).parse().unwrap_or(default)
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    // 1) 握手自描述（#19 监督循环等这行 hello；显式 flush，管道下也要立刻送达）。
    {
        let mut out = std::io::stdout();
        let _ = writeln!(
            out,
            r#"{{"t":"hello","plugin":"http-notifier","name":"通用 HTTP notifier","contract_version":"1.0.0","kind":"plugin"}}"#
        );
        let _ = out.flush();
    }

    // 2) 自决载体 + 宿主连接（全部插件侧 env；宿主不知道也不存这些）。
    let host: SocketAddr = env_or("FARM_HOST", "127.0.0.1:3721")
        .parse()
        .expect("FARM_HOST must be host:port");
    let endpoint = env_or("FARM_NOTIFIER_URL", "");
    let headers = parse_headers(&env_or("FARM_NOTIFIER_HEADERS", ""));
    let template = env_or("FARM_NOTIFIER_TEMPLATE", "");
    let body_template = if template.trim().is_empty() {
        DEFAULT_BODY_TEMPLATE.to_string()
    } else {
        template
    };
    let config = NotifierConfig {
        endpoint,
        headers,
        body_template,
        retry: RetryConfig {
            max_retries: env_usize("FARM_NOTIFIER_MAX_RETRIES", 3) as u32,
            base_delay: Duration::from_millis(env_usize("FARM_NOTIFIER_BASE_DELAY_MS", 1000) as u64),
            backoff_multiplier: env_usize("FARM_NOTIFIER_BACKOFF_MULTIPLIER", 2) as u32,
        },
        rate_limit: RateLimitConfig {
            min_interval: Duration::from_millis(env_usize("FARM_NOTIFIER_MIN_INTERVAL_MS", 0) as u64),
        },
        host,
        repo_id: env_or("FARM_NOTIFIER_REPO", "default"),
        op_token: env_or("FARM_TOKEN", ""),
        admin_token: env_or("FARM_ADMIN_TOKEN", ""),
        poll_interval: Duration::from_millis(env_usize("FARM_NOTIFIER_POLL_MS", 2000) as u64),
    };

    if config.endpoint.is_empty() {
        eprintln!("[notifier] FARM_NOTIFIER_URL 未设：无推送端点，插件只订阅不外发");
    }

    // 3) stdin EOF 监察：宿主关 stdin = 有序停机（#19；本骨架不接任何指令）。
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let mut reader = BufReader::new(tokio::io::stdin());
        let mut buf = [0u8; 64];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => break,     // EOF：宿主关了 stdin
                Ok(_) => continue,  // 忽略任何输入（插件不驱动循环、不接指令）
                Err(_) => break,
            }
        }
        let _ = shutdown_tx.send(());
    });

    // 4) 常驻：订阅 + 过滤 + 投递，直到 stdin EOF。
    run(config, shutdown_rx).await;
    eprintln!("[notifier] stdin EOF：有序停机");
}

/// 解析 `K:V,K:V` 形式的请求头清单（空段忽略）。
fn parse_headers(spec: &str) -> Vec<(String, String)> {
    spec.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|pair| {
            let (key, value) = pair.split_once(':')?;
            Some((key.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}
