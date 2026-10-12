//! 通知通道（Notification Channel）的契约形状（ticket #21，GLOSSARY「通知通道 / Notifier 插件」）。
//!
//! 夹具 JSON 全部手写（独立于实现的真相源）：类型一旦与这些线格式漂移，测试就红。
//! 最重要的一条：**默认过滤集**——blocked / needs_attention / stall 开、merge 完成关
//! （GLOSSARY：merge 是常态流量，票看板已是其投影，默认不再外发）。

use farm_sdk::events::EventKind;
use farm_sdk::notifier::{NotificationChannelConfig, NotificationChannelUpdate, NotifierFilter};

#[test]
fn the_default_filter_forwards_blocked_needs_attention_and_stall_but_not_merge_complete() {
    // GLOSSARY「通知通道」默认过滤集：blocked / needs_attention / stall 开、merge 完成关。
    let filter = NotifierFilter::default();
    assert!(filter.blocked, "blocked 默认开");
    assert!(filter.needs_attention, "needs_attention 默认开");
    assert!(filter.stall, "stall 默认开");
    assert!(!filter.merge_complete, "merge 完成默认关（常态流量，票看板已是其投影）");
}

#[test]
fn the_filter_answers_whether_a_critical_event_kind_is_enabled() {
    let filter = NotifierFilter::default();
    // 四类关键事件各映射到一个开关（GLOSSARY：事件集闭包保证可订阅）。
    assert!(filter.enabled(EventKind::Blocked));
    assert!(filter.enabled(EventKind::NeedsAttention));
    assert!(filter.enabled(EventKind::StallKill), "stall 映射到 stall_kill");
    assert!(!filter.enabled(EventKind::Merge), "merge 完成默认关");

    // 闭包之外的事件种类永不过滤（通知通道只关心四类关键事件）。
    assert!(!filter.enabled(EventKind::Dispatch));
    assert!(!filter.enabled(EventKind::ClaimAcquired));
    assert!(!filter.enabled(EventKind::PluginRestarted));
}

#[test]
fn the_filter_can_be_told_to_forward_merge_complete_too() {
    let filter = NotifierFilter {
        merge_complete: true,
        ..NotifierFilter::default()
    };
    assert!(filter.enabled(EventKind::Merge), "显式打开后 merge 完成也外发");
    assert!(filter.enabled(EventKind::Blocked), "其余开关不受影响");
}

#[test]
fn the_critical_event_kinds_are_exactly_the_notification_closure() {
    // GLOSSARY：事件集闭包 = blocked / needs_attention / merge 完成 / stall 四类。
    let critical = EventKind::NOTIFICATION_CRITICAL.iter().copied().collect::<Vec<_>>();
    assert_eq!(critical.len(), 4, "闭包固定四类");
    for kind in [
        EventKind::Blocked,
        EventKind::NeedsAttention,
        EventKind::Merge,
        EventKind::StallKill,
    ] {
        assert!(kind.is_notification_critical(), "{kind} 必须属通知闭包");
    }
    assert!(!EventKind::Dispatch.is_notification_critical());
}

#[test]
fn a_notifier_filter_round_trips_through_json_in_the_documented_shape() {
    let json = r#"{
      "blocked": true,
      "needs_attention": true,
      "stall": false,
      "merge_complete": true
    }"#;
    let filter: NotifierFilter = serde_json::from_str(json).expect("fixture must parse");
    assert!(filter.blocked);
    assert!(!filter.stall, "stall 可被显式关掉");
    assert!(filter.merge_complete);
    // 缺席字段不允许猜：四个开关都是显式的（与 OverlayEntry.temporary 同纪律）。
    let partial = r#"{"blocked":true}"#;
    assert!(
        serde_json::from_str::<NotifierFilter>(partial).is_err(),
        "过滤集形状必须完整，不能缺开关"
    );
}

#[test]
fn a_channel_config_carries_the_repo_the_enabled_switch_the_filter_and_a_timestamp() {
    let json = r#"{
      "repo_id": "repo-a",
      "enabled": true,
      "filter": {
        "blocked": true,
        "needs_attention": true,
        "stall": true,
        "merge_complete": false
      },
      "updated_at": "2026-10-20T00:00:00Z"
    }"#;
    let config: NotificationChannelConfig = serde_json::from_str(json).expect("fixture");
    assert_eq!(config.repo_id, "repo-a");
    assert!(config.enabled);
    assert_eq!(config.filter, NotifierFilter::default());
    assert_eq!(config.updated_at, "2026-10-20T00:00:00Z");

    // 宿主零推送域知识：通道配置里没有 url / headers / body 模板 / 重试 / 限流——
    // 那些是插件侧自决载体（GLOSSARY「Notifier 插件」）。序列化形状必须能证明这一点。
    let rendered = serde_json::to_string(&config).expect("serialize");
    for forbidden in ["url", "headers", "template", "retry", "rate_limit", "endpoint"] {
        assert!(!rendered.contains(forbidden), "通道配置不得含推送域字段 {forbidden}: {rendered}");
    }
}

#[test]
fn a_channel_update_sets_the_enabled_switch_and_the_filter() {
    let json = r#"{
      "enabled": false,
      "filter": {
        "blocked": false,
        "needs_attention": true,
        "stall": false,
        "merge_complete": false
      }
    }"#;
    let update: NotificationChannelUpdate = serde_json::from_str(json).expect("fixture");
    assert!(!update.enabled);
    assert!(!update.filter.blocked);
    assert!(update.filter.needs_attention);
    // 更新只带 enabled + filter（repo_id 在路径上，updated_at 由宿主张章）。
    assert!(!serde_json::to_string(&update).expect("ser").contains("repo_id"));
}
