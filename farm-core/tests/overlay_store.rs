//! 覆盖层存储（farm-core::overlay）：落宿主私有 SQLite，per-仓库 keyed。
//!
//! ADR-0005 §2/§6：覆盖层是持久项；per-仓库 keyed；**不落被编排仓库**（写进仓库会
//! 污染 PR diff）。ADR-0002 §2：临时性是显式标记，不是暗示——存储层用 `temporary`
//! 列 + CHECK 把门，宿主写库时必填。
//!
//! 本测试钉住：往返、per-仓库隔离、临时标记不可缺席、重启后仍在（重开同库文件）。

use farm_core::db::Database;
use farm_core::migrations::{Migrator, MIGRATIONS};
use farm_core::overlay::migrations::{v9_workflow_overlay, OVERLAY_TABLE};
use farm_core::overlay::store::{OverlayStore, StoredOverlay};
use farm_sdk::overlay::OverlayKey;

fn store_on(path: &std::path::Path) -> OverlayStore {
    let db = Database::open(path).expect("open store");
    db.migrate(&Migrator::new(MIGRATIONS)).expect("migrate");
    OverlayStore::new(db)
}

fn entry(repo: &str, key: OverlayKey, value: serde_json::Value, note: Option<&str>) -> StoredOverlay {
    StoredOverlay {
        repo_id: repo.to_string(),
        key,
        value,
        note: note.map(str::to_string),
        temporary: true,
        updated_at: "2026-10-20T00:00:00Z".to_string(),
    }
}

#[test]
fn a_put_overlay_round_trips_through_the_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store_on(&dir.path().join("farm.db"));

    assert!(store.list("repo-a").expect("list").is_empty(), "初始无覆盖层");
    store
        .put(&entry("repo-a", OverlayKey::MaxConcurrency, serde_json::json!(1), Some("压并发")))
        .expect("put");

    let got = store.list("repo-a").expect("list");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].key, OverlayKey::MaxConcurrency);
    assert_eq!(got[0].value, serde_json::json!(1));
    assert_eq!(got[0].note.as_deref(), Some("压并发"));
    assert!(got[0].temporary, "临时标记必须原样读回");
}

#[test]
fn overlays_are_keyed_per_repository_and_per_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store_on(&dir.path().join("farm.db"));

    store
        .put(&entry("repo-a", OverlayKey::MaxConcurrency, serde_json::json!(1), None))
        .expect("put a");
    store
        .put(&entry("repo-b", OverlayKey::MaxConcurrency, serde_json::json!(9), None))
        .expect("put b");
    store
        .put(&entry(
            "repo-a",
            OverlayKey::StallThresholdSecs,
            serde_json::json!(60),
            None,
        ))
        .expect("put a second key");

    assert_eq!(store.list("repo-a").expect("a").len(), 2, "同仓库多键");
    assert_eq!(store.list("repo-b").expect("b").len(), 1, "per-仓库隔离");
    // 同键再写是覆盖，不是追加。
    store
        .put(&entry("repo-a", OverlayKey::MaxConcurrency, serde_json::json!(4), None))
        .expect("overwrite");
    let a = store.list("repo-a").expect("a");
    assert_eq!(a.len(), 2);
    assert_eq!(
        a.iter().find(|e| e.key == OverlayKey::MaxConcurrency).expect("key").value,
        serde_json::json!(4),
        "同键覆盖"
    );
}

#[test]
fn clearing_one_key_leaves_the_others_and_clearing_all_empties_the_repo() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store_on(&dir.path().join("farm.db"));
    for key in [OverlayKey::MaxConcurrency, OverlayKey::StallThresholdSecs] {
        store
            .put(&entry("repo-a", key, serde_json::json!(1), None))
            .expect("put");
    }

    assert!(store.delete("repo-a", OverlayKey::MaxConcurrency).expect("delete"), "真删了一条");
    assert_eq!(store.list("repo-a").expect("list").len(), 1);
    assert!(
        !store.delete("repo-a", OverlayKey::MaxConcurrency).expect("delete again"),
        "删不存在的键 = 没删（幂等）"
    );

    assert_eq!(store.clear_repo("repo-a").expect("clear all"), 1, "回报清了几条");
    assert!(store.list("repo-a").expect("list").is_empty());
    // 别的仓库不受影响。
    store
        .put(&entry("repo-b", OverlayKey::MaxConcurrency, serde_json::json!(2), None))
        .expect("put b");
    store.clear_repo("repo-a").expect("clear a again");
    assert_eq!(store.list("repo-b").expect("b").len(), 1, "清一个仓库不碰另一个");
}

