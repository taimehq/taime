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

/// Run a git command with extra environment (e.g. `GIT_INDEX_FILE` for a
/// throwaway snapshot index, or a stable `GIT_*_NAME/EMAIL` so `commit-tree`
/// works in a worktree that has no user identity configured).
fn run_git_env(cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> std::io::Result<std::process::Output> {
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd).args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output()
}

/// `run_git_env` + trimmed-stdout-on-success (mirrors [`git_stdout`]).
fn git_stdout_env(cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Option<String> {
    let out = run_git_env(cwd, args, env).ok()?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!s.is_empty()).then_some(s)
    } else {
        None
    }
}

/// Whether `rel_path`'s basename is a transient temp/swap/backup file — the same
/// rule the fs watcher and the diff surfaces use, so archive snapshots never
/// capture write-churn noise (`foo.md.tmp.<pid>.<hash>`, `notes~`, …).
fn is_transient(rel_path: &str) -> bool {
    let name = rel_path.rsplit('/').next().unwrap_or(rel_path);
    crate::fswatch::is_transient_file(name)
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

/// What an agent's worktree held when we tried to archive it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveOutcome {
    /// Snapshot written to `refs/taime/archive/<id>`; the cached render patch is
    /// returned. The checkout is now safe to reclaim — nothing is lost.
    Archived(ArchiveResult),
    /// The worktree was clean at its provision base — there was nothing to keep.
    /// Safe to reclaim directly (no ref, no patch).
    NoChanges,
    /// Could not snapshot (missing inputs / git error). The caller MUST keep the
    /// checkout — we never reclaim what we couldn't preserve.
    Failed(String),
}

/// The durable record an [`archive_agent`] produced: the keep-around ref + the
/// cached, rendered review patch (and its digest) for the reclaimed agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveResult {
    pub archive_ref: String,
    pub base_sha: String,
    pub digest: String,
    pub diff_blob: String,
    pub files_changed: usize,
}

