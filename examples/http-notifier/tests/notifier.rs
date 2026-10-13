//! http-notifier 插件的接缝测试（ticket #21）——全部经**真实 HTTP**打接缝。
//!
//! fake 服务器（[`common`]）同时扮三个角色：事件总线快照（`GET /v1/events/history`）、
//! 通知通道配置（`GET /admin/notifier/channel/{repo}`）、推送端点（`POST /notify`）。
//! 覆盖验收：① 订阅总线关键事件并按模板 POST 到 fake 端点；② 默认过滤集生效，merge 完成默认不发；
//! ③ 目标不可达时插件侧重试/限流，有界不拖垮宿主；④ 通道配置经管理 API 可改（改后即影响转发）。

mod common;

use std::time::Duration;

use common::{FakeHttp, RecordedRequest};
use http_notifier::{
    deliver, poll_once, NotifierConfig, RateLimitConfig, RateLimiter, RetryConfig, DeliverError,
    DEFAULT_BODY_TEMPLATE,
};

/// 事件总线快照（独立于实现的真相源：四条关键事件，含一条 merge 完成）。
const HISTORY: &str = r#"{"events":[
  {"id":"e1","kind":"blocked","occurred_at":"2026-10-20T00:00:00Z","actor":"agent-runner","ticket_id":"11","payload":{"reason":"stuck"}},
  {"id":"e2","kind":"needs_attention","occurred_at":"2026-10-20T00:00:01Z","actor":"host","payload":{"reason":"cred"}},
  {"id":"e3","kind":"merge","occurred_at":"2026-10-20T00:00:02Z","actor":"merge","ticket_id":"13","payload":{"commit":"abc"}},
  {"id":"e4","kind":"stall_kill","occurred_at":"2026-10-20T00:00:03Z","actor":"agent-runner","ticket_id":"12","payload":{}}
],"truncated":false}"#;

/// 默认过滤集的通道配置（merge 完成关）。
const CHANNEL_DEFAULT: &str = r#"{"repo_id":"repo-a","enabled":true,"filter":{"blocked":true,"needs_attention":true,"stall":true,"merge_complete":false},"updated_at":"2026-10-20T00:00:00Z"}"#;

/// 打开 merge 完成的通道配置。
const CHANNEL_MERGE_ON: &str = r#"{"repo_id":"repo-a","enabled":true,"filter":{"blocked":true,"needs_attention":true,"stall":true,"merge_complete":true},"updated_at":"2026-10-20T00:00:00Z"}"#;

fn config_for(fake: &FakeHttp) -> NotifierConfig {
    NotifierConfig {
        endpoint: format!("http://{}/notify", fake.addr),
        headers: vec![("Content-Type".to_string(), "application/json".to_string())],
        body_template: DEFAULT_BODY_TEMPLATE.to_string(),
        retry: RetryConfig {
            max_retries: 3,
            base_delay: Duration::from_millis(1),
            backoff_multiplier: 2,
        },
        rate_limit: RateLimitConfig { min_interval: Duration::ZERO },
        host: fake.addr,
        repo_id: "repo-a".to_string(),
        op_token: "op-token".to_string(),
        admin_token: "admin-token".to_string(),
        poll_interval: Duration::from_millis(20),
    }
}

/// 从 POST body 里取 `kind` 字段。
fn kinds(posts: &[RecordedRequest]) -> Vec<String> {
    posts
        .iter()
        .filter_map(|p| {
            serde_json::from_str::<serde_json::Value>(&p.body)
                .ok()
                .and_then(|v| v.get("kind").and_then(|k| k.as_str()).map(str::to_string))
        })
        .collect()
}

