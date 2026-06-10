//! Daemon-owned git worktree provisioning (CAO-replacement Phase 3) — the Rust
//! port of CAO's `worktree_service.ensure_worktree`. Each agent runs in an
//! isolated `git worktree` so *which file* a change lives in **proves** which
//! agent made it (attribution certainty). Falls back to "shared" mode (the
//! project dir, heuristic attribution) for a non-git root or any git failure —
//! never fatal.
//!
//! Worktrees live under the app-data dir (`…/taime/worktrees/<slug>/<key>`), so
//! the watcher never sees a nested checkout and the main tree stays pristine.

use std::path::{Path, PathBuf};
use std::process::Command;

use taime_protocol::WorktreeInfo;

fn run_git(cwd: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("git").current_dir(cwd).args(args).output()
}

/// Run a git command, returning trimmed stdout on success (else `None`).
fn git_stdout(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = run_git(cwd, args).ok()?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    } else {
        None
    }
}

fn fnv1a32(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// `dirs::data_dir()/taime/worktrees/` (durable; falls back to a temp dir).
fn worktrees_dir() -> PathBuf {
    crate::store::data_dir()
        .unwrap_or_else(|| std::env::temp_dir().join("taime"))
        .join("worktrees")
}

/// Filesystem-safe `basename-<hash8>` of the repo root (CAO `_project_slug`).
fn project_slug(repo_root: &str) -> String {
    let base = Path::new(repo_root)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("repo");
    let safe: String = base
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    format!("{safe}-{:08x}", fnv1a32(repo_root))
}

fn shared(project_root: &str, agent_id: &str, error: Option<String>) -> WorktreeInfo {
    WorktreeInfo {
        agent_id: agent_id.to_string(),
        project_root: project_root.to_string(),
        repo_root: None,
        worktree_path: project_root.to_string(),
        branch: None,
        base_sha: None,
        mode: "shared".to_string(),
        error,
    }
}

/// Outcome of a GC attempt on one provisioned worktree.
#[derive(Debug, PartialEq, Eq)]
pub enum GcOutcome {
    /// Checkout (+ branch) removed — provably worthless: clean tree, branch tip
    /// still at the provision base.
    Removed,
    /// Kept: uncommitted changes or commits beyond base — that is the user's
    /// in-progress work, preserved for attribution and any review before merge.
    KeptHasWork,
    /// Kept: state couldn't be verified (git error). Never delete what we
    /// can't prove worthless.
    KeptUnverified(String),
}

/// Garbage-collect ONE dead agent's isolated worktree, conservatively.
/// Removes the checkout and its `taime/…` branch ONLY when both are provably
/// worthless: `git status --porcelain` is empty AND the branch tip still
/// equals `base_sha`. Shared-mode rows must never reach here (the "worktree
/// path" is the user's real project dir) — callers gate on `mode`.
pub fn gc_one(
    worktree_path: &str,
    repo_root: &str,
    branch: Option<&str>,
    base_sha: Option<&str>,
) -> GcOutcome {
    let wt = Path::new(worktree_path);
    let repo = Path::new(repo_root);
    if !repo.is_dir() {
        return GcOutcome::KeptUnverified("repo root missing".into());
    }
    if !wt.exists() {
        // Checkout already gone (manual delete / lost disk): prune git's stale
        // bookkeeping, and drop the branch only when it's still at base.
        let _ = run_git(repo, &["worktree", "prune"]);
        if let (Some(b), Some(base)) = (branch, base_sha) {
            if git_stdout(repo, &["rev-parse", b]).as_deref() == Some(base) {
                let _ = run_git(repo, &["branch", "-D", b]);
            }
        }
        return GcOutcome::Removed;
    }
    // Uncommitted or untracked changes → user work; keep.
    let status = match run_git(wt, &["status", "--porcelain"]) {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => return GcOutcome::KeptUnverified("git status failed".into()),
    };
    if !status.is_empty() {
        return GcOutcome::KeptHasWork;
    }
    // Commits beyond the provision base → user work; keep.
    if let (Some(b), Some(base)) = (branch, base_sha) {
        match git_stdout(repo, &["rev-parse", b]) {
            Some(tip) if tip == base => {}
            Some(_) => return GcOutcome::KeptHasWork,
            None => return GcOutcome::KeptUnverified("branch tip unreadable".into()),
        }
    }
    if !matches!(run_git(repo, &["worktree", "remove", worktree_path]), Ok(o) if o.status.success())
    {
        return GcOutcome::KeptUnverified("git worktree remove failed".into());
    }
    if let Some(b) = branch {
        // Tip == base verified above, so -D destroys nothing.
        let _ = run_git(repo, &["branch", "-D", b]);
    }
    GcOutcome::Removed
}

/// Force-remove a provisioned ISOLATED worktree checkout + its branch, for the
/// "delete workspace" teardown. Unlike [`gc_one`], this does NOT preserve dirty
/// or forked work — the user asked to delete the whole workspace. Best-effort:
/// never errors. NEVER pass a shared-mode `worktree_path` (that's the user's
/// real project dir) — callers gate on `mode == "isolated"`; the equality guard
/// below is a second line of defense.
pub fn remove_force(worktree_path: &str, repo_root: Option<&str>, branch: Option<&str>) {
    // Refuse to fs-delete a path that is the repo root itself (shared mode).
    let same_as_repo = repo_root.map(|r| Path::new(r) == Path::new(worktree_path)).unwrap_or(false);
    if let Some(repo) = repo_root {
        let repo = Path::new(repo);
        if repo.is_dir() {
            let _ = run_git(repo, &["worktree", "remove", "--force", worktree_path]);
            if let Some(b) = branch {
                let _ = run_git(repo, &["branch", "-D", b]);
            }
            let _ = run_git(repo, &["worktree", "prune"]);
        }
    }
    // If git didn't remove it (repo gone / not registered), drop the checkout dir
    // — but only when it's a real isolated worktree path, never the project root.
    if !same_as_repo && Path::new(worktree_path).exists() {
        let _ = std::fs::remove_dir_all(worktree_path);
    }
}

/// Provision (or idempotently resolve) an isolated worktree for `agent_id`.
/// Branch is `taime/<provider>-<agent_id>`, matching CAO.
pub fn provision(
    project_root: &str,
    provider: &str,
    isolate: bool,
    agent_id: &str,
) -> WorktreeInfo {
    if !isolate {
        return shared(project_root, agent_id, None);
    }
    let proj = Path::new(project_root);
    if !proj.is_dir() {
        return shared(project_root, agent_id, Some("project root is not a directory".into()));
    }
    let repo_root = match git_stdout(proj, &["rev-parse", "--show-toplevel"]) {
        Some(r) => r,
        None => return shared(project_root, agent_id, Some("not a git repository".into())),
    };
    let repo = Path::new(&repo_root);
    let base_sha = match git_stdout(repo, &["rev-parse", "HEAD"]) {
        Some(s) => s,
        None => return shared(project_root, agent_id, Some("no commits (empty repo)".into())),
    };

    let branch = format!("taime/{provider}-{agent_id}");
    let wt_dir = worktrees_dir().join(project_slug(&repo_root)).join(agent_id);
    if let Some(parent) = wt_dir.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let wt_path = wt_dir.to_string_lossy().to_string();

    let ok = WorktreeInfo {
        agent_id: agent_id.to_string(),
        project_root: project_root.to_string(),
        repo_root: Some(repo_root.clone()),
        worktree_path: wt_path.clone(),
        branch: Some(branch.clone()),
        base_sha: Some(base_sha.clone()),
        mode: "isolated".to_string(),
        error: None,
    };

    // Idempotent reuse: an existing worktree checkout (relaunch on same key).
    if wt_dir.join(".git").exists() {
        return ok;
    }

    // Create a fresh worktree on a new branch; if the branch already exists
    // (relaunch after a crash that lost the dir), attach to it.
    let add = run_git(repo, &["worktree", "add", "-b", &branch, &wt_path, &base_sha]);
    let created = matches!(&add, Ok(o) if o.status.success());
    if !created {
        let retry = run_git(repo, &["worktree", "add", &wt_path, &branch]);
        if !matches!(&retry, Ok(o) if o.status.success()) {
            let err = retry
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stderr).trim().to_string())
                .unwrap_or_default();
            // The `-b` attempt can leave a partially-created branch behind (review
            // L14). It's uniquely ours (agent_id is fresh per provision), so
            // best-effort delete it before falling back to shared — otherwise it
            // leaks forever (worktree GC skips shared rows and only -D's a branch
            // it still has a row for).
            let _ = run_git(repo, &["branch", "-D", &branch]);
            return shared(project_root, agent_id, Some(format!("git worktree add failed: {err}")));
        }
    }
    link_gitignored_deps(repo, &wt_dir);
    ok
}

