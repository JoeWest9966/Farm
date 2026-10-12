//! 宿主私有存储（host-private store）：嵌入式 SQLite + WAL（ADR-0005 §1/§7）。
//!
//! 全局单库，路径 `$XDG_DATA_HOME/farm/farm.db`（可覆盖）。**schema 是宿主私有
//! 实现细节，永不外泄为契约**（ADR-0005 §5）：插件与面板都不直连 DB，一切读走
//! 契约 API / 宿主投影 API。本票只建 migration runner 自己的工作表，服务域表归 #13–#15。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::Connection;

use crate::error::CoreError;
use crate::migrations::{Migrator, Migration};

/// 已落地 migration 的记录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedMigration {
    pub version: u32,
    pub name: String,
    pub applied_at: String,
}

/// runner 自己的工作表（不进契约，也不进服务域）。
pub const MIGRATIONS_TABLE: &str = "_farm_migrations";

/// SQLite 句柄：包一层 `Mutex`（rusqlite 的 `Connection` 不是 `Sync`），
/// 宿主内部所有服务经 `with_conn` 串行访问。
#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
    path: PathBuf,
}

impl Database {
    /// 打开（或创建）宿主私有存储并设好 WAL 等 pragma。
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(&path)?;
        Self::configure(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            path,
        })
    }

    /// 内存库（测试/一次性工具用；WAL 在内存库上不可用）。
    pub fn open_in_memory() -> Result<Self, CoreError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            path: PathBuf::from(":memory:"),
        })
    }

    fn configure(conn: &Connection) -> Result<(), CoreError> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=5000;
             PRAGMA synchronous=NORMAL;",
        )?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, CoreError> {
        self.conn.lock().map_err(|_| CoreError::StorePoisoned)
    }

    /// 在宿主私有存储上执行一段读/写逻辑。
    pub fn with_conn<T>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, CoreError> {
        let guard = self.lock()?;
        f(&guard).map_err(CoreError::Store)
    }

    /// 当前 journal mode（ADR-0005 §1 要求 WAL）。
    pub fn journal_mode(&self) -> Result<String, CoreError> {
        self.with_conn(|conn| conn.query_row("PRAGMA journal_mode", [], |r| r.get(0)))
    }

    /// 跑一遍 migration runner：按版本升序补差，幂等。
    pub fn migrate(&self, migrator: &Migrator<'_>) -> Result<Vec<u32>, CoreError> {
        migrator.validate()?;
        self.ensure_migrations_table()?;
        let already = self.applied_versions()?;

        let mut applied = Vec::new();
        for migration in migrator.migrations() {
            if already.contains(&migration.version) {
                continue;
            }
            self.apply_one(migration)?;
            applied.push(migration.version);
        }
        Ok(applied)
    }

    fn ensure_migrations_table(&self) -> Result<(), CoreError> {
        self.with_conn(|conn| {
            conn.execute_batch(&format!(
                "CREATE TABLE IF NOT EXISTS {MIGRATIONS_TABLE} (
                     version     INTEGER PRIMARY KEY,
                     name        TEXT    NOT NULL,
                     applied_at  TEXT    NOT NULL
                 );"
            ))
        })
    }

    fn applied_versions(&self) -> Result<Vec<u32>, CoreError> {
        self.with_conn(|conn| {
            let mut stmt =
                conn.prepare(&format!("SELECT version FROM {MIGRATIONS_TABLE} ORDER BY version"))?;
            let versions = stmt.query_map([], |r| r.get::<_, u32>(0))?;
            versions.collect::<rusqlite::Result<Vec<_>>>()
        })
    }

    /// 已落地清单（宿主管理面/启动日志可读）。
    pub fn applied_migrations(&self) -> Result<Vec<AppliedMigration>, CoreError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT version, name, applied_at FROM {MIGRATIONS_TABLE} ORDER BY version"
            ))?;
            let rows = stmt.query_map([], |r| {
                Ok(AppliedMigration {
                    version: r.get(0)?,
                    name: r.get(1)?,
                    applied_at: r.get(2)?,
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
    }

    /// 单条 migration 一个事务：失败整体回滚，且不记成已落地。
    fn apply_one(&self, migration: &Migration) -> Result<(), CoreError> {
        let mut guard = self.lock()?;
        let tx = guard.transaction().map_err(|source| CoreError::Migration {
            version: migration.version,
            name: migration.name.to_string(),
            source,
        })?;
        (migration.up)(&tx).map_err(|source| CoreError::Migration {
            version: migration.version,
            name: migration.name.to_string(),
            source,
        })?;
        tx.execute(
            &format!("INSERT INTO {MIGRATIONS_TABLE} (version, name, applied_at) VALUES (?1, ?2, ?3)"),
            rusqlite::params![
                migration.version,
                migration.name,
                crate::now_rfc3339(),
            ],
        )
        .map_err(|source| CoreError::Migration {
            version: migration.version,
            name: migration.name.to_string(),
            source,
        })?;
        tx.commit().map_err(|source| CoreError::Migration {
            version: migration.version,
            name: migration.name.to_string(),
            source,
        })
    }
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database").field("path", &self.path).finish()
    }
}
