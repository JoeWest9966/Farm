//! 通知通道配置服务句柄：把「per-仓库哑存储 + 默认化」缝成宿主共享状态。
//!
//! 这是 [`crate::AppState`] 的 `notifier` 字段。行为契约（GLOSSARY「通知通道」）：
//!
//! - **默认化**：没配过的仓库回**默认过滤集**（[`NotifierFilter::default`]：blocked /
//!   needs_attention / stall 开、merge 完成关）+ 总闸开；`updated_at` 为空串表示「从未配置」。
//! - **可改**：`set` 落库并盖 `now_rfc3339()` 时间戳，回写后的配置。
//! - **宿主零推送域知识**：本服务只搬 enabled + 过滤集，不碰 url/模板/重试/限流（载体插件侧）。
//!
//! ## 为什么不发事件
//! 与覆盖层不同：通知通道的消费者是**轮询**管理 API 的 notifier 插件（见
//! `examples/http-notifier`），下一次轮询即取到新配置，无需在总线上广播一次配置变更
//! （那会是噪音）。宿主只做持久面 + 默认化，推送决策全在插件侧。

use farm_sdk::notifier::{NotificationChannelConfig, NotificationChannelUpdate, NotifierFilter};

use crate::db::Database;
use crate::error::CoreError;

use super::store::{NotifierStore, StoredNotifierConfig};

/// 宿主共享的通知通道配置句柄。`Clone` 廉价（内部 `Database` 可 clone），可被 axum handler 共享。
#[derive(Clone)]
pub struct NotifierService {
    store: NotifierStore,
}

impl NotifierService {
    pub fn new(db: Database) -> Self {
        Self {
            store: NotifierStore::new(db),
        }
    }

    /// 底层存储句柄（宿主内部用；schema 不外泄，ADR-0005 §5）。
    pub fn store(&self) -> &NotifierStore {
        &self.store
    }

    /// 某仓库的生效配置：配过 → 读出的配置；没配过 → 默认过滤集 + 总闸开。
    ///
    /// `updated_at` 为空串表示「从未配置，正在用默认」——宿主管理 API 据此让面板区分
    /// 「默认生效」与「显式配置」。
    pub fn get(&self, repo_id: &str) -> Result<NotificationChannelConfig, CoreError> {
        match self.store.get(repo_id)? {
            Some(stored) => Ok(stored.to_config()),
            None => Ok(NotificationChannelConfig {
                repo_id: repo_id.to_string(),
                enabled: true,
                filter: NotifierFilter::default(),
                updated_at: String::new(),
            }),
        }
    }

    /// 设置某仓库的配置：落库 + 盖时间戳，回写后的配置。
    pub fn set(
        &self,
        repo_id: &str,
        update: &NotificationChannelUpdate,
    ) -> Result<NotificationChannelConfig, CoreError> {
        let stored = StoredNotifierConfig {
            repo_id: repo_id.to_string(),
            enabled: update.enabled,
            filter: update.filter,
            updated_at: crate::now_rfc3339(),
        };
        self.store.upsert(&stored)?;
        Ok(stored.to_config())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::{Migrator, MIGRATIONS};

    fn service() -> NotifierService {
        let db = Database::open_in_memory().expect("db");
        db.migrate(&Migrator::new(MIGRATIONS)).expect("migrate");
        NotifierService::new(db)
    }

    #[test]
    fn an_unconfigured_repo_defaults_to_the_open_channel_with_the_default_filter() {
        let service = service();
        let config = service.get("ghost").expect("default");
        assert!(config.enabled);
        assert_eq!(config.filter, NotifierFilter::default());
        assert!(config.updated_at.is_empty(), "空时间戳 = 从未配置");
    }

    #[test]
    fn setting_a_config_stamps_a_timestamp_and_reads_back() {
        let service = service();
        let saved = service
            .set(
                "repo-a",
                &NotificationChannelUpdate {
                    enabled: false,
                    filter: NotifierFilter::default_filter(),
                },
            )
            .expect("set");
        assert!(!saved.enabled);
        assert!(!saved.updated_at.is_empty(), "显式配置要盖时间戳");
        assert!(!service.get("repo-a").expect("read").enabled);
    }
}
