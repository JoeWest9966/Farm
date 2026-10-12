//! Workspace 服务（ticket #14，GLOSSARY 五服务之一）。
//!
//! 拥有 **spec ↔ 集成分支 ↔ worktree** 映射并负责 worktree 的创建与清理；映射持久化在
//! 宿主私有存储（ADR-0005 §9）。merge 服务与面板**读**映射、不拥有它。
//!
//! ## write-ahead 半写（ADR-0005 §9）
//! `reserve` 先把映射落成一列 `status='pending'`（**映射在、worktree 未必建**），
//! 再 `git worktree add` 建目录、转 `built`。若进程在「先写映射、后建 worktree」之间被杀，
//! 启动 `reconcile` 发现目录缺/损：能重建则重建（→`built`），不能则标 `broken`；
//! merge 服务拒绝 `broken` 映射。
//!
//! ## 边界
//! - SQLite schema 是宿主私有实现细节，**永不外泄为契约**（ADR-0005 §5）：对外只投影到
//!   farm-sdk 的 [`WorkspaceMapping`]/[`WorktreeRef`]/[`WorktreeState`]（[`WorkspaceService::mapping_view`]）。
//! - worktree 的 git 操作走**真实 `git` CLI**（`std::process::Command`），不用 libgit2。
//! - 本服务只提供原语；编排决策（何时建/清/合并）归编排器。

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use farm_sdk::services::workspace::{WorktreeRef, WorktreeState, WorkspaceMapping};
use farm_sdk::ContractError;

use crate::db::Database;
use crate::error::CoreError;
use crate::now_rfc3339;

/// 映射的内部状态（SQLite `status` 列；闭集 pending/built/broken）。
///
/// 与契约态 [`WorktreeState`] 的投影见 [`MappingStatus::to_worktree_state`]；
/// `removed` 不是持久态——清理即删行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MappingStatus {
    /// write-ahead 已落映射、worktree 尚未建成。
    Pending,
    /// worktree 已建成且对账健康。
    Built,
    /// 对账发现目录缺/损且无法重建（merge 服务拒收）。
    Broken,
}

impl MappingStatus {
    /// SQLite `status` 列的规范字符串。
    pub fn as_str(&self) -> &'static str {
        match self {
            MappingStatus::Pending => "pending",
            MappingStatus::Built => "built",
            MappingStatus::Broken => "broken",
        }
    }

    /// 解析 `status` 列；未知值返回 `None`（数据损坏）。
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(MappingStatus::Pending),
            "built" => Some(MappingStatus::Built),
            "broken" => Some(MappingStatus::Broken),
            _ => None,
        }
    }

    /// 投影到契约态 [`WorktreeState`]（ADR-0005 §5：schema 不外泄，只投影）。
    pub fn to_worktree_state(&self) -> WorktreeState {
        match self {
            MappingStatus::Pending => WorktreeState::Creating,
            MappingStatus::Built => WorktreeState::Ready,
            MappingStatus::Broken => WorktreeState::Broken,
        }
    }
}

/// 一条映射行（宿主私有形状；投影到契约见 [`WorkspaceService::mapping_view`]）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MappingRow {
    pub ticket_id: String,
    pub spec_id: String,
    pub integration_branch: String,
    /// 源仓库（`git worktree` 的 `-C` 目标；重建时需要）。
    pub repo_path: String,
    pub worktree_path: String,
    pub worktree_branch: String,
    /// 建分支的起点（commit/branch）；`None` = 基于 HEAD。
    pub start_point: Option<String>,
    pub status: MappingStatus,
    pub created_at: String,
    pub updated_at: String,
}

/// 建立映射的请求（spec ↔ 集成分支 ↔ worktree 的全部入参）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EstablishRequest {
    pub spec_id: String,
    pub integration_branch: String,
    pub ticket_id: String,
    pub repo_path: std::path::PathBuf,
    pub worktree_path: std::path::PathBuf,
    pub worktree_branch: String,
    pub start_point: Option<String>,
}

/// 启动对账报告（ADR-0005 §9）：每票的裁决去向。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// 目录缺/损、已跑 `git worktree add` 重建成功（→ built）。
    pub rebuilt: Vec<String>,
    /// 目录缺/损、重建失败，已标 broken（merge 拒收）。
    pub marked_broken: Vec<String>,
    /// 目录健康、无需重建（含 pending→built 的就地确认）。
    pub healthy: Vec<String>,
}

/// Workspace 服务错误（宿主私有；投影到契约闭集见 [`WorkspaceError::to_contract_error`]）。
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("host-private store error: {0}")]
    Store(#[from] CoreError),

    #[error("workspace git command `{command}` failed: {stderr}")]
    Git { command: String, stderr: String },

    #[error("no workspace mapping for ticket `{0}`")]
    NotFound(String),

    #[error("workspace mapping `{ticket_id}` is {status:?}, cannot {action}")]
    InvalidState {
        ticket_id: String,
        status: MappingStatus,
        action: &'static str,
    },
}

