//! 事件日志（event log）：宿主私有存储里的事件持久面（ticket #15）。
//!
//! GLOSSARY：事件总线是五服务之一，编排器与 agent-runner 的统一事件回流面，面板只做
//! 投影层。ADR-0002 §4/§7：面板数据通路 = 快照 API + 增量推送；ADR-0005 §2/§10：
//! **可过滤事件日志**落宿主私有存储，`events` 表**有界**（保留窗口，超限归档进
//! Kron 冷层归 #20）。
//!
//! 两条不变式（WS 投影无丢失无重复的地基）：
//!
//! 1. **日志先于总线**：宿主发布路径先写本表再广播（见 [`crate::AppState::publish`]），
//!    所以「总线里出现过的事件必在日志里」。
//! 2. **`seq` 是持久的次序游标，`id` 是去重键**：`seq` 单调自增且不复用
//!    （`AUTOINCREMENT`），事件 id 由总线盖章、全局唯一；WS 投影用快照的 id 集合
//!    作去重游标（见 farm-host 的 `events_ws`）。
//!
//! schema 是宿主私有实现细节，永不外泄为契约（ADR-0005 §5）：面板与插件都不直连 DB，
//! 一切读走 `GET /v1/events/history` 与 `GET /v1/events/stream`。

use rusqlite::Connection;

use farm_sdk::events::{EventEnvelope, EventKind};
use farm_sdk::services::events::EventHistoryResponse;

use crate::db::Database;
use crate::error::CoreError;

/// 事件日志表名（宿主私有 schema）。
pub const EVENTS_TABLE: &str = "events";

/// 有界保留窗口的默认行数（ADR-0005 §10：events 表有界；具体数值归宿主/workflow 配置，
/// 所以这里只是默认值，可经 [`EventLog::with_retention`] 覆写）。
pub const DEFAULT_RETENTION: usize = 10_000;

/// 历史查询的默认条数（`limit` 缺省时）。
pub const DEFAULT_HISTORY_LIMIT: usize = 200;

/// 单次历史查询的条数上限（翻页用；与保留窗口是两件事）。
pub const MAX_HISTORY_LIMIT: usize = 1_000;

/// 事件日志查询条件（面板/编排器的过滤面，与契约 `EventHistoryQuery` 同形）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EventLogFilter {
    /// RFC3339 下界（含）：只回 `occurred_at >= since` 的事件。
    /// 任意 RFC3339 形状都收（带偏移量/亚秒精度），[`EventLog::history`] 会先归一。
    pub since: Option<String>,
    /// 事件种类（闭集成员）。
    pub kind: Option<EventKind>,
    /// 票 id。
    pub ticket_id: Option<String>,
    /// 最多回多少条**最近**的事件；`None` = 不设限（仍受保留窗口约束）。
    pub limit: Option<usize>,
}

/// 事件日志句柄：只持有保留窗口，数据全在宿主私有存储里（可廉价复制）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventLog {
    retention: usize,
}

impl EventLog {
    pub fn new() -> Self {
        Self::with_retention(DEFAULT_RETENTION)
    }

    /// 指定保留窗口行数（至少 1 行）。
    pub fn with_retention(retention: usize) -> Self {
        Self {
            retention: retention.max(1),
        }
    }

    /// 当前保留窗口行数。
    pub fn retention(&self) -> usize {
        self.retention
    }

    /// 落一条事件（发布路径的持久面）。保留窗口在同一个连接内顺势收敛。
    pub fn append(&self, db: &Database, envelope: &EventEnvelope) -> Result<(), CoreError> {
        db.with_conn_core(|conn| self.append_conn(conn, envelope).map_err(CoreError::Store))
    }

    /// 裸连接形态：供发布路径在**同一把存储锁内**做完 append + 广播
    /// （日志次序与总线次序因此严格一致）。
    pub fn append_conn(
        &self,
        conn: &Connection,
        envelope: &EventEnvelope,
    ) -> rusqlite::Result<()> {
        conn.execute(
            &format!(
                "INSERT INTO {EVENTS_TABLE}
                     (id, kind, occurred_at, actor, ticket_id, attempt_id, payload, audited)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
            ),
            rusqlite::params![
                envelope.id,
                envelope.kind.as_str(),
                envelope.occurred_at,
                envelope.actor,
                envelope.ticket_id,
                envelope.attempt_id,
                serde_json::to_string(&envelope.payload)
                    .unwrap_or_else(|_| "null".to_string()),
                i64::from(envelope.audited),
            ],
        )?;
        self.prune_conn(conn)?;
        Ok(())
    }

