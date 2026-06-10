//! Per-session source-tree watcher (Phase 6 — daemon-owned fs attribution).
//!
//! Each live agent's worktree gets ONE debounced recursive watcher, owned by the
//! daemon (not the app), so attribution is durable even with the UI closed. The
//! watcher classifies + filters raw filesystem events (the same noise rules the
//! old app-side watcher used: deny build/cache dirs, lockfiles, `.gitignore`,
//! `CACHEDIR.TAG`) and hands the session a batch of `(relative_path, kind)`. The
//! session accumulates the dirty set, records events to the store, populates each
//! turn's `fs_dirty_paths`, and pushes `FsDirty` to the attached client.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{EventKind, RecursiveMode, Watcher};
use notify_debouncer_full::{new_debouncer, DebouncedEvent, Debouncer, FileIdMap};

const DEBOUNCE: Duration = Duration::from_millis(250);

/// Directory names that never carry meaningful agent-driven source changes.
const DENY_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    ".turbo",
    ".cache",
    ".venv",
    "venv",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    "coverage",
    ".idea",
    ".vscode",
    ".DS_Store",
];

/// Held alive (RAII) by the owning session: dropping it stops the OS watch.
pub type SessionWatcher = Debouncer<notify::RecommendedWatcher, FileIdMap>;

/// One accepted change in a debounce window: path relative to the watched root,
/// and a coarse kind (`create` | `modify` | `delete`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsChange {
    pub path: String,
    pub kind: &'static str,
}

/// Start a recursive debounced watcher on `dir`, calling `on_batch` with the
/// filtered, de-duplicated changes for each debounce window (latest kind wins
/// per path). Returns the watcher; drop it to stop. `on_batch` runs on the
/// debouncer's own thread, so it must not block on the session's locks for long.
pub fn spawn_watcher<F>(dir: &Path, on_batch: F) -> Result<SessionWatcher, String>
where
    F: Fn(Vec<FsChange>) + Send + 'static,
{
    let canonical = std::fs::canonicalize(dir).map_err(|e| format!("cannot resolve {dir:?}: {e}"))?;
    if !canonical.is_dir() {
        return Err(format!("{} is not a directory", canonical.display()));
    }
    let gitignore = build_gitignore(&canonical);
    let root = canonical.clone();
    let mut debouncer = new_debouncer(
        DEBOUNCE,
        None,
        move |result: Result<Vec<DebouncedEvent>, Vec<notify::Error>>| {
            let Ok(events) = result else { return };
            let batch = filter_batch(&events, &root, gitignore.as_ref());
            if !batch.is_empty() {
                on_batch(batch);
            }
        },
    )
    .map_err(|e| format!("watcher init failed: {e}"))?;
    debouncer
        .watcher()
        .watch(&canonical, RecursiveMode::Recursive)
        .map_err(|e| format!("watch failed: {e}"))?;
    Ok(debouncer)
}

/// Reduce a debounce window of raw events to accepted `(rel, kind)` changes,
/// latest kind winning per path (insertion order preserved).
fn filter_batch(events: &[DebouncedEvent], root: &Path, gitignore: Option<&Gitignore>) -> Vec<FsChange> {
    let mut out: Vec<FsChange> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for ev in events {
        let kind = classify_kind(&ev.kind);
        for path in &ev.paths {
            if let Some(rel) = accept_path(path, root, gitignore) {
                match index.get(&rel) {
                    Some(&i) => out[i].kind = kind,
                    None => {
                        index.insert(rel.clone(), out.len());
                        out.push(FsChange { path: rel, kind });
                    }
                }
            }
        }
    }
    out
}

/// Coarse change classification from notify's fine-grained `EventKind`.
fn classify_kind(kind: &EventKind) -> &'static str {
    match kind {
        EventKind::Create(_) => "create",
        EventKind::Remove(_) => "delete",
        _ => "modify",
    }
}

/// Clearly-transient files no human reviews: atomic-write temps (`*.tmp` and the
/// `<file>.tmp.<pid>.<hash>` pattern editors/agents write), editor swap/backup
/// files, and OS cruft. Shared by the watcher AND the diff/contention surfaces
/// (see diff.rs) so write-churn noise never reaches the dirty / review /
/// contention lists. Deliberately does NOT include lockfiles or logs — those can
/// be real, reviewable project files (Cargo.lock, package-lock.json, …).
pub fn is_transient_file(name: &str) -> bool {
    name.ends_with(".tmp")
        || name.contains(".tmp.") // atomic-write temp: foo.md.tmp.<pid>.<hash>
        || name.ends_with('~') // editor backup
        || name.ends_with(".swp")
        || name.ends_with(".swx")
        || name.ends_with(".orig")
        || name.ends_with(".bak")
        || name.starts_with(".#") // emacs lock
        || name == "4913" // vim atomic-write probe
        || name == ".DS_Store"
}

