//! Merge 服务的 git 原语：**真实 `git` CLI**（`std::process::Command`）。
//!
//! 与 Workspace 服务（#14）同一纪律：Farm 不用 libgit2，一切 git 事实都由子进程
//! 产出。这里只封装 merge 服务需要的几句——读分支顶点、问祖先关系、问工作树干净、
//! 找「集成分支检出在哪个工作树」、执行 `merge --no-ff`。
//!
//! 机械校验的答案只有两种形状：**确定的否定**（`Ok(false)` / `Ok(None)`，例如
//! `--is-ancestor` 退 1 = 分叉了）与**宿主故障**（`Err`，例如 git 都 spawn 不出来）。
//! 前者是拒收提议的理由，后者才是 `internal`。

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::merge::MergeError;

/// 一条 git 子命令的产出。
struct GitOutput {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl GitOutput {
    fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

fn run(cwd: &Path, args: &[&str]) -> Result<GitOutput, MergeError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .map_err(|e| {
            MergeError::Failed(format!(
                "cannot spawn git {} in {}: {e}",
                args.join(" "),
                cwd.display()
            ))
        })?;
    Ok(GitOutput {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
    })
}

/// 把任意 revision 解析成 commit sha；解析不了（对象不存在/不是 commit）返回 `None`。
pub fn rev_parse(cwd: &Path, rev: &str) -> Result<Option<String>, MergeError> {    let out = run(cwd, &["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")])?;
    if out.ok() && !out.stdout.is_empty() {
        Ok(Some(out.stdout))
    } else {
        Ok(None)
    }
}

/// HEAD 的 commit sha。
pub fn head(cwd: &Path) -> Result<String, MergeError> {
    rev_parse(cwd, "HEAD")?
        .ok_or_else(|| MergeError::Failed(format!("{} has no HEAD commit", cwd.display())))
}

/// `git status --porcelain` 的非空行（含未跟踪文件）：空 = 工作树干净。
pub fn dirty_entries(cwd: &Path) -> Result<Vec<String>, MergeError> {
    let out = run(cwd, &["status", "--porcelain"])?;
    if !out.ok() {
        return Err(MergeError::Failed(format!(
            "git status failed in {}: {}",
            cwd.display(),
            out.stderr
        )));
    }
    Ok(out
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

/// `ancestor` 是否是 `descendant` 的祖先（即把 `descendant` 合进 `ancestor` 可以 FF）。
///
/// 退 0 = 是；退 1 = **否（已分叉）**；其余退出码是 git 自己出问题（`Err`）。
pub fn is_ancestor(cwd: &Path, ancestor: &str, descendant: &str) -> Result<bool, MergeError> {
    let out = run(
        cwd,
        &["merge-base", "--is-ancestor", ancestor, descendant],
    )?;
    match out.code {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(MergeError::Failed(format!(
            "git merge-base --is-ancestor {ancestor} {descendant} failed: {}",
            out.stderr
        ))),
    }
}

/// 哪个工作树正检出着 `branch`（`git worktree list --porcelain`）。
///
/// merge 必须发生在「集成分支被检出的那个工作树」里——git 不允许在别处更新一个
/// 已检出的分支。找不到（集成分支还没被任何工作树检出）返回 `None`，调用方改在
/// 票 worktree 里检出它再合。
pub fn worktree_holding(cwd: &Path, branch: &str) -> Result<Option<PathBuf>, MergeError> {
    let out = run(cwd, &["worktree", "list", "--porcelain"])?;
    if !out.ok() {
        return Err(MergeError::Failed(format!(
            "git worktree list failed in {}: {}",
            cwd.display(),
            out.stderr
        )));
    }
    let wanted = format!("refs/heads/{branch}");
    let mut current: Option<PathBuf> = None;
    for line in out.stdout.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current = Some(PathBuf::from(path.trim()));
        } else if let Some(branch_ref) = line.trim().strip_prefix("branch ") {
            if branch_ref == wanted {
                if let Some(path) = current.take() {
                    return Ok(Some(path));
                }
            }
        }
    }
    Ok(None)
}

/// 检出分支（worktree 已验证干净；失败即宿主故障）。
pub fn checkout(cwd: &Path, branch: &str) -> Result<(), MergeError> {
    let out = run(cwd, &["checkout", branch])?;
    if out.ok() {
        Ok(())
    } else {
        Err(MergeError::Failed(format!(
            "git checkout {branch} failed in {}: {}",
            cwd.display(),
            out.stderr
        )))
    }
}

