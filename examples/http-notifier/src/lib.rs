//! # http-notifier — 第一方通用 HTTP Notifier 插件（ticket #21）
//!
//! GLOSSARY「Notifier 插件」：通知通道的一等实现形态——**进程级 IPC 插件**，订阅事件总线、
//! **自决载体**（URL/headers/body 模板、重试、限流全住插件侧），宿主零推送域知识。它消费事件、
//! **不驱动循环**，与编排器类插件（契约客户端）是两类插件。
//!
//! 第一方只做一个通用 HTTP notifier，覆盖 ntfy/Bark/Discord/Slack 这类**裸 POST 即达**的服务；
//! Telegram 等专用格式插件留给第三方，不进 Farm 仓库。
//!
//! ## 载体自决，宿主零推送域知识
//! 本 crate 只 depend `farm-sdk`（纯契约类型）。端点 URL、请求头、body 模板、重试退避、限流
//! 全部由插件侧配置（env）决定——宿主既不知道也不存储它们（宿主只跑总线 + 插件）。宿主侧的
//! 通知通道配置（`enabled` + 过滤集）经宿主管理 API 读取，插件据此决定转发哪些关键事件；
//! 拉不到配置时回退到**默认过滤集**（[`farm_sdk::notifier::NotifierFilter::default`]）。
//!
//! ## 经事件总线订阅
//! 插件经宿主的事件总线投影读事件（ADR-0002 §4「快照 API + 增量推送」；任务允许「WS 或快照」）。
//! v1 走**快照 API**（`GET /v1/events/history`）轮询：用同一套极简 HTTP/1.1 客户端（与
//! `examples/plugin-skeleton` 同宗），按事件 id 去重，不引 WS 依赖。转发决策只认四类关键事件
//! （[`farm_sdk::events::EventKind::NOTIFICATION_CRITICAL`]），默认过滤集下 merge 完成不外发。
//!
//! ## 目标不可达 → 插件侧重试/限流，不拖垮宿主
//! 每次投递自带**有界**重试（分级退避）+ 限流（最小间隔），全在插件进程内：失败只记日志、
//! 继续下一条，既不 panic 也不无限重试。宿主跑总线、不碰推送，天然不受端点故障影响
//! （进程级隔离；插件进程崩溃另由 #19 的分级退避/隔离监督）。

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use farm_sdk::events::EventEnvelope;
use farm_sdk::handshake::AuthScheme;
use farm_sdk::notifier::{NotificationChannelConfig, NotifierFilter};
use farm_sdk::services::events::EventHistoryResponse;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// 自决载体 + 宿主连接信息（全部插件侧；宿主不知道也不存这些）。
#[derive(Clone, Debug)]
pub struct NotifierConfig {
    /// 推送端点 URL（`http://host:port/path`；v1 只做 HTTP，裸 POST 即达）。
    pub endpoint: String,
    /// 附到每次 POST 上的请求头（`Content-Type` 等由模板使用者自定）。
    pub headers: Vec<(String, String)>,
    /// body 模板（占位符见 [`render_body`]）。
    pub body_template: String,
    /// 投递重试策略（插件侧）。
    pub retry: RetryConfig,
    /// 投递限流（插件侧：两次 POST 的最小间隔）。
    pub rate_limit: RateLimitConfig,
    /// Farm 宿主地址（读事件总线 + 通道配置）。
    pub host: SocketAddr,
    /// 被编排仓库 id（决定读哪个仓库的通知通道配置）。
    pub repo_id: String,
    /// 契约面 op token（读事件总线快照）。
    pub op_token: String,
    /// 管理面 admin token（读通知通道配置；#23 独立凭据）。
    pub admin_token: String,
    /// 轮询事件总线的间隔。
    pub poll_interval: Duration,
}

/// 分级退避重试策略（插件侧，与宿主插件监督的 1s/10s/1min 同形但独立）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryConfig {
    pub max_retries: u32,
    pub base_delay: Duration,
    pub backoff_multiplier: u32,
}

/// 投递限流：两次 POST 之间的最小间隔（`Duration::ZERO` = 不限）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateLimitConfig {
    pub min_interval: Duration,
}

/// 默认 body 模板：把事件渲染成一行 JSON（payload 原样嵌入）。
pub const DEFAULT_BODY_TEMPLATE: &str =
    r#"{"kind":"{{kind}}","ticket":"{{ticket_id}}","payload":{{payload}}}"#;

impl NotifierConfig {
    /// 第 `attempt` 次重试（0 起算）前要睡多久（分级退避：base * multiplier^attempt）。
    fn backoff_for(&self, attempt: u32) -> Duration {
        let factor = self.retry.backoff_multiplier.saturating_pow(attempt);
        self.retry.base_delay.saturating_mul(factor)
    }
}