#[test]
fn the_temporary_marker_is_enforced_at_the_storage_layer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Database::open(&dir.path().join("farm.db")).expect("open");
    db.migrate(&Migrator::new(MIGRATIONS)).expect("migrate");

    // 绕过类型系统直塞 `temporary = 0`：CHECK 必须挡住（ADR-0002 §2 显式标记）。
    let rejected = db.with_conn(|conn| {
        conn.execute(
            &format!(
                "INSERT INTO {OVERLAY_TABLE} (repo_id, key, value_json, temporary, updated_at)
                 VALUES ('repo-a', 'max_concurrency', '1', 0, 't')"
            ),
            [],
        )
    });
    assert!(rejected.is_err(), "temporary 标记由 CHECK 约束把门");

    // 键也由 CHECK 把门（自由字符串在存储层就进不来）。
    let rejected = db.with_conn(|conn| {
        conn.execute(
            &format!(
                "INSERT INTO {OVERLAY_TABLE} (repo_id, key, value_json, temporary, updated_at)
                 VALUES ('repo-a', 'nope', '1', 1, 't')"
            ),
            [],
        )
    });
    assert!(rejected.is_err(), "覆盖层键闭集由 CHECK 约束把门");
}

#[test]
fn an_overlay_survives_a_host_restart_and_never_touches_the_repo() {
    // 「重启」= 关库再开同一个库文件（ADR-0005 §4 崩溃恢复的前提）。
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("farm.db");
    {
        let store = store_on(&path);
        store
            .put(&entry("repo-a", OverlayKey::MaxConcurrency, serde_json::json!(1), Some("压并发")))
            .expect("put");
    }
    let reopened = store_on(&path);
    let got = reopened.list("repo-a").expect("list after restart");
    assert_eq!(got.len(), 1, "覆盖层经重启仍在（落宿主库，不落被编排仓库）");
    assert_eq!(got[0].value, serde_json::json!(1));
    assert!(got[0].temporary);
    // 宿主私有库里只有这一个覆盖层相关的表，别的东西没被顺手建出来。
    let tables = reopened
        .database()
        .with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type='table'")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .expect("tables");
    assert!(tables.contains(&OVERLAY_TABLE.to_string()));
}

#[test]
fn the_migration_set_registers_v9_for_the_overlay_domain() {
    let v9 = MIGRATIONS
        .iter()
        .find(|migration| migration.version == 9)
        .expect("v9 must be registered");
    assert_eq!(v9.name, "workflow_overlay");
    assert_eq!(
        v9.up as usize, v9_workflow_overlay as usize,
        "登记表的 up 必须指向 overlay 域的 SQL"
    );
    assert_eq!(OVERLAY_TABLE, "workflow_overlay");
}

#[test]
fn every_repo_with_overlays_is_listable_for_the_restart_prompt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store_on(&dir.path().join("farm.db"));
    assert!(store.repos_with_overlays().expect("repos").is_empty());

    store
        .put(&entry("repo-a", OverlayKey::MaxConcurrency, serde_json::json!(1), None))
        .expect("put a");
    store
        .put(&entry("repo-b", OverlayKey::StallThresholdSecs, serde_json::json!(60), None))
        .expect("put b");

    let repos = store.repos_with_overlays().expect("repos");
    assert_eq!(repos, vec!["repo-a".to_string(), "repo-b".to_string()], "按 repo_id 升序，确定序");
}
