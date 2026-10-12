//! 覆盖层存储：per-仓库 keyed 的宿主私有 SQLite 访问（ADR-0005 §6）。
//!
//! 表由 migration v9 建（表名单一源在 [`OVERLAY_TABLE`]，与 `migrations` 共用）。
//! 本模块是**哑存储**：不校验值、不发事件、不算生效配置——那些住
//! [`super::service::OverlayService`]。schema 是宿主私有实现细节，永不外泄为契约
//! （ADR-0005 §5）：读只经服务 / 宿主管理 API。

use farm_sdk::overlay::OverlayKey;

use crate::db::Database;
use crate::error::CoreError;

use super::migrations::OVERLAY_TABLE;

/// 存储里的一条覆盖层（宿主私有形状；投影成契约 [`farm_sdk::overlay::OverlayEntry`]
/// 的活儿在服务层）。
#[derive(Clone, Debug, PartialEq)]
pub struct StoredOverlay {
    pub repo_id: String,
    pub key: OverlayKey,
    /// 该格的运行时值（JSON；类型由键决定）。
    pub value: serde_json::Value,
    /// 人读理由。
    pub note: Option<String>,
    /// 临时标记（ADR-0002 §2：显式标记，宿主写库时恒为真）。
    pub temporary: bool,
    /// 最近一次写入/更新时间（RFC3339）。
    pub updated_at: String,
}

/// per-仓库的覆盖层存储。
#[derive(Clone)]
pub struct OverlayStore {
    db: Database,
}

impl OverlayStore {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// 底层存储句柄（宿主内部用；schema 不外泄，ADR-0005 §5）。
    pub fn database(&self) -> &Database {
        &self.db
    }

    /// 写/覆盖一条覆盖层（同 `(repo_id, key)` 即覆盖，不是追加）。
    pub fn put(&self, entry: &StoredOverlay) -> Result<(), CoreError> {
        let value_json = serde_json::to_string(&entry.value).map_err(|e| {
            CoreError::Config(format!("overlay value for {} is not serializable: {e}", entry.key))
        })?;
        self.db.with_conn(|conn| {
            conn.execute(
                &format!(
                    "INSERT INTO {OVERLAY_TABLE} (repo_id, key, value_json, note, temporary, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT(repo_id, key) DO UPDATE SET
                         value_json = excluded.value_json,
                         note       = excluded.note,
                         temporary  = excluded.temporary,
                         updated_at = excluded.updated_at"
                ),
                rusqlite::params![
                    entry.repo_id,
                    entry.key.as_str(),
                    value_json,
                    entry.note,
                    i32::from(entry.temporary),
                    entry.updated_at,
                ],
            )?;
            Ok(())
        })
    }

    /// 某仓库的覆盖层清单（按键的 wire 字符串升序，确定序）。
    pub fn list(&self, repo_id: &str) -> Result<Vec<StoredOverlay>, CoreError> {
        let rows: Vec<(String, String, Option<String>, i32, String)> = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT key, value_json, note, temporary, updated_at FROM {OVERLAY_TABLE}
                 WHERE repo_id = ?1 ORDER BY key ASC"
            ))?;
            let mut rows = stmt.query([repo_id])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?));
            }
            Ok(out)
        })?;
        rows.into_iter()
            .map(|(key, value_json, note, temporary, updated_at)| {
                let key = OverlayKey::parse(&key).ok_or_else(|| {
                    CoreError::Config(format!("overlay key {key} is outside the closed set"))
                })?;
                let value = serde_json::from_str(&value_json).map_err(|e| {
                    CoreError::Config(format!("corrupt overlay value for {repo_id}/{key}: {e}"))
                })?;
                Ok(StoredOverlay {
                    repo_id: repo_id.to_string(),
                    key,
                    value,
                    note,
                    // 存储层的 CHECK 已把 0 挡在门外；这里只做形状投影。
                    temporary: temporary != 0,
                    updated_at,
                })
            })
            .collect()
    }

    /// 删一条覆盖层；返回是否真删了（删不存在的键 = 没删，幂等）。
    pub fn delete(&self, repo_id: &str, key: OverlayKey) -> Result<bool, CoreError> {
        let removed = self.db.with_conn(|conn| {
            conn.execute(
                &format!("DELETE FROM {OVERLAY_TABLE} WHERE repo_id = ?1 AND key = ?2"),
                rusqlite::params![repo_id, key.as_str()],
            )
        })?;
        Ok(removed > 0)
    }

    /// 清掉某仓库的全部覆盖层，回报清了几条。
    pub fn clear_repo(&self, repo_id: &str) -> Result<u64, CoreError> {
        let removed = self.db.with_conn(|conn| {
            conn.execute(
                &format!("DELETE FROM {OVERLAY_TABLE} WHERE repo_id = ?1"),
                rusqlite::params![repo_id],
            )
        })?;
        Ok(removed as u64)
    }

    /// 有覆盖层的仓库清单（按 `repo_id` 升序）——启动提示的数据源。
    pub fn repos_with_overlays(&self) -> Result<Vec<String>, CoreError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT DISTINCT repo_id FROM {OVERLAY_TABLE} ORDER BY repo_id ASC"
            ))?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
    }
}