/// 某事件是否被过滤集放行（四类关键事件各一开关；闭包外一律 false）。
///
/// 宿主零推送域知识：转发哪些关键事件由宿主管理的过滤集决定（[`NotifierFilter`]），
/// 本函数只是把事件种类映射到那个开关。
pub fn should_forward(envelope: &EventEnvelope, filter: &NotifierFilter) -> bool {
    filter.enabled(envelope.kind)
}

/// 把一条模板渲染成 body：替换 `{{kind}}`/`{{id}}`/`{{ticket_id}}`/`{{attempt_id}}`/
/// `{{actor}}`/`{{occurred_at}}`/`{{payload}}`。缺失的可选字段替换成空串。
pub fn render_body(template: &str, envelope: &EventEnvelope) -> String {
    let payload = serde_json::to_string(&envelope.payload).unwrap_or_else(|_| "null".to_string());
    template
        .replace("{{kind}}", envelope.kind.as_str())
        .replace("{{id}}", &envelope.id)
        .replace("{{ticket_id}}", envelope.ticket_id.as_deref().unwrap_or(""))
        .replace("{{attempt_id}}", envelope.attempt_id.as_deref().unwrap_or(""))
        .replace("{{actor}}", &envelope.actor)
        .replace("{{occurred_at}}", &envelope.occurred_at)
        .replace("{{payload}}", &payload)
}

/// 解析拉到的通道配置成 `(enabled, filter)`；拉不到（宿主未配 / 管理面不可达）→ 用默认。
///
/// 回退到默认是**有意为之**：真 AFK 的人不能被「临时读不到配置」静默断掉 blocked 通知
/// （GLOSSARY：通知是 HITL 的前半程）。要真关通道，操作员显式设 `enabled=false`。
pub fn resolve_channel(fetched: Option<&NotificationChannelConfig>) -> (bool, NotifierFilter) {
    match fetched {
        Some(config) => (config.enabled, config.filter),
        None => (true, NotifierFilter::default()),
    }
}

/// 解析 `http://host:port/path` → `(host, port, path)`（v1 只做 HTTP；默认端口 80）。
pub fn parse_http_url(url: &str) -> Option<(String, u16, String)> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (host, port.parse::<u16>().ok()?),
        None => (authority, 80),
    };
    if host.is_empty() {
        return None;
    }
    Some((host.to_string(), port, path.to_string()))
}

/// 投递限流器：保证两次 [`RateLimiter::acquire`] 之间至少相隔 `min_interval`。
#[derive(Debug)]
pub struct RateLimiter {
    min_interval: Duration,
    last: Mutex<Option<Instant>>,
}

impl RateLimiter {
    pub fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            last: Mutex::new(None),
        }
    }

    /// 取得一次投递许可（必要时睡到最小间隔满足）。单消费者顺序投递，无并发竞争。
    pub async fn acquire(&self) {
        let wait = {
            let mut last = self.last.lock().expect("rate limiter lock");
            let now = Instant::now();
            match *last {
                Some(prev) if now.duration_since(prev) < self.min_interval => {
                    Some(self.min_interval - now.duration_since(prev))
                }
                _ => {
                    *last = Some(now);
                    None
                }
            }
        };
        if let Some(wait) = wait {
            tokio::time::sleep(wait).await;
            *self.last.lock().expect("rate limiter lock") = Some(Instant::now());
        }
    }
}

/// 投递失败（有界重试耗尽 / 端点永久拒绝）。
#[derive(Debug, PartialEq, Eq)]
pub enum DeliverError {
    /// 重试耗尽仍未成功（网络错误或 5xx）。
    Exhausted { attempts: u32 },
    /// 端点回了 4xx（永久拒绝，不重试）。
    Rejected { status: u16 },
}

impl std::fmt::Display for DeliverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeliverError::Exhausted { attempts } => {
                write!(f, "delivery failed after {attempts} attempts")
            }
            DeliverError::Rejected { status } => {
                write!(f, "endpoint rejected the notification with HTTP {status}")
            }
        }
    }
}