/// What the WATCHER ignores: transients PLUS lockfiles/logs — their churn
/// shouldn't wake the dirty timeline (but they're fine to review in a diff).
fn is_denied_file(name: &str) -> bool {
    is_transient_file(name) || name.ends_with(".lock") || name.ends_with(".log")
}

/// Memo of directories known to carry a `CACHEDIR.TAG` marker.
fn cache_dir_memo() -> &'static Mutex<HashSet<PathBuf>> {
    static MEMO: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(HashSet::new()))
}

/// True if any ancestor of `path` (up to `root`) holds a `CACHEDIR.TAG` file —
/// the cross-tool marker for a cache/build directory (cargo `target` + its
/// random-suffixed temp variants, etc.). Memoizes hits.
fn under_cachedir_tag(path: &Path, root: &Path) -> bool {
    let memo = cache_dir_memo();
    {
        let known = memo.lock().unwrap();
        let mut d = path.parent();
        while let Some(dir) = d {
            if known.contains(dir) {
                return true;
            }
            if dir == root {
                break;
            }
            d = dir.parent();
        }
    }
    let mut d = path.parent();
    while let Some(dir) = d {
        if dir.join("CACHEDIR.TAG").is_file() {
            memo.lock().unwrap().insert(dir.to_path_buf());
            return true;
        }
        if dir == root {
            break;
        }
        d = dir.parent();
    }
    false
}

/// How long a shared-mode change stays "claimed" by the session that first
/// observed it, locking out co-watchers (review item 4). Just long enough to span
/// the debounce skew between two watchers seeing the SAME edit (both fire within a
/// `DEBOUNCE` window); short enough that a genuinely independent later edit by
/// another agent isn't swallowed.
const SHARED_DEDUP_WINDOW: Duration = Duration::from_secs(2);

/// (root, rel-path) → (owning session id, when claimed): the shared-mode de-dup map.
type SharedClaims = HashMap<(String, String), (String, Instant)>;

/// Process-singleton de-dup map, like [`cache_dir_memo`] — the daemon owns every
/// session in one process.
fn shared_claims() -> &'static Mutex<SharedClaims> {
    static CLAIMS: OnceLock<Mutex<SharedClaims>> = OnceLock::new();
    CLAIMS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Cross-session de-dup for SHARED-mode watchers (review item 4): when two shared
/// agents watch the same `root`, each watcher sees the other's edits. Returns
/// whether `owner` should RECORD `(root, path)` — true for the first session to
/// claim it (and for the same `owner` re-editing its own file, which always
/// re-claims), false for a DIFFERENT session within [`SHARED_DEDUP_WINDOW`], so a
/// shared edit is attributed once (to one agent), never double-counted as phantom
/// contention. Isolated worktrees are unique per agent, so callers invoke this in
/// shared mode only. `now` is injected so the window is unit-testable.
pub fn claim_shared_change(root: &str, path: &str, owner: &str, now: Instant) -> bool {
    let mut map = shared_claims().lock().unwrap();
    // Opportunistic prune so the map can't grow unbounded (entries are short-lived).
    map.retain(|_, (_, when)| now.duration_since(*when) < SHARED_DEDUP_WINDOW);
    let key = (root.to_string(), path.to_string());
    match map.get(&key) {
        // A DIFFERENT session claimed it within the window — it recorded it; skip.
        Some((existing, _)) if existing != owner => false,
        // Free, expired, or our own re-edit: (re)claim and record.
        _ => {
            map.insert(key, (owner.to_string(), now));
            true
        }
    }
}

fn build_gitignore(dir: &Path) -> Option<Gitignore> {
    let mut builder = GitignoreBuilder::new(dir);
    let gi = dir.join(".gitignore");
    if gi.exists() {
        builder.add(gi);
    }
    builder.build().ok()
}

