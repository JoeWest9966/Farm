//! Agent-Runner 服务面（GLOSSARY：以统一接口派发与管理 agent；本地后端）。
//!
//! 编排四件已落地的接缝：`process`（spawn + 读循环）→ `ladder`（判据阶梯）→
//! `accounting`（token 绝对累计）→ `store`（attempts 持久化），外加
//! `credentials`（凭据只在 spawn 窗口内过路，#17 的实现面）。
//!
//! ## 形状
//! - [`AgentRunner`] 是挂在 [`crate::AppState`] 上的**句柄**（`Clone` 廉价）；
//! - 每个 attempt 一个 tokio 任务（「一个 agent 一个 pi 子进程」，与研究票 §3 同构）；
//! - 任务经 [`crate::AppState::publish`] 把 disposition 与终态回流进事件总线
//!   （持久化路径：先落事件日志再广播，所以投影流与历史查询看到同一份事实）。
//!
//! ## 为什么 publish 要由调用方把 `&AppState` 传进来
//! 构造期循环：`AppState` 持有 `AgentRunner`，句柄不能再反向持有一个 `AppState`
//! （值或 `Arc` 都构造不出来）。于是发布面作为参数传入——监督任务拿一份
//! **`AppState` 克隆**（字段全是 `Arc`/`Clone`，廉价且 `'static`），
//! 既不循环，也不需要第二条「持久化再广播」的实现。
//!
//! ## pi 子进程不是插件
//! ADR-0004 决策 4 末句：pi 子进程不进插件重试策略。崩溃后的唯一恢复手段是判据阶梯
//! 第四梯 **fork 找回**（重 spawn 续会话 + 从某条 user message 分叉），
//! 次数有上限（[`RunnerConfig::max_fork_recoveries`]）——不是无限重启。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};

use farm_sdk::events::{EventKind, NewEvent};
use farm_sdk::services::agent_runner::{
    AbortRequest, AbortResponse, AttemptState, DispatchRequest, DispatchResponse, Disposition,
    SteerRequest, SteerResponse,
};

use crate::agent_runner::accounting::{TokenAccounting, TokenSnapshot};
use crate::agent_runner::credentials::{
    CredentialCourier, CredentialRequest, EnvPolicy, NoCredentialCourier,
};
use crate::agent_runner::ladder::{EventSignals, JudgementLadder, LadderAction, LadderConfig, LadderPhase};
use crate::agent_runner::process::{
    probe_version, round_trip, PiProcess, PiProcessParts, PiSpawnPlan, PiVersionProbe,
};
use crate::agent_runner::rpc::{parse_frame, CommandKind, Frame, RpcCommand, RpcResponse};
use crate::agent_runner::store::{AttemptRecord, AttemptStore};
use crate::error::CoreError;

/// 本地后端的运行配置（测试把 `program` 指向 fake pi 替身）。
#[derive(Clone, Debug)]
pub struct RunnerConfig {
    /// pi 可执行文件（真机 `pi`；测试 fake pi）。
    pub program: String,
    /// spawn 参数（默认 `["--mode", "rpc"]`）。
    pub args: Vec<String>,
    /// 规格钉的 wire protocol 版本（探测到别的版本只记录偏差，不硬失败）。
    pub expected_version: String,
    /// 判据阶梯（两口钟 + abort 探针）。
    pub ladder: LadderConfig,
    /// 启动探测 / 查询往返的超时。
    pub probe_timeout: Duration,
    /// 收尾宽限：关 stdin 之后等这么久再强杀（研究票 §6 的三段式）。
    pub shutdown_grace: Duration,
    /// fork 找回的最大次数（不是无限重启：pi 子进程不进插件重试策略）。
    pub max_fork_recoveries: usize,
    /// 只进 pi 子进程的 env（测试注入 `FAKE_PI_SCENARIO` 等；凭据另走 courier）。
    pub scenario_env: Vec<(String, String)>,
    /// pi 子进程的 cwd（agent 的工作目录）。
    pub cwd: Option<std::path::PathBuf>,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            program: "pi".into(),
            args: vec!["--mode".to_string(), "rpc".into()],
            expected_version: "1.0.2".into(),
            ladder: LadderConfig::default(),
            probe_timeout: Duration::from_secs(10),
            shutdown_grace: Duration::from_secs(5),
            max_fork_recoveries: 1,
            scenario_env: Vec::new(),
            cwd: None,
        }
    }
}

impl RunnerConfig {
    /// 测试用：program 指向 fake pi，两口钟压到毫秒级。
    pub fn for_fake_pi(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: vec!["--mode".to_string(), "rpc".into()],
            ladder: LadderConfig::fast(400, 10_000, 600),
            probe_timeout: Duration::from_secs(5),
            shutdown_grace: Duration::from_secs(2),
            ..Default::default()
        }
    }
}

/// agent-runner 自己的错误（宿主内部形状；投影到契约闭集见
/// [`RunnerError::to_contract_error`]）。
#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("attempt {attempt_id} is not known to this host")]
    NotFound { attempt_id: String },

    #[error("ticket {ticket_id} already has a running attempt ({attempt_id})")]
    Conflict { ticket_id: String, attempt_id: String },

    #[error("attempt {attempt_id} is no longer running (state {state:?})")]
    NotRunning { attempt_id: String, state: AttemptState },

    #[error("attempt {attempt_id} cannot accept injections: {reason}")]
    Unavailable { attempt_id: String, reason: String },

    #[error("agent-runner rejected the request: {0}")]
    Invalid(String),

    /// 宿主私有存储 / 事件总线故障（投影回 [`CoreError`] 的闭集形状）。
    #[error("host-private store or event bus failure: {0}")]
    Store(#[from] CoreError),
}

