//! 内存事件总线原语（GLOSSARY：编排器与 agent-runner 的统一事件回流面）。
//!
//! 本票只落地广播原语；服务域事件（派发、merge、审批…）由 #15/#16/#18 发布进来。

use farm_sdk::events::{EventEnvelope, EventKind, NewEvent};
use farm_core::events::EventBus;
use farm_core::now_rfc3339;
use tokio::sync::broadcast::error::TryRecvError;

fn bus() -> EventBus {
    EventBus::with_capacity(8)
}

#[tokio::test]
async fn a_published_event_reaches_every_subscriber() {
    let bus = bus();
    let mut a = bus.subscribe();
    let mut b = bus.subscribe();

    let published = bus.publish(NewEvent::new(EventKind::Dispatch, "agent-runner"));

    let got_a = a.recv().await.expect("subscriber a must receive");
    let got_b = b.recv().await.expect("subscriber b must receive");
    assert_eq!(got_a, published);
    assert_eq!(got_b, published);
    assert_eq!(published.kind, EventKind::Dispatch);
}

#[tokio::test]
async fn the_bus_stamps_id_timestamp_and_unaudited_flag() {
    let bus = bus();
    let mut rx = bus.subscribe();

    let first = bus.publish(NewEvent::new(EventKind::Merge, "host").with_ticket("11"));
    let second = bus.publish(NewEvent::new(EventKind::Merge, "host").with_ticket("12"));

    assert_ne!(first.id, second.id, "事件 id 必须唯一");
    assert!(!first.audited, "未入账事件默认 audited=false（ADR-0005 §8）");
    assert!(!first.occurred_at.is_empty());
    assert_eq!(first.ticket_id.as_deref(), Some("11"));
    assert!(rx.recv().await.is_ok());
    assert!(rx.recv().await.is_ok());
}

#[tokio::test]
async fn a_subscriber_only_sees_events_published_after_it_subscribed() {
    let bus = bus();
    bus.publish(NewEvent::new(EventKind::Dispatch, "host"));

    let mut late = bus.subscribe();
    let published = bus.publish(NewEvent::new(EventKind::Blocked, "host"));

    let got = late.recv().await.expect("只应收到订阅后的事件");
    assert_eq!(got, published);
}

#[tokio::test]
async fn a_lagging_subscriber_is_told_it_lagged_instead_of_blocking_the_publisher() {
    let bus = EventBus::with_capacity(1);
    let mut slow = bus.subscribe();

    for i in 0..4 {
        bus.publish(NewEvent::new(EventKind::Dispatch, "host").with_ticket(format!("t{i}")));
    }

    let err = slow.try_recv().expect_err("容量 1 的订阅者必然 lag");
    assert!(matches!(err, TryRecvError::Lagged(_)), "收到 {err:?}");
}

#[tokio::test]
async fn publishing_without_subscribers_does_not_fail() {
    let bus = bus();
    let published = bus.publish(NewEvent::new(EventKind::HostRestarted, "host"));
    assert_eq!(published.kind, EventKind::HostRestarted);
}

#[test]
fn the_envelope_is_a_valid_rfc3339_stamped_event() {
    // 独立已知量：now_rfc3339 的形状与可解析性。
    let now = now_rfc3339();
    assert!(now.ends_with('Z'), "RFC3339 UTC 以 Z 结尾：{now}");
    assert_eq!(now.len(), 20, "YYYY-MM-DDTHH:MM:SSZ：{now}");
    assert!(now.contains('T'));

    let env = EventEnvelope::from_new(
        NewEvent::new(EventKind::StallKill, "agent-runner").with_attempt("attempt-9"),
        "evt-1",
        now.clone(),
    );
    assert_eq!(env.occurred_at, now);
    assert_eq!(env.attempt_id.as_deref(), Some("attempt-9"));
}
