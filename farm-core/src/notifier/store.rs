//! 通知通道配置存储：per-仓库 keyed 的宿主私有 SQLite 访问（ADR-0005 §6）。
//!
//! 表由 migration v10 建（表名单一源在 [`NOTIFIER_TABLE`]，与 `migrations` 共用）。
//! 本模块是**哑存储**：不默认化、不发事件——那些住 [`super::service::NotifierService`]。
//! schema 是宿主私有实现细节，永不外泄为契约（ADR-0005 §5）：读只经宿主管理 API 投影。

use rusqlite::{OptionalExtension, Row};

use farm_sdk::notifier::{NotificationChannelConfig, NotifierFilter};

use crate::db::Database;
use crate::error::CoreError;

use super::migrations::NOTIFIER_TABLE;

/// 存储里的一条通知通道配置（宿主私有形状）。
///
/// 与契约 [`NotificationChannelConfig`] 同形：宿主零推送域知识，只有 enabled + 过滤集。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredNotifierConfig {
    pub repo_id: String,
    pub enabled: bool,
    pub filter: NotifierFilter,
    pub updated_at: String,
}

impl StoredNotifierConfig {
    /// 投影成契约形状（宿主管理 API 的读源）。
    pub fn to_config(&self) -> NotificationChannelConfig {
        NotificationChannelConfig {
            repo_id: self.repo_id.clone(),
            enabled: self.enabled,
            filter: self.filter,
            updated_at: self.updated_at.clone(),
        }
    }
}

/// per-仓库的通知通道配置存储。
#[derive(Clone)]
pub struct NotifierStore {
    db: Database,
}

impl NotifierStore {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// 底层存储句柄（宿主内部用；schema 不外泄，ADR-0005 §5）。
    pub fn database(&self) -> &Database {
        &self.db
    }

    /// 写/覆盖一条配置（同 `repo_id` 即覆盖，不是追加）。
    pub fn upsert(&self, config: &StoredNotifierConfig) -> Result<(), CoreError> {
        self.db.with_conn(|conn| {
            conn.execute(
                &format!(
                    "INSERT INTO {NOTIFIER_TABLE}
                        (repo_id, enabled, notify_blocked, notify_needs_attention,
                         notify_stall, notify_merge_complete, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                     ON CONFLICT(repo_id) DO UPDATE SET
                        enabled               = excluded.enabled,
                        notify_blocked        = excluded.notify_blocked,
                        notify_needs_attention = excluded.notify_needs_attention,
                        notify_stall          = excluded.notify_stall,
                        notify_merge_complete = excluded.notify_merge_complete,
                        updated_at            = excluded.updated_at"
                ),
                rusqlite::params![
                    config.repo_id,
                    i32::from(config.enabled),
                    i32::from(config.filter.blocked),
                    i32::from(config.filter.needs_attention),
                    i32::from(config.filter.stall),
                    i32::from(config.filter.merge_complete),
                    config.updated_at,
                ],
            )?;
            Ok(())
        })
    }

    /// 某仓库的配置（不存在 → `None`）。
    pub fn get(&self, repo_id: &str) -> Result<Option<StoredNotifierConfig>, CoreError> {
        self.db
            .with_conn(|conn| {
                conn.query_row(
                    &format!(
                        "SELECT repo_id, enabled, notify_blocked, notify_needs_attention,
                                notify_stall, notify_merge_complete, updated_at
                         FROM {NOTIFIER_TABLE} WHERE repo_id = ?1"
                    ),
                    rusqlite::params![repo_id],
                    row_to_config,
                )
                .optional()
            })
    }

    /// 全部配置（按 repo_id 升序，确定序）。
    pub fn list(&self) -> Result<Vec<StoredNotifierConfig>, CoreError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT repo_id, enabled, notify_blocked, notify_needs_attention,
                        notify_stall, notify_merge_complete, updated_at
                 FROM {NOTIFIER_TABLE} ORDER BY repo_id"
            ))?;
            let rows = stmt.query_map([], row_to_config)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
    }
}

fn row_to_config(row: &Row<'_>) -> rusqlite::Result<StoredNotifierConfig> {
    Ok(StoredNotifierConfig {
        repo_id: row.get("repo_id")?,
        enabled: row.get::<_, i32>("enabled")? != 0,
        filter: NotifierFilter {
            blocked: row.get::<_, i32>("notify_blocked")? != 0,
            needs_attention: row.get::<_, i32>("notify_needs_attention")? != 0,
            stall: row.get::<_, i32>("notify_stall")? != 0,
            merge_complete: row.get::<_, i32>("notify_merge_complete")? != 0,
        },
        updated_at: row.get("updated_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::{Migrator, MIGRATIONS};

    fn store() -> NotifierStore {
        let db = Database::open_in_memory().expect("db");
        db.migrate(&Migrator::new(MIGRATIONS)).expect("migrate");
        NotifierStore::new(db)
    }

    #[test]
    fn a_stored_config_round_trips_the_filter_switches() {
        let store = store();
        assert_eq!(store.get("repo-a").expect("get"), None, "没配过就是没有");
        let config = StoredNotifierConfig {
            repo_id: "repo-a".into(),
            enabled: true,
            filter: NotifierFilter {
                blocked: true,
                needs_attention: false,
                stall: true,
                merge_complete: true,
            },
            updated_at: "2026-10-20T00:00:00Z".into(),
        };
        store.upsert(&config).expect("upsert");
        let read = store.get("repo-a").expect("get").expect("present");
        assert_eq!(read, config, "四个开关原样往返");
        assert_eq!(
            read.to_config().filter.merge_complete,
            true,
            "投影成契约形状"
        );
    }

    #[test]
    fn upserting_the_same_repo_overwrites_rather_than_appends() {
        let store = store();
        let base = StoredNotifierConfig {
            repo_id: "repo-a".into(),
            enabled: true,
            filter: NotifierFilter::default(),
            updated_at: "t0".into(),
        };
        store.upsert(&base).expect("first");
        store
            .upsert(&StoredNotifierConfig { enabled: false, updated_at: "t1".into(), ..base })
            .expect("second");
        assert_eq!(store.list().expect("list").len(), 1, "一仓库一行");
        assert!(!store.get("repo-a").expect("get").expect("present").enabled);
    }
}