#[tokio::test]
async fn it_subscribes_to_the_bus_and_posts_matching_events_to_the_endpoint() {
    // 验收①：订阅总线关键事件并按模板 POST 到 fake 端点。
    let fake = FakeHttp::start(|req| match (req.method.as_str(), req.path.as_str()) {
        ("GET", p) if p.starts_with("/v1/events/history") => (200, HISTORY.to_string()),
        ("GET", "/admin/notifier/channel/repo-a") => (200, CHANNEL_DEFAULT.to_string()),
        ("POST", _) => (200, "{}".to_string()),
        _ => (404, "{}".to_string()),
    })
    .await;

    let config = config_for(&fake);
    let mut seen = std::collections::HashSet::new();
    let limiter = RateLimiter::new(Duration::ZERO);
    let delivered = poll_once(&config, &mut seen, &limiter).await;

    let posts = fake.posts();
    let forwarded = kinds(&posts);
    assert_eq!(delivered.len(), 3, "投递了三条关键事件：{forwarded:?}");
    assert!(forwarded.contains(&"blocked".to_string()), "blocked 必须外发：{forwarded:?}");
    assert!(forwarded.contains(&"needs_attention".to_string()));
    assert!(forwarded.contains(&"stall_kill".to_string()));
    assert!(
        !forwarded.contains(&"merge".to_string()),
        "默认过滤集下 merge 完成不外发：{forwarded:?}"
    );
    // body 按模板渲染（带票 id 与 payload）。
    let blocked = posts.iter().find(|p| p.body.contains("\"blocked\"")).expect("blocked post");
    assert!(blocked.body.contains("\"ticket\":\"11\""), "模板渲染票 id：{}", blocked.body);
    assert!(blocked.body.contains("\"reason\":\"stuck\""), "模板嵌入 payload：{}", blocked.body);
    assert!(
        blocked.headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("content-type") && v == "application/json"),
        "POST 带配置的 Content-Type 头：{:?}",
        blocked.headers
    );
    // 端点确实经 HTTP 被打到（bus + config 也都读过）。
    assert!(!fake.gets("/v1/events/history").is_empty(), "读过事件总线快照");
    assert!(!fake.gets("/admin/notifier/channel").is_empty(), "读过通道配置");
}

#[tokio::test]
async fn the_default_filter_set_does_not_send_merge_complete() {
    // 验收②：默认过滤集生效，merge 完成默认不发（经完整轮询回路）。
    let fake = FakeHttp::start(|req| match (req.method.as_str(), req.path.as_str()) {
        ("GET", p) if p.starts_with("/v1/events/history") => (200, HISTORY.to_string()),
        ("GET", "/admin/notifier/channel/repo-a") => (200, CHANNEL_DEFAULT.to_string()),
        ("POST", _) => (200, "{}".to_string()),
        _ => (404, "{}".to_string()),
    })
    .await;
    let config = config_for(&fake);
    let mut seen = std::collections::HashSet::new();
    poll_once(&config, &mut seen, &RateLimiter::new(Duration::ZERO)).await;
    assert!(
        !fake.posts().iter().any(|p| p.body.contains("\"merge\"")),
        "merge 完成默认不发"
    );
}

#[tokio::test]
async fn an_operator_can_turn_on_merge_complete_and_it_gets_forwarded() {
    // 通道配置经管理 API 可改 → 改后即影响插件转发（merge 完成被显式打开）。
    let fake = FakeHttp::start(|req| match (req.method.as_str(), req.path.as_str()) {
        ("GET", p) if p.starts_with("/v1/events/history") => (200, HISTORY.to_string()),
        ("GET", "/admin/notifier/channel/repo-a") => (200, CHANNEL_MERGE_ON.to_string()),
        ("POST", _) => (200, "{}".to_string()),
        _ => (404, "{}".to_string()),
    })
    .await;
    let config = config_for(&fake);
    let mut seen = std::collections::HashSet::new();
    poll_once(&config, &mut seen, &RateLimiter::new(Duration::ZERO)).await;
    let forwarded = kinds(&fake.posts());
    assert!(forwarded.contains(&"merge".to_string()), "显式打开后 merge 完成也外发：{forwarded:?}");
    assert_eq!(forwarded.len(), 4, "四类关键事件全发");
}

#[tokio::test]
async fn an_unreachable_channel_config_falls_back_to_the_default_filter() {
    // 通道配置端点不可达（404）→ 回退默认过滤集，仍不外发 merge、不漏 blocked。
    let fake = FakeHttp::start(|req| match (req.method.as_str(), req.path.as_str()) {
        ("GET", p) if p.starts_with("/v1/events/history") => (200, HISTORY.to_string()),
        ("POST", _) => (200, "{}".to_string()),
        _ => (404, "{}".to_string()), // /admin/notifier/channel/* 落到这里
    })
    .await;
    let config = config_for(&fake);
    let mut seen = std::collections::HashSet::new();
    poll_once(&config, &mut seen, &RateLimiter::new(Duration::ZERO)).await;
    let forwarded = kinds(&fake.posts());
    assert_eq!(forwarded.len(), 3, "回退默认过滤集：三条关键事件");
    assert!(!forwarded.contains(&"merge".to_string()));
}

