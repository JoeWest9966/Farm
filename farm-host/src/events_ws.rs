//! WS 事件流（ticket #11）：`GET /v1/events/stream`。
//!
//! ADR-0003 决策 3：面板实时通路是 WebSocket，**单连接订阅事件总线**；操作
//! （claim/steer/abort）走契约 HTTP API，WS 只承事件投影流。
//!
//! 本票只打通「鉴权 + 订阅内存总线 + 转发信封」这一段：upgrade 前的 token 校验
//! 由契约面中间件统一完成（与 HTTP 同一形状的 `unauthorized`），upgrade 成功后
//! 先回一帧 [`WsHello`] 自描述，再把 [`farm_core::EventBus`] 上的
//! [`EventEnvelope`] 以 JSON 文本帧转发。持久化事件日志、历史查询与真正的投影
//! 服务归 #15；审计双写（Kron）归 #20。

use axum::extract::State;
use axum::extract::ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use farm_core::AppState;
use farm_sdk::events::WsHello;
use farm_sdk::{EventKind, CONTRACT_VERSION};
use futures_util::{SinkExt, StreamExt};

/// WS upgrade 入口。token 已在中间件里验过；这里只负责升级与订阅。
pub async fn events_stream(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| async move {
        serve_subscription(socket, state).await;
    })
}

/// 一条订阅的生命周期：hello 帧 → 转发总线信封 → 对端关闭或总线消失即收尾。
async fn serve_subscription(socket: WebSocket, state: AppState) {
    let mut receiver = state.events.subscribe();
    let (mut sink, mut incoming) = socket.split();

    // 先回自描述：编排器/面板据此确认契约版本与可订阅的事件闭集。
    let hello = WsHello::new(CONTRACT_VERSION, EventKind::ALL.to_vec());
    let hello_json = serde_json::to_string(&hello).expect("WsHello must serialize");
    if sink
        .send(Message::Text(Utf8Bytes::from(hello_json)))
        .await
        .is_err()
    {
        return; // 对端在 hello 之前就消失了
    }

    loop {
        tokio::select! {
            event = receiver.recv() => match event {
                Ok(envelope) => {
                    let json = serde_json::to_string(&envelope)
                        .expect("EventEnvelope must serialize");
                    if sink.send(Message::Text(Utf8Bytes::from(json))).await.is_err() {
                        break; // 订阅者掉线
                    }
                }
                // 内存总线容量溢出：丢事件但不掉线（持久化与补账归 #15/#20）。
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::debug!(skipped, "ws subscriber lagged behind the in-memory event bus");
                }
                // 总线关闭：宿主正在退出。
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            frame = incoming.next() => match frame {
                // 客户端关闭：回一个 close 帧后收尾。
                Some(Ok(Message::Close(_))) => {
                    let _ = sink.close().await;
                    break;
                }
                // 操作走契约 HTTP API，WS 上不接指令；ping/pong 由 axum 自动应答。
                Some(Ok(_)) => {}
                // 对端错误或流结束。
                Some(Err(_)) | None => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use farm_core::HostConfig;
    use farm_sdk::events::EventEnvelope;
    use farm_sdk::NewEvent;

    fn state() -> AppState {
        AppState::new_in_memory(HostConfig {
            bind: "127.0.0.1:3721".parse().expect("valid bind"),
            op_token: "unit-op-token".into(),
            admin_token: None,
            db_path: std::env::temp_dir().join("farm-host-unit.db"),
        })
        .expect("in-memory state")
    }

    #[tokio::test]
    async fn the_hello_frame_advertises_the_contract_version_and_the_event_closed_set() {
        let hello = WsHello::new(CONTRACT_VERSION, EventKind::ALL.to_vec());
        let json = serde_json::to_string(&hello).expect("serialize");
        assert!(json.contains(CONTRACT_VERSION), "{json}");
        assert!(json.contains("\"blocked\""), "一等事件必须可订阅：{json}");
        assert!(json.contains("\"needs_attention\""), "{json}");
    }

    #[tokio::test]
    async fn envelopes_on_the_bus_are_the_shape_the_ws_face_forwards() {
        let state = state();
        let published = state.events.publish(
            NewEvent::new(EventKind::Blocked, "agent-runner").with_ticket("11"),
        );
        let json = serde_json::to_string(&published).expect("serialize");
        let parsed: EventEnvelope = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed.kind, EventKind::Blocked);
        assert_eq!(parsed.ticket_id.as_deref(), Some("11"));
        assert!(!parsed.audited, "未入账事件带 audited=false（ADR-0005 §8）");
    }
}
