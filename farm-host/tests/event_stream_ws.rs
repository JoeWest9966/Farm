//! WS 投影通路端到端（ticket #15 验收 §2）：新订阅者先拿快照再接增量，无丢失无重复。
//!
//! 用真实 TCP + tokio-tungstenite 客户端打 `GET /v1/events/stream`，验证据
//! `events_ws.rs` 的三段式：先订阅 → 取快照 → 接增量（按事件 id 去重）。

use std::net::SocketAddr;

use farm_core::{AppState, HostConfig};
use farm_host::build_router;
use futures_util::StreamExt;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

const OP_TOKEN: &str = "test-op-token";

fn state() -> AppState {
    AppState::new_in_memory(HostConfig {
        bind: "127.0.0.1:0".parse().expect("bind"),
        op_token: OP_TOKEN.into(),
        admin_token: None,
        db_path: std::env::temp_dir().join("farm-host-ws-e2e.db"),
    })
    .expect("in-memory state")
}

/// 起一个真实 axum 服务，返回其地址。
async fn spawn_host(state: AppState) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let app = build_router(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    addr
}

/// 读下一帧文本并解析为 JSON。
async fn next_json<S>(ws: &mut S) -> serde_json::Value
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        match ws.next().await.expect("frame").expect("ok frame") {
            Message::Text(t) => return serde_json::from_str(&t).expect("json frame"),
            other => panic!("expected text frame, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_new_subscriber_gets_the_snapshot_then_increments_without_loss_or_duplication() {
    let state = state();
    // 连接前先发两条 → 它们进事件日志，构成新订阅者的快照。
    let pre = [
        state.publish(farm_sdk::NewEvent::new(farm_sdk::EventKind::Blocked, "agent-runner").with_ticket("t0")).expect("publish"),
        state.publish(farm_sdk::NewEvent::new(farm_sdk::EventKind::Dispatch, "agent-runner").with_ticket("t1")).expect("publish"),
    ];

    let addr = spawn_host(state.clone()).await;

    let mut req = format!("ws://{addr}/v1/events/stream")
        .into_client_request()
        .expect("request");
    req.headers_mut().insert(
        "x-farm-token",
        OP_TOKEN.parse().expect("header value"),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req)
        .await
        .expect("ws upgrade with token");

    // 1) hello 帧自描述。
    let hello = next_json(&mut ws).await;
    assert!(hello.get("contract_version").is_some(), "hello 帧：{hello}");

    // 2) 快照：连接前的两条，按 seq 升序，无丢失。
    let snap0 = next_json(&mut ws).await;
    let snap1 = next_json(&mut ws).await;
    assert_eq!(snap0["id"], pre[0].id, "快照第一条 = 最早发布");
    assert_eq!(snap1["id"], pre[1].id, "快照第二条");

    // 3) 快照已确认订阅活着 → 现在发两条增量。
    let inc = [
        state.publish(farm_sdk::NewEvent::new(farm_sdk::EventKind::NeedsAttention, "agent-runner").with_ticket("t2")).expect("publish"),
        state.publish(farm_sdk::NewEvent::new(farm_sdk::EventKind::Merge, "host").with_ticket("t3")).expect("publish"),
    ];
    let inc0 = next_json(&mut ws).await;
    let inc1 = next_json(&mut ws).await;
    assert_eq!(inc0["id"], inc[0].id, "增量无丢失");
    assert_eq!(inc1["id"], inc[1].id, "增量无丢失");

    // 无重复：全部事件 id 互异。
    let ids: Vec<String> = [snap0["id"].as_str(), snap1["id"].as_str(), inc0["id"].as_str(), inc1["id"].as_str()]
        .into_iter()
        .map(|s| s.expect("id").to_string())
        .collect();
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "无重复：{ids:?}");

    let _ = ws.close(None).await;
}

#[tokio::test]
async fn the_ws_upgrade_is_rejected_without_the_token() {
    let addr = spawn_host(state()).await;
    let req = format!("ws://{addr}/v1/events/stream")
        .into_client_request()
        .expect("request");
    // 不带 token：upgrade 应被中间件拒（非 101）。
    let result = tokio_tungstenite::connect_async(req).await;
    assert!(result.is_err(), "无 token 的 WS upgrade 必须被拒");
}