impl WorkspaceError {
    /// 投影到契约闭集错误（ADR-0005 §5：内部细节对客户端只有闭集码）。
    pub fn to_contract_error(&self) -> ContractError {
        match self {
            WorkspaceError::Store(e) => e.to_contract_error(),
            WorkspaceError::Git { .. } => {
                ContractError::internal("workspace git operation failed")
            }
            WorkspaceError::NotFound(t) => {
                ContractError::not_found(format!("no workspace mapping for ticket {t}"))
            }
            WorkspaceError::InvalidState {
                ticket_id,
                status,
                action,
            } => ContractError::conflict(format!(
                "workspace mapping {ticket_id} is {status:?}, cannot {action}"
            )),
        }
    }
}

impl From<WorkspaceError> for ContractError {
    fn from(e: WorkspaceError) -> Self {
        e.to_contract_error()
    }
}

/// `status` 列损坏时的行解析错误（理论路径：写入口径固定为三种规范字符串）。
#[derive(Debug)]
struct UnknownStatus(String);

impl std::fmt::Display for UnknownStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown workspace_mapping.status: {}", self.0)
    }
}

impl std::error::Error for UnknownStatus {}

/// 从查询行构造 [`MappingRow`]（列序见各 SELECT）。
fn row_from_query(row: &rusqlite::Row<'_>) -> rusqlite::Result<MappingRow> {
    let status_str: String = row.get(7)?;
    let status = MappingStatus::from_str(&status_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            7,
            rusqlite::types::Type::Text,
            Box::new(UnknownStatus(status_str.clone())),
        )
    })?;
    Ok(MappingRow {
        ticket_id: row.get(0)?,
        spec_id: row.get(1)?,
        integration_branch: row.get(2)?,
        repo_path: row.get(3)?,
        worktree_path: row.get(4)?,
        worktree_branch: row.get(5)?,
        start_point: row.get(6)?,
        status,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

const SELECT_COLS: &str = "SELECT ticket_id, spec_id, integration_branch, repo_path, \
                           worktree_path, worktree_branch, start_point, status, created_at, updated_at \
                           FROM workspace_mapping";

/// 跑一条 git 子命令，非零退出即 [`WorkspaceError::Git`]（带 stderr 便于对账诊断）。
fn run_git(mut cmd: Command, label: &str) -> Result<(), WorkspaceError> {
    let out = cmd.output().map_err(|e| WorkspaceError::Git {
        command: format!("{label} (spawn failed: {e})"),
        stderr: String::new(),
    })?;
    if !out.status.success() {
        return Err(WorkspaceError::Git {
            command: label.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(())
}

/// 分支是否已存在于源仓库（决定 `git worktree add` 用 `-b` 新建还是直接检出）。
fn branch_exists(repo: &Path, branch: &str) -> Result<bool, WorkspaceError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("show-ref")
        .arg("--verify")
        .arg("--quiet")
        .arg(format!("refs/heads/{branch}"))
        .output()
        .map_err(|e| WorkspaceError::Git {
            command: format!("show-ref (spawn failed: {e})"),
            stderr: String::new(),
        })?;
    Ok(out.status.success())
}

/// 建 worktree：分支已存在则检出它，否则 `-b` 从 `start_point`（缺省 HEAD）新建。
fn worktree_add(row: &MappingRow) -> Result<(), WorkspaceError> {
    let repo = Path::new(&row.repo_path);
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo)
        .arg("worktree")
        .arg("add")
        .arg(&row.worktree_path);
    if branch_exists(repo, &row.worktree_branch)? {
        cmd.arg(&row.worktree_branch);
    } else {
        cmd.arg("-b").arg(&row.worktree_branch);
        if let Some(sp) = &row.start_point {
            cmd.arg(sp);
        }
    }
    run_git(cmd, "worktree add")
}

/// 移除 worktree（`--force`）。
fn worktree_remove(repo: &Path, worktree_path: &str) -> Result<(), WorkspaceError> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo)
        .arg("worktree")
        .arg("remove")
        .arg("--force")
        .arg(worktree_path);
    run_git(cmd, "worktree remove")
}

/// Workspace 服务句柄：持有宿主私有存储句柄（`Clone` 廉价，可被 axum 广泛共享）。
#[derive(Clone, Debug)]
pub struct WorkspaceService {
    db: Database,
}

impl WorkspaceService {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// **write-ahead**：先把映射落成 `pending` 行（不碰 git），返回该行。
    ///
    /// 同票重入是 UPSERT（重置为 pending、刷新可变字段），便于失败重派。
    pub fn reserve(&self, req: &EstablishRequest) -> Result<MappingRow, WorkspaceError> {
        let now = now_rfc3339();
        self.db
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO workspace_mapping
                        (ticket_id, spec_id, integration_branch, repo_path, worktree_path,
                         worktree_branch, start_point, status, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, ?8)
                     ON CONFLICT(ticket_id) DO UPDATE SET
                        spec_id = excluded.spec_id,
                        integration_branch = excluded.integration_branch,
                        repo_path = excluded.repo_path,
                        worktree_path = excluded.worktree_path,
                        worktree_branch = excluded.worktree_branch,
                        start_point = excluded.start_point,
                        status = 'pending',
                        updated_at = excluded.updated_at",
                    rusqlite::params![
                        req.ticket_id,
                        req.spec_id,
                        req.integration_branch,
                        req.repo_path.to_string_lossy(),
                        req.worktree_path.to_string_lossy(),
                        req.worktree_branch,
                        req.start_point,
                        now,
                    ],
                )?;
                Ok(())
            })
            .map_err(WorkspaceError::Store)?;
        self.find(&req.ticket_id)?
            .ok_or_else(|| WorkspaceError::NotFound(req.ticket_id.clone()))
    }

    /// **建 worktree**：`git worktree add` → 转 `built`。幂等（已 built 且目录健康则原样返回）。
    ///
    /// git 失败时返回错误且行**保持 pending**（write-ahead：映射已持久化，留给 `reconcile`）。
    pub fn build(&self, ticket_id: &str) -> Result<MappingRow, WorkspaceError> {
        let row = self
            .find(ticket_id)?
            .ok_or_else(|| WorkspaceError::NotFound(ticket_id.to_string()))?;
        if row.status == MappingStatus::Built && Self::worktree_is_healthy(Path::new(&row.worktree_path)) {
            return Ok(row);
        }
        worktree_add(&row)?;
        self.set_status(ticket_id, MappingStatus::Built)?;
        self.find(ticket_id)?
            .ok_or_else(|| WorkspaceError::NotFound(ticket_id.to_string()))
    }

    /// **全链路**：write-ahead `reserve` + `build`。git 失败则 pending 行仍在（可被对账重建/标 broken）。
    pub fn establish(&self, req: &EstablishRequest) -> Result<MappingRow, WorkspaceError> {
        self.reserve(req)?;
        self.build(&req.ticket_id)
    }

    /// **清理**：移除 worktree（尽力而为，git 失败再兜底删目录）并删除映射行。
    pub fn cleanup(&self, ticket_id: &str) -> Result<(), WorkspaceError> {
        let row = self
            .find(ticket_id)?
            .ok_or_else(|| WorkspaceError::NotFound(ticket_id.to_string()))?;
        let wt = Path::new(&row.worktree_path);
        if wt.exists() {
            let _ = worktree_remove(Path::new(&row.repo_path), &row.worktree_path);
            if wt.exists() {
                let _ = std::fs::remove_dir_all(wt);
            }
        }
        self.db
            .with_conn(|conn| {
                conn.execute(
                    "DELETE FROM workspace_mapping WHERE ticket_id = ?1",
                    [ticket_id],
                )?;
                Ok(())
            })
            .map_err(WorkspaceError::Store)
    }

    /// 按票号查一行。
    pub fn find(&self, ticket_id: &str) -> Result<Option<MappingRow>, WorkspaceError> {
        self.db
            .with_conn(|conn| {
                let mut stmt = conn.prepare(&format!("{SELECT_COLS} WHERE ticket_id = ?1"))?;
                let mut rows = stmt.query_map([ticket_id], row_from_query)?;
                rows.next().transpose()
            })
            .map_err(WorkspaceError::Store)
    }

    /// 全部映射（按票号升序；稳定序，供视图分组与对账）。
    pub fn list(&self) -> Result<Vec<MappingRow>, WorkspaceError> {
        self.db
            .with_conn(|conn| {
                let mut stmt = conn.prepare(&format!("{SELECT_COLS} ORDER BY ticket_id"))?;
                let rows = stmt.query_map([], row_from_query)?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
            })
            .map_err(WorkspaceError::Store)
    }

    /// 按票号查状态（merge/面板便捷查询）。
    pub fn status_of(&self, ticket_id: &str) -> Result<Option<MappingStatus>, WorkspaceError> {
        Ok(self.find(ticket_id)?.map(|r| r.status))
    }

    /// 把内部行投影到契约视图：按 (spec_id, integration_branch) 归组成 [`WorkspaceMapping`]，
    /// 每票成一个 [`WorktreeRef`]，状态经 [`MappingStatus::to_worktree_state`] 投影。
    pub fn mapping_view(&self) -> Result<Vec<WorkspaceMapping>, WorkspaceError> {
        let rows = self.list()?;
        let mut order: Vec<(String, String)> = Vec::new();
        let mut groups: HashMap<(String, String), Vec<WorktreeRef>> = HashMap::new();
        for row in rows {
            let key = (row.spec_id.clone(), row.integration_branch.clone());
            let entry = groups.entry(key.clone()).or_insert_with(|| {
                order.push(key.clone());
                Vec::new()
            });
            entry.push(WorktreeRef {
                ticket_id: row.ticket_id,
                path: row.worktree_path,
                branch: row.worktree_branch,
                state: row.status.to_worktree_state(),
            });
        }
        let view = order
            .into_iter()
            .filter_map(|key| {
                let worktrees = groups.remove(&key)?;
                Some(WorkspaceMapping {
                    spec_id: key.0,
                    integration_branch: key.1,
                    worktrees,
                })
            })
            .collect();
        Ok(view)
    }

    /// **启动对账**（ADR-0005 §9）：逐票看 worktree 目录——健康则确认 `built`；
    /// 缺/损则尝试 `git worktree add` 重建（→`built`），重建不了则标 `broken`。
    pub fn reconcile(&self) -> Result<ReconcileReport, WorkspaceError> {
        let mut report = ReconcileReport::default();
        for row in self.list()? {
            if Self::worktree_is_healthy(Path::new(&row.worktree_path)) {
                if row.status != MappingStatus::Built {
                    self.set_status(&row.ticket_id, MappingStatus::Built)?;
                }
                report.healthy.push(row.ticket_id);
            } else {
                match worktree_add(&row) {
                    Ok(()) => {
                        self.set_status(&row.ticket_id, MappingStatus::Built)?;
                        report.rebuilt.push(row.ticket_id);
                    }
                    Err(_) => {
                        self.set_status(&row.ticket_id, MappingStatus::Broken)?;
                        report.marked_broken.push(row.ticket_id);
                    }
                }
            }
        }
        Ok(report)
    }

    /// 全部 `broken` 映射（merge 服务拒收清单 / 面板告警源）。
    pub fn broken(&self) -> Result<Vec<MappingRow>, WorkspaceError> {
        self.db
            .with_conn(|conn| {
                let mut stmt =
                    conn.prepare(&format!("{SELECT_COLS} WHERE status = 'broken' ORDER BY ticket_id"))?;
                let rows = stmt.query_map([], row_from_query)?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
            })
            .map_err(WorkspaceError::Store)
    }

    /// 按 worktree 分支查一行（分支全局唯一；merge 提议带分支，是 merge 的主查询柄）。
    pub fn find_by_branch(&self, branch: &str) -> Result<Option<MappingRow>, WorkspaceError> {
        self.db
            .with_conn(|conn| {
                let mut stmt =
                    conn.prepare(&format!("{SELECT_COLS} WHERE worktree_branch = ?1"))?;
                let mut rows = stmt.query_map([branch], row_from_query)?;
                rows.next().transpose()
            })
            .map_err(WorkspaceError::Store)
    }

    /// 按 worktree 分支查状态（merge 拒收 broken 的便捷判断）。
    pub fn status_of_branch(&self, branch: &str) -> Result<Option<MappingStatus>, WorkspaceError> {
        Ok(self.find_by_branch(branch)?.map(|r| r.status))
    }

    /// 更新状态与 `updated_at`（内部；由 build/reconcile 驱动）。
    fn set_status(
        &self,
        ticket_id: &str,
        status: MappingStatus,
    ) -> Result<(), WorkspaceError> {
        self.db
            .with_conn(|conn| {
                conn.execute(
                    "UPDATE workspace_mapping SET status = ?2, updated_at = ?3 WHERE ticket_id = ?1",
                    rusqlite::params![ticket_id, status.as_str(), now_rfc3339()],
                )?;
                Ok(())
            })
            .map_err(WorkspaceError::Store)
    }

    /// worktree 是否健康：目录在且有 `.git`（linked worktree 是 `.git` 文件）。
    fn worktree_is_healthy(path: &Path) -> bool {
        path.join(".git").exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_status_round_trips_through_its_column_string() {
        for status in [MappingStatus::Pending, MappingStatus::Built, MappingStatus::Broken] {
            assert_eq!(MappingStatus::from_str(status.as_str()), Some(status));
        }
        assert_eq!(MappingStatus::from_str("removed"), None, "removed 不是持久态");
    }

    #[test]
    fn internal_status_projects_to_the_contract_worktree_state() {
        assert_eq!(MappingStatus::Pending.to_worktree_state(), WorktreeState::Creating);
        assert_eq!(MappingStatus::Built.to_worktree_state(), WorktreeState::Ready);
        assert_eq!(MappingStatus::Broken.to_worktree_state(), WorktreeState::Broken);
    }
}