    /// 收敛保留窗口：删掉比「最新 seq - retention」更旧的行。返回删除行数。
    pub fn prune(&self, db: &Database) -> Result<usize, CoreError> {
        db.with_conn(|conn| self.prune_conn(conn))
    }

    fn prune_conn(&self, conn: &Connection) -> rusqlite::Result<usize> {
        // 空表时 MAX(seq) 为 NULL，比较结果为 NULL ⇒ 一行不删。
        conn.execute(
            &format!(
                "DELETE FROM {EVENTS_TABLE}
                 WHERE seq <= (SELECT MAX(seq) FROM {EVENTS_TABLE}) - ?1"
            ),
            rusqlite::params![self.retention as i64],
        )
    }

    /// 当前表内行数（保留窗口的占用情况）。
    pub fn count(&self, db: &Database) -> Result<usize, CoreError> {
        db.with_conn(|conn| {
            conn.query_row(
                &format!("SELECT COUNT(*) FROM {EVENTS_TABLE}"),
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n as usize)
        })
    }

    /// 历史查询（快照 API 的后端）：按 `seq` 升序回**最近** `limit` 条匹配事件。
    pub fn history(
        &self,
        db: &Database,
        filter: &EventLogFilter,
    ) -> Result<EventHistoryResponse, CoreError> {
        db.with_conn_core(|conn| self.history_conn(conn, filter))
    }

    /// 裸连接形态的历史查询（同一把锁内读，快照才与发布路径原子对齐）。
    pub fn history_conn(
        &self,
        conn: &Connection,
        filter: &EventLogFilter,
    ) -> Result<EventHistoryResponse, CoreError> {
        let (where_sql, params) = where_clause(filter)?;

        // 先取最近 N 条（倒序查、正序回），再反推「还有没有更旧的」。
        let mut sql =
            format!("SELECT {COLUMNS} FROM {EVENTS_TABLE}{where_sql} ORDER BY seq DESC");
        let mut limited = params;
        if let Some(limit) = filter.limit {
            sql.push_str(" LIMIT ?");
            limited.push(Box::new(limit as i64));
        }
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_from(&limited), stored_event)?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut events = Vec::with_capacity(rows.len());
        for row in rows {
            events.push(row.into_envelope()?);
        }
        events.reverse(); // 升序：面板时间线与 WS 快照同一方向

        let (where_sql, params) = where_clause(filter)?;
        let matched: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM {EVENTS_TABLE}{where_sql}"),
            params_from(&params),
            |r| r.get(0),
        )?;
        // 表内最早一行已经不是 seq=1 ⇒ 保留窗口删过行（ADR-0005 §10）。
        let pruned = conn
            .query_row(
                &format!("SELECT MIN(seq) FROM {EVENTS_TABLE}"),
                [],
                |r| r.get::<_, Option<i64>>(0),
            )?
            .is_some_and(|min| min > 1);

        let truncated = matched > events.len() as i64 || pruned;
        Ok(EventHistoryResponse { events, truncated })
    }

    /// 未入账事件（`audited=0`），按 `seq` 升序，最多 `limit` 条（ticket #20 对账补账用）。
    pub fn unposted(&self, db: &Database, limit: usize) -> Result<Vec<EventEnvelope>, CoreError> {
        db.with_conn_core(|conn| self.unposted_conn(conn, limit))
    }

    /// 裸连接形态（同一把锁内读）。
    pub fn unposted_conn(
        &self,
        conn: &Connection,
        limit: usize,
    ) -> Result<Vec<EventEnvelope>, CoreError> {
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM {EVENTS_TABLE} WHERE audited = 0 ORDER BY seq ASC LIMIT ?"
        ))?;
        let rows = stmt
            .query_map([limit as i64], stored_event)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter().map(|r| r.into_envelope()).collect()
    }
}

impl Default for EventLog {
    fn default() -> Self {
        Self::new()
    }
}

/// 查询列（`seq` 也在内：次序游标，虽然信封不带它）。
const COLUMNS: &str =
    "seq, id, kind, occurred_at, actor, ticket_id, attempt_id, payload, audited";

/// 落库的一行（信封的存储形态；`kind` 还是文本，回读时才过闭集）。
struct StoredEvent {
    id: String,
    kind: String,
    occurred_at: String,
    actor: String,
    ticket_id: Option<String>,
    attempt_id: Option<String>,
    payload: String,
    audited: bool,
}