/// 把一条 body POST 到端点：限流 + 有界分级退避重试。2xx/3xx 即成功；4xx 不重试。
///
/// 重试与限流全在插件进程内、有界（`max_retries` 封顶）——失败既不 panic 也不无限重试，
/// 绝不拖垮宿主（进程级隔离，宿主不碰推送）。
pub async fn deliver(
    config: &NotifierConfig,
    body: &str,
    limiter: &RateLimiter,
) -> Result<(), DeliverError> {
    let mut attempt: u32 = 0;
    loop {
        limiter.acquire().await;
        match post(&config.endpoint, &config.headers, body).await {
            Ok(status) if (200..400).contains(&status) => return Ok(()),
            Ok(status) if (400..500).contains(&status) => {
                return Err(DeliverError::Rejected { status });
            }
            _ => {} // 网络错误或 5xx：落进重试
        }
        if attempt >= config.retry.max_retries {
            return Err(DeliverError::Exhausted {
                attempts: attempt + 1,
            });
        }
        tokio::time::sleep(config.backoff_for(attempt)).await;
        attempt += 1;
    }
}

/// 一次轮询：读通道配置 → 读事件总线快照 → 去重 + 过滤 + 渲染 + 投递。
///
/// 返回本次实际投递的事件 id（测试与管理面观测用）。投递失败只记日志、继续下一条。
pub async fn poll_once(
    config: &NotifierConfig,
    seen: &mut HashSet<String>,
    limiter: &RateLimiter,
) -> Vec<String> {
    let (enabled, filter) = resolve_channel(fetch_channel(config).await.as_ref());
    let envelopes = match fetch_history(config).await {
        Ok(envelopes) => envelopes,
        Err(e) => {
            eprintln!("[notifier] event bus snapshot unavailable: {e}");
            return Vec::new();
        }
    };
    let mut delivered = Vec::new();
    for envelope in &envelopes {
        if !seen.insert(envelope.id.clone()) {
            continue; // 已处理过：按事件 id 去重
        }
        if !enabled || !should_forward(envelope, &filter) {
            continue; // 通道关 或 过滤集不放行
        }
        let body = render_body(&config.body_template, envelope);
        match deliver(config, &body, limiter).await {
            Ok(()) => delivered.push(envelope.id.clone()),
            Err(e) => eprintln!("[notifier] deliver {} failed: {e}", envelope.id),
        }
    }
    delivered
}

/// 读宿主管理 API 的通知通道配置（`GET /admin/notifier/channel/{repo_id}`）。
///
/// 任何失败（不可达 / 非 200 / 形状不符）→ `None`（调用方回退默认过滤集）。
async fn fetch_channel(config: &NotifierConfig) -> Option<NotificationChannelConfig> {
    let path = format!("/admin/notifier/channel/{}", config.repo_id);
    // 管理面用**独立管理 token**（#23：X-Farm-Admin-Token，与 op token 不是同一个 header）。
    let scheme = AuthScheme::default();
    let auth = (!config.admin_token.is_empty())
        .then(|| (scheme.admin_header.as_str(), config.admin_token.as_str()));
    match http(config.host, "GET", &path, auth, None).await {
        Ok((200, body)) => serde_json::from_str(&body).ok(),
        _ => None,
    }
}

/// 读事件总线快照（`GET /v1/events/history`，有界保留窗口内按 seq 升序）。
async fn fetch_history(config: &NotifierConfig) -> Result<Vec<EventEnvelope>, String> {
    // 契约面用 op token（X-Farm-Token）。
    let scheme = AuthScheme::default();
    let auth = (!config.op_token.is_empty())
        .then(|| (scheme.http_header.as_str(), config.op_token.as_str()));
    let (status, body) = http(config.host, "GET", "/v1/events/history?limit=1000", auth, None)
        .await
        .map_err(|e| e.to_string())?;
    if status != 200 {
        return Err(format!("event history answered HTTP {status}"));
    }
    let page: EventHistoryResponse =
        serde_json::from_str(&body).map_err(|e| format!("event history is not a page: {e}"))?;
    Ok(page.events)
}

