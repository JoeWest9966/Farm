//! 持久化发布路径（ticket #15）：`AppState::publish` 让事件回流面的**活面**（内存总线）
//! 与**持久面**（事件日志）同源、同序。
//!
//! 两条不变式是面板投影「无丢失无重复」的地基：
//!
//! 1. 日志先于总线——订阅者收到事件时，它一定已经在事件日志里。
//! 2. 日志 `seq` 次序 == 总线次序——面板时间线（快照）与实时增量不会互相矛盾。

use farm_core::event_log::EventLogFilter;
use farm_core::state::AppState;
use farm_core::{CoreError, HostConfig};
use farm_sdk::events::{EventEnvelope, EventKind, NewEvent};

fn state() -> AppState {
    AppState::new_in_memory(HostConfig {
        bind: "127.0.0.1:3721".parse().expect("valid bind"),
        op_token: "unit-op-token".into(),
        admin_token: None,
        db_path: std::env::temp_dir().join("farm-core-publish-unit.db"),
    })
    .expect("in-memory state")
}

#[tokio::test]
async fn a_published_event_reaches_bus_subscribers_and_the_event_log() {
    let state = state();
    let mut rx = state.events.subscribe();

    let published = state
        .publish(
            NewEvent::new(EventKind::Blocked, "agent-runner")
                .with_ticket("15")
                .with_payload(serde_json::json!({"reason": "waiting on human"})),
        )
        .expect("publish");

    let live = rx.recv().await.expect("subscriber must receive");
    assert_eq!(live, published, "活面上就是宿主盖章的那个信封");

    let page = state
        .event_history(&EventLogFilter::default())
        .expect("history");
    assert_eq!(page.events, vec![published], "持久面上是同一个信封");
}

#[tokio::test]
async fn the_event_log_never_lags_behind_the_bus() {
    let state = state();
    let mut rx = state.events.subscribe();

    for i in 0..3 {
        let published = state
            .publish(NewEvent::new(EventKind::Dispatch, "agent-runner").with_ticket(format!("t{i}")))
            .expect("publish");
        let live = rx.recv().await.expect("live");
        assert_eq!(live, published);

        // 订阅者**收到的那一刻**，事件必须已经在日志里（否则先订阅再取快照会丢）。
        let page = state
            .event_history(&EventLogFilter {
                ticket_id: Some(format!("t{i}")),
                ..Default::default()
            })
            .expect("history");
        assert_eq!(
            page.events.len(),
            1,
            "第 {i} 条事件必须在收到之前就已落库"
        );
        assert_eq!(page.events[0].id, published.id);
    }
}

#[tokio::test]
async fn the_bus_order_and_the_event_log_order_are_the_same_under_concurrent_publishes() {
    let state = state();
    let mut rx = state.events.subscribe();

    let mut publishers = Vec::new();
    for t in 0..8u32 {
        let state = state.clone();
        publishers.push(tokio::spawn(async move {
            for i in 0..20u32 {
                state
                    .publish(
                        NewEvent::new(EventKind::Dispatch, "agent-runner")
                            .with_ticket(format!("t{t}-{i}")),
                    )
                    .expect("publish");
            }
        }));
    }
    for publisher in publishers {
        publisher.await.expect("publisher task");
    }

    let mut live = Vec::new();
    while let Ok(envelope) = rx.try_recv() {
        live.push(envelope.id);
    }
    let page = state
        .event_history(&EventLogFilter::default())
        .expect("history");
    let logged: Vec<String> = page.events.iter().map(|e| e.id.clone()).collect();

    assert_eq!(live.len(), 160, "每条发布都必须送达订阅者");
    assert_eq!(logged.len(), 160, "每条发布都必须落库");
    assert_eq!(live, logged, "总线次序必须与事件日志次序一致");
}

#[tokio::test]
async fn a_failed_log_write_does_not_broadcast_the_event() {
    let state = state();
    // 让存储面失效（模拟私有存储写不进去）：整条发布必须失败且不上总线。
    state
        .db
        .with_conn(|conn| conn.execute_batch("DROP TABLE events"))
        .expect("drop the event log table");
    let mut rx = state.events.subscribe();

    let err = state
        .publish(NewEvent::new(EventKind::Blocked, "agent-runner"))
        .expect_err("落库失败必须上报");
    assert!(matches!(err, CoreError::Store(_)), "收到 {err:?}");

    assert_eq!(state.events.receiver_count(), 1);
    assert!(
        rx.try_recv().is_err(),
        "落库失败的事件不得广播：面板从未见过它，也就无所谓丢"
    );
}

#[tokio::test]
async fn the_raw_bus_still_broadcasts_without_touching_the_log() {
    // 活面原语仍在：#11 的 EventBus::publish 语义不变（纯广播、不落库）。
    let state = state();
    let mut rx = state.events.subscribe();

    let published: EventEnvelope = state
        .events
        .publish(NewEvent::new(EventKind::HostRestarted, "host"));

    assert_eq!(rx.recv().await.expect("live"), published);
    let page = state
        .event_history(&EventLogFilter::default())
        .expect("history");
    assert!(
        page.events.is_empty(),
        "裸总线广播不落库：持久化是 AppState::publish 的职责"
    );
}