impl StoredEvent {
    /// 回读成信封。kind 不在闭集 / payload 不是 JSON ⇒ 行已损坏，
    /// 绝不静默降级成一个假事件（ADR-0001 决策 6：闭集是契约）。
    fn into_envelope(self) -> Result<EventEnvelope, CoreError> {
        let kind: EventKind = serde_json::from_value(serde_json::Value::String(self.kind.clone()))
            .map_err(|_| CoreError::CorruptEventLogRow {
                id: self.id.clone(),
                detail: format!(
                    "kind {:?} is not a member of the EventKind closed set",
                    self.kind
                ),
            })?;
        let payload: serde_json::Value =
            serde_json::from_str(&self.payload).map_err(|e| CoreError::CorruptEventLogRow {
                id: self.id.clone(),
                detail: format!("payload is not valid json: {e}"),
            })?;
        Ok(EventEnvelope {
            id: self.id,
            kind,
            occurred_at: self.occurred_at,
            actor: self.actor,
            ticket_id: self.ticket_id,
            attempt_id: self.attempt_id,
            payload,
            audited: self.audited,
        })
    }
}

fn stored_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredEvent> {
    Ok(StoredEvent {
        id: row.get(1)?,
        kind: row.get(2)?,
        occurred_at: row.get(3)?,
        actor: row.get(4)?,
        ticket_id: row.get(5)?,
        attempt_id: row.get(6)?,
        payload: row.get(7)?,
        audited: row.get::<_, i64>(8)? != 0,
    })
}

type Param = Box<dyn rusqlite::ToSql>;

/// 由过滤条件拼 WHERE 子句与参数（编号占位符按追加顺序生成）。
///
/// `since` 在这里归一成宿主写入的时间戳形状（UTC、秒级）：字典序才等于时间序。
fn where_clause(filter: &EventLogFilter) -> Result<(String, Vec<Param>), CoreError> {
    let mut sql = String::from(" WHERE 1=1");
    let mut params: Vec<Param> = Vec::new();
    if let Some(since) = &filter.since {
        let canonical =
            canonical_timestamp(since).ok_or_else(|| CoreError::InvalidFilter(format!(
                "since must be an RFC3339 timestamp, got {since:?}"
            )))?;
        sql.push_str(" AND occurred_at >= ?");
        params.push(Box::new(canonical));
    }
    if let Some(kind) = &filter.kind {
        sql.push_str(" AND kind = ?");
        params.push(Box::new(kind.as_str().to_string()));
    }
    if let Some(ticket_id) = &filter.ticket_id {
        sql.push_str(" AND ticket_id = ?");
        params.push(Box::new(ticket_id.clone()));
    }
    Ok((sql, params))
}

fn params_from(params: &[Param]) -> impl rusqlite::Params + '_ {
    rusqlite::params_from_iter(params.iter().map(|p| p.as_ref()))
}

/// 把调用方给的 RFC3339 时间戳规范成宿主写入的形状（UTC、秒级、`...Z`）。
///
/// 事件时间戳是秒级精度（[`crate::now_rfc3339`]），字典序即时间序；带偏移量或
/// 亚秒精度的入参在这里归一，比较才不会错。解析不了返回 `None`（调用方回
/// 闭集 `invalid_request`）。
pub fn canonical_timestamp(text: &str) -> Option<String> {
    use time::format_description::well_known::Rfc3339;
    let parsed = time::OffsetDateTime::parse(text, &Rfc3339).ok()?;
    parsed
        .to_offset(time::UtcOffset::UTC)
        .replace_nanosecond(0)
        .ok()?
        .format(&Rfc3339)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_timestamp_normalizes_offsets_to_the_host_shape() {
        assert_eq!(
            canonical_timestamp("2026-01-01T08:00:00+08:00").as_deref(),
            Some("2026-01-01T00:00:00Z")
        );
        assert_eq!(
            canonical_timestamp("2026-01-01T00:00:00.500Z").as_deref(),
            Some("2026-01-01T00:00:00Z")
        );
        assert_eq!(canonical_timestamp("not-a-timestamp"), None);
        assert_eq!(canonical_timestamp(""), None);
    }

    #[test]
    fn the_default_retention_window_is_bounded() {
        assert!(EventLog::new().retention() >= 1);
        assert!(EventLog::with_retention(0).retention() >= 1, "窗口至少一行");
    }
}
