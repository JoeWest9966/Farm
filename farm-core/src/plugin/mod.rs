//! # plugin —— 进程级 IPC 插件监督（ticket #19，ADR-0004 决策 4）
//!
//! GLOSSARY「插件骨架」：**插件是宿主子进程**，宿主持有其 stdin（关 stdin 即请求有序停机，
//! 父死子陪葬），崩溃后由宿主按**分级指数退避**重试（1s/10s/1min，上限 3 次），重试耗尽
//! 进**隔离**（quarantined：停止重启、发 `needs_attention`、面板可一键恢复）。
//!
//! ```text
//!   PluginHost::launch ──► spawn 子进程（stdin/stdout 管道，kill_on_drop）
//!                          │  stdin ← 宿主持有（关它 = 有序停机）
//!                          │  stdout → `hello` 握手行（契约版本自描述）
//!                          ▼
//!                     监督循环 ── 正常退出请求（stop）──► 关 stdin → 等宽限 → 强杀
//!                          │
//!                          └─ 非正常退出 ─► 崩溃计数 +1（v8 落账）
//!                                            ├─ 计数 ≤ 3 ─► 睡退避（1s/10s/1min）后重启
//!                                            │              └─ 重启后插件从契约 API 读状态自愈
//!                                            └─ 计数 > 3 ─► quarantined
//!                                                           ├─ plugin_quarantined 事件
//!                                                           ├─ needs_attention 一等事件
//!                                                           └─ 停止重启（防崩溃风暴）
//!                       恢复接口 recover()：重置计数 → 重启 → 自愈
//! ```
//!
//! ## 插件是契约客户端，不是后门
//! 插件经 `farm-sdk` 以**契约客户端**身份操作（`POST /handshake`、`GET /v1/tracker/issues`、
//! `POST /v1/tracker/claims`）——与编排器、面板**同一条路由、同一个原子 claim**
//! （GLOSSARY「原子 Claim」）。宿主不为插件开任何私有接口；插件也不直连宿主私有存储
//! （ADR-0005 §5）。所以插件的自愈 = 重启后重新走一遍契约读（示例插件见
//! `examples/plugin-skeleton`）。
//!
//! ## pi 子进程不是插件
//! ADR-0004 决策 4 末句：agent-runner 的 pi 子进程归**判据阶梯**管，不进本模块的重试策略。
//!
//! ## 模块分层（与 tracker / merge / agent-runner 同构）
//! - [`mod`]（本文件）—— [`PluginHost`] 句柄 + 监督循环 + 事件投影；
//! - [`process`] —— 子进程 spawn / stdin 持有 / 握手行读取 / 有序停机原语；
//! - [`store`] —— 插件状态的宿主私有存储（migration v8 `plugin_state` 表）；
//! - [`migrations`] —— v8 的 SQL（`crate::migrations` 只登记版本）；
//! - [`retry`] —— 分级指数退避策略（1s/10s/1min，上限 3 次）。

pub mod migrations;
pub mod process;
pub mod retry;
pub mod store;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use farm_sdk::admin::PluginSummary;
use farm_sdk::events::{EventKind, NewEvent};
use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinHandle;

use crate::db::Database;
use crate::error::CoreError;
use crate::state::AppState;
use crate::now_rfc3339;

pub use farm_sdk::admin::PluginState;
pub use process::{PluginHello, PluginProcess, PluginSpec};
pub use retry::{RetryPolicy, BACKOFF_SCHEDULE_MS, DEFAULT_MAX_RESTARTS};
pub use store::{parse_state, PluginStore, PLUGIN_TABLE};

/// 一次插件状态的完整快照（宿主私有；面板读它经宿主管理 API 的投影）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginStatus {
    pub plugin_id: String,
    pub name: String,
    /// 拉起命令（`program args…`，只作诊断展示）。
    pub program: String,
    /// 契约面状态（[`farm_sdk::admin::PluginState`] 三态闭集）。
    pub state: PluginState,
    /// 分级退避已用掉的重启次数（v8 持久项）。
    pub restarts: u32,
    /// 当前子进程 pid（没有活着的子进程 → `None`）。
    pub pid: Option<u32>,
    /// 最近一次崩溃的原因（恢复后清掉）。
    pub last_error: Option<String>,
    /// 退避窗口内的下次重启时刻（RFC3339；`None` = 没有待定的重启）。
    pub next_restart_at: Option<String>,
    pub updated_at: String,
}

