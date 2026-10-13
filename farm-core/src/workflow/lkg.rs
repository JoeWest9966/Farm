//! last-known-good 存储：LKG 落宿主私有 SQLite，**per-仓库 keyed**。
//!
//! ADR-0005 §2：LKG 是持久项；§6：per-仓库 keyed，落宿主私有库，**不落被编排仓库**
//! （写进仓库会污染 PR diff）。表由 migration v2 建（表名单一源在 [`LKG_TABLE`]）。
//! schema 是宿主私有实现细节，永不外泄为契约（§5）——读只经服务/查询 API。

use farm_sdk::workflow::WorkflowConfig;

use crate::db::Database;
use crate::error::CoreError;

/// LKG 表名（单一源：migration v2 与本模块共用）。
pub const LKG_TABLE: &str = "workflow_config_lkg";

/// 从存储读回的一条 LKG。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredLkg {
    pub config: WorkflowConfig,
    /// 最近一次写入/更新时间（RFC3339）。
    pub updated_at: String,
}

/// per-仓库的 last-known-good 存储。
#[derive(Clone)]
pub struct LkgStore {
    db: Database,
}

impl LkgStore {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// 读某仓库的 LKG（无则 `Ok(None)`）。库内 JSON 损坏 → `CoreError::Config`。
    pub fn get(&self, repo_id: &str) -> Result<Option<StoredLkg>, CoreError> {
        let row: Option<(String, String)> = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT config_json, updated_at FROM {LKG_TABLE} WHERE repo_id = ?1"
            ))?;
            let mut rows = stmt.query([repo_id])?;
            match rows.next()? {
                Some(r) => Ok(Some((r.get(0)?, r.get(1)?))),
                None => Ok(None),
            }
        })?;
        self.decode(repo_id, row)
    }

    /// 写/覆盖某仓库的 LKG（INSERT .. ON CONFLICT DO UPDATE）。
    pub fn put(
        &self,
        repo_id: &str,
        config: &WorkflowConfig,
        updated_at: &str,
    ) -> Result<(), CoreError> {
        let json = serde_json::to_string(config).map_err(|e| {
            CoreError::Config(format!("workflow config is not serializable: {e}"))
        })?;
        self.db.with_conn(|conn| {
            conn.execute(
                &format!(
                    "INSERT INTO {LKG_TABLE} (repo_id, config_json, updated_at) VALUES (?1, ?2, ?3)
                     ON CONFLICT(repo_id) DO UPDATE SET config_json = excluded.config_json, updated_at = excluded.updated_at"
                ),
                rusqlite::params![repo_id, json, updated_at],
            )?;
            Ok(())
        })
    }

    fn decode(
        &self,
        repo_id: &str,
        row: Option<(String, String)>,
    ) -> Result<Option<StoredLkg>, CoreError> {
        match row {
            Some((json, updated_at)) => {
                let config = serde_json::from_str(&json).map_err(|e| {
                    CoreError::Config(format!("corrupt workflow LKG for {repo_id}: {e}"))
                })?;
                Ok(Some(StoredLkg { config, updated_at }))
            }
            None => Ok(None),
        }
    }
}
