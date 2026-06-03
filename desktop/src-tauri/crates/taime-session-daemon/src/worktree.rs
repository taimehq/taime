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

fn shared(project_root: &str, terminal_key: &str, error: Option<String>) -> WorktreeInfo {
    WorktreeInfo {
        terminal_key: terminal_key.to_string(),
        project_root: project_root.to_string(),
        repo_root: None,
        worktree_path: project_root.to_string(),
        branch: None,
        base_sha: None,
        mode: "shared".to_string(),
        error,
    }
}

/// Provision (or idempotently resolve) an isolated worktree for `terminal_key`.
/// Branch is `taime/<provider>-<terminal_key>`, matching CAO.
pub fn provision(
    project_root: &str,
    provider: &str,
    isolate: bool,
    terminal_key: &str,
) -> WorktreeInfo {
    if !isolate {
        return shared(project_root, terminal_key, None);
    }
    let proj = Path::new(project_root);
    if !proj.is_dir() {
        return shared(project_root, terminal_key, Some("project root is not a directory".into()));
    }
    let repo_root = match git_stdout(proj, &["rev-parse", "--show-toplevel"]) {
        Some(r) => r,
        None => return shared(project_root, terminal_key, Some("not a git repository".into())),
    };
    let repo = Path::new(&repo_root);
    let base_sha = match git_stdout(repo, &["rev-parse", "HEAD"]) {
        Some(s) => s,
        None => return shared(project_root, terminal_key, Some("no commits (empty repo)".into())),
    };

    let branch = format!("taime/{provider}-{terminal_key}");
    let wt_dir = worktrees_dir().join(project_slug(&repo_root)).join(terminal_key);
    if let Some(parent) = wt_dir.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let wt_path = wt_dir.to_string_lossy().to_string();

    let ok = WorktreeInfo {
        terminal_key: terminal_key.to_string(),
        project_root: project_root.to_string(),
        repo_root: Some(repo_root.clone()),
        worktree_path: wt_path.clone(),
        branch: Some(branch.clone()),
        base_sha: Some(base_sha.clone()),
        mode: "worktree".to_string(),
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
            return shared(project_root, terminal_key, Some(format!("git worktree add failed: {err}")));
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
        assert_eq!(info.mode, "worktree", "error: {:?}", info.error);
        assert_eq!(info.branch.as_deref(), Some("taime/claude_code-abc12345"));
        assert!(info.base_sha.is_some());
        let wt = Path::new(&info.worktree_path);
        assert!(wt.join(".git").exists(), "worktree checkout created");
        assert!(wt.join("README.md").exists(), "tracked file present");
        assert!(wt.join(".env").exists(), ".env copied into worktree");

        // Idempotent: re-provisioning the same key reuses it.
        let again = provision(repo.to_str().unwrap(), "claude_code", true, "abc12345");
        assert_eq!(again.mode, "worktree");
        assert_eq!(again.worktree_path, info.worktree_path);

        // Cleanup.
        let _ = run_git(&repo, &["worktree", "remove", "--force", &info.worktree_path]);
        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(Path::new(&info.worktree_path).parent().unwrap());
    }
}
