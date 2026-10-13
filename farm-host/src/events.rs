//! 事件总线服务面（ticket #15）：**快照 API** `GET /v1/events/history`。
//!
//! ADR-0002 §4：面板只消费宿主投影层（快照 API + 增量推送），不直连编排器或存储；
//! ADR-0005 §5：SQLite schema 永不外泄——所以这里只经契约面把事件日志投影出去。
//! WS 单连接增量推送在 [`crate::events_ws`]；两条路共用同一个事件日志后端。
//!
//! 查询形状是契约里的 `EventHistoryQuery`（farm-sdk 单一源）：`since` / `kind` /
//! `ticket_id` / `limit`。查询串解析失败一律回闭集 `invalid_request`（不用 axum
//! 默认的纯文本拒绝，与 `POST /handshake` 同一纪律）。

use axum::extract::{RawQuery, State};
use axum::Json;
use farm_core::event_log::{EventLogFilter, MAX_HISTORY_LIMIT};
use farm_core::state::AppState;
use farm_sdk::services::events::{EventHistoryQuery, EventHistoryResponse};
use farm_sdk::ContractError;

use crate::error::ApiError;

/// 单页条数下限（`limit=0` 没有意义，夹到 1）。
const MIN_PAGE: usize = 1;

/// `GET /v1/events/history`：事件历史查询（有界保留窗口内的最近事件）。
pub async fn events_history(
    State(state): State<AppState>,
    query: RawQuery,
) -> Result<Json<EventHistoryResponse>, ApiError> {
    let query: EventHistoryQuery = match query.0.as_deref() {
        Some(raw) => serde_urlencoded::from_str(raw).map_err(|e| {
            ApiError(ContractError::invalid_request(format!(
                "malformed event history query: {e}"
            )))
        })?,
        None => EventHistoryQuery::default(),
    };

    let limit = match query.limit {
        Some(requested) => (requested as usize).clamp(MIN_PAGE, MAX_HISTORY_LIMIT),
        None => farm_core::event_log::DEFAULT_HISTORY_LIMIT,
    };

    let page = state
        .event_history(&EventLogFilter {
            since: query.since,
            kind: query.kind,
            ticket_id: query.ticket_id,
            limit: Some(limit),
        })
        .map_err(|e| ApiError(e.into()))?;

    Ok(Json(page))
}

#[cfg(test)]
mod tests {
    use super::*;
    use farm_sdk::EventKind;

    fn filter_from(query: EventHistoryQuery) -> EventLogFilter {
        let limit = match query.limit {
            Some(requested) => (requested as usize).clamp(MIN_PAGE, MAX_HISTORY_LIMIT),
            None => farm_core::event_log::DEFAULT_HISTORY_LIMIT,
        };
        EventLogFilter {
            since: query.since,
            kind: query.kind,
            ticket_id: query.ticket_id,
            limit: Some(limit),
        }
    }

    #[test]
    fn an_absent_query_gets_the_default_page_size() {
        let filter = filter_from(EventHistoryQuery::default());
        assert_eq!(filter.limit, Some(farm_core::event_log::DEFAULT_HISTORY_LIMIT));
        assert!(filter.since.is_none());
        assert!(filter.kind.is_none());
        assert!(filter.ticket_id.is_none());
    }

    #[test]
    fn an_absurd_limit_is_clamped_instead_of_rejected() {
        let huge = filter_from(EventHistoryQuery {
            limit: Some(u32::MAX),
            ..Default::default()
        });
        assert_eq!(huge.limit, Some(MAX_HISTORY_LIMIT));

        let zero = filter_from(EventHistoryQuery {
            limit: Some(0),
            ..Default::default()
        });
        assert_eq!(zero.limit, Some(MIN_PAGE));
    }

    #[test]
    fn the_query_shape_carries_the_contract_filters() {
        let filter = filter_from(EventHistoryQuery {
            since: Some("2026-01-01T00:00:00Z".into()),
            kind: Some(EventKind::Blocked),
            ticket_id: Some("15".into()),
            limit: Some(7),
        });
        assert_eq!(filter.since.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(filter.kind, Some(EventKind::Blocked));
        assert_eq!(filter.ticket_id.as_deref(), Some("15"));
        assert_eq!(filter.limit, Some(7));
    }
}
