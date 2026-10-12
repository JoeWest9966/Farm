//! 通知通道配置（ticket #21）的接缝测试：per-仓库、宿主私有存储、经宿主管理 API 投影。
//!
//! 全部经**真实存储**打接缝：`AppState.notifier`（[`farm_core::notifier::NotifierService`]）
//! 读写宿主私有 SQLite（migration v10 `notifier_channel_config`）。覆盖验收④：
//! 默认过滤集、经管理面可改、**重启后生效**（文件库重开，配置不丢）。
//!
//! 宿主零推送域知识：配置只有 enabled + 过滤集，没有一个 url/模板/重试/限流字段
//! （GLOSSARY「通知通道 / Notifier 插件」）——载体全住插件侧。

use farm_core::notifier::NotifierService;
use farm_core::{AppState, HostConfig};
use farm_sdk::notifier::{NotificationChannelUpdate, NotifierFilter};

fn in_memory() -> AppState {
    AppState::new_in_memory(HostConfig {
        bind: "127.0.0.1:3721".parse().expect("bind"),
        op_token: "unit-op-token".into(),
        admin_token: None,
        db_path: std::env::temp_dir().join("farm-core-notifier-unit.db"),
    })
    .expect("in-memory state")
}

#[test]
fn an_unconfigured_repo_gets_the_default_filter_set_with_merge_complete_off() {
    let state = in_memory();
    let config = state.notifier.get("repo-a").expect("default read");
    assert_eq!(config.repo_id, "repo-a");
    assert!(config.enabled, "默认通道开");
    assert_eq!(config.filter, NotifierFilter::default());
    assert!(!config.filter.merge_complete, "merge 完成默认关（GLOSSARY 默认过滤集）");
    assert!(config.filter.enabled(farm_sdk::events::EventKind::Blocked));
    assert!(config.filter.enabled(farm_sdk::events::EventKind::StallKill));
}

#[test]
fn setting_a_channel_config_persists_and_is_read_back() {
    let state = in_memory();
    let update = NotificationChannelUpdate {
        enabled: false,
        filter: NotifierFilter {
            blocked: false,
            needs_attention: true,
            stall: false,
            merge_complete: true,
        },
    };
    let saved = state.notifier.set("repo-a", &update).expect("set");
    assert!(!saved.enabled);
    assert!(saved.filter.merge_complete, "显式打开 merge 完成");
    assert!(!saved.updated_at.is_empty(), "写入要盖时间戳");

    let read_back = state.notifier.get("repo-a").expect("read back");
    assert_eq!(read_back.filter, update.filter);
    assert_eq!(read_back.enabled, update.enabled);
}

#[test]
fn the_channel_config_survives_a_restart() {
    // 验收④：配置落宿主私有 SQLite，宿主重启（文件库重开）后仍生效。
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("farm.db");
    let update = NotificationChannelUpdate {
        enabled: true,
        filter: NotifierFilter {
            blocked: true,
            needs_attention: false,
            stall: true,
            merge_complete: true,
        },
    };
    {
        let state = AppState::new(HostConfig {
            bind: "127.0.0.1:3721".parse().expect("bind"),
            op_token: "unit-op-token".into(),
            admin_token: None,
            db_path: path.clone(),
        })
        .expect("first boot");
        state.notifier.set("repo-a", &update).expect("set before restart");
    }
    // 重启：同一文件库重新打开（不重放 migration，ADR-0005 §4）。
    let reopened = AppState::new(HostConfig {
        bind: "127.0.0.1:3721".parse().expect("bind"),
        op_token: "unit-op-token".into(),
        admin_token: None,
        db_path: path.clone(),
    })
    .expect("second boot");
    let config = reopened.notifier.get("repo-a").expect("read after restart");
    assert_eq!(config.filter, update.filter, "重启后过滤集不变");
    assert!(config.enabled);
    // 没配过的仓库重启后仍是默认（不互相污染）。
    assert_eq!(
        reopened.notifier.get("repo-b").expect("other repo").filter,
        NotifierFilter::default()
    );
}

#[test]
fn each_repo_keeps_its_own_channel_config() {
    let state = in_memory();
    state
        .notifier
        .set(
            "repo-a",
            &NotificationChannelUpdate { enabled: true, filter: NotifierFilter::default_filter() },
        )
        .expect("set a");
    state
        .notifier
        .set(
            "repo-b",
            &NotificationChannelUpdate {
                enabled: false,
                filter: NotifierFilter {
                    blocked: false,
                    needs_attention: false,
                    stall: false,
                    merge_complete: false,
                },
            },
        )
        .expect("set b");
    assert!(state.notifier.get("repo-a").expect("a").enabled);
    assert!(!state.notifier.get("repo-b").expect("b").enabled, "per-仓库互不干扰");
}

#[test]
fn a_notifier_service_handle_is_cheap_to_clone() {
    let state = in_memory();
    let cloned: NotifierService = state.notifier.clone();
    assert_eq!(
        cloned.get("repo-a").expect("clone reads same store").filter,
        NotifierFilter::default()
    );
}
