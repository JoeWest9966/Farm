//! Merge 服务面（GLOSSARY「Merge 服务」：宿主侧确定性的合并执行者）。
//!
//! ## 边界（ADR-0001 决策 2 + GLOSSARY「Merge 服务」）
//! - agent **只能提议**：契约面只有一个 `POST /v1/merge/proposals`，提议即全部；
//! - 宿主**机械校验**三项——worktree 干净、FF 可行（集成分支是票分支的祖先）、
//!   测试过（宿主自己跑测试命令）——任一不过即拒收，worktree 原样保留；
//! - 校验通过后宿主执行**真正的 merge**（`git merge --no-ff`），**完成信号 =
//!   集成分支上出现该票的 merge commit**，是宿主可验证的机械事实，**永不采信
//!   agent 自述**（连它自报的 commit sha 也只是被核对的对象）；
//! - merge 之后宿主**机械执行投影**（关票等），由 workflow 配置开关
//!   （[`crate::merge::projection`]）；
//! - 拒收 broken workspace 映射（ADR-0005 §9）。
//!
//! ## 为什么 publish 要由调用方把 `&AppState` 传进来
//! 与 agent-runner（#16）同一理由：`AppState` 持有 `MergeService`，句柄不能再反向
//! 持有一个 `AppState`。发布面（[`crate::AppState::publish`]，先落事件日志再广播）
//! 作为参数传入，事件因此既进总线也进持久日志——`merge` 事件可被 #26 一致性套件
//! 从事件历史里断言为机械事实。
//!
//! ## 状态机（提议行，write-ahead）
//! ```text
//! proposed ──校验全过──▶ validated ──merge 执行──▶ merged
//!    │                       │
//!    └──任一校验失败──────────┴──────────────────▶ rejected（行保留，worktree 保留）
//! ```

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use farm_sdk::events::{EventKind, NewEvent};
use farm_sdk::services::merge::{MergeProposal, MergeProposalState};
use farm_sdk::{ContractError, ErrorCode};

use crate::db::Database;
use crate::error::CoreError;
use crate::merge::git;
use crate::merge::projection::{self, ProjectionOutcome};
use crate::merge::store::{MergeProposalRecord, MergeStore};
use crate::workspace::{MappingRow, MappingStatus};
use crate::AppState;

/// merge 服务的宿主私有配置：机械校验里的「测试过」跑什么命令、跑多久。
///
/// 命令是**宿主**的配置，不是 agent 的输入——否则「测试过」就沦为自述。默认
/// `cargo test`（Farm 被编排的仓库以 Rust 为主），超时 15 分钟；具体数值归宿主
/// /workflow 配置（#12/#23 的接线位）。
#[derive(Clone, Debug)]
pub struct MergeConfig {
    /// argv（第一项是可执行文件）；空 = 不设测试闸门（宿主运营者的显式选择）。
    pub test_command: Vec<String>,
    /// 测试命令的死线（超时即拒收）。
    pub test_timeout: Duration,
}

impl Default for MergeConfig {
    fn default() -> Self {
        Self {
            test_command: vec!["cargo".to_string(), "test".to_string()],
            test_timeout: Duration::from_secs(15 * 60),
        }
    }
}

/// 机械闸门名（拒收时落库 + 进错误 `details`，客户端可穷举）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectionStage {
    /// workspace 映射缺失 / broken / pending / 票号对不上。
    Mapping,
    /// agent 自报的 commit 与分支顶点不符（永不采信自述）。
    CommitClaim,
    /// worktree 不干净。
    WorktreeClean,
    /// FF 不可行（集成分支与票分支已分叉 / 分支不存在）。
    FastForward,
    /// 测试没过（或超时）。
    Tests,
}

impl RejectionStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            RejectionStage::Mapping => "mapping",
            RejectionStage::CommitClaim => "commit_claim",
            RejectionStage::WorktreeClean => "worktree_clean",
            RejectionStage::FastForward => "fast_forward",
            RejectionStage::Tests => "tests",
        }
    }
}

impl std::fmt::Display for RejectionStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Merge 服务的错误面（宿主私有；投影到契约闭集见 [`MergeError::to_contract_error`]）。
#[derive(Debug, thiserror::Error)]
pub enum MergeError {
    /// 提议本身不合法（缺字段）。
    #[error("invalid merge proposal: {0}")]
    Invalid(String),

    /// 提议指向的东西不存在（没有这个 worktree 分支的映射）。
    #[error("merge proposal target not found: {0}")]
    NotFound(String),