/// Decide whether a changed path is a meaningful source change. Returns the path
/// relative to `root` if accepted, else None.
fn accept_path(path: &Path, root: &Path, gitignore: Option<&Gitignore>) -> Option<String> {
    // Scan only the components BELOW the watched root for denied dirs — otherwise
    // a project nested under e.g. `…/build/myproject` would match `build` in its
    // own path prefix and drop every event.
    let rel = path.strip_prefix(root).unwrap_or(path);
    for comp in rel.components() {
        let s = comp.as_os_str().to_string_lossy();
        if DENY_DIRS.contains(&s.as_ref()) {
            return None;
        }
    }
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if is_denied_file(name) {
            return None;
        }
    }
    if under_cachedir_tag(path, root) {
        return None;
    }
    if let Some(gi) = gitignore {
        let is_dir = path.is_dir();
        if gi.matched_path_or_any_parents(path, is_dir).is_ignore() {
            return None;
        }
    }
    Some(rel.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denies_build_dirs() {
        let root = Path::new("/proj");
        assert!(accept_path(Path::new("/proj/node_modules/x.js"), root, None).is_none());
        assert!(accept_path(Path::new("/proj/.git/HEAD"), root, None).is_none());
        assert!(accept_path(Path::new("/proj/target/debug/a"), root, None).is_none());
    }

    #[test]
    fn denies_noise_files() {
        let root = Path::new("/proj");
        assert!(accept_path(Path::new("/proj/pnpm-lock.lock"), root, None).is_none());
        assert!(accept_path(Path::new("/proj/x.swp"), root, None).is_none());
        assert!(accept_path(Path::new("/proj/debug.log"), root, None).is_none());
        // Atomic-write temp (editor/agent): `<file>.tmp.<pid>.<hash>` — the noise
        // that was leaking into the dirty/review/contention surfaces.
        assert!(
            accept_path(Path::new("/proj/notes.md.tmp.26298.0a12ff87c3d1"), root, None).is_none()
        );
        assert!(accept_path(Path::new("/proj/out.tmp"), root, None).is_none());
    }

    #[test]
    fn transient_classifies_temps_but_not_lockfiles() {
        // Atomic-write temps + editor/OS cruft are transient (filtered everywhere).
        assert!(is_transient_file("notes.md.tmp.26298.0a12ff87c3d1"));
        assert!(is_transient_file("out.tmp"));
        assert!(is_transient_file("main.rs~"));
        assert!(is_transient_file("buffer.swp"));
        assert!(is_transient_file("patch.orig"));
        assert!(is_transient_file("data.bak"));
        assert!(is_transient_file(".#main.rs"));
        assert!(is_transient_file("4913"));
        assert!(is_transient_file(".DS_Store"));
        // Lockfiles/logs are NOT transient — they can be real, reviewable changes
        // (so the diff keeps them), even though the watcher still ignores them.
        assert!(!is_transient_file("Cargo.lock"));
        assert!(!is_transient_file("pnpm-lock.yaml"));
        assert!(!is_transient_file("debug.log"));
        assert!(!is_transient_file("main.rs"));
        assert!(is_denied_file("Cargo.lock") && is_denied_file("debug.log"));
    }

    #[test]
    fn accepts_source_with_relative_path() {
        let root = Path::new("/proj");
        let got = accept_path(Path::new("/proj/src/main.rs"), root, None);
        assert_eq!(got.as_deref(), Some("src/main.rs"));
    }

    #[test]
    fn deny_dirs_in_the_root_prefix_do_not_drop_events() {
        // A project legitimately nested under a `build/` (or `dist/`, `.venv/`…)
        // ANCESTOR must still get events — only deny dirs BELOW the root count.
        let root = Path::new("/home/alice/build/myproject");
        let got = accept_path(Path::new("/home/alice/build/myproject/src/main.rs"), root, None);
        assert_eq!(got.as_deref(), Some("src/main.rs"));
        // …but a node_modules INSIDE the project is still denied.
        assert!(accept_path(
            Path::new("/home/alice/build/myproject/node_modules/x.js"),
            root,
            None
        )
        .is_none());
    }

    #[test]
    fn shared_change_dedup_locks_out_co_watchers_within_the_window() {
        // Unique root per test run so the process-singleton map can't collide with
        // a parallel test.
        let root = "/dedup-test-root-A";
        let t0 = Instant::now();
        // First co-watcher to observe the edit records it.
        assert!(claim_shared_change(root, "src/a.rs", "agent-1", t0));
        // A DIFFERENT co-watcher seeing the SAME edit within the window is locked
        // out — no double attribution / phantom contention.
        assert!(!claim_shared_change(root, "src/a.rs", "agent-2", t0));
        // The owner re-editing its own file always re-claims (keeps recording).
        assert!(claim_shared_change(root, "src/a.rs", "agent-1", t0));
        // A different PATH is independent.
        assert!(claim_shared_change(root, "src/b.rs", "agent-2", t0));
        // A different ROOT is independent (different shared workspace).
        assert!(claim_shared_change("/dedup-test-root-B", "src/a.rs", "agent-2", t0));

        // After the window lapses, the lock-out clears and a new owner can claim.
        let later = t0 + SHARED_DEDUP_WINDOW + Duration::from_millis(1);
        assert!(claim_shared_change(root, "src/a.rs", "agent-2", later));
    }

    #[test]
    fn classifies_event_kinds() {
        use notify::event::{CreateKind, ModifyKind, RemoveKind};
        assert_eq!(classify_kind(&EventKind::Create(CreateKind::File)), "create");
        assert_eq!(classify_kind(&EventKind::Remove(RemoveKind::File)), "delete");
        assert_eq!(
            classify_kind(&EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Content))),
            "modify"
        );
        assert_eq!(classify_kind(&EventKind::Any), "modify");
    }
}