#[tokio::test]
async fn an_unreachable_endpoint_is_retried_with_backoff_without_hanging() {
    // 验收③：端点持续 5xx → 有界分级退避重试，重试耗尽报错，不 panic 不无限重试。
    let fake = FakeHttp::start(|req| match req.method.as_str() {
        "POST" => (500, "{}".to_string()),
        _ => (404, "{}".to_string()),
    })
    .await;
    let config = NotifierConfig {
        retry: RetryConfig {
            max_retries: 2,
            base_delay: Duration::from_millis(1),
            backoff_multiplier: 2,
        },
        ..config_for(&fake)
    };
    let started = std::time::Instant::now();
    let result = deliver(&config, "{}", &RateLimiter::new(Duration::ZERO)).await;
    assert_eq!(
        result,
        Err(DeliverError::Exhausted { attempts: 3 }),
        "重试耗尽（1 + 2 次重试 = 3 次尝试）后报错"
    );
    assert_eq!(fake.posts().len(), 3, "确实重试了：3 次 POST");
    assert!(started.elapsed() < Duration::from_secs(5), "有界：不挂起（{:?}）", started.elapsed());
}

#[tokio::test]
async fn delivery_is_rate_limited_on_the_plugin_side() {
    // 验收③：两次投递被限流（最小间隔），全在插件进程内。
    let fake = FakeHttp::start(|req| match req.method.as_str() {
        "POST" => (200, "{}".to_string()),
        _ => (404, "{}".to_string()),
    })
    .await;
    let min_interval = Duration::from_millis(150);
    let config = NotifierConfig {
        rate_limit: RateLimitConfig { min_interval },
        ..config_for(&fake)
    };
    let limiter = RateLimiter::new(min_interval);
    deliver(&config, "{}", &limiter).await.expect("first");
    deliver(&config, "{}", &limiter).await.expect("second");
    let posts = fake.posts();
    assert_eq!(posts.len(), 2);
    let gap = posts[1].received_at.duration_since(posts[0].received_at);
    assert!(
        gap >= Duration::from_millis(100),
        "两次投递被限流间隔开：{gap:?}（min_interval={min_interval:?}）"
    );
}

#[tokio::test]
async fn the_bus_read_uses_the_op_token_and_the_config_read_uses_the_admin_token() {
    // #23：契约面（事件总线）用 op token（X-Farm-Token），管理面（通道配置）用**独立**管理
    // token（X-Farm-Admin-Token）。两者不是同一个 header——插件分别带上，别用错。
    let fake = FakeHttp::start(|req| match (req.method.as_str(), req.path.as_str()) {
        ("GET", p) if p.starts_with("/v1/events/history") => (200, HISTORY.to_string()),
        ("GET", "/admin/notifier/channel/repo-a") => (200, CHANNEL_DEFAULT.to_string()),
        ("POST", _) => (200, "{}".to_string()),
        _ => (404, "{}".to_string()),
    })
    .await;
    let config = config_for(&fake);
    let mut seen = std::collections::HashSet::new();
    poll_once(&config, &mut seen, &RateLimiter::new(Duration::ZERO)).await;

    let has_header = |req: &RecordedRequest, name: &str, value: &str| {
        req.headers.iter().any(|(k, v)| k.eq_ignore_ascii_case(name) && v == value)
    };
    let history = fake.gets("/v1/events/history").into_iter().next().expect("history read");
    assert!(has_header(&history, "x-farm-token", "op-token"), "总线用 op token：{:?}", history.headers);
    let channel = fake.gets("/admin/notifier/channel").into_iter().next().expect("config read");
    assert!(
        has_header(&channel, "x-farm-admin-token", "admin-token"),
        "通道配置用管理 token：{:?}",
        channel.headers
    );
    assert!(!has_header(&channel, "x-farm-token", "admin-token"), "管理 token 不递在 op header 上");
}

#[tokio::test]
async fn a_four_xx_from_the_endpoint_is_not_retried() {
    // 4xx = 端点永久拒绝（如坏请求），不重试、立即报错。
    let fake = FakeHttp::start(|req| match req.method.as_str() {
        "POST" => (400, "{}".to_string()),
        _ => (404, "{}".to_string()),
    })
    .await;
    let config = config_for(&fake);
    let result = deliver(&config, "{}", &RateLimiter::new(Duration::ZERO)).await;
    assert_eq!(result, Err(DeliverError::Rejected { status: 400 }));
    assert_eq!(fake.posts().len(), 1, "4xx 不重试：只 POST 一次");
}