    /// 机械闸门拒收：worktree 保留、票未合入、无完成信号。
    #[error("merge proposal {proposal_id} rejected at the `{stage}` gate: {detail}")]
    Rejected {
        proposal_id: String,
        stage: RejectionStage,
        code: ErrorCode,
        detail: String,
    },

    /// 宿主侧故障（git 执行失败等；不可重试）。
    #[error("merge execution failed: {0}")]
    Failed(String),

    /// 宿主私有存储故障。
    #[error(transparent)]
    Store(#[from] CoreError),

    /// Workspace 服务（#14）侧的故障：merge 只读映射，映射读不到即宿主故障。
    #[error(transparent)]
    Workspace(#[from] crate::workspace::WorkspaceError),
}

impl MergeError {
    /// 一处机械校验的否定结论（拒收提议，worktree 原样保留）。
    fn rejection(
        proposal_id: &str,
        stage: RejectionStage,
        detail: impl Into<String>,
    ) -> Self {
        MergeError::Rejected {
            proposal_id: proposal_id.to_string(),
            stage,
            code: ErrorCode::Conflict,
            detail: detail.into(),
        }
    }

    /// 投影到契约闭集（ADR-0001 决策 6：不加新码）。
    ///
    /// 拒收一律 `conflict`，`details` 带 `proposal_id` / `stage` / `detail`——
    /// 客户端不必再查一次就能知道该修什么。
    pub fn to_contract_error(&self) -> ContractError {
        match self {
            MergeError::Invalid(message) => ContractError::invalid_request(message.clone()),
            MergeError::NotFound(message) => ContractError::not_found(message.clone()),
            MergeError::Rejected {
                proposal_id,
                stage,
                detail,
                ..
            } => ContractError::conflict(format!(
                "merge proposal {proposal_id} rejected at the `{}` gate: {detail}",
                stage.as_str()
            ))
            .with_details(serde_json::json!({
                "proposal_id": proposal_id,
                "stage": stage.as_str(),
                "detail": detail,
            })),
            MergeError::Failed(message) => ContractError::internal(message.clone()),
            MergeError::Store(source) => source.to_contract_error(),
            MergeError::Workspace(source) => source.to_contract_error(),
        }
    }
}

impl From<MergeError> for ContractError {
    fn from(err: MergeError) -> Self {
        err.to_contract_error()
    }
}

/// 一次提议的受理结果（宿主私有；投影到契约见 farm-sdk `MergeProposalResponse`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeReport {
    pub proposal_id: String,
    pub state: MergeProposalState,
    /// 完成信号（`merged` 时必有）。
    pub merge_commit: Option<String>,
    /// 投影结局（按 workflow 开关）。
    pub projection: Vec<ProjectionOutcome>,
}

/// Merge 服务句柄（挂在 [`crate::AppState`] 上；`Clone` 廉价）。
#[derive(Clone, Debug)]
pub struct MergeService {
    db: Database,
    store: MergeStore,
    config: MergeConfig,
    /// 提议 id 的进程内计数（与事件 id 同款：进程内唯一）。
    seq: std::sync::Arc<AtomicU64>,
    epoch: u64,
}

impl MergeService {
    /// 默认配置（测试命令 `cargo test`）。
    pub fn new(db: Database) -> Self {
        Self::with_config(db, MergeConfig::default())
    }

    /// 指定「测试过」闸门的命令与死线（测试注入快命令 / 宿主按仓库配置）。
    pub fn with_config(db: Database, config: MergeConfig) -> Self {
        let store = MergeStore::new(&db);
        Self {
            db,
            store,
            config,
            seq: std::sync::Arc::new(AtomicU64::new(0)),
            epoch: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        }
    }

    /// 当前生效的「测试过」闸门配置（宿主运营者可查）。
    pub fn config(&self) -> &MergeConfig {
        &self.config
    }

    /// 底层存储句柄（宿主内部用；schema 不外泄，ADR-0005 §5）。
    pub fn database(&self) -> &Database {
        &self.db
    }

    /// 提议存储句柄（#25 审阅队列与投影都经它读）。
    pub fn store(&self) -> &MergeStore {
        &self.store
    }

    /// 按提议 id 查一行（#25 审阅队列）。
    pub fn proposal(&self, proposal_id: &str) -> Result<Option<MergeProposalRecord>, CoreError> {
        self.store.find(proposal_id)
    }