/// Snapshot an agent's isolated worktree into `refs/taime/archive/<agent_id>` so
/// the physical checkout can be reclaimed without losing anything. The snapshot
/// is a real commit (parent = `base_sha`, tree = the full worktree state INCLUDING
/// untracked files, transient temps filtered) built through a throwaway index, so
/// it never touches the worktree's own index/HEAD and works on a detached
/// (branchless) worktree. Gc-safe (a ref), perfect-fidelity, near-zero marginal
/// disk (git dedupes objects).
///
/// Idempotent and crash-safe: every durable write (the ref + the returned cache)
/// happens BEFORE the caller deletes anything, and a re-run rewrites the same ref.
/// Shared-mode rows must never reach here — callers gate on `mode == "isolated"`.
pub fn archive_agent(
    agent_id: &str,
    worktree_path: &str,
    repo_root: &str,
    base_sha: &str,
) -> ArchiveOutcome {
    let wt = Path::new(worktree_path);
    let repo = Path::new(repo_root);
    if base_sha.is_empty() {
        return ArchiveOutcome::Failed("no base_sha to archive against".into());
    }
    if !repo.is_dir() {
        return ArchiveOutcome::Failed("repo root missing".into());
    }
    if !wt.exists() {
        return ArchiveOutcome::Failed("worktree checkout missing".into());
    }

    // A throwaway index so `add -A` (which stages untracked + modifications +
    // deletions, respecting .gitignore) never disturbs the agent's real index.
    let tmp_index = std::env::temp_dir()
        .join(format!("taime-archive-{agent_id}-{:08x}.idx", rand::random::<u32>()));
    let tmp_index_str = tmp_index.to_string_lossy().to_string();
    let idx_env = [("GIT_INDEX_FILE", tmp_index_str.as_str())];
    let cleanup_index = || {
        let _ = std::fs::remove_file(&tmp_index);
    };

    // Start the index at base, then stage the whole worktree, so the resulting
    // tree diffed against base is exactly the agent's full change set.
    if !ok(run_git_env(wt, &["read-tree", base_sha], &idx_env)) {
        cleanup_index();
        return ArchiveOutcome::Failed("read-tree base failed".into());
    }
    if !ok(run_git_env(wt, &["add", "-A"], &idx_env)) {
        cleanup_index();
        return ArchiveOutcome::Failed("git add -A failed".into());
    }
    // Drop transient temp/swap/backup files from the snapshot (match the live
    // review filter so the archived diff is the same change set the user saw).
    if let Some(staged) = git_stdout_env(wt, &["diff", "--cached", "--name-only", base_sha], &idx_env)
    {
        for path in staged.lines().filter(|p| !p.is_empty() && is_transient(p)) {
            let _ = run_git_env(wt, &["rm", "--cached", "--quiet", "--", path], &idx_env);
        }
    }
    let tree = match git_stdout_env(wt, &["write-tree"], &idx_env) {
        Some(t) => t,
        None => {
            cleanup_index();
            return ArchiveOutcome::Failed("write-tree failed".into());
        }
    };
    cleanup_index();

    // Clean worktree (tree identical to base's tree) ⇒ nothing to preserve.
    let base_tree = git_stdout(repo, &["rev-parse", &format!("{base_sha}^{{tree}}")]);
    if base_tree.as_deref() == Some(tree.as_str()) {
        return ArchiveOutcome::NoChanges;
    }

    // Commit the tree. A stable identity so `commit-tree` works even when the
    // worktree has no user.name/email configured.
    let id_env = [
        ("GIT_AUTHOR_NAME", "taime"),
        ("GIT_AUTHOR_EMAIL", "archive@taime.local"),
        ("GIT_COMMITTER_NAME", "taime"),
        ("GIT_COMMITTER_EMAIL", "archive@taime.local"),
    ];
    let msg = format!("taime archive: agent {agent_id}");
    let commit = match git_stdout_env(repo, &["commit-tree", &tree, "-p", base_sha, "-m", &msg], &id_env)
    {
        Some(c) => c,
        None => return ArchiveOutcome::Failed("commit-tree failed".into()),
    };
    let archive_ref = format!("refs/taime/archive/{agent_id}");
    if !ok(run_git(repo, &["update-ref", &archive_ref, &commit])) {
        return ArchiveOutcome::Failed("update-ref failed".into());
    }

    // The cached render patch: `git diff base..archive`, plain text (binaries as
    // "Binary files differ" stubs, exactly like the live review surfaces). The
    // ref is the system of record for re-merge fidelity; this is the render cache.
    let diff_blob = run_git(repo, &["diff", base_sha, &commit])
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let files_changed = git_stdout(repo, &["diff", "--name-only", base_sha, &commit])
        .map(|s| s.lines().filter(|l| !l.is_empty()).count())
        .unwrap_or(0);
    let digest = crate::diff::patch_digest(&diff_blob);

    ArchiveOutcome::Archived(ArchiveResult {
        archive_ref,
        base_sha: base_sha.to_string(),
        digest,
        diff_blob,
        files_changed,
    })
}

/// Whether a git invocation succeeded.
fn ok(out: std::io::Result<std::process::Output>) -> bool {
    matches!(out, Ok(o) if o.status.success())
}

/// Reclaim a dead agent's physical checkout AFTER its work is archived (or it had
/// none). Unlocks first (provision locks live worktrees), removes the worktree,
/// drops any legacy per-agent branch, prunes git's bookkeeping. Best-effort —
/// never errors. The `taime_worktrees` row and the archive ref survive; only the
/// disposable checkout goes. NEVER pass a shared-mode path — callers gate on
/// `mode == "isolated"`; the equality guard in [`remove_force`] also defends it.
pub fn reclaim_checkout(worktree_path: &str, repo_root: &str, branch: Option<&str>) {
    let repo = Path::new(repo_root);
    if repo.is_dir() {
        // A live worktree is git-locked; a single `--force` won't remove a locked
        // tree, so unlock first (no-op / harmless error if it wasn't locked).
        let _ = run_git(repo, &["worktree", "unlock", worktree_path]);
    }
    remove_force(worktree_path, Some(repo_root), branch);
}