/// 执行真 merge：`merge --no-ff`，回传产生的 merge commit sha。
///
/// `--no-ff` 是**故意的**：FF 可行性只是校验闸门（未分叉），而完成信号要求集成分支上
/// **每票一个 merge commit**（可被 #26 机械断言、可被 #25 展示）。FF 可行时若用
/// `--ff-only` 就不会留下 merge commit，完成信号随之消失。
pub fn merge_no_ff(cwd: &Path, branch: &str, message: &str) -> Result<String, MergeError> {
    let out = run(
        cwd,
        &["merge", "--no-ff", "--no-edit", "-m", message, branch],
    )?;
    if !out.ok() {
        return Err(MergeError::Failed(format!(
            "git merge --no-ff {branch} failed in {}: {}",
            cwd.display(),
            out.stderr
        )));
    }
    head(cwd)
}

/// 两个分支 tip 之间的 diff（面板呈现提议改动用，#25）。
///
/// 用两档 `git diff <base> <head>`：面板要展示的是「并入集成分支会引入什么」，
/// 即集成分支 tip 与 worktree 分支 tip 的差异。失败即宿主故障。
pub fn diff_between(cwd: &Path, base: &str, head: &str) -> Result<String, MergeError> {
    let out = run(cwd, &["diff", base, head])?;
    if !out.ok() {
        return Err(MergeError::Failed(format!(
            "git diff {base} {head} failed in {}: {}",
            cwd.display(),
            out.stderr
        )));
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .output()
                .expect("spawn git");
            assert!(out.status.success(), "git {args:?} failed");
        };
        git(&["init", "--initial-branch=main"]);
        git(&["config", "user.email", "test@farm.local"]);
        git(&["config", "user.name", "Farm Test"]);
        std::fs::write(dir.path().join("a.txt"), "a\n").expect("write");
        git(&["add", "."]);
        git(&["commit", "-m", "init"]);
        dir
    }

    #[test]
    fn rev_parse_resolves_a_branch_and_reports_an_unknown_one_as_absent() {
        let repo = scratch_repo();
        let tip = rev_parse(repo.path(), "main").expect("rev-parse").expect("main exists");
        assert_eq!(tip.len(), 40, "完整 sha");
        assert_eq!(rev_parse(repo.path(), "nope").expect("rev-parse"), None);
    }

    #[test]
    fn is_ancestor_answers_yes_no_and_a_bad_revision_is_a_host_failure() {
        let repo = scratch_repo();
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["branch", "feature"])
            .output()
            .expect("spawn git");
        assert!(is_ancestor(repo.path(), "main", "feature").expect("ancestor"), "同点即祖先");

        // 分叉：main 与 feature 各走一个提交。
        std::fs::write(repo.path().join("b.txt"), "b\n").expect("write");
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["add", "."])
            .output()
            .expect("spawn git");
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["commit", "-m", "main moves"])
            .output()
            .expect("spawn git");
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["checkout", "feature"])
            .output()
            .expect("spawn git");
        std::fs::write(repo.path().join("c.txt"), "c\n").expect("write");
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["add", "."])
            .output()
            .expect("spawn git");
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["commit", "-m", "feature moves"])
            .output()
            .expect("spawn git");

        assert!(
            !is_ancestor(repo.path(), "main", "feature").expect("ancestor"),
            "分叉后 main 不再是 feature 的祖先"
        );
        assert!(
            is_ancestor(repo.path(), "ghost", "feature").is_err(),
            "坏 revision 是宿主故障，不是「否」"
        );
    }

    #[test]
    fn worktree_holding_finds_the_worktree_that_has_the_branch_checked_out() {
        let repo = scratch_repo();
        let wt_parent = tempfile::tempdir().expect("tempdir");
        let wt = wt_parent.path().join("t18");
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["worktree", "add", "-b", "farm/t18"])
            .arg(&wt)
            .output()
            .expect("spawn git");

        let holding = worktree_holding(repo.path(), "farm/t18").expect("worktree list");
        assert_eq!(holding, Some(wt.clone()), "票 worktree 检出着 farm/t18");
        assert_eq!(
            worktree_holding(repo.path(), "main").expect("worktree list"),
            Some(repo.path().to_path_buf()),
            "主仓库检出着 main"
        );
        assert_eq!(
            worktree_holding(repo.path(), "nobody").expect("worktree list"),
            None,
            "没人检出的分支找不到"
        );
    }
}