    /// 全部提议（最新优先；#25 审阅队列的读面）。
    pub fn proposals(&self) -> Result<Vec<MergeProposalRecord>, CoreError> {
        self.store.list()
    }

    /// 某票的提议时间线（最早优先）。
    pub fn proposals_for_ticket(
        &self,
        ticket_id: &str,
    ) -> Result<Vec<MergeProposalRecord>, CoreError> {
        self.store.for_ticket(ticket_id)
    }

    /// 某状态的提议（#25 按状态过滤审阅队列）。
    pub fn proposals_in_state(
        &self,
        state: MergeProposalState,
    ) -> Result<Vec<MergeProposalRecord>, CoreError> {
        self.store.in_state(state)
    }

    /// **提议合并**：agent 的全部权力到此为止。
    ///
    /// 顺序（write-ahead 先落库，再动作）：
    /// 1. 落一行 `proposed`（#25 审阅队列 + 审计轨迹）；
    /// 2. 机械校验（映射健康度 + 自报 commit 核对 + worktree 干净 + FF 可行 + 测试过）；
    /// 3. 任一不过 → 行转 `rejected`、发 `needs_attention`、回闭集 `conflict`，
    ///    **worktree 原样保留**；
    /// 4. 全过 → 行转 `validated`，执行真 merge（`--no-ff`），行转 `merged`；
    /// 5. 按 workflow 开关机械执行投影；
    /// 6. 发 `merge` 事件（持久路径：先落事件日志再广播）。
    pub async fn propose(
        &self,
        host: &AppState,
        proposal: &MergeProposal,
    ) -> Result<MergeReport, MergeError> {
        let ticket_id = proposal.ticket_id.trim();
        let worktree_branch = proposal.worktree_branch.trim();
        if ticket_id.is_empty() || worktree_branch.is_empty() {
            return Err(MergeError::Invalid(
                "a merge proposal needs a non-empty ticket_id and worktree_branch".into(),
            ));
        }

        // 提议的对象是 workspace 映射（GLOSSARY：merge 服务读映射、不拥有它）。
        let mapping = host
            .workspace
            .find_by_branch(worktree_branch)
            .map_err(MergeError::Workspace)?
            .ok_or_else(|| {
                MergeError::NotFound(format!(
                    "no workspace mapping for worktree branch `{worktree_branch}`"
                ))
            })?;

        // 1. write-ahead：提议先落库。
        let mut record = MergeProposalRecord::proposed(
            self.next_proposal_id(),
            ticket_id,
            worktree_branch,
            &mapping.integration_branch,
            &mapping.repo_path,
            &mapping.worktree_path,
            proposal.commit.as_deref(),
            proposal.message.as_deref().unwrap_or_default(),
            &crate::now_rfc3339(),
        );
        self.store.save(&record)?;

        // 2. 机械校验。
        if let Err(err) = self
            .validate(&record.proposal_id, &mapping, proposal)
            .await
        {
            // 3. 拒收：行保留（审计 + #25），worktree 不动，无完成信号。
            if let MergeError::Rejected { stage, detail, .. } = &err {
                let at = crate::now_rfc3339();
                record.mark_rejected(
                    stage.as_str(),
                    ErrorCode::Conflict.as_str(),
                    detail,
                    &at,
                );
                self.store.save(&record)?;
                self.announce_rejection(host, &record, *stage, detail)?;
            }
            return Err(err);
        }

        // 4. 执行真 merge：完成信号 = 集成分支上该票的 merge commit。
        record.mark_validated(&crate::now_rfc3339());
        self.store.save(&record)?;
        let merge_commit = self.execute(&mapping, proposal)?;
        record.mark_merged(&merge_commit, &crate::now_rfc3339());

        // 5. 投影（由 workflow 开关闸门；失败不影响完成信号）。
        let outcomes =
            projection::run(host, &mapping.repo_path, &mapping.ticket_id, &merge_commit);
        record.set_projection(outcomes.clone(), &crate::now_rfc3339());
        self.store.save(&record)?;

        // 6. merge 结果事件进总线（持久路径）。
        self.announce_merge(host, &record, &merge_commit, &outcomes)?;

        Ok(MergeReport {
            proposal_id: record.proposal_id,
            state: MergeProposalState::Merged,
            merge_commit: Some(merge_commit),
            projection: outcomes,
        })
    }

    // -----------------------------------------------------------------------
    // 机械校验（每一项都是一个确定的否定结论，不是一个异常）
    // -----------------------------------------------------------------------

