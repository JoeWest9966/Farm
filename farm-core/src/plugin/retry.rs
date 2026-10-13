//! 分级指数退避重试策略（ADR-0004 决策 4）：**1s / 10s / 1min，上限 3 次**。
//!
//! 崩溃一次 → 睡一档再重启；三档用完还崩 → 进隔离（quarantined），宿主**停止重启**
//! （防崩溃风暴），恢复是人的裁决。档位与上限都是**已知字面量**（ADR 钉死），测试按字面量
//! 断言，不重算实现；测试用快策略验证机制，不必真睡 71 秒。

use std::time::Duration;

/// 规范档位（毫秒）：第 1 次重启前等 1s，第 2 次等 10s，第 3 次等 1min。
pub const BACKOFF_SCHEDULE_MS: [u64; 3] = [1_000, 10_000, 60_000];

/// 规范重启上限：3 次。第 4 次崩溃 → quarantined。
pub const DEFAULT_MAX_RESTARTS: u32 = 3;

/// 分级指数退避策略（可注入：测试用快档验证机制）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    schedule_ms: Vec<u64>,
    max_restarts: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::tiered()
    }
}

impl RetryPolicy {
    /// 规范策略（ADR-0004 决策 4）：1s/10s/1min，上限 3 次。
    pub fn tiered() -> Self {
        Self {
            schedule_ms: BACKOFF_SCHEDULE_MS.to_vec(),
            max_restarts: DEFAULT_MAX_RESTARTS,
        }
    }

    /// 自定义档位与上限（测试注入快档；档数可以不是 3）。
    pub fn with_schedule(schedule_ms: Vec<u64>, max_restarts: u32) -> Self {
        Self {
            schedule_ms,
            max_restarts,
        }
    }

    /// 档位表（毫秒；只读）。
    pub fn schedule_ms(&self) -> &[u64] {
        &self.schedule_ms
    }

    /// 重启上限。
    pub fn max_restarts(&self) -> u32 {
        self.max_restarts
    }

    /// 第 `restart` 次重启（1 起算）之前要睡多久；`None` = 重试预算已耗尽。
    ///
    /// 档位用完了不循环、不加码：超出即 `None`，由监督循环判隔离。
    pub fn backoff_for(&self, restart: u32) -> Option<Duration> {
        if restart == 0 || restart > self.max_restarts {
            return None;
        }
        let index = (restart - 1) as usize;
        self.schedule_ms
            .get(index)
            .map(|ms| Duration::from_millis(*ms))
    }

    /// 已经崩掉 `restarts` 次之后，还允不允许再重启一次。
    ///
    /// `restarts` 是**已用掉**的重启次数：三次重启是第 1/2/3 次崩溃后各一次，
    /// 所以第 3 次崩溃后仍有一次重启机会（1min 档），第 4 次崩溃才耗尽。
    pub fn is_exhausted(&self, restarts: u32) -> bool {
        restarts > self.max_restarts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tiered_schedule_is_one_second_ten_seconds_one_minute_three_times() {
        // 已知字面量（ADR-0004 决策 4）：1s / 10s / 1min，上限 3 次。
        let policy = RetryPolicy::tiered();
        assert_eq!(policy.schedule_ms(), &[1_000, 10_000, 60_000]);
        assert_eq!(policy.max_restarts(), DEFAULT_MAX_RESTARTS);
        assert_eq!(policy.backoff_for(1), Some(Duration::from_secs(1)));
        assert_eq!(policy.backoff_for(2), Some(Duration::from_secs(10)));
        assert_eq!(policy.backoff_for(3), Some(Duration::from_secs(60)));
    }

    #[test]
    fn the_fourth_crash_exhausts_the_budget_and_quarantines() {
        let policy = RetryPolicy::tiered();
        assert_eq!(policy.backoff_for(4), None, "第 4 次没有档位可睡");
        assert_eq!(policy.backoff_for(0), None, "重启次数从 1 起算");
        // 三次崩溃各换来一次重启（1s / 10s / 1min）；第 4 次崩溃才耗尽。
        assert!(!policy.is_exhausted(1));
        assert!(!policy.is_exhausted(2));
        assert!(!policy.is_exhausted(3), "第三次崩溃后还有 1min 档那一次重启");
        assert!(policy.is_exhausted(4), "第四次崩溃 → quarantined");
    }

    #[test]
    fn an_injected_schedule_never_wraps_around() {
        let policy = RetryPolicy::with_schedule(vec![5, 6], 2);
        assert_eq!(policy.backoff_for(1), Some(Duration::from_millis(5)));
        assert_eq!(policy.backoff_for(2), Some(Duration::from_millis(6)));
        assert_eq!(policy.backoff_for(3), None, "超上限不给档位（不循环）");
        assert!(!policy.is_exhausted(2), "第二次崩溃后还有第二次重启");
        assert!(policy.is_exhausted(3));
    }
}