/// Delete an agent's archive keep-around ref (`refs/taime/archive/<id>`), for the
/// "delete workspace" teardown where the durable record is being discarded too.
/// Best-effort. The snapshot commit becomes unreachable and is GC'd by git.
pub fn drop_archive_ref(repo_root: &str, archive_ref: &str) {
    let repo = Path::new(repo_root);
    if repo.is_dir() {
        let _ = run_git(repo, &["update-ref", "-d", archive_ref]);
    }
}

/// Force-remove a provisioned ISOLATED worktree checkout + its branch, for the
/// "delete workspace" teardown (and the reclaim path via [`reclaim_checkout`]).
/// Best-effort: never errors. NEVER pass a shared-mode `worktree_path` (that's
/// the user's real project dir) — callers gate on `mode == "isolated"`; the
/// equality guard below is a second line of defense.
pub fn remove_force(worktree_path: &str, repo_root: Option<&str>, branch: Option<&str>) {
    // Refuse to fs-delete a path that is the repo root itself (shared mode).
    let same_as_repo = repo_root.map(|r| Path::new(r) == Path::new(worktree_path)).unwrap_or(false);
    if let Some(repo) = repo_root {
        let repo = Path::new(repo);
        if repo.is_dir() {
            // Unlock so `--force` removes even a still-locked live worktree.
            let _ = run_git(repo, &["worktree", "unlock", worktree_path]);
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
///
/// BRANCHLESS (the archive-then-reclaim model): the worktree is created with a
/// DETACHED HEAD at `base_sha`, not on a `taime/<provider>-<agent_id>` branch.
/// The per-agent branch was functionally inert — diffs use `base_sha`, `git apply`
/// merges hunks, and attribution is keyed by `agent_id`, never the branch — so
/// dropping it makes `git branch` show `main` (and only `main`) as a steady state,
/// while the durable record lives in `refs/taime/archive/*`. We also `git worktree
/// lock` the live checkout so neither the retention sweep nor an external `git
/// worktree prune` can reap a running agent's tree.
pub fn provision(
    project_root: &str,
    _provider: &str,
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
        // Branchless: detached HEAD, no per-agent branch.
        branch: None,
        base_sha: Some(base_sha.clone()),
        mode: "isolated".to_string(),
        error: None,
    };

    // Idempotent reuse: an existing worktree checkout (relaunch on same key).
    if wt_dir.join(".git").exists() {
        // Re-assert the live lock (a prior daemon may have unlocked it at exit).
        let _ = run_git(repo, &["worktree", "lock", "--reason", "taime: live agent", &wt_path]);
        return ok;
    }

    // Create a fresh detached worktree at base. If git still has stale
    // bookkeeping for this path (a crash that lost the dir), prune and retry once.
    let add = run_git(repo, &["worktree", "add", "--detach", &wt_path, &base_sha]);
    if !matches!(&add, Ok(o) if o.status.success()) {
        let _ = run_git(repo, &["worktree", "prune"]);
        let retry = run_git(repo, &["worktree", "add", "--detach", &wt_path, &base_sha]);
        if !matches!(&retry, Ok(o) if o.status.success()) {
            let err = retry
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stderr).trim().to_string())
                .unwrap_or_default();
            return shared(project_root, agent_id, Some(format!("git worktree add failed: {err}")));
        }
    }
    // Lock the live checkout so retention can't reap a running agent's tree.
    let _ = run_git(repo, &["worktree", "lock", "--reason", "taime: live agent", &wt_path]);
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
    fn git_repo_provisions_branchless_isolated_worktree() {
        let repo = temp_repo();
        let info = provision(repo.to_str().unwrap(), "claude_code", true, "abc12345");
        assert_eq!(info.mode, "isolated", "error: {:?}", info.error);
        // Branchless: no per-agent branch, and `git branch` stays main-only.
        assert_eq!(info.branch, None, "provision is branchless (detached HEAD)");
        assert!(info.base_sha.is_some());
        let branches = git_stdout(&repo, &["branch", "--list"]).unwrap_or_default();
        assert!(
            !branches.contains("taime/"),
            "no per-agent branch leaks into `git branch`: {branches:?}"
        );
        // The worktree HEAD is detached at base (not on a branch).
        let wt = Path::new(&info.worktree_path);
        assert!(wt.join(".git").exists(), "worktree checkout created");
        assert!(wt.join("README.md").exists(), "tracked file present");
        assert!(wt.join(".env").exists(), ".env copied into worktree");
        assert_eq!(
            git_stdout(wt, &["rev-parse", "HEAD"]).as_deref(),
            info.base_sha.as_deref(),
            "detached HEAD sits at base"
        );

        // Idempotent: re-provisioning the same key reuses it.
        let again = provision(repo.to_str().unwrap(), "claude_code", true, "abc12345");
        assert_eq!(again.mode, "isolated");
        assert_eq!(again.worktree_path, info.worktree_path);

        // Cleanup (reclaim unlocks the live lock, then removes).
        reclaim_checkout(&info.worktree_path, repo.to_str().unwrap(), None);
        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(Path::new(&info.worktree_path).parent().unwrap());
    }

    #[test]
    fn archive_then_reclaim_round_trips() {
        let repo = temp_repo();
        let root = repo.to_str().unwrap();
        let info = provision(root, "claude_code", true, "ar111111");
        let base = info.base_sha.clone().unwrap();
        let wt = Path::new(&info.worktree_path);
        // A modification + an untracked new file + a transient temp that must NOT
        // be captured.
        std::fs::write(wt.join("README.md"), "hi\nedited\n").unwrap();
        std::fs::write(wt.join("new.txt"), "agent created\n").unwrap();
        std::fs::write(wt.join("README.md.tmp.123.abcdef"), "scratch\n").unwrap();

        let res = match archive_agent("ar111111", &info.worktree_path, root, &base) {
            ArchiveOutcome::Archived(r) => r,
            other => panic!("expected Archived, got {other:?}"),
        };
        assert!(res.diff_blob.contains("new.txt"), "untracked file archived");
        assert!(res.diff_blob.contains("edited"), "modification archived");
        assert!(!res.diff_blob.contains(".tmp."), "transient temp filtered out");
        assert!(res.files_changed >= 2);
        // The keep-around ref exists and the digest matches the cached patch.
        assert!(git_stdout(&repo, &["rev-parse", "--verify", &res.archive_ref]).is_some());
        assert_eq!(res.digest, crate::diff::patch_digest(&res.diff_blob));

        // Reclaim: the checkout is gone, but the ref (and thus the work) survives,
        // and re-diffing the ref reproduces the cached patch byte-for-byte.
        reclaim_checkout(&info.worktree_path, root, info.branch.as_deref());
        assert!(!wt.exists(), "physical checkout reclaimed");
        let from_ref = run_git(&repo, &["diff", &base, &res.archive_ref]).unwrap();
        let from_ref = String::from_utf8_lossy(&from_ref.stdout).to_string();
        assert_eq!(from_ref, res.diff_blob, "diff rebuilds identically from the ref");

        // Idempotent: archiving again after reclaim fails cleanly (checkout gone),
        // and the ref is still intact for review/merge.
        assert!(matches!(
            archive_agent("ar111111", &info.worktree_path, root, &base),
            ArchiveOutcome::Failed(_)
        ));
        assert!(git_stdout(&repo, &["rev-parse", "--verify", &res.archive_ref]).is_some());

        let wt_parent = wt.parent().map(|p| p.to_path_buf());
        let _ = std::fs::remove_dir_all(&repo);
        if let Some(p) = wt_parent {
            let _ = std::fs::remove_dir_all(p);
        }
    }

    #[test]
    fn archive_clean_worktree_is_nochanges() {
        let repo = temp_repo();
        let root = repo.to_str().unwrap();
        let info = provision(root, "claude_code", true, "clean000");
        let base = info.base_sha.clone().unwrap();
        assert_eq!(
            archive_agent("clean000", &info.worktree_path, root, &base),
            ArchiveOutcome::NoChanges,
            "a clean worktree has nothing to keep"
        );
        reclaim_checkout(&info.worktree_path, root, None);
        let wt_parent = Path::new(&info.worktree_path).parent().map(|p| p.to_path_buf());
        let _ = std::fs::remove_dir_all(&repo);
        if let Some(p) = wt_parent {
            let _ = std::fs::remove_dir_all(p);
        }
    }
}