impl PluginStatus {
    /// 宿主管理 API 的投影形状（GLOSSARY「宿主管理 API」：#23 的 `GET /admin/plugins`
    /// 直接用它，契约形状的单一源仍在 farm-sdk）。
    pub fn to_summary(&self) -> PluginSummary {
        PluginSummary {
            plugin_id: self.plugin_id.clone(),
            name: self.name.clone(),
            state: self.state,
            restarts: self.restarts,
        }
    }
}

/// 插件宿主错误（宿主侧形状；投影到契约闭集错误由调用方决定）。
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("plugin {plugin_id} is not registered")]
    Unknown { plugin_id: String },

    #[error("plugin {plugin_id} is already supervised; recovery is for stopped/quarantined plugins")]
    AlreadyRunning { plugin_id: String },

    #[error("plugin {plugin_id} is {state}, not running")]
    NotRunning { plugin_id: String, state: PluginState },

    #[error("cannot spawn plugin {plugin_id} ({program}): {source}")]
    Spawn {
        plugin_id: String,
        program: String,
        #[source]
        source: CoreError,
    },

    #[error("plugin {plugin_id} did not handshake within {timeout_ms} ms: {reason}")]
    Handshake {
        plugin_id: String,
        timeout_ms: u64,
        reason: String,
    },

    #[error("plugin {plugin_id} declined the handshake: {reason}")]
    HandshakeRejected { plugin_id: String, reason: String },

    #[error("plugin {plugin_id} exited with status {status} before its handshake")]
    ExitedBeforeHandshake { plugin_id: String, status: String },

    #[error("plugin spec is invalid: {0}")]
    InvalidSpec(String),

    #[error(transparent)]
    Store(#[from] CoreError),
}

/// 插件宿主句柄（`Clone` 廉价：内部共享同一个注册表与存储句柄）。
///
/// 挂在 [`crate::AppState`] 上（加法字段 `AppState::plugin`）：宿主启动时按清单拉起插件，
/// 停机时有序收尾；宿主管理 API（#23）与面板（#25）读 [`PluginHost::statuses`] 与
/// [`PluginHost::recover`]。
#[derive(Clone, Debug)]
pub struct PluginHost {
    inner: Arc<PluginHostInner>,
}

#[derive(Debug)]
struct PluginHostInner {
    store: PluginStore,
    policy: RetryPolicy,
    /// 拉起计划（plugin_id → spec）。运行期事实，**不落库**：spec 的 env 可能带凭据
    /// （ADR-0004 决策 3：Farm 对凭据零缓存、零落盘），宿主重启后由启动清单重新注册。
    specs: Mutex<HashMap<String, PluginSpec>>,
    /// 活着的监督任务（plugin_id → 收尾信号 + join 句柄）。
    live: Mutex<HashMap<String, LivePlugin>>,
}