/// Best-effort: make a fresh worktree usable without a rebuild — copy `.env*`
/// files and symlink heavy gitignored dirs from the main checkout. Never fails.
fn link_gitignored_deps(repo: &Path, wt: &Path) {
    // Copy dotenv files (secrets/config the agent needs, gitignored).
    if let Ok(entries) = std::fs::read_dir(repo) {
        for e in entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(".env") {
                let _ = std::fs::copy(e.path(), wt.join(&*name));
            }
        }
    }
    // Symlink common heavy gitignored dependency dirs if present.
    for dep in ["node_modules", ".venv", "venv", "target", "vendor"] {
        let src = repo.join(dep);
        let dst = wt.join(dep);
        if src.is_dir() && !dst.exists() {
            #[cfg(unix)]
            let _ = std::os::unix::fs::symlink(&src, &dst);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(cwd: &Path, args: &[&str]) {
        let out = Command::new("git").current_dir(cwd).args(args).output().unwrap();
        assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    }

    fn temp_repo() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("taime-wt-test-{:08x}", rand::random::<u32>()));
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        git(&dir, &["config", "user.email", "t@t"]);
        git(&dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("README.md"), "hi").unwrap();
        std::fs::write(dir.join(".env"), "SECRET=1").unwrap();
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-qm", "init"]);
        dir
    }

    #[test]
    fn isolate_false_is_shared() {
        let info = provision("/tmp/whatever", "claude_code", false, "k1");
        assert_eq!(info.mode, "shared");
        assert_eq!(info.worktree_path, "/tmp/whatever");
    }

    #[test]
    fn non_git_dir_is_shared_with_error() {
        let dir = std::env::temp_dir().join(format!("taime-nogit-{:08x}", rand::random::<u32>()));
        std::fs::create_dir_all(&dir).unwrap();
        let info = provision(dir.to_str().unwrap(), "codex", true, "k2");
        assert_eq!(info.mode, "shared");
        assert!(info.error.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn git_repo_provisions_isolated_worktree() {
        let repo = temp_repo();
        let info = provision(repo.to_str().unwrap(), "claude_code", true, "abc12345");
        assert_eq!(info.mode, "isolated", "error: {:?}", info.error);
        assert_eq!(info.branch.as_deref(), Some("taime/claude_code-abc12345"));
        assert!(info.base_sha.is_some());
        let wt = Path::new(&info.worktree_path);
        assert!(wt.join(".git").exists(), "worktree checkout created");
        assert!(wt.join("README.md").exists(), "tracked file present");
        assert!(wt.join(".env").exists(), ".env copied into worktree");

        // Idempotent: re-provisioning the same key reuses it.
        let again = provision(repo.to_str().unwrap(), "claude_code", true, "abc12345");
        assert_eq!(again.mode, "isolated");
        assert_eq!(again.worktree_path, info.worktree_path);

        // Cleanup.
        let _ = run_git(&repo, &["worktree", "remove", "--force", &info.worktree_path]);
        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(Path::new(&info.worktree_path).parent().unwrap());
    }

    #[test]
    fn gc_removes_clean_keeps_dirty_and_forked() {
        let repo = temp_repo();
        let root = repo.to_str().unwrap();

        // Clean + unforked → removed (checkout AND branch).
        let a = provision(root, "claude_code", true, "gcaaaaaa");
        assert_eq!(a.mode, "isolated");
        assert_eq!(
            gc_one(&a.worktree_path, root, a.branch.as_deref(), a.base_sha.as_deref()),
            GcOutcome::Removed
        );
        assert!(!Path::new(&a.worktree_path).exists(), "clean checkout deleted");
        assert!(
            git_stdout(&repo, &["rev-parse", a.branch.as_deref().unwrap()]).is_none(),
            "base-only branch deleted"
        );

        // Uncommitted changes → kept untouched.
        let b = provision(root, "claude_code", true, "gcbbbbbb");
        std::fs::write(Path::new(&b.worktree_path).join("wip.txt"), "unreviewed").unwrap();
        assert_eq!(
            gc_one(&b.worktree_path, root, b.branch.as_deref(), b.base_sha.as_deref()),
            GcOutcome::KeptHasWork
        );
        assert!(Path::new(&b.worktree_path).join("wip.txt").exists(), "dirty work preserved");

        // Committed-beyond-base (clean tree) → kept: the branch IS the work.
        let c = provision(root, "claude_code", true, "gccccccc");
        let cwt = Path::new(&c.worktree_path);
        std::fs::write(cwt.join("done.txt"), "committed").unwrap();
        git(cwt, &["add", "-A"]);
        git(cwt, &["commit", "-qm", "agent work"]);
        assert_eq!(
            gc_one(&c.worktree_path, root, c.branch.as_deref(), c.base_sha.as_deref()),
            GcOutcome::KeptHasWork
        );
        assert!(cwt.exists(), "forked checkout preserved");

        // Cleanup.
        for info in [&b, &c] {
            let _ = run_git(&repo, &["worktree", "remove", "--force", &info.worktree_path]);
        }
        let wt_parent = Path::new(&b.worktree_path).parent().map(|p| p.to_path_buf());
        let _ = std::fs::remove_dir_all(&repo);
        if let Some(p) = wt_parent {
            let _ = std::fs::remove_dir_all(p);
        }
    }
}