impl RunnerError {
    /// 投影到契约闭集错误（ADR-0001 决策 6：错误码是闭集）。
    pub fn to_contract_error(&self) -> farm_sdk::ContractError {
        use farm_sdk::ContractError;
        match self {
            RunnerError::NotFound { attempt_id } => {
                ContractError::not_found(format!("attempt {attempt_id} is not known to this host"))
            }
            RunnerError::Conflict {
                ticket_id,
                attempt_id,
            } => ContractError::conflict(format!(
                "ticket {ticket_id} already has a running attempt ({attempt_id})"
            )),
            RunnerError::NotRunning { attempt_id, state } => ContractError::conflict(format!(
                "attempt {attempt_id} is no longer running (state {state:?})"
            )),
            RunnerError::Unavailable { attempt_id, reason } => {
                ContractError::service_unavailable(format!(
                    "attempt {attempt_id} cannot accept injections: {reason}"
                ))
            }
            RunnerError::Invalid(message) => ContractError::invalid_request(message.clone()),
            RunnerError::Store(error) => error.to_contract_error(),
        }
    }
}

impl From<RunnerError> for farm_sdk::ContractError {
    fn from(err: RunnerError) -> Self {
        err.to_contract_error()
    }
}

/// 监督任务发回给 steer/follow_up 调用方的受理结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InjectionOutcome {
    /// pi 给的 disposition（缺失 = 版本偏差：记录，不当失败）。
    pub disposition: Option<Disposition>,
    /// `success` 字段（受理的权威判据）。
    pub success: bool,
    /// pi 给的错误文本（`success == false` 时）。
    pub error: Option<String>,
}

/// 运行中 attempt 的命令入口（steer/follow_up/abort/clear_queue 都走这条通道）。
#[derive(Clone, Debug)]
struct LiveAttempt {
    commands: mpsc::UnboundedSender<RunnerCommand>,
}

/// 宿主 → 监督任务的命令（需要回执的才带 oneshot）。
enum RunnerCommand {
    Inject {
        channel: &'static str,
        message: String,
        reply: oneshot::Sender<Result<InjectionOutcome, CoreError>>,
    },
    Abort {
        reason: Option<String>,
    },
    ClearQueue {
        reply: oneshot::Sender<Result<u64, CoreError>>,
    },
}

/// 监督任务的一次结局（判据阶梯给的状态 + 人读原因）。
#[derive(Clone, Debug, PartialEq, Eq)]
struct AttemptOutcome {
    state: AttemptState,
    reason: String,
}

/// Agent-Runner 句柄（挂在 [`crate::AppState`] 上；`Clone` 廉价）。
#[derive(Clone, Debug)]
pub struct AgentRunner {
    db: crate::db::Database,
    store: AttemptStore,
    config: RunnerConfig,
    credentials: Arc<dyn CredentialCourier>,
    live: Arc<Mutex<HashMap<String, LiveAttempt>>>,
}

impl Default for AgentRunner {
    fn default() -> Self {
        Self::new(crate::db::Database::open_in_memory().expect("in-memory store"))
    }
}