#[derive(Debug)]
struct LivePlugin {
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl PluginHost {
    /// 用宿主私有存储构造（监督策略用规范默认值）。
    pub fn new(db: &Database) -> Self {
        Self::with_policy(db, RetryPolicy::tiered())
    }

    /// 覆写监督策略（测试注入快退避，不必真睡 1s/10s/1min）。
    pub fn with_policy(db: &Database, policy: RetryPolicy) -> Self {
        Self {
            inner: Arc::new(PluginHostInner {
                store: PluginStore::new(db),
                policy,
                specs: Mutex::new(HashMap::new()),
                live: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// 底层存储句柄（宿主启动对账 / 诊断用；schema 不外泄，ADR-0005 §5）。
    pub fn store(&self) -> &PluginStore {
        &self.inner.store
    }

    /// 监督策略（1s/10s/1min、上限 3 次）。
    pub fn policy(&self) -> &RetryPolicy {
        &self.inner.policy
    }

    /// 拉起一个插件：write-ahead 落账 → spawn 子进程 → 等 `hello` 握手 → 交监督循环。
    ///
    /// 握手失败**不等于**拉起失败：监督循环已接手，会按分级退避重试（起不来也是一种崩溃）。
    /// 调用方拿到的 `Err` 只描述「第一次为什么没成」。
    pub async fn launch(
        &self,
        host: &AppState,
        spec: &PluginSpec,
    ) -> Result<PluginHello, PluginError> {
        self.start(host, spec).await
    }

    /// **恢复接口**（GLOSSARY「隔离」：面板可一键恢复）。
    ///
    /// quarantined / stopped 的插件重置重试预算后重新拉起；重启后插件自己从契约 API
    /// 读状态自愈（示例插件见 `examples/plugin-skeleton`）。宿主管理 API（#23）与面板（#25）
    /// 经它实现一键恢复，**不**另开私有后门。
    pub async fn recover(
        &self,
        host: &AppState,
        plugin_id: &str,
    ) -> Result<PluginHello, PluginError> {
        let spec = self.spec_of(plugin_id).await?;
        self.start(host, &spec).await
    }

    /// 有序停机一个插件：关 stdin → 宽限等待 → 强杀（[`process::PluginProcess::shutdown`]）。
    pub async fn shutdown(&self, plugin_id: &str) -> Result<(), PluginError> {
        let live = { self.inner.live.lock().await.remove(plugin_id) };
        let Some(mut live) = live else {
            return Err(PluginError::Unknown {
                plugin_id: plugin_id.to_string(),
            });
        };
        let _ = live.stop.send(());
        // 监督循环自己收尾（关 stdin + 宽限 + 强杀）；这里只等它做完。
        let grace = self
            .spec_of(plugin_id)
            .await
            .map(|spec| spec.shutdown_grace)
            .unwrap_or(DEFAULT_SHUTDOWN_GRACE);
        match tokio::time::timeout(grace + SHUTDOWN_JOIN_SLACK, &mut live.task).await {
            Ok(joined) => {
                if let Err(e) = joined {
                    tracing::warn!(plugin = %plugin_id, error = %e, "plugin supervision task panicked");
                }
            }
            Err(_elapsed) => {
                // 超时 = 监督循环没在宽限内收尾。**必须 abort，不能 detach**：detach 的
                // 任务会继续跑完它的退避睡眠，然后在没人知道的情况下重启插件——那个进程
                // 不在 `live` 表里，`shutdown_all` / `Drop` 都 reap 不到它（孤儿）。
                // abort 丢掉任务的 future，其中的 `PluginProcess` 随之 drop，
                // `kill_on_drop` 保证子进程陪葬（ADR-0004 决策 4「父死子陪葬」）。
                live.task.abort();
                tracing::warn!(
                    plugin = %plugin_id,
                    grace_ms = grace.as_millis(),
                    "plugin supervision did not finish within the shutdown window; the task was aborted"
                );
            }
        }
        Ok(())
    }

    /// 宿主停机：把**所有**插件有序收尾（一个孤儿都不留）。
    pub async fn shutdown_all(&self) {
        let ids: Vec<String> = { self.inner.live.lock().await.keys().cloned().collect() };
        for id in ids {
            if let Err(e) = self.shutdown(&id).await {
                tracing::warn!(plugin = %id, error = %e, "plugin shutdown reported an error");
            }
        }
    }

    /// 正在被监督的插件 id（宿主管理面可读）。
    pub async fn supervised(&self) -> Vec<String> {
        let mut ids: Vec<String> = { self.inner.live.lock().await.keys().cloned().collect() };
        ids.sort();
        ids
    }

    /// 单插件状态快照（v8 落账的那一行）。
    pub fn status(&self, plugin_id: &str) -> Result<Option<PluginStatus>, CoreError> {
        self.inner.store.get(plugin_id)
    }

    /// 全部插件状态快照（面板 / 宿主管理 API 的读源）。
    pub fn statuses(&self) -> Result<Vec<PluginStatus>, CoreError> {
        self.inner.store.list()
    }

    /// 等到某个插件进入目标状态（测试与管理面轮询用；超时回 `Err`）。
    pub async fn wait_for_state(
        &self,
        plugin_id: &str,
        want: PluginState,
        timeout: Duration,
    ) -> Result<PluginStatus, PluginError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match self.status(plugin_id)? {
                Some(status) if status.state == want => return Ok(status),
                Some(_) => {}
                None => {
                    return Err(PluginError::Unknown {
                        plugin_id: plugin_id.to_string(),
                    })
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(PluginError::Handshake {
                    plugin_id: plugin_id.to_string(),
                    timeout_ms: timeout.as_millis() as u64,
                    reason: format!("plugin never reached state {want}"),
                });
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// launch / recover 共用的起动路径。
    async fn start(&self, host: &AppState, spec: &PluginSpec) -> Result<PluginHello, PluginError> {
        spec.validate()?;
        if self.inner.live.lock().await.contains_key(&spec.plugin_id) {
            return Err(PluginError::AlreadyRunning {
                plugin_id: spec.plugin_id.clone(),
            });
        }
        // 计划登记（恢复接口靠它重建子进程；不落库，见 `PluginHostInner::specs`）。
        self.inner
            .specs
            .lock()
            .await
            .insert(spec.plugin_id.clone(), spec.clone());

        // write-ahead：先落账再 spawn（崩在 spawn 半路也有账可查）。
        self.inner.store.upsert(&PluginStatus {
            plugin_id: spec.plugin_id.clone(),
            name: spec.name.clone(),
            program: spec.describe(),
            state: PluginState::Running,
            restarts: 0,
            pid: None,
            last_error: None,
            next_restart_at: None,
            updated_at: now_rfc3339(),
        })?;

        let (stop_tx, stop_rx) = oneshot::channel();
        let (handshake_tx, handshake_rx) = oneshot::channel();
        // 任务只持 `Arc<PluginHostInner>`（不持 PluginHost 句柄）：否则宿主最后一个句柄
        // 被丢掉时引用计数不归零，`Drop` 的兜底强杀永远等不到。
        let inner = Arc::clone(&self.inner);
        let host = host.clone();
        let spec_owned = spec.clone();
        let task = tokio::spawn(async move {
            supervise(inner, host, spec_owned, None, stop_rx, handshake_tx).await;
        });
        self.inner.live.lock().await.insert(
            spec.plugin_id.clone(),
            LivePlugin {
                stop: stop_tx,
                task,
            },
        );

        // 等第一次握手：调用方（宿主启动 / 管理面）要一个同步的答案。
        match tokio::time::timeout(spec.handshake_timeout + SHUTDOWN_JOIN_SLACK, handshake_rx).await
        {
            Ok(Ok(result)) => result,
            Ok(Err(_closed)) => Err(PluginError::Handshake {
                plugin_id: spec.plugin_id.clone(),
                timeout_ms: spec.handshake_timeout.as_millis() as u64,
                reason: "the supervision task ended before the handshake".into(),
            }),
            Err(_elapsed) => Err(PluginError::Handshake {
                plugin_id: spec.plugin_id.clone(),
                timeout_ms: spec.handshake_timeout.as_millis() as u64,
                reason: "no handshake before the deadline".into(),
            }),
        }
    }

    async fn spec_of(&self, plugin_id: &str) -> Result<PluginSpec, PluginError> {
        self.inner
            .specs
            .lock()
            .await
            .get(plugin_id)
            .cloned()
            .ok_or_else(|| PluginError::Unknown {
                plugin_id: plugin_id.to_string(),
            })
    }
}

/// 监督循环（每插件一个任务，自持：终态已落账，没人需要 join 它）。
///
/// ```text
/// spawn → hello 握手 ──成功──► 监督：select!{ 子进程退出 / 插件说话 / 停机信号 }
///              │                     │
///              │                     └─ 停机 ─► 关 stdin → 宽限 → 强杀 ─► stopped
///              │                     └─ 自己退了（有序） ─► stopped，不重试
///              │                     └─ 崩了 ─► 计数 +1 ─► 退避重试 / 隔离
///              └─ 失败 ─► 计数 +1 ─► 同上
/// ```
async fn supervise(
    inner: Arc<PluginHostInner>,
    host: AppState,
    spec: PluginSpec,
    mut process: Option<PluginProcess>,
    mut stop: oneshot::Receiver<()>,
    first_handshake: oneshot::Sender<Result<PluginHello, PluginError>>,
) {
    // 只有**第一次**握手结果要回给调用方（launch/recover 的同步答案）。
    let mut first_handshake = Some(first_handshake);
    let plugin_id = spec.plugin_id.clone();
    loop {
        if process.is_none() {
            match PluginProcess::spawn(&spec).await {
                Ok(spawned) => process = Some(spawned),
                Err(e) => {
                    let reason = e.to_string();
                    if let Some(tx) = first_handshake.take() {
                        let _ = tx.send(Err(e));
                    }
                    if !on_crash(&inner, &host, &spec, &reason, &mut stop).await {
                        break;
                    }
                    continue;
                }
            }
        }
        let mut child = process.take().expect("a live child was just ensured");
        match child.handshake().await {
            Ok(hello) => {
                if let Some(tx) = first_handshake.take() {
                    let _ = tx.send(Ok(hello.clone()));
                }
                on_handshake(&inner, &spec, &hello, child.pid());
                let outcome = loop {
                    let outcome = tokio::select! {
                        status = child.child.wait() => Supervision::Exited(status.ok()),
                        Some(_line) = child.lines.recv() => Supervision::Chatter,
                        _ = &mut stop => Supervision::StopRequested,
                    };
                    if matches!(outcome, Supervision::Chatter) {
                        continue;
                    }
                    break outcome;
                };
                match outcome {
                    Supervision::StopRequested => {
                        shutdown_plugin(&inner, &spec, child).await;
                        break;
                    }
                    Supervision::Exited(Some(status)) if status.success() => {
                        // 插件自己退了（有序停机或自愿退出）：不重试、不升级。
                        on_voluntary_exit(&inner, &spec, &status);
                        break;
                    }
                    Supervision::Exited(status) => {
                        let reason = match status {
                            Some(status) => format!("exited with {status}"),
                            None => "exited without a status".to_string(),
                        };
                        drop(child);
                        if !on_crash(&inner, &host, &spec, &reason, &mut stop).await {
                            break;
                        }
                    }
                    Supervision::Chatter => unreachable!("chatter loops above"),
                }
            }
            Err(e) => {
                let reason = e.to_string();
                if let Some(tx) = first_handshake.take() {
                    let _ = tx.send(Err(e));
                }
                child.kill().await;
                drop(child);
                if !on_crash(&inner, &host, &spec, &reason, &mut stop).await {
                    break;
                }
            }
        }
    }
    inner.live.lock().await.remove(&plugin_id);
    tracing::debug!(plugin = %plugin_id, "plugin supervision task finished");
}

/// 握手成功：重试预算归零（自愈从干净账面开始）+ 记 pid。
fn on_handshake(inner: &PluginHostInner, spec: &PluginSpec, hello: &PluginHello, pid: Option<u32>) {
    if let Err(e) = inner.store.reset(&spec.plugin_id, pid, &now_rfc3339()) {
        tracing::warn!(plugin = %spec.plugin_id, error = %e, "plugin state could not be reset");
        }
        tracing::info!(
            plugin = %spec.plugin_id,
            contract = %hello.contract_version,
            kind = ?hello.kind,
            pid = ?pid,
            "plugin handshook and is supervised (contract client, no private backdoor)"
        );
    }

/// 插件自己退了（成功退出）：标 stopped，**不进**重试策略（GLOSSARY：隔离只防崩溃风暴）。
fn on_voluntary_exit(inner: &PluginHostInner, spec: &PluginSpec, status: &std::process::ExitStatus) {
    let at = now_rfc3339();
    let _ = inner
        .store
        .set_state(&spec.plugin_id, PluginState::Stopped, &at);
    let _ = inner
        .store
        .set_next_restart(&spec.plugin_id, None, &at);
    let _ = inner.store.set_pid(&spec.plugin_id, None, &at);
    tracing::info!(plugin = %spec.plugin_id, %status, "plugin exited on its own (stopped, no retry)");
}

    /// 记一次崩溃并决定下一步：退避后重启（`true`）或进隔离（`false`）。
    ///
    /// ADR-0004 决策 4：分级指数退避 1s/10s/1min，上限 3 次；耗尽 → quarantined
    /// （停止重启 + `needs_attention` + 面板一键恢复）。崩溃/重启同时进 Kron 账
    /// （escalation，`actor=farm`）——经 #20 的审计双写路径，未入账只标记不阻塞。
    ///
    /// `stop` 是宿主的停机信号（oneshot 接收端）：退避睡眠与它 `select!`，停机先行即
    /// 返回 `false`（不再重启），监督循环因此退出（code review P1-2）。
async fn on_crash(
    inner: &PluginHostInner,
    host: &AppState,
    spec: &PluginSpec,
    reason: &str,
    stop: &mut oneshot::Receiver<()>,
) -> bool {
    let at = now_rfc3339();
    let restarts = match inner.store.note_crash(&spec.plugin_id, reason, &at) {
            Ok(restarts) => restarts,
            Err(e) => {
                tracing::error!(plugin = %spec.plugin_id, error = %e, "plugin crash could not be recorded");
                0
            }
        };
        let max = inner.policy.max_restarts();

        if restarts > max {
            // 重试耗尽 → 隔离（quarantined）：停止重启，等人裁决。
            let _ = inner
                .store
                .set_state(&spec.plugin_id, PluginState::Quarantined, &at);
            let _ = inner
                .store
                .set_next_restart(&spec.plugin_id, None, &at);
            let _ = inner.store.set_pid(&spec.plugin_id, None, &at);
            if let Ok(Some(status)) = inner.store.get(&spec.plugin_id) {
                publish(host, quarantined_event(&status));
                publish(host, needs_attention_event(&status, "plugin_quarantined"));
            }
            tracing::error!(
                plugin = %spec.plugin_id,
                restarts,
                max_restarts = max,
                "plugin quarantined after the retry budget was exhausted; restart is a human decision"
            );
            return false;
        }

        let wait = inner.policy.backoff_for(restarts).unwrap_or_default();
        let _ = inner
            .store
            .set_next_restart(&spec.plugin_id, Some(&rfc3339_after(wait)), &at);
        publish(
            host,
            NewEvent::new(EventKind::PluginRestarted, "host").with_payload(serde_json::json!({
                PLUGIN_ID_KEY: spec.plugin_id,
                "plugin": spec.name,
                "restarts": restarts,
                "maxRestarts": max,
                "backoffMs": wait.as_millis(),
                "reason": reason,
            })),
        );
        tracing::warn!(
            plugin = %spec.plugin_id,
            restarts,
            max_restarts = max,
            backoff_ms = wait.as_millis(),
            "plugin crashed; restarting after the tiered backoff"
        );
        // 退避睡眠**必须**可被停机信号打断（code review P1-2）：否则 `shutdown` 在
        // 10s/60s 档上等满宽限后只能 detach 监督任务，任务睡醒照样重启插件——那个
        // 进程不在 `live` 表里，宿主收尾（`shutdown_all` / `Drop`）reap 不到它，
        // 成为孤儿（破 ADR-0004 决策 4「父死子陪葬」与 #19「宿主收尾后无孤儿插件进程」）。
        tokio::select! {
            _ = tokio::time::sleep(wait) => true,
            _ = &mut *stop => {
                // 停机先行：账面落 stopped（不留「running + 有待定重启」的假象），不再重启。
                let at = now_rfc3339();
                let _ = inner.store.set_state(&spec.plugin_id, PluginState::Stopped, &at);
                let _ = inner.store.set_next_restart(&spec.plugin_id, None, &at);
                let _ = inner.store.set_pid(&spec.plugin_id, None, &at);
                tracing::info!(
                    plugin = %spec.plugin_id,
                    backoff_ms = wait.as_millis(),
                    "plugin shutdown arrived during the backoff window; the restart is cancelled"
                );
                false
            }
        }
}

/// 有序停机：关 stdin → 宽限 → 强杀，然后落 `stopped`。
async fn shutdown_plugin(inner: &PluginHostInner, spec: &PluginSpec, child: PluginProcess) {
    let outcome = child.shutdown(spec.shutdown_grace).await;
    let at = now_rfc3339();
    let _ = inner
        .store
        .set_state(&spec.plugin_id, PluginState::Stopped, &at);
    let _ = inner
        .store
        .set_next_restart(&spec.plugin_id, None, &at);
    let _ = inner.store.set_pid(&spec.plugin_id, None, &at);
    tracing::info!(
        plugin = %spec.plugin_id,
        status = ?outcome,
        "plugin shut down in order (stdin closed, then a grace window, then SIGKILL)"
    );
}

impl Drop for PluginHost {
    fn drop(&mut self) {
        // 最后一个句柄也没了（宿主卸载 / 测试收尾）：监督任务立刻停。
        // 有序收尾是 [`PluginHost::shutdown_all`]；这里是**兜底**——任务一 abort，
        // 它手里的 `PluginProcess` 被丢掉，`kill_on_drop` 保证子进程陪葬（无孤儿）。
        let Ok(mut live) = self.inner.live.try_lock() else {
            return; // 有任务正拿着锁：它会自己收尾，不阻塞 drop
        };
        for (plugin_id, entry) in live.drain() {
            let _ = entry.stop.send(());
            entry.task.abort();
            tracing::debug!(plugin = %plugin_id, "plugin supervision aborted with the host handle");
        }
    }
}

/// 监督循环的一个回合结果。
enum Supervision {
    /// 子进程退出（`None` = 拿不到退出状态）。
    Exited(Option<std::process::ExitStatus>),
    /// 插件在 stdout 上说话（忽略内容，只保持管道被抽干）。
    Chatter,
    /// 宿主请求有序停机。
    StopRequested,
}

/// 轮询间隔（等状态 / 等第一次握手）：小到不拖慢测试，大到不烧 CPU。
pub const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// 等监督任务收尾的额外宽限（停机宽限之外）。
pub const SHUTDOWN_JOIN_SLACK: Duration = Duration::from_secs(5);

/// 宿主启动对账：上一次运行里还是 `running` 的插件，其子进程已随宿主死去。
///
/// ADR-0004 决策 4 + GLOSSARY「中断态」的精神：宿主不装作它们还活着（标 `stopped`），
/// 也**不**悄悄重启（重启由启动清单或人裁决），`quarantined` 一律保持隔离。
pub fn reconcile_on_startup(host: &PluginHost) -> Result<usize, CoreError> {
    let at = now_rfc3339();
    let mut stopped = 0;
    for status in host.store().list()? {
        if status.state == PluginState::Running {
            host.store()
                .set_state(&status.plugin_id, PluginState::Stopped, &at)?;
            stopped += 1;
        }
    }
    Ok(stopped)
}

/// 停机宽限的默认值：关 stdin 之后等插件自己退出的时长，超时才强杀。
pub const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// 握手等待的默认值：拉起后等插件自描述（`hello` 行）的时长。
pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// 事件载荷里的插件身份键（面板 / 通知通道按它聚合）。
pub const PLUGIN_ID_KEY: &str = "plugin_id";

/// 隔离时发的一等事件（GLOSSARY：`needs_attention` 面板必渲染）。
pub fn needs_attention_event(status: &PluginStatus, reason: &str) -> NewEvent {
    NewEvent::new(EventKind::NeedsAttention, "host").with_payload(serde_json::json!({
        "reason": reason,
        PLUGIN_ID_KEY: status.plugin_id,
        "plugin": status.name,
        "restarts": status.restarts,
        "recovery": "plugin recovery interface (host admin API, ticket #23)",
    }))
}

/// 隔离本身的事件（闭集里的 `plugin_quarantined`）。
pub fn quarantined_event(status: &PluginStatus) -> NewEvent {
    NewEvent::new(EventKind::PluginQuarantined, "host").with_payload(serde_json::json!({
        PLUGIN_ID_KEY: status.plugin_id,
        "plugin": status.name,
        "restarts": status.restarts,
        "maxRestarts": DEFAULT_MAX_RESTARTS,
        "backoffMs": BACKOFF_SCHEDULE_MS,
    }))
}

/// 发一条事件；落库失败只告警（监督循环不该被事件日志拖死）。
fn publish(host: &AppState, new: NewEvent) {
    if let Err(e) = host.publish(new) {
        tracing::warn!(error = %e, "plugin event could not be published");
    }
}

/// `now + delta` 的 RFC3339 文本（退避窗口的到期时刻）。
fn rfc3339_after(delta: Duration) -> String {
    use time::format_description::well_known::Rfc3339;
    let at = time::OffsetDateTime::now_utc()
        + time::Duration::nanoseconds(delta.as_nanos().min(i64::MAX as u128) as i64);
    at.replace_nanosecond(0)
        .ok()
        .and_then(|t| t.format(&Rfc3339).ok())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}