/// 极简 HTTP/1.1 客户端（`Connection: close` + 读到 EOF），返回 `(状态码, body)`。
///
/// 与 `examples/plugin-skeleton` 同宗：用最少机器证明契约/推送可达，不引 reqwest。
async fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    auth: Option<(&str, &str)>,
    body: Option<&str>,
) -> std::io::Result<(u16, String)> {
    let mut stream = TcpStream::connect(addr).await?;
    let body = body.unwrap_or("");
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    if let Some((header, value)) = auth {
        req.push_str(&format!("{header}: {value}\r\n"));
    }
    if !body.is_empty() {
        req.push_str("Content-Type: application/json\r\n");
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    stream.write_all(req.as_bytes()).await?;
    stream.flush().await?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    let status = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let payload = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    Ok((status, payload))
}

/// POST 到端点 URL（自带 host/port/path 解析 + 配置的请求头），返回 HTTP 状态码。
async fn post(endpoint: &str, headers: &[(String, String)], body: &str) -> std::io::Result<u16> {
    let (host, port, path) = parse_http_url(endpoint)
        .ok_or_else(|| std::io::Error::other(format!("bad endpoint url: {endpoint}")))?;
    let addr: SocketAddr = match format!("{host}:{port}").parse() {
        Ok(addr) => addr,
        Err(_) => {
            // host 是域名：留待未来 DNS 解析；v1 端点用 IP（裸 POST 即达的本地/内网服务）。
            return Err(std::io::Error::other(format!(
                "endpoint host {host:?} is not an IP:port (v1 supports http://IP:port/path)"
            )));
        }
    };
    let mut stream = TcpStream::connect(addr).await?;
    let mut req = format!("POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n");
    for (key, value) in headers {
        req.push_str(&format!("{key}: {value}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    stream.write_all(req.as_bytes()).await?;
    stream.flush().await?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await?;
    let text = String::from_utf8_lossy(&buf);
    Ok(text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0))
}

/// 常驻循环：反复 [`poll_once`]，直到 `shutdown` 触发（宿主关 stdin = 有序停机）。
pub async fn run(config: NotifierConfig, mut shutdown: tokio::sync::oneshot::Receiver<()>) {
    let mut seen: HashSet<String> = HashSet::new();
    let limiter = RateLimiter::new(config.rate_limit.min_interval);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = tokio::time::sleep(config.poll_interval) => {
                poll_once(&config, &mut seen, &limiter).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use farm_sdk::events::{EventKind, NewEvent};

    fn envelope(kind: EventKind, ticket: Option<&str>) -> EventEnvelope {
        let mut new = NewEvent::new(kind, "agent-runner").with_payload(serde_json::json!({"msg":"hi"}));
        if let Some(ticket) = ticket {
            new = new.with_ticket(ticket);
        }
        EventEnvelope::from_new(new, "evt-1", "2026-10-20T00:00:00Z")
    }

    #[test]
    fn the_default_filter_forwards_the_critical_closure_except_merge() {
        let filter = NotifierFilter::default();
        assert!(should_forward(&envelope(EventKind::Blocked, Some("11")), &filter));
        assert!(should_forward(&envelope(EventKind::NeedsAttention, None), &filter));
        assert!(should_forward(&envelope(EventKind::StallKill, Some("12")), &filter));
        assert!(
            !should_forward(&envelope(EventKind::Merge, Some("13")), &filter),
            "merge 完成默认不外发（GLOSSARY 默认过滤集）"
        );
        assert!(!should_forward(&envelope(EventKind::Dispatch, None), &filter), "闭包外不过滤");
    }

    #[test]
    fn turning_on_merge_complete_in_the_filter_forwards_it() {
        let filter = NotifierFilter { merge_complete: true, ..NotifierFilter::default() };
        assert!(should_forward(&envelope(EventKind::Merge, Some("13")), &filter));
    }

    #[test]
    fn a_template_renders_the_event_fields_into_the_body() {
        let body = render_body(DEFAULT_BODY_TEMPLATE, &envelope(EventKind::Blocked, Some("11")));
        assert_eq!(body, r#"{"kind":"blocked","ticket":"11","payload":{"msg":"hi"}}"#);
        // 缺失的可选字段替换成空串，不炸。
        let body = render_body("t={{ticket_id}} a={{attempt_id}}", &envelope(EventKind::Blocked, None));
        assert_eq!(body, "t= a=");
    }

    #[test]
    fn a_channel_config_that_cannot_be_fetched_falls_back_to_the_default_filter() {
        let (enabled, filter) = resolve_channel(None);
        assert!(enabled, "读不到配置也不能静默断掉通知");
        assert_eq!(filter, NotifierFilter::default());
        let fetched = NotificationChannelConfig {
            repo_id: "repo-a".into(),
            enabled: false,
            filter: NotifierFilter::default(),
            updated_at: "t".into(),
        };
        let (enabled, _) = resolve_channel(Some(&fetched));
        assert!(!enabled, "显式关闭要被尊重");
    }

    #[test]
    fn an_http_url_parses_into_host_port_and_path() {
        assert_eq!(
            parse_http_url("http://127.0.0.1:8080/notify"),
            Some(("127.0.0.1".to_string(), 8080, "/notify".to_string()))
        );
        assert_eq!(
            parse_http_url("http://10.0.0.2/hook"),
            Some(("10.0.0.2".to_string(), 80, "/hook".to_string()))
        );
        assert_eq!(parse_http_url("https://x/y"), None, "v1 只做 http://");
        assert_eq!(parse_http_url("http:///nope"), None);
    }
}
