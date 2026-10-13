//! 内存事件总线（in-memory event bus）：tokio broadcast。
//!
//! GLOSSARY：事件总线是五服务之一，编排器与 agent-runner 的统一事件回流面，
//! 面板只做投影层。本票只提供广播原语与盖章职责（id / occurred_at / audited）；
//! 持久化的事件日志与 WS 投影面归 #15，`ledger record` 补账归 #20。

use std::sync::atomic::{AtomicU64, Ordering};

use farm_sdk::events::{EventEnvelope, NewEvent};
use tokio::sync::broadcast;

use crate::now_rfc3339;

/// 事件 id 的进程内前缀（保证重启后可区分）。
fn process_epoch() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 广播型事件总线。
#[derive(Clone, Debug)]
pub struct EventBus {
    tx: broadcast::Sender<EventEnvelope>,
    capacity: usize,
    seq: std::sync::Arc<AtomicU64>,
    epoch: u64,
}

impl EventBus {
    /// 默认容量：足够面板 + 编排器 + notifier 各开一条订阅而不轻易 lag。
    pub const DEFAULT_CAPACITY: usize = 1024;

    pub fn with_capacity(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity.max(1));
        Self {
            tx,
            capacity: capacity.max(1),
            seq: std::sync::Arc::new(AtomicU64::new(0)),
            epoch: process_epoch(),
        }
    }

    pub fn new() -> Self {
        Self::with_capacity(Self::DEFAULT_CAPACITY)
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// 开一条订阅（面板/编排器/notifier 各一条）。
    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.tx.subscribe()
    }

    /// 当前订阅者数量。
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }

    /// 发布一条事件：宿主盖章（id / occurred_at / audited）后广播。
    ///
    /// 没有订阅者时事件被丢弃（内存总线不是持久化；持久化归 #15）。
    /// 宿主内的发布一律走 [`crate::AppState::publish`]（先落库再广播），
    /// 本方法只服务「就要一条活面广播」的场景（测试、纯内存工具）。
    pub fn publish(&self, new: NewEvent) -> EventEnvelope {
        let envelope = self.stamp(new);
        self.broadcast(&envelope);
        envelope
    }

    /// 盖一个信封（id / occurred_at / audited=false）但**不广播**。
    ///
    /// 发布路径（[`crate::AppState::publish`]）必须先落事件日志再广播，
    /// 所以盖章与广播必须能分开（ticket #15）。
    pub fn stamp(&self, new: NewEvent) -> EventEnvelope {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        EventEnvelope::from_new(
            new,
            format!("evt-{}-{:06}", self.epoch, seq),
            now_rfc3339(),
        )
    }

    /// 广播一个已盖章的信封。没有订阅者时事件被丢弃——这不是错误。
    pub fn broadcast(&self, envelope: &EventEnvelope) {
        // 无订阅者时 send 返回 Err（事件被丢弃）——这不是错误。
        let _ = self.tx.send(envelope.clone());
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}
