//! tracker provider / adapter 配置（ticket #23 宿主管理面）：选哪个 provider adapter。
//!
//! GLOSSARY「Tracker 服务」+ ADR-0001 决策 3：provider 语义（GitHub/Gogs/native）关进
//! adapter，服务与契约不动。**换 provider = 加一个 [`TrackerAdapter`] 实现**，不是改服务。
//!
//! 持久化：选择落在 v1 的 `host_meta` key/value 表（复用，不加 migration——与 #20 的
//! `audit.unposted_reason` 同先例）。schema 是宿主私有实现细节，永不外泄为契约
//! （ADR-0005 §5）；面板经宿主管理 API（#23）的投影读它，不直连库。
//!
//! v1 唯一 adapter 是 `native`：本配置面记录「选了谁」，为将来的 github/gogs adapter
//! 预留位置；真换活 adapter 是把选择实例化成 `Arc<dyn TrackerAdapter>`，属后续票
//! （v1 native 是唯一 adapter，选择它即默认态）。

use rusqlite::OptionalExtension;

use super::adapter::NATIVE_PROVIDER;
use crate::db::Database;
use crate::error::CoreError;

/// `host_meta` 里记 tracker provider 选择的键（复用 v1 key/value 表，不加 migration）。
pub const TRACKER_PROVIDER_KEY: &str = "tracker.provider";

/// v1 已知的 provider adapter 名（host-private 闭集；换 provider = 加一个实现并登记这里）。
pub const KNOWN_PROVIDERS: &[&str] = &[NATIVE_PROVIDER];

/// 当前选中的 provider（未设置 → `native` 默认）。
pub fn current_provider(db: &Database) -> Result<String, CoreError> {
    let stored = db.with_conn(|conn| {
        conn.query_row(
            "SELECT value FROM host_meta WHERE key = ?1",
            rusqlite::params![TRACKER_PROVIDER_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()
    })?;
    Ok(stored.unwrap_or_else(|| NATIVE_PROVIDER.to_string()))
}

/// 校验并设置 provider。未知名 → [`CoreError::Config`]（投影为闭集 `invalid_request`）。
pub fn set_provider(db: &Database, provider: &str) -> Result<(), CoreError> {
    if !KNOWN_PROVIDERS.contains(&provider) {
        return Err(CoreError::Config(format!(
            "unknown tracker provider {provider:?}; known providers: {KNOWN_PROVIDERS:?}"
        )));
    }
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO host_meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![TRACKER_PROVIDER_KEY, provider],
        )?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migrated_db() -> crate::db::Database {
        let db = crate::db::Database::open_in_memory().expect("in-memory store");
        db.migrate(&crate::migrations::Migrator::new(crate::migrations::MIGRATIONS))
            .expect("migrate");
        db
    }

    #[test]
    fn the_default_provider_is_native_before_anything_is_configured() {
        let db = migrated_db();
        assert_eq!(current_provider(&db).expect("read"), NATIVE_PROVIDER);
    }

    #[test]
    fn a_known_provider_persists_through_the_host_meta_key() {
        let db = migrated_db();
        set_provider(&db, NATIVE_PROVIDER).expect("native is known");
        assert_eq!(current_provider(&db).expect("read"), NATIVE_PROVIDER);
    }

    #[test]
    fn an_unknown_provider_is_rejected_and_does_not_change_the_config() {
        let db = migrated_db();
        let err = set_provider(&db, "github").expect_err("github adapter is not implemented in v1");
        assert!(
            matches!(err, CoreError::Config(_)),
            "未知 provider 须是 Config 错误（投影 invalid_request）：{err:?}"
        );
        assert_eq!(
            current_provider(&db).expect("read"),
            NATIVE_PROVIDER,
            "拒绝的写入不得改动现有配置"
        );
    }
}
