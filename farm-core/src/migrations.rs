//! 版本化顺序 migration runner（ADR-0005 §7）。
//!
//! 全局单库、顺序 migration：版本号严格升序，已落地的跳过，未落地的一次一条事务。
//! 服务域表（票快照、attempts、事件日志、workspace 映射、覆盖层、通知通道配置）
//! **不在这里**——归 #13–#15 各自追加，追加即加法演进。

use rusqlite::Connection;

use crate::error::CoreError;

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

/// ticket #15：事件日志表 + 有界保留窗口所需的索引。
///
/// ADR-0005 §2/§10：**可过滤事件日志**落宿主私有存储，`events` 表**有界**（保留窗口，
/// 超限归档进 Kron 冷层归 #20）。`seq` 用 `AUTOINCREMENT`：删过最旧的行之后 id 也不复用，
/// 所以它是持久的次序游标；`id` 是总线盖章的事件 id，全局唯一，作去重键。
///
/// `kind` 列的 `CHECK` 约束由 [`farm_sdk::events::EventKind::ALL`] **生成**——闭集的单一源
/// 仍在 farm-sdk（ADR-0001 决策 6），SQL 只是第二道闸：自由字符串在存储层就进不来。
fn v5_event_log(conn: &Connection) -> rusqlite::Result<()> {
    let kinds = farm_sdk::events::EventKind::ALL
        .iter()
        .map(|k| format!("'{}'", k.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS events (
             seq          INTEGER PRIMARY KEY AUTOINCREMENT,
             id           TEXT    NOT NULL UNIQUE,
             kind         TEXT    NOT NULL CHECK (kind IN ({kinds})),
             occurred_at  TEXT    NOT NULL,
             actor        TEXT    NOT NULL,
             ticket_id    TEXT,
             attempt_id   TEXT,
             payload      TEXT    NOT NULL,
             audited      INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS idx_events_kind        ON events(kind);
         CREATE INDEX IF NOT EXISTS idx_events_ticket_id   ON events(ticket_id);
         CREATE INDEX IF NOT EXISTS idx_events_occurred_at ON events(occurred_at);
         CREATE INDEX IF NOT EXISTS idx_events_audited     ON events(audited);"
    ))
}

/// 骨架 migration 集：runner 自己的工作表 + 宿主级 key/value 表（#11）+ 事件日志（#15）。
/// 服务域表由 #13–#15 各自追加（加法演进，不改这里的历史版本）。
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "host_meta",
        up: v1_host_meta,
    },
    Migration {
        version: 5,
        name: "event_log",
        up: v5_event_log,
    },
];