    async fn validate(
        &self,
        proposal_id: &str,
        mapping: &MappingRow,
        proposal: &MergeProposal,
    ) -> Result<(), MergeError> {
        // 映射必须健康：broken 拒收（ADR-0005 §9）、pending = worktree 从未建成。
        match mapping.status {
            MappingStatus::Broken => {
                return Err(MergeError::rejection(
                    proposal_id,
                    RejectionStage::Mapping,
                    format!(
                        "workspace mapping for ticket `{}` is broken (ADR-0005 §9); \
                         reconcile it before proposing a merge",
                        mapping.ticket_id
                    ),
                ))
            }
            MappingStatus::Pending => {
                return Err(MergeError::rejection(
                    proposal_id,
                    RejectionStage::Mapping,
                    format!(
                        "workspace mapping for ticket `{}` is pending: its worktree was never built",
                        mapping.ticket_id
                    ),
                ))
            }
            MappingStatus::Built => {}
        }
        if mapping.ticket_id != proposal.ticket_id.trim() {
            return Err(MergeError::rejection(
                proposal_id,
                RejectionStage::Mapping,
                format!(
                    "worktree branch `{}` belongs to ticket `{}`, not `{}`",
                    mapping.worktree_branch, mapping.ticket_id, proposal.ticket_id
                ),
            ));
        }

        let repo = Path::new(&mapping.repo_path);
        let worktree = Path::new(&mapping.worktree_path);

        // 票分支必须真实存在。
        let branch_tip = match git::rev_parse(repo, &mapping.worktree_branch)? {
            Some(tip) => tip,
            None => {
                return Err(MergeError::rejection(
                    proposal_id,
                    RejectionStage::FastForward,
                    format!(
                        "worktree branch `{}` does not exist in {}",
                        mapping.worktree_branch, mapping.repo_path
                    ),
                ))
            }
        };

        // 自报 commit 只是被核对的对象：不符即拒（永不采信自述）。
        if let Some(claimed) = proposal
            .commit
            .as_deref()
            .map(str::trim)
            .filter(|commit| !commit.is_empty())
        {
            match git::rev_parse(repo, claimed)? {
                Some(sha) if sha == branch_tip => {}
                _ => {
                    return Err(MergeError::rejection(
                        proposal_id,
                        RejectionStage::CommitClaim,
                        format!(
                            "the proposal claims commit `{claimed}` but `{}` is at {branch_tip}",
                            mapping.worktree_branch
                        ),
                    ))
                }
            }
        }

        // 校验一：worktree 干净。
        let dirty = git::dirty_entries(worktree)?;
        if !dirty.is_empty() {
            return Err(MergeError::rejection(
                proposal_id,
                RejectionStage::WorktreeClean,
                format!(
                    "worktree {} is not clean: {}",
                    mapping.worktree_path,
                    dirty.join("; ")
                ),
            ));
        }

        // 校验二：FF 可行（集成分支是票分支的祖先 = 未分叉）。
        if git::rev_parse(repo, &mapping.integration_branch)?.is_none() {
            return Err(MergeError::rejection(
                proposal_id,
                RejectionStage::FastForward,
                format!(
                    "integration branch `{}` does not exist in {}",
                    mapping.integration_branch, mapping.repo_path
                ),
            ));
        }
        if !git::is_ancestor(repo, &mapping.integration_branch, &mapping.worktree_branch)? {
            return Err(MergeError::rejection(
                proposal_id,
                RejectionStage::FastForward,
                format!(
                    "integration branch `{}` has diverged from `{}`: rebase the ticket \
                     branch and propose again",
                    mapping.integration_branch, mapping.worktree_branch
                ),
            ));
        }

        // 校验三：测试过（宿主自己跑，不看 agent 怎么说）。
        self.run_tests(proposal_id, worktree).await?;
        Ok(())
    }

