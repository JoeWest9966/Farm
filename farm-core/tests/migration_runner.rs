//! 宿主私有存储（ADR-0005 §1/§7）的 migration runner：版本化、顺序、幂等。

use farm_core::db::Database;
use farm_core::migrations::{Migrator, Migration, MIGRATIONS};
use farm_core::CoreError;

fn temp_store() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Database::open(&dir.path().join("farm.db")).expect("open store");
    (dir, db)
}

#[test]
fn the_migration_runner_applies_every_migration_in_version_order() {
    let (_dir, db) = temp_store();
    let migrator = Migrator::new(&MIGRATIONS);

    let applied = db.migrate(&migrator).expect("first migration run");

    let expected: Vec<u32> = MIGRATIONS.iter().map(|m| m.version).collect();
    assert_eq!(applied, expected, "必须按版本升序全部落地");
    assert!(!expected.is_empty(), "骨架至少要有一条 migration");
}

#[test]
fn running_the_migration_runner_twice_is_a_no_op() {
    let (_dir, db) = temp_store();
    let migrator = Migrator::new(&MIGRATIONS);

    let first = db.migrate(&migrator).expect("first run");
    let second = db.migrate(&migrator).expect("second run");

    assert!(!first.is_empty());
    assert!(second.is_empty(), "幂等：第二次不得重放任何 migration");
    assert_eq!(
        db.applied_migrations().expect("applied list").len(),
        first.len(),
        "已落地清单不得因重跑而翻倍"
    );
}

#[test]
fn reopening_an_existing_store_does_not_replay_migrations() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("farm.db");
    let migrator = Migrator::new(&MIGRATIONS);

    let first = {
        let db = Database::open(&path).expect("open store");
        db.migrate(&migrator).expect("first run")
    };
    let reopened = {
        let db = Database::open(&path).expect("reopen store");
        db.migrate(&migrator).expect("reopen run")
    };

    assert_eq!(first.len(), MIGRATIONS.len());
    assert!(reopened.is_empty(), "重启后不得重放（ADR-0005 §4 崩溃恢复前提）");
}

#[test]
fn migrations_run_inside_a_single_sequential_transaction_per_migration() {
    // 一条会失败的 migration 不得留下半套表，也不得被记成已落地。
    let (_dir, db) = temp_store();
    let boom = Migration {
        version: 1,
        name: "boom",
        up: |conn| {
            conn.execute_batch("CREATE TABLE ok_table (id INTEGER PRIMARY KEY);")?;
            conn.execute_batch("CREATE TABLE ok_table (id INTEGER PRIMARY KEY);") // 第二句必然失败
        },
    };
    let set = [boom];
    let migrator = Migrator::new(&set);

    let err = db.migrate(&migrator).expect_err("失败必须上报");
    assert!(matches!(err, CoreError::Migration { .. }), "收到 {err:?}");

    let applied = db.applied_migrations().expect("applied list");
    assert!(applied.is_empty(), "失败的 migration 不得记成已落地");

    let tables = db
        .with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='ok_table'")?;
            let names: Vec<String> = stmt.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
            Ok(names)
        })
        .expect("query sqlite_master");
    assert!(tables.is_empty(), "失败的 migration 必须整体回滚");
}

#[test]
fn migration_versions_must_strictly_increase() {
    let dup = [
        Migration { version: 1, name: "a", up: |_| Ok(()) },
        Migration { version: 1, name: "b", up: |_| Ok(()) },
    ];
    let migrator = Migrator::new(&dup);
    let (_dir, db) = temp_store();
    let err = db.migrate(&migrator).expect_err("重复版本必须被拒");
    assert!(
        matches!(err, CoreError::InvalidMigrations(_)),
        "非法 migration 集必须在碰库之前就被拒：{err:?}"
    );
    assert!(
        err.to_string().contains("strictly increase"),
        "错误要说明顺序规则：{err}"
    );
}

#[test]
fn a_fresh_migration_is_applied_on_top_of_an_older_store() {
    // 演进模拟：先只有 v1，之后加入 v2，重开旧库只补 v2。
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("farm.db");
    let v1 = Migration {
        version: 1,
        name: "v1",
        up: |conn| conn.execute_batch("CREATE TABLE t1 (id INTEGER PRIMARY KEY);"),
    };
    let v2 = Migration {
        version: 2,
        name: "v2",
        up: |conn| conn.execute_batch("CREATE TABLE t2 (id INTEGER PRIMARY KEY);"),
    };

    {
        let db = Database::open(&path).expect("open");
        let only_v1 = [v1.clone()];
        db.migrate(&Migrator::new(&only_v1)).expect("v1 only");
    }
    let db = Database::open(&path).expect("reopen");
    let both = [v1.clone(), v2];
    let applied = db
        .migrate(&Migrator::new(&both))
        .expect("additive migration");
    assert_eq!(applied, vec![2], "只补新版本");
    assert_eq!(db.applied_migrations().expect("list").len(), 2);
}

#[test]
fn the_host_private_store_runs_in_wal_mode() {
    let (_dir, db) = temp_store();
    db.migrate(&Migrator::new(&MIGRATIONS)).expect("migrate");
    assert_eq!(db.journal_mode().expect("journal_mode"), "wal", "ADR-0005 §1：rusqlite + WAL");
}
