//! last-known-good 存储（farm-core::workflow::lkg）：LKG 落宿主私有 SQLite，per-仓库。
//!
//! ADR-0005 §2/§6：LKG 是持久项；per-仓库 keyed；不落被编排仓库。本测试钉住
//! put/get 往返、per-仓库隔离、以及**重启后仍生效**（重开同库文件仍在）。

use farm_core::db::Database;
use farm_core::migrations::{Migrator, MIGRATIONS};
use farm_core::workflow::lkg::LkgStore;
use farm_sdk::workflow::{ProjectionSwitches, RetryBackoff, WorkflowConfig};

fn sample(concurrency: u32) -> WorkflowConfig {
    WorkflowConfig {
        dispatch_labels: vec!["agent-ready".into()],
        max_concurrency: concurrency,
        retry_backoff: RetryBackoff {
            max_retries: 2,
            base_delay_ms: 100,
            backoff_multiplier: 2,
        },
        stall_threshold_secs: 120,
        prompt_template: "do {{ticket_id}}".into(),
        projection: ProjectionSwitches {
            open_pr: true,
            close_issue: false,
            ..Default::default()
        },
    }
}

fn store_on(path: &std::path::Path) -> LkgStore {
    let db = Database::open(path).expect("open store");
    db.migrate(&Migrator::new(MIGRATIONS)).expect("migrate");
    LkgStore::new(db)
}

#[test]
fn a_put_lkg_round_trips_through_the_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store_on(&dir.path().join("farm.db"));

    assert!(store.get("repo-a").expect("get").is_none(), "初始无 LKG");
    store
        .put("repo-a", &sample(3), "2026-01-01T00:00:00Z")
        .expect("put");

    let got = store.get("repo-a").expect("get").expect("LKG present");
    assert_eq!(got.config, sample(3));
    assert_eq!(got.updated_at, "2026-01-01T00:00:00Z");
}

#[test]
fn lkg_is_keyed_per_repository() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store_on(&dir.path().join("farm.db"));

    store.put("repo-a", &sample(1), "t-a").expect("put a");
    store.put("repo-b", &sample(9), "t-b").expect("put b");

    assert_eq!(store.get("repo-a").expect("a").unwrap().config.max_concurrency, 1);
    assert_eq!(store.get("repo-b").expect("b").unwrap().config.max_concurrency, 9);
    assert!(store.get("repo-c").expect("c").is_none(), "未登记的仓库无 LKG");
}

#[test]
fn putting_again_overwrites_the_previous_lkg() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store_on(&dir.path().join("farm.db"));

    store.put("repo-a", &sample(1), "t1").expect("put 1");
    store.put("repo-a", &sample(7), "t2").expect("put 2 (overwrite)");

    let got = store.get("repo-a").expect("get").expect("present");
    assert_eq!(got.config.max_concurrency, 7, "后写覆盖先写");
    assert_eq!(got.updated_at, "t2");
}

#[test]
fn lkg_survives_a_restart_reopening_the_same_store_file() {
    // ADR-0005 §4 崩溃恢复前提：LKG 落盘，重开同库文件仍读得到。
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("farm.db");
    {
        let store = store_on(&path);
        store.put("repo-a", &sample(4), "t-persist").expect("put");
    } // store / db 在这里 drop
    let reopened = store_on(&path);
    let got = reopened.get("repo-a").expect("get").expect("LKG survived restart");
    assert_eq!(got.config.max_concurrency, 4);
    assert_eq!(got.updated_at, "t-persist");
}