    /// 跑宿主的测试命令（cwd = 票 worktree）。非零退出 / 超时都是拒收理由。
    async fn run_tests(&self, proposal_id: &str, worktree: &Path) -> Result<(), MergeError> {
        if self.config.test_command.is_empty() {
            return Ok(()); // 宿主显式不设测试闸门
        }
        let program = &self.config.test_command[0];
        let mut command = tokio::process::Command::new(program);
        command
            .args(&self.config.test_command[1..])
            .current_dir(worktree)
            .stdin(std::process::Stdio::null());
        let started = Instant::now();
        let output = match tokio::time::timeout(self.config.test_timeout, command.output()).await {
            Ok(result) => result.map_err(|e| {
                MergeError::Failed(format!(
                    "cannot spawn test command `{}` in {}: {e}",
                    self.config.test_command.join(" "),
                    worktree.display()
                ))
            })?,
            Err(_elapsed) => {
                return Err(MergeError::rejection(
                    proposal_id,
                    RejectionStage::Tests,
                    format!(
                        "test command `{}` did not finish within {:?}",
                        self.config.test_command.join(" "),
                        self.config.test_timeout
                    ),
                ))
            }
        };
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = truncate(stderr.trim(), 400);
        let exit = output
            .status
            .code()
            .map(|code| code.to_string())
            .unwrap_or_else(|| "a signal".to_string());
        Err(MergeError::rejection(
            proposal_id,
            RejectionStage::Tests,
            format!(
                "test command `{}` exited with {exit} after {:?}: {}",
                self.config.test_command.join(" "),
                started.elapsed(),
                if detail.is_empty() { "<no stderr>" } else { detail }
            ),
        ))
    }

    // -----------------------------------------------------------------------
    // 执行
    // -----------------------------------------------------------------------

    /// 执行真 merge，回传 merge commit sha（完成信号）。
    ///
    /// merge 必须发生在「集成分支被检出的那个工作树」里（git 不允许在别处更新一个
    /// 已检出的分支）：找得到就在那儿合，找不到就在票 worktree 里检出集成分支再合
    /// （worktree 刚校验过干净）。
    fn execute(&self, mapping: &MappingRow, proposal: &MergeProposal) -> Result<String, MergeError> {
        let repo = Path::new(&mapping.repo_path);
        let worktree = Path::new(&mapping.worktree_path);
        let target = match git::worktree_holding(repo, &mapping.integration_branch)? {
            Some(path) => path,
            None => {
                git::checkout(worktree, &mapping.integration_branch)?;
                worktree.to_path_buf()
            }
        };
        let subject = format!(
            "merge: ticket #{}{}",
            mapping.ticket_id,
            proposal
                .message
                .as_deref()
                .map(str::trim)
                .filter(|message| !message.is_empty())
                .map(|message| format!(" {message}"))
                .unwrap_or_default()
        );
        git::merge_no_ff(&target, &mapping.worktree_branch, &subject)
    }

    // -----------------------------------------------------------------------
    // 事件（持久发布路径：先落事件日志再广播，#15）
    // -----------------------------------------------------------------------

    fn announce_merge(
        &self,
        host: &AppState,
        record: &MergeProposalRecord,
        merge_commit: &str,
        outcomes: &[ProjectionOutcome],
    ) -> Result<(), MergeError> {
        host.publish(
            NewEvent::new(EventKind::Merge, "merge")
                .with_ticket(&record.ticket_id)
                .with_payload(serde_json::json!({
                    "proposal_id": record.proposal_id,
                    "worktree_branch": record.worktree_branch,
                    "integration_branch": record.integration_branch,
                    "merge_commit": merge_commit,
                    // 完成信号的机械事实：集成分支上出现了该票的 merge commit。
                    "completion_signal": "merge_commit",
                    "projection": outcomes.iter().map(ProjectionOutcome::to_json).collect::<Vec<_>>(),
                })),
        )?;
        Ok(())
    }

    fn announce_rejection(
        &self,
        host: &AppState,
        record: &MergeProposalRecord,
        stage: RejectionStage,
        detail: &str,
    ) -> Result<(), MergeError> {
        // 一等事件：提议被拒，需要人类/编排器回应（GLOSSARY「事件总线」）。
        // 载荷里显式写着没有完成信号——不采信自述的机械留痕。
        host.publish(
            NewEvent::new(EventKind::NeedsAttention, "merge")
                .with_ticket(&record.ticket_id)
                .with_payload(serde_json::json!({
                    "proposal_id": record.proposal_id,
                    "worktree_branch": record.worktree_branch,
                    "integration_branch": record.integration_branch,
                    "stage": stage.as_str(),
                    "code": record.rejection_code,
                    "detail": detail,
                    "completion_signal": "none",
                })),
        )?;
        Ok(())
    }

    fn next_proposal_id(&self) -> String {
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        format!("mrg-{}-{:06}", self.epoch, n)
    }
}

/// 截断到 `max` 字符（错误消息里不放整份测试输出）。
fn truncate(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((idx, _)) => &text[..idx],
        None => text,
    }
}
