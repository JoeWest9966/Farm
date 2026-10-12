//! 版本化顺序 migration runner（ADR-0005 §7）。
//!
//! 全局单库、顺序 migration：版本号严格升序，已落地的跳过，未落地的一次一条事务。
//!
//! **按五服务分域**（ADR-0005 §5）的组织先例由 #13 建立：服务域表**不在这里**，
//! 住在各服务自己的 `farm-core/src/<service>/migrations.rs`；本文件只是**版本登记表**
//! （`up: tracker::migrations::v3_...`）。版本号在集成分支上协调分配：#11 = v1
//! `host_meta`，#13 = v3 tracker 票快照与状态历史（v2 仍空置，未被任何票占用）；
//! 后续服务域（#14/#15/#18…）依合并顺序取 v4/v5/…，只准追加、不准改历史版本。

use rusqlite::Connection;

use crate::error::CoreError;
use crate::tracker;

/// 一条 migration。`up` 在一个事务里执行，失败整体回滚。
#[derive(Clone)]
pub struct Migration {
    pub version: u32,
    pub name: &'static str,
    pub up: fn(&Connection) -> rusqlite::Result<()>,
}

impl std::fmt::Debug for Migration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Migration")
            .field("version", &self.version)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// migration 集合 + 顺序规则。
pub struct Migrator<'a> {
    migrations: &'a [Migration],
}

impl<'a> Migrator<'a> {
    pub fn new(migrations: &'a [Migration]) -> Self {
        Self { migrations }
    }

    pub fn migrations(&self) -> &'a [Migration] {
        self.migrations
    }

    /// 版本必须严格升序（顺序 migration 的硬规则）。
    pub fn validate(&self) -> Result<(), CoreError> {
        if self.migrations.is_empty() {
            return Err(CoreError::InvalidMigrations("migration set is empty".into()));
        }
        for pair in self.migrations.windows(2) {
            if pair[1].version <= pair[0].version {
                return Err(CoreError::InvalidMigrations(format!(
                    "versions must strictly increase: v{} ({}) then v{} ({})",
                    pair[0].version, pair[0].name, pair[1].version, pair[1].name
                )));
            }
        }
        Ok(())
    }
}

fn v1_host_meta(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS host_meta (
             key    TEXT PRIMARY KEY,
             value  TEXT NOT NULL
         );",
    )
}

/// 骨架 migration 集：runner 自己的工作表 + 宿主级 key/value 表 + tracker 服务域表。
///
/// 服务域的 SQL 不住这里（按五服务分域，见模块文档）：这里只按版本登记。
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "host_meta",
        up: v1_host_meta,
    },
    // --- tracker 服务域（#13）：票快照 + blocking 边 + 状态历史 ---
    Migration {
        version: 3,
        name: "tracker_issue_snapshot",
        up: tracker::migrations::v3_tracker_issue_snapshot,
    },
];