impl AgentRunner {
    /// 默认后端：真 `pi` + 无凭据源（#17 之前的状态）。
    pub fn new(db: crate::db::Database) -> Self {
        Self {
            store: AttemptStore::new(&db),
            db,
            config: RunnerConfig::default(),
            credentials: Arc::new(NoCredentialCourier),
            live: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 换配置（测试注入 fake pi 替身；生产换 program）。
    pub fn with_config(mut self, config: RunnerConfig) -> Self {
        self.config = config;
        self
    }

    /// 换凭据源（#17 用 `nw key get` 子进程实现）。
    pub fn with_credentials(mut self, courier: Arc<dyn CredentialCourier>) -> Self {
        self.credentials = courier;
        self
    }

    pub fn config(&self) -> &RunnerConfig {
        &self.config
    }

    pub fn store(&self) -> &AttemptStore {
        &self.store
    }

    /// 底层存储句柄（宿主内部用；schema 不外泄，ADR-0005 §5）。
    pub fn database(&self) -> &crate::db::Database {
        &self.db
    }

    /// 派发一个 attempt：write-ahead 落一行 `running`，再起监督任务。
    ///
    /// 双跑防护：同一票已有 `running` attempt 时拒绝（GLOSSARY：claim 是排他声明，
    /// 宿主侧再兜一层）。
    pub async fn dispatch(
        &self,
        host: &crate::AppState,
        request: &DispatchRequest,
    ) -> Result<DispatchResponse, RunnerError> {
        if request.issue_id.trim().is_empty() {
            return Err(RunnerError::Invalid("dispatch requires a non-empty issue_id".into()).into());
        }
        if let Some(running) = self
            .store
            .list_for_ticket(&request.issue_id)?
            .into_iter()
            .find(|attempt| attempt.state == AttemptState::Running)
        {
            return Err(RunnerError::Conflict {
                ticket_id: request.issue_id.clone(),
                attempt_id: running.attempt_id,
            }
            .into());
        }

        let attempt_id = new_attempt_id(&request.issue_id);
        let record = AttemptRecord::running(
            attempt_id.clone(),
            request.issue_id.clone(),
            request.profile.clone(),
        );
        self.store.upsert(&record)?;

        // 派发是关键事件（审计双写）：落库失败即整条发布失败。
        host.publish(
            NewEvent::new(EventKind::Dispatch, "agent-runner")
                .with_ticket(&request.issue_id)
                .with_attempt(&attempt_id)
                .with_payload(json!({
                    "profile": request.profile,
                    "program": self.config.program,
                    "args": self.config.args,
                    "pinnedProtocolVersion": self.config.expected_version,
                    "silentClockMs": self.config.ladder.silent_clock.as_millis(),
                    "totalClockMs": self.config.ladder.total_clock.as_millis(),
                    "abortProbeMs": self.config.ladder.abort_probe.as_millis(),
                })),
        )?;

        let (tx, rx) = mpsc::unbounded_channel::<RunnerCommand>();
        self.live
            .lock()
            .map_err(|_| CoreError::StorePoisoned)?
            .insert(attempt_id.clone(), LiveAttempt { commands: tx });

        let runner = self.clone();
        let host = host.clone();
        let prompt = request
            .prompt
            .clone()
            .unwrap_or_else(|| format!("work on ticket {}", request.issue_id));
        let profile = request.profile.clone();
        let ticket = request.issue_id.clone();
        // 监督任务是自持的：终态已落库 + 已回流，没人需要 join 它。
        tokio::spawn(async move {
            runner
                .supervise(host, attempt_id, ticket, profile, prompt, rx)
                .await;
        });

        Ok(DispatchResponse {
            attempt_id: record.attempt_id,
            state: AttemptState::Running,
        })
    }

    /// steer：向运行中的 attempt 注入改道指令（当前轮次工具调用之后、下次 LLM 调用前送达）。
    ///
    /// 不需要 `&AppState`：disposition 回流由监督循环在收到 pi 响应时发布
    /// （那一处才知道 disposition 是什么）。
    pub async fn steer(&self, request: &SteerRequest) -> Result<SteerResponse, RunnerError> {
        self.inject(&request.attempt_id, "steer", &request.message)
            .await
    }

    /// follow_up：等 agent 完全停下才送达的追加任务。
    pub async fn follow_up(
        &self,
        attempt_id: &str,
        message: &str,
    ) -> Result<SteerResponse, RunnerError> {
        self.inject(attempt_id, "follow_up", message).await
    }

    /// abort：中止 attempt（abort 探针的独立死线由监督循环持有）。
    pub async fn abort(
        &self,
        host: &crate::AppState,
        request: &AbortRequest,
    ) -> Result<AbortResponse, RunnerError> {
        let record = self.require_attempt(&request.attempt_id)?;
        if record.state != AttemptState::Running {
            return Err(RunnerError::NotRunning {
                attempt_id: request.attempt_id.clone(),
                state: record.state,
            }
            .into());
        }
        let reason = request.reason.clone();
        self.send_command(&request.attempt_id, |commands| {
            commands.send(RunnerCommand::Abort { reason }).is_ok()
        })?;
        host.publish(
            NewEvent::new(EventKind::Abort, "agent-runner")
                .with_ticket(&record.ticket_id)
                .with_attempt(&request.attempt_id)
                .with_payload(json!({
                    "reason": request.reason,
                    "abortProbeMs": self.config.ladder.abort_probe.as_millis(),
                })),
        )?;
        Ok(AbortResponse {
            attempt_id: request.attempt_id.clone(),
            state: AttemptState::Running,
            reason: request.reason.clone(),
        })
    }

    /// clear_queue 往返：清掉 steering/followUp/被推迟的注入，返回清了几条。
    pub async fn clear_queue(&self, attempt_id: &str) -> Result<u64, RunnerError> {
        let record = self.require_attempt(attempt_id)?;
        if record.state != AttemptState::Running {
            return Err(RunnerError::NotRunning {
                attempt_id: attempt_id.to_string(),
                state: record.state,
            }
            .into());
        }
        let (tx, rx) = oneshot::channel();
        self.send_command(attempt_id, |commands| {
            commands.send(RunnerCommand::ClearQueue { reply: tx }).is_ok()
        })?;
        // 监督任务活着才可能回；断了就是 attempt 已经收尾。
        rx.await
            .map_err(|_| RunnerError::Unavailable {
                attempt_id: attempt_id.to_string(),
                reason: "the supervision task has already finished".into(),
            })?
            .map_err(RunnerError::from)
    }

    /// 单条 attempt（面板/编排器读 attempts 时间线）。
    pub fn attempt(&self, attempt_id: &str) -> Result<Option<AttemptRecord>, CoreError> {
        self.store.find(attempt_id)
    }

    /// 某票的 attempts 时间线。
    pub fn attempts_for_ticket(&self, ticket_id: &str) -> Result<Vec<AttemptRecord>, CoreError> {
        self.store.list_for_ticket(ticket_id)
    }

    /// 崩溃恢复对账：宿主重启时把 `running` attempt 标成第四终态 `interrupted`
    /// （GLOSSARY：worktree 不清、记账以最后快照为准并标截断；是否重派归编排器）。
    pub fn recover_interrupted(&self, host: &crate::AppState) -> Result<Vec<String>, CoreError> {
        let interrupted = self.store.mark_running_as_interrupted(&crate::now_rfc3339())?;
        for attempt_id in &interrupted {
            let (ticket_id, snapshot) = match self.store.find(attempt_id)? {
                Some(record) => (record.ticket_id, record.tokens.to_json()),
                None => (String::new(), Value::Null),
            };
            host.publish(
                NewEvent::new(EventKind::Interrupted, "agent-runner")
                    .with_ticket(ticket_id)
                    .with_attempt(attempt_id)
                    .with_payload(json!({
                        "reason": "host restarted while the attempt was running",
                        "accountingTruncated": true,
                        "tokens": snapshot,
                    })),
            )?;
        }
        Ok(interrupted)
    }

    // -----------------------------------------------------------------------
    // 内部
    // -----------------------------------------------------------------------

    fn require_attempt(&self, attempt_id: &str) -> Result<AttemptRecord, RunnerError> {
        self.store
            .find(attempt_id)?
            .ok_or(RunnerError::NotFound {
                attempt_id: attempt_id.to_string(),
            })
    }

    async fn inject(
        &self,
        attempt_id: &str,
        channel: &'static str,
        message: &str,
    ) -> Result<SteerResponse, RunnerError> {
        if message.trim().is_empty() {
            return Err(
                RunnerError::Invalid(format!("{channel} requires a non-empty message")).into(),
            );
        }
        let record = self.require_attempt(attempt_id)?;
        if record.state != AttemptState::Running {
            return Err(RunnerError::NotRunning {
                attempt_id: attempt_id.to_string(),
                state: record.state,
            }
            .into());
        }
        let (tx, rx) = oneshot::channel();
        self.send_command(attempt_id, |commands| {
            commands
                .send(RunnerCommand::Inject {
                    channel,
                    message: message.to_string(),
                    reply: tx,
                })
                .is_ok()
        })?;

        let outcome = rx
            .await
            .map_err(|_| RunnerError::Unavailable {
                attempt_id: attempt_id.to_string(),
                reason: "the supervision task has already finished".into(),
            })??;

        // 注：disposition 回流由监督循环在收到 pi 响应时发（见 `absorb_response`）——
        // 那一处才知道 disposition 是什么，且能把消息原文一起带回总线。

        Ok(SteerResponse {
            attempt_id: attempt_id.to_string(),
            state: AttemptState::Running,
            disposition: outcome.disposition,
        })
    }

    fn send_command(
        &self,
        attempt_id: &str,
        build: impl FnOnce(&mpsc::UnboundedSender<RunnerCommand>) -> bool,
    ) -> Result<(), RunnerError> {
        let live = self.live.lock().map_err(|_| CoreError::StorePoisoned)?;
        let Some(attempt) = live.get(attempt_id) else {
            return Err(RunnerError::Unavailable {
                attempt_id: attempt_id.to_string(),
                reason: "no live supervision task for this attempt".into(),
            });
        };
        if !build(&attempt.commands) {
            return Err(RunnerError::Unavailable {
                attempt_id: attempt_id.to_string(),
                reason: "the supervision task has already finished".into(),
            });
        }
        Ok(())
    }

    /// 监督循环：spawn 窗口（凭据）→ 探测 → prompt → 判据阶梯 → 收尾。
    ///
    /// fork 找回（第四梯）用外层循环实现：读循环非预期死亡且会话可续时，
    /// 重 spawn 续会话、从某条 user message 分叉、重发 prompt。**阶梯对象不重建**，
    /// 所以总钟预算跨 fork 继续有效。
    async fn supervise(
        &self,
        host: crate::AppState,
        attempt_id: String,
        ticket_id: String,
        profile: Option<String>,
        prompt: String,
        mut commands: mpsc::UnboundedReceiver<RunnerCommand>,
    ) {
        let mut record = match self.store.find(&attempt_id) {
            Ok(Some(record)) => record,
            _ => AttemptRecord::running(attempt_id.clone(), ticket_id.clone(), profile.clone()),
        };
        let mut probe: Option<PiVersionProbe> = None;
        let mut forks_used = 0usize;
        let mut accounting = TokenAccounting::new();
        let mut final_snapshot: Option<TokenSnapshot> = None;
        // 阶梯跨 fork 存活：总钟预算不因找回而续命。
        let mut ladder = JudgementLadder::new(self.config.ladder.clone());
        // fork 找回计划：带上 `--session <id>` 重 spawn，再从某条 user message 分叉。
        let mut recovery: Option<RecoveryPlan> = None;

        let outcome = loop {
            // --- spawn 窗口：凭据只在这一窗口内存在（GLOSSARY「过路财神」）---
            let credentials = match self.credentials.collect(&CredentialRequest {
                ticket: ticket_id.clone(),
                attempt_id: attempt_id.clone(),
                profile: profile.clone(),
            }) {
                Ok(pairs) => pairs,
                Err(error) => {
                    // 取凭据失败 = spawn 失败：走 needs_attention，不进插件重试策略。
                    record.note = Some(format!("credential courier failed: {error}"));
                    record.finish(AttemptState::Failed, "credential courier failed");
                    let _ = self.store.upsert(&record);
                    let _ = host.publish(
                        NewEvent::new(EventKind::NeedsAttention, "agent-runner")
                            .with_ticket(&ticket_id)
                            .with_attempt(&attempt_id)
                            .with_payload(json!({
                                "reason": "credential courier failed inside the spawn window",
                                "detail": error.to_string(),
                            })),
                    );
                    break AttemptOutcome {
                        state: AttemptState::Failed,
                        reason: "credential courier failed".into(),
                    };
                }
            };

            let mut plan = self.spawn_plan(recovery.as_ref());
            for (key, value) in &credentials {
                plan = plan.with_env(key.clone(), value.clone());
            }
            let process = match PiProcess::spawn(plan).await {
                Ok(process) => process,
                Err(error) => {
                    record.finish(AttemptState::Failed, format!("pi spawn failed: {error}"));
                    let _ = self.store.upsert(&record);
                    let _ = host.publish(
                        NewEvent::new(EventKind::NeedsAttention, "agent-runner")
                            .with_ticket(&ticket_id)
                            .with_attempt(&attempt_id)
                            .with_payload(json!({
                                "reason": "pi child could not be spawned",
                                "detail": error.to_string(),
                            })),
                    );
                    break AttemptOutcome {
                        state: AttemptState::Failed,
                        reason: format!("pi spawn failed: {error}"),
                    };
                }
            };
            record.pid = process.pid();
            let mut parts = process.into_parts();

            // --- 启动版本探测：记录偏差，不硬失败 ---
            let (version_probe, leftovers) =
                probe_version(&mut parts, &self.config.expected_version, self.config.probe_timeout)
                    .await;
            probe = Some(version_probe);
            if let Some(probe) = &probe {
                record.session_id = probe.session_id.clone();
                if !probe.deviations.is_empty() {
                    record.note = Some(format!("version probe: {}", probe.deviations.join("; ")));
                }
            }
            let _ = self.store.upsert(&record);
            // 探测期间错过的帧不能丢：回放进阶梯与账本。
            replay_frames(&leftovers, &mut ladder, &mut accounting, &mut record);

            // --- 第四梯：fork 找回（重 spawn 之后先分叉，再重发 prompt）---
            if let Some(plan) = recovery.take() {
                let entry_id = match plan.entry_id {
                    Some(entry_id) => entry_id,
                    None => last_user_entry(&mut parts, self.config.probe_timeout)
                        .await
                        .unwrap_or_default(),
                };
                let (forked, leftovers) = round_trip(
                    &mut parts,
                    RpcCommand {
                        id: format!("{attempt_id}-fork"),
                        kind: CommandKind::Fork {
                            entry_id: Some(entry_id.clone()),
                        },
                    },
                    self.config.probe_timeout,
                )
                .await;
                replay_frames(&leftovers, &mut ladder, &mut accounting, &mut record);
                let forked_ok = forked.map(|response| response.success).unwrap_or(false);
                record.note = Some(format!(
                    "fork recovery #{forks_used}: replayed session {} from entry {entry_id} (acknowledged: {forked_ok})",
                    plan.session_id
                ));
                let _ = self.store.upsert(&record);
            }

            // --- prompt：这一注的 disposition 也要回流 ---
            let (prompt_response, leftovers) = round_trip(
                &mut parts,
                RpcCommand {
                    id: format!("{attempt_id}-prompt"),
                    kind: CommandKind::Prompt {
                        message: prompt.clone(),
                        streaming_behavior: None,
                    },
                },
                self.config.probe_timeout,
            )
            .await;
            replay_frames(&leftovers, &mut ladder, &mut accounting, &mut record);
            if let Some(response) = prompt_response {
                if let Some(disposition) = &response.disposition {
                    record.disposition = Some(disposition.as_str().to_string());
                }
                let _ = self.store.upsert(&record);
                let _ = host.publish(
                    NewEvent::new(EventKind::AttemptStarted, "agent-runner")
                        .with_ticket(&ticket_id)
                        .with_attempt(&attempt_id)
                        .with_payload(json!({
                            "disposition": record.disposition,
                            "accepted": response.success,
                            "sessionId": record.session_id,
                        })),
                );
            }

            // --- 判据阶梯主循环 ---
            let mut run = RunState {
                pending: HashMap::new(),
                pending_clear: None,
            };
            let ended = self
                .run_ladder(
                    &host,
                    &attempt_id,
                    &ticket_id,
                    parts,
                    &mut commands,
                    &mut ladder,
                    &mut accounting,
                    &mut record,
                    &mut run,
                )
                .await;

            match ended {
                RunEnd::Finished { outcome, mut parts } => {
                    // 权威对账：settle 时用 get_session_stats 的聚合校准（宁少报不虚报）。
                    if outcome.state == AttemptState::Settled {
                        final_snapshot = Some(match session_stats(&mut parts, self.config.probe_timeout).await {
                            Some(stats) => accounting.snapshot(Some(&stats), false),
                            None => accounting.snapshot(None, false),
                        });
                    }
                    parts_shutdown(&mut parts, self.config.shutdown_grace).await;
                    break outcome;
                }
                RunEnd::ReadLoopDied { mut parts } => {
                    parts_shutdown(&mut parts, Duration::from_millis(0)).await;
                    // 第四梯：fork 找回。只在「见过帧」（读循环健康过）且还能再试时启用。
                    if forks_used >= self.config.max_fork_recoveries || ladder.frames_seen() == 0 {
                        break AttemptOutcome {
                            state: AttemptState::Failed,
                            reason: if ladder.frames_seen() == 0 {
                                "read loop died before any frame: the pi child never started".into()
                            } else {
                                "read loop died: the pi child exited without settling".into()
                            },
                        };
                    }
                    let Some(session_id) = record.session_id.clone() else {
                        break AttemptOutcome {
                            state: AttemptState::Failed,
                            reason: "read loop died and no session id was probed: nothing to fork from"
                                .into(),
                        };
                    };
                    forks_used += 1;
                    let _ = host.publish(
                        NewEvent::new(EventKind::Blocked, "agent-runner")
                            .with_ticket(&ticket_id)
                            .with_attempt(&attempt_id)
                            .with_payload(json!({
                                "reason": "read loop died; recovering by fork",
                                "sessionId": session_id,
                                "forkAttempt": forks_used,
                            })),
                    );
                    recovery = Some(RecoveryPlan {
                        session_id,
                        entry_id: None,
                    });
                    continue;
                }
            }
        };

        // --- 收尾：终态落库 + 事件回流 ---
        // 非 settle 的结局（判杀 / abort / 读循环死）没有权威聚合可对账：
        // 落我方绝对累计并标 truncated——中断态以最后快照为准（GLOSSARY）。
        let tokens = final_snapshot.unwrap_or_else(|| accounting.snapshot(None, true));
        record.tokens = tokens;
        record.finish(outcome.state, outcome.reason.clone());
        let _ = self.store.upsert(&record);
        if let Ok(mut live) = self.live.lock() {
            live.remove(&attempt_id);
        }

        let kind = match outcome.state {
            AttemptState::Settled => EventKind::AgentSettled,
            AttemptState::Aborted => EventKind::Abort,
            AttemptState::Interrupted => EventKind::Interrupted,
            AttemptState::Running | AttemptState::Failed => EventKind::AttemptFinished,
        };
        let _ = host.publish(
            NewEvent::new(kind, "agent-runner")
                .with_ticket(&ticket_id)
                .with_attempt(&attempt_id)
                .with_payload(json!({
                    "state": outcome.state,
                    "reason": outcome.reason,
                    "tokens": record.tokens.to_json(),
                    "accountingTruncated": record.accounting_truncated,
                    "versionProbe": probe.as_ref().map(PiVersionProbe::to_json),
                })),
        );
    }

    fn spawn_plan(&self, recovery: Option<&RecoveryPlan>) -> PiSpawnPlan {
        let mut args = self.config.args.clone();
        if let Some(recovery) = recovery {
            // fork 找回：续上崩溃前那个会话（pi --session <path|id>）。
            args.push("--session".to_string());
            args.push(recovery.session_id.clone());
        }
        // 仍然 `InheritPlus`：真 pi 要 PATH / HOME / 各provider 的 API key。但
        // [`crate::agent_runner::credentials::EnvPolicy::apply`] 收尾会扣下
        // `HOST_SECRET_ENV_KEYS`（`FARM_TOKEN` / `FARM_ADMIN_TOKEN`）——宿主自己的
        // 鉴权凭据绝不随继承进 pi 子进程（#23 独立 token 裁决的前提，code review P1-1）。
        let mut plan = PiSpawnPlan::new(self.config.program.clone(), args)
            .with_env_policy(EnvPolicy::InheritPlus(Vec::new()));
        if let Some(cwd) = &self.config.cwd {
            plan = plan.with_cwd(cwd.clone());
        }
        for (key, value) in &self.config.scenario_env {
            plan = plan.with_env(key.clone(), value.clone());
        }
        plan
    }

    /// 判据阶梯主循环：帧 / 命令 / 两口钟三路 select。
    #[allow(clippy::too_many_arguments)]
    async fn run_ladder(
        &self,
        host: &crate::AppState,
        attempt_id: &str,
        ticket_id: &str,
        mut parts: PiProcessParts,
        commands: &mut mpsc::UnboundedReceiver<RunnerCommand>,
        ladder: &mut JudgementLadder,
        accounting: &mut TokenAccounting,
        record: &mut AttemptRecord,
        run: &mut RunState,
    ) -> RunEnd {
        loop {
            let wait = ladder.next_wait(Instant::now());
            tokio::select! {
                frame = parts.frames.recv() => {
                    let Some(frame) = frame else {
                        // 读循环健康度前置：EOF = 子进程把自己收尾了。
                        return if ladder.phase() == LadderPhase::Aborting {
                            RunEnd::Finished {
                                outcome: AttemptOutcome {
                                    state: AttemptState::Aborted,
                                    reason: "abort acknowledged: the pi child exited".into(),
                                },
                                parts,
                            }
                        } else {
                            RunEnd::ReadLoopDied { parts }
                        };
                    };
                    match parse_frame(&frame) {
                        Frame::Response(response) => {
                            absorb_response(
                                host, attempt_id, ticket_id, &response, record, run,
                            )
                            .await;
                        }
                        Frame::Event(event) => {
                            let signals = ladder.observe_event(&event, Instant::now());
                            absorb_usage(accounting, &signals);
                            if ladder.is_settled() {
                                return RunEnd::Finished {
                                    outcome: settled_outcome(),
                                    parts,
                                };
                            }
                        }
                        Frame::Unparsable(text) => {
                            record.note = Some(format!("unparsable frame on the wire: {text}"));
                        }
                    }
                }
                command = commands.recv() => {
                    let Some(command) = command else { continue };
                    match command {
                        RunnerCommand::Inject { channel, message, reply } => {
                            let id = format!("{attempt_id}-{channel}");
                            match parts_send(&mut parts, &id, channel_kind(channel, &message)).await {
                                Ok(()) => {
                                    run.pending.insert(
                                        channel,
                                        PendingInjection { reply, message },
                                    );
                                }
                                Err(error) => {
                                    let _ = reply.send(Err(error));
                                }
                            }
                        }
                        RunnerCommand::Abort { reason } => {
                            let _ = parts_send(&mut parts, &format!("{attempt_id}-abort"), CommandKind::Abort).await;
                            ladder.request_abort(Instant::now());
                            let _ = host.publish(
                                NewEvent::new(EventKind::StallKill, "agent-runner")
                                    .with_ticket(ticket_id)
                                    .with_attempt(attempt_id)
                                    .with_payload(json!({
                                        "reason": reason.unwrap_or_else(|| "abort requested".into()),
                                        "rung": "abort probe",
                                        "abortProbeMs": self.config.ladder.abort_probe.as_millis(),
                                    })),
                            );
                        }
                        RunnerCommand::ClearQueue { reply } => {
                            match parts_send(&mut parts, &format!("{attempt_id}-clear-queue"), CommandKind::ClearQueue).await {
                                Ok(()) => run.pending_clear = Some(reply),
                                Err(error) => {
                                    let _ = reply.send(Err(error));
                                }
                            }
                        }
                    }
                }
                _ = tokio::time::sleep(wait) => {
                    match ladder.check(Instant::now()) {
                        LadderAction::KeepWaiting => {}
                        LadderAction::Settled => {
                            return RunEnd::Finished { outcome: settled_outcome(), parts };
                        }
                        LadderAction::SilentClockExpired { silent_for } => {
                            // 第一梯：发 abort，进探针相（死线由此独立起算）。
                            let _ = parts_send(&mut parts, &format!("{attempt_id}-abort"), CommandKind::Abort).await;
                            ladder.request_abort(Instant::now());
                            let _ = host.publish(
                                NewEvent::new(EventKind::StallKill, "agent-runner")
                                    .with_ticket(ticket_id)
                                    .with_attempt(attempt_id)
                                    .with_payload(json!({
                                        "reason": "silent clock expired",
                                        "rung": "silent clock",
                                        "silentForMs": silent_for.as_millis(),
                                        "silentClockMs": self.config.ladder.silent_clock.as_millis(),
                                    })),
                            );
                        }
                        LadderAction::TotalClockExpired { elapsed } => {
                            // 第二梯：总钟是硬预算，直接强杀，不等 abort 回应。
                            return RunEnd::Finished {
                                outcome: AttemptOutcome {
                                    state: AttemptState::Failed,
                                    reason: format!("total clock expired after {}ms", elapsed.as_millis()),
                                },
                                parts,
                            };
                        }
                        LadderAction::AbortProbeExpired { waited } => {
                            // 第三梯：pi 不理 abort，独立死线到点强杀。
                            return RunEnd::Finished {
                                outcome: AttemptOutcome {
                                    state: AttemptState::Failed,
                                    reason: format!(
                                        "abort probe deadline expired after {}ms: the pi child ignored abort",
                                        waited.as_millis()
                                    ),
                                },
                                parts,
                            };
                        }
                    }
                }
            }
        }
    }
}

/// fork 找回计划：续哪个会话、从哪条 entry 分叉（`None` = 现查 `get_entries`）。
#[derive(Clone, Debug)]
struct RecoveryPlan {
    session_id: String,
    entry_id: Option<String>,
}

/// 一条等着 pi 回执的注入（消息原文随回执一起留著，供事件回流引用）。
struct PendingInjection {
    reply: oneshot::Sender<Result<InjectionOutcome, CoreError>>,
    message: String,
}

/// 主循环里等着回执的注入 / clear_queue。
struct RunState {
    pending: HashMap<&'static str, PendingInjection>,
    pending_clear: Option<oneshot::Sender<Result<u64, CoreError>>>,
}

/// 主循环的两种出口：正常终态，或读循环死了（可能还能 fork 找回）。
enum RunEnd {
    Finished {
        outcome: AttemptOutcome,
        parts: PiProcessParts,
    },
    ReadLoopDied {
        parts: PiProcessParts,
    },
}

fn settled_outcome() -> AttemptOutcome {
    AttemptOutcome {
        state: AttemptState::Settled,
        reason: "agent_settled and the last authoritative assistant message stopped with stopReason=stop"
            .into(),
    }
}

fn channel_kind(channel: &str, message: &str) -> CommandKind {
    match channel {
        "follow_up" => CommandKind::FollowUp {
            message: message.to_string(),
        },
        _ => CommandKind::Steer {
            message: message.to_string(),
        },
    }
}

/// 把权威用量来源喂进账本（宁少报不虚报：缺用量的来源整条跳过）。
fn absorb_usage(accounting: &mut TokenAccounting, signals: &EventSignals<'_>) {
    if let Some(message) = &signals.message_end {
        accounting.add_authoritative_message(message.id, message.role, message.usage);
    }
    for message in &signals.agent_end_messages {
        accounting.add_authoritative_message(message.id, message.role, message.usage);
    }
    if signals.nested_usage.is_some() || signals.parent_tool_call_id.is_some() {
        accounting.add_nested_usage(signals.parent_tool_call_id, signals.nested_usage);
    }
}

/// 回放探测/往返期间错过的帧（不丢 `agent_start`，也不丢权威 `message_end`）。
fn replay_frames(
    frames: &[Vec<u8>],
    ladder: &mut JudgementLadder,
    accounting: &mut TokenAccounting,
    record: &mut AttemptRecord,
) {
    for frame in frames {
        match parse_frame(frame) {
            Frame::Event(event) => {
                let signals = ladder.observe_event(&event, Instant::now());
                absorb_usage(accounting, &signals);
            }
            Frame::Response(response) => {
                if let Some(disposition) = &response.disposition {
                    record.disposition = Some(disposition.as_str().to_string());
                }
            }
            Frame::Unparsable(_) => {}
        }
    }
}

/// 处理一条响应帧：fulfill 回执 + disposition 回流。
async fn absorb_response(
    host: &crate::AppState,
    attempt_id: &str,
    ticket_id: &str,
    response: &RpcResponse,
    record: &mut AttemptRecord,
    run: &mut RunState,
) {
    match response.command.as_str() {
        "steer" | "follow_up" => {
            if let Some(disposition) = &response.disposition {
                record.disposition = Some(disposition.as_str().to_string());
            }
            let message = run
                .pending
                .get(response.command.as_str())
                .map(|pending| pending.message.clone());
            if let Some(pending) = run.pending.remove(response.command.as_str()) {
                let _ = pending.reply.send(Ok(InjectionOutcome {
                    disposition: response.disposition.clone(),
                    success: response.success,
                    error: response.error.clone(),
                }));
            }
            // disposition 回流：这一注是被排上了（queued）还是被推迟到 settle 后（deferred）。
            // 只发这一条（不在 inject 里另发），消息原文随 disposition 一起回到总线上。
            let _ = host.publish(
                NewEvent::new(EventKind::Steer, "agent-runner")
                    .with_ticket(ticket_id)
                    .with_attempt(attempt_id)
                    .with_payload(json!({
                        "channel": response.command,
                        "message": message,
                        "disposition": record.disposition,
                        "accepted": response.success,
                        "error": response.error,
                    })),
            );
        }
        "prompt" => {
            if let Some(disposition) = &response.disposition {
                record.disposition = Some(disposition.as_str().to_string());
            }
        }
        "clear_queue" => {
            let cleared = response
                .data
                .as_ref()
                .and_then(|data| data.get("cleared"))
                .and_then(|cleared| cleared.as_u64())
                .unwrap_or(0);
            if let Some(reply) = run.pending_clear.take() {
                let _ = reply.send(Ok(cleared));
            }
        }
        _ => {}
    }
}

/// 取最后一条 user message 的 entry id（fork 找回的分叉点）。
async fn last_user_entry(parts: &mut PiProcessParts, timeout: Duration) -> Option<String> {
    let (response, leftovers) = round_trip(
        parts,
        RpcCommand {
            id: format!("fork-probe-{}", std::process::id()),
            kind: CommandKind::GetEntries { since: None },
        },
        timeout,
    )
    .await;
    for frame in leftovers {
        tracing::debug!(
            target: "agent_runner",
            frame = %String::from_utf8_lossy(&frame),
            "frame missed while probing fork points"
        );
    }
    let data = response?.data?;
    let entries = data.get("entries")?.as_array()?;
    entries
        .iter()
        .rev()
        .find(|entry| {
            entry
                .get("message")
                .and_then(|message| message.get("role"))
                .and_then(|role| role.as_str())
                == Some("user")
        })
        .and_then(|entry| entry.get("id"))
        .and_then(|id| id.as_str())
        .map(str::to_string)
}

/// settle 时的权威聚合（对账用；拿不到就返回 `None`，只报我方累计）。
async fn session_stats(parts: &mut PiProcessParts, timeout: Duration) -> Option<Value> {
    let (response, _leftovers) = round_trip(
        parts,
        RpcCommand {
            id: format!("stats-{}", std::process::id()),
            kind: CommandKind::GetSessionStats,
        },
        timeout,
    )
    .await;
    response.and_then(|response| response.data)
}

/// attempt id：`att-<ticket>-<epoch>-<rand>`（宿主私有，不进契约）。
fn new_attempt_id(ticket_id: &str) -> String {
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut rand = [0u8; 4];
    let _ = getrandom::fill(&mut rand);
    format!(
        "att-{}-{}-{}",
        sanitize(ticket_id),
        epoch,
        rand.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// 往 stdin 写一条命令（读循环与写循环共用同一条管子）。
async fn parts_send(parts: &mut PiProcessParts, id: &str, kind: CommandKind) -> Result<(), CoreError> {
    let line = serde_json::to_vec(&RpcCommand {
        id: id.to_string(),
        kind,
    })
    .map_err(|e| CoreError::Config(format!("rpc command must serialize: {e}")))?;
    let stdin = parts.stdin.as_mut().ok_or_else(|| {
        CoreError::Io(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "pi stdin is closed",
        ))
    })?;
    use tokio::io::AsyncWriteExt;
    stdin.write_all(&line).await.map_err(CoreError::Io)?;
    stdin.write_all(b"\n").await.map_err(CoreError::Io)?;
    stdin.flush().await.map_err(CoreError::Io)
}

/// 三段式收尾（研究票 §6）：关 stdin → 宽限等待 → 强杀。
async fn parts_shutdown(parts: &mut PiProcessParts, grace: Duration) {
    if let Some(mut stdin) = parts.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.flush().await;
        drop(stdin);
    }
    if grace.is_zero() {
        let _ = parts.child.kill().await;
        let _ = parts.child.wait().await;
        return;
    }
    let deadline = tokio::time::Instant::now() + grace;
    while tokio::time::Instant::now() < deadline {
        match parts.child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => tokio::time::sleep(Duration::from_millis(20)).await,
            Err(_) => break,
        }
    }
    let _ = parts.child.kill().await;
    let _ = parts.child.wait().await;
}

/// 供测试与宿主查询用的辅助：把一条记录投影成契约 steer 响应的状态。
pub fn record_state(record: &AttemptRecord) -> AttemptState {
    record.state
}

/// 供测试断言的快照读法（宿主私有形状的只读投影）。
pub fn snapshot_of(record: &AttemptRecord) -> &TokenSnapshot {
    &record.tokens
}
