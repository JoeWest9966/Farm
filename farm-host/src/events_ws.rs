//! WS 事件流（ticket #15）：`GET /v1/events/stream` 的**投影通路**。
//!
//! ADR-0003 决策 3：面板实时通路是 WebSocket，**单连接订阅事件总线**；操作
//! （claim/steer/abort）走契约 HTTP API，WS 只承事件投影流。ADR-0002 §4：面板数据
//! 通路 = **快照 API + 增量推送**。
//!
//! 所以一条订阅的生命周期是三段：
//!
//! 1. **先订阅**内存总线（总线不回放，订阅之前的广播收不到）；
//! 2. **再取快照**：从事件日志（宿主私有存储，`AppState::publish` 的持久面）读有界
//!    保留窗口内的全部事件，按 `seq` 升序推送；
//! 3. **后接增量**：转发总线上的新信封，用**事件 id** 作去重游标——落在「快照与
//!    总线缓冲交叠区」的事件只出现一次。
//!
//! 无丢失的依据：`AppState::publish` 先写日志再广播，所以「总线里出现过的事件必在
//! 日志里」；订阅又早于快照，故而任何事件要么进快照、要么进增量，不会两头落空。
//! 无重复的依据：快照的 id 集合就是去重游标。
//!
//! 订阅者被总线容量甩下（`Lagged`）时不掉线：事件日志是持久源，按 id 补齐即可。

use std::collections::HashSet;

use axum::extract::State;
use axum::extract::ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use farm_core::event_log::EventLogFilter;
use farm_core::state::AppState;
use farm_sdk::events::WsHello;
use farm_sdk::{ContractError, EventKind, CONTRACT_VERSION};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;

/// WS upgrade 入口。token 已在中间件里验过；这里只负责升级与订阅。
pub async fn events_stream(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| async move {
        serve_subscription(socket, state).await;
    })
}

/// 一条订阅的生命周期：hello 帧 → 快照 → 增量（去重后）。
async fn serve_subscription(socket: WebSocket, state: AppState) {
    // 1. 先订阅：订阅之后的广播才不会漏（内存总线不回放）。
    let mut receiver = state.events.subscribe();
    let (mut sink, mut incoming) = socket.split();

    // 2. 自描述：编排器/面板据此确认契约版本与可订阅的事件闭集。
    let hello = WsHello::new(CONTRACT_VERSION, EventKind::ALL.to_vec());
    if !send_json(&mut sink, &hello).await {
        return; // 对端在 hello 之前就消失了
    }

    // 3. 快照：有界保留窗口内的全部事件（`limit` 缺省 = 不设限）。
    let snapshot = match state.event_history(&EventLogFilter::default()) {
        Ok(page) => page.events,
        Err(e) => {
            // 存储面读不了：回一个闭集错误帧（与 HTTP 同一信封）后收尾。
            let err: ContractError = e.into();
            let _ = send_json(&mut sink, &err).await;
            let _ = sink.close().await;
            return;
        }
    };
    // 去重游标 = 快照的事件 id 集合。
    let mut seen: HashSet<String> = snapshot.iter().map(|e| e.id.clone()).collect();
    for envelope in &snapshot {
        if !send_json(&mut sink, envelope).await {
            return; // 订阅者掉线
        }
    }

    // 4. 增量：总线上的新信封，快照里出现过的按 id 跳过。
    loop {
        tokio::select! {
            event = receiver.recv() => match event {
                Ok(envelope) => {
                    if !seen.insert(envelope.id.clone()) {
                        continue; // 快照已推过这条 ⇒ 不重复
                    }
                    if !send_json(&mut sink, &envelope).await {
                        break; // 订阅者掉线
                    }
                }
                // 内存总线容量溢出：事件日志是持久源，按 id 补齐，不掉线。
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::debug!(
                        skipped,
                        "ws subscriber lagged behind the in-memory event bus; resyncing from the event log"
                    );
                    if !resync(&mut sink, &state, &mut seen).await {
                        break;
                    }
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

/// 补齐：从事件日志重读一遍，把订阅者还没见过的事件按 `seq` 升序补推。
///
/// 返回 `false` 表示对端已断开（调用方应收尾）。保留窗口之外的事件无可挽回——
/// 那是 ADR-0005 §10 的有界性，不是丢失。
async fn resync(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    state: &AppState,
    seen: &mut HashSet<String>,
) -> bool {
    let page = match state.event_history(&EventLogFilter::default()) {
        Ok(page) => page,
        Err(e) => {
            let err: ContractError = e.into();
            let _ = send_json(sink, &err).await;
            return false;
        }
    };
    for envelope in &page.events {
        if !seen.insert(envelope.id.clone()) {
            continue;
        }
        if !send_json(sink, envelope).await {
            return false;
        }
    }
    true
}

/// 把一个可序列化值写成 WS 文本帧。对端已断开时返回 `false`。
async fn send_json(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    value: &impl Serialize,
) -> bool {
    let json = match serde_json::to_string(value) {
        Ok(json) => json,
        Err(e) => {
            tracing::error!(%e, "event frame must serialize");
            return false;
        }
    };
    sink.send(Message::Text(Utf8Bytes::from(json))).await.is_ok()
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
        let published = state
            .publish(
                NewEvent::new(EventKind::Blocked, "agent-runner").with_ticket("11"),
            )
            .expect("publish");
        let json = serde_json::to_string(&published).expect("serialize");
        let parsed: EventEnvelope = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed.kind, EventKind::Blocked);
        assert_eq!(parsed.ticket_id.as_deref(), Some("11"));
        assert!(!parsed.audited, "未入账事件带 audited=false（ADR-0005 §8）");
    }

    #[tokio::test]
    async fn the_snapshot_is_the_bounded_window_of_the_event_log() {
        let state = state();
        for i in 0..3 {
            state
                .publish(NewEvent::new(EventKind::Dispatch, "agent-runner").with_ticket(format!("t{i}")))
                .expect("publish");
        }
        let page = state
            .event_history(&EventLogFilter::default())
            .expect("history");
        assert_eq!(page.events.len(), 3, "WS 快照就是事件日志的有界窗口");
        assert!(!page.truncated);
    }
}
