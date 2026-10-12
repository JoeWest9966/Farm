//! 插件状态持久化接缝（ticket #19，migration v8 `plugin_state`）。
//!
//! GLOSSARY「宿主私有存储」：插件监督状态（running / stopped / quarantined、重启计数、
//! 下次重启时刻）是宿主私有实现细节，落 SQLite，**永不外泄为契约**（ADR-0005 §5）；
//! 面板与插件读它只能经宿主管理 API（#23）的投影。插件自身**不直连 DB**。
//!
//! 表名单一源在 [`farm_core::plugin::store::PLUGIN_TABLE`]（先例：#13/#18）。

use farm_core::db::Database;
use farm_core::migrations::{Migrator, MIGRATIONS};
use farm_core::plugin::{PluginState, PluginStatus, PluginStore};

/// 一行插件状态（构造助手：只给测试用，字段名即语义）。
fn status(plugin_id: &str, state: PluginState, restarts: u32) -> PluginStatus {
    PluginStatus {
        plugin_id: plugin_id.to_string(),
        name: format!("{plugin_id}-name"),
        program: format!("/bin/{plugin_id}"),
        state,
        restarts,
        pid: None,
        last_error: None,
        next_restart_at: None,
        updated_at: "2026-01-01T00:00:00Z".to_string(),
    }
}

fn store() -> (tempfile::TempDir, PluginStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Database::open(dir.path().join("farm.db")).expect("open store");
    db.migrate(&Migrator::new(MIGRATIONS))
        .expect("migrations must apply");
    (dir, PluginStore::new(&db))
}

#[test]
fn plugin_supervision_state_is_persisted_by_migration_v8() {
    let (_dir, store) = store();
    store
        .upsert(&status("notifier", PluginState::Running, 0))
        .expect("write-ahead row");

    let loaded = store.get("notifier").expect("get").expect("row must exist");
    assert_eq!(loaded.name, "notifier-name");
    assert_eq!(loaded.state, PluginState::Running);
    assert_eq!(loaded.restarts, 0);
    assert_eq!(loaded.pid, None);
}

#[test]
fn the_restart_counter_and_quarantine_survive_a_reopen_of_the_store() {
    // ADR-0004 决策 4：重启计数与隔离态是**持久项**——宿主重启后不悄悄清零、
    // 不悄悄把一个 quarantined 插件拉起来（恢复是人的裁决）。
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("farm.db");
    {
        let db = Database::open(&path).expect("open");
        db.migrate(&Migrator::new(MIGRATIONS)).expect("migrate");
        let store = PluginStore::new(&db);
        store
            .upsert(&status("notifier", PluginState::Running, 0))
            .expect("upsert");
        let restarts = store
            .note_crash("notifier", "exited with code 1", "2026-01-01T00:00:01Z")
            .expect("note crash");
        assert_eq!(restarts, 1);
        assert!(store
            .set_state("notifier", PluginState::Quarantined, "2026-01-01T00:00:02Z")
            .expect("quarantine"));
    }
    let db = Database::open(&path).expect("reopen");
    db.migrate(&Migrator::new(MIGRATIONS))
        .expect("reopen migrate is a no-op for v8");
    let store = PluginStore::new(&db);

    let loaded = store.get("notifier").expect("get").expect("row");
    assert_eq!(loaded.state, PluginState::Quarantined, "隔离态不得因宿主重启丢失");
    assert_eq!(loaded.restarts, 1, "重启计数不得因宿主重启清零");
    assert_eq!(loaded.last_error.as_deref(), Some("exited with code 1"));
    assert_eq!(store.list().expect("list").len(), 1);
}

#[test]
fn a_crash_bumps_the_counter_and_a_recovery_resets_it() {
    let (_dir, store) = store();
    store
        .upsert(&status("notifier", PluginState::Running, 0))
        .expect("upsert");

    for expected in 1..=3u32 {
        let restarts = store
            .note_crash("notifier", "boom", "2026-01-01T00:00:00Z")
            .expect("note crash");
        assert_eq!(restarts, expected, "每次崩溃计数 +1");
    }

    store
        .reset("notifier", Some(4242), "2026-01-01T00:01:00Z")
        .expect("recovery resets the budget");
    let after = store.get("notifier").expect("get").expect("row");
    assert_eq!(after.restarts, 0, "恢复后重试预算重置（自愈从干净账面开始）");
    assert_eq!(after.state, PluginState::Running);
    assert_eq!(after.pid, Some(4242), "重启后的新子进程 pid 要落账");
    assert_eq!(after.last_error, None, "恢复清掉上一次的崩溃原因");
}

#[test]
fn the_next_restart_window_is_recorded_for_the_panel_to_read() {
    let (_dir, store) = store();
    store
        .upsert(&status("notifier", PluginState::Running, 0))
        .expect("upsert");
    store
        .note_crash("notifier", "boom", "2026-01-01T00:00:00Z")
        .expect("crash");
    store
        .set_next_restart("notifier", Some("2026-01-01T00:00:01Z"), "2026-01-01T00:00:00Z")
        .expect("schedule");

    let scheduled = store.get("notifier").expect("get").expect("row");
    assert_eq!(
        scheduled.next_restart_at.as_deref(),
        Some("2026-01-01T00:00:01Z"),
        "退避窗口要可读（面板能显示还有多久重启）"
    );

    store
        .set_next_restart("notifier", None, "2026-01-01T00:00:02Z")
        .expect("clear");
    let cleared = store.get("notifier").expect("get").expect("row");
    assert_eq!(cleared.next_restart_at, None);
}

#[test]
fn upserting_twice_is_idempotent_and_unknown_plugins_are_absent() {
    let (_dir, store) = store();
    let row = status("notifier", PluginState::Running, 0);
    store.upsert(&row).expect("first");
    store.upsert(&row).expect("second");

    assert_eq!(store.list().expect("list").len(), 1, "同 id 两次 upsert 是一行");
    assert_eq!(store.get("nope").expect("get"), None);
    assert!(
        !store
            .set_state("nope", PluginState::Stopped, "now")
            .expect("set_state"),
        "不存在的插件不得被偷偷建成一行"
    );
}
