//! Per-session source-tree watcher (Phase 6 — daemon-owned fs attribution).
//!
//! Each live agent's worktree gets ONE debounced recursive watcher, owned by the
//! daemon (not the app), so attribution is durable even with the UI closed. The
//! watcher classifies + filters raw filesystem events (the same noise rules the
//! old app-side watcher used: deny build/cache dirs, lockfiles, `.gitignore`,
//! `CACHEDIR.TAG`) and hands the session a batch of `(relative_path, kind)`. The
//! session accumulates the dirty set, records events to the store, populates each
//! turn's `fs_dirty_paths`, and pushes `FsDirty` to the attached client.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

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

/// File suffixes/markers we ignore (lockfiles, logs, editor temp/swap).
fn is_denied_file(name: &str) -> bool {
    name.ends_with(".lock")
        || name.ends_with(".log")
        || name.ends_with('~')
        || name.ends_with(".swp")
        || name.ends_with(".swx")
        || name == "4913" // vim atomic-write probe
        || name == ".DS_Store"
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
