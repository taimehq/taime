//! Source-tree file watcher with dirty-state surfacing.
//!
//! Design (terminal ↔ working-dir mapping): React owns frame lifecycle and the
//! terminal ids. When a frame opens, React resolves the terminal's working
//! directory (`GET /terminals/{id}/working-directory`) and calls `watch_terminal`.
//! We keep:
//!   * `terminals: terminal_id -> canonical_dir`
//!   * `watchers: canonical_dir -> WatchEntry` (one OS watcher per distinct dir)
//! A filesystem event resolves path -> owning watched dir -> the terminal(s) on
//! that dir, and we emit `terminal://{id}/fs-dirty` per affected terminal.
//!
//! In v1 single-project mode every agent shares one workspace dir, so this
//! collapses to a single watcher fanning out to all terminals — but the
//! per-dir structure already supports distinct dirs later.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use std::time::{SystemTime, UNIX_EPOCH};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{EventKind, RecursiveMode, Watcher};
use notify_debouncer_full::{new_debouncer, DebouncedEvent, Debouncer, FileIdMap};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

const DEBOUNCE: Duration = Duration::from_millis(250);

/// Directory names that never carry meaningful agent-driven source changes.
/// Aggressively pruned so a burst inside node_modules/.git can't flood events.
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

/// Memo of directories known to carry a `CACHEDIR.TAG` marker, so a build
/// storm under one doesn't re-stat its ancestors on every event.
fn cache_dir_memo() -> &'static Mutex<HashSet<PathBuf>> {
    static MEMO: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(HashSet::new()))
}

/// True if any ancestor directory of `path` (up to `root`) holds a
/// `CACHEDIR.TAG` file — the cross-tool marker for a cache/build directory
/// (cargo's `target`, including random-suffixed temp variants like
/// `targetIXC1zL`, plus many other tools). Everything under such a directory is
/// build output, not an agent-driven source change.
fn under_cachedir_tag(path: &Path, root: &Path) -> bool {
    let memo = cache_dir_memo();
    // Fast path: a known cache dir is an ancestor.
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
    // Slow path: probe ancestors for the marker, memoizing any hit.
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

/// File suffixes/markers we ignore (lockfiles, logs, editor temp/swap).
fn is_denied_file(name: &str) -> bool {
    name.ends_with(".lock")
        || name.ends_with(".log")
        || name.ends_with("~")
        || name.ends_with(".swp")
        || name.ends_with(".swx")
        || name == "4913" // vim atomic-write probe
        || name == ".DS_Store"
}

type Deb = Debouncer<notify::RecommendedWatcher, FileIdMap>;

struct WatchEntry {
    // Held to keep the OS watch alive (RAII): dropping the debouncer stops the
    // watch. Not read directly, hence the allow.
    #[allow(dead_code)]
    debouncer: Deb,
    gitignore: Option<Gitignore>,
    /// Accumulated changed paths (relative to the watched dir) since last clear.
    dirty: HashSet<String>,
}

#[derive(Default)]
struct Inner {
    /// terminal_id -> canonical watched dir
    terminals: HashMap<String, PathBuf>,
    /// canonical dir -> watcher
    watchers: HashMap<PathBuf, WatchEntry>,
}

#[derive(Clone)]
pub struct FsWatchState {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirtyPayload {
    pub terminal_id: String,
    pub count: usize,
    pub paths: Vec<String>,
}

/// A single attributed filesystem change: which file, what kind, and when.
/// Powers the per-agent activity timeline (Taime attribution).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEvent {
    /// Path relative to the watched dir.
    pub path: String,
    /// "create" | "modify" | "delete".
    pub kind: String,
    /// Milliseconds since the Unix epoch (wall clock at handling time).
    pub ts: u64,
}

/// A debounce-window batch of file events for one terminal.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FsEventBatch {
    pub terminal_id: String,
    pub events: Vec<FileEvent>,
}

/// Coarse change classification from notify's fine-grained EventKind.
fn classify_kind(kind: &EventKind) -> &'static str {
    match kind {
        EventKind::Create(_) => "create",
        EventKind::Remove(_) => "delete",
        _ => "modify", // Modify/Access/Any/Other all read as "modify"
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl FsWatchState {
    pub fn new() -> Self {
        FsWatchState {
            inner: Arc::new(Mutex::new(Inner::default())),
        }
    }

    /// Determine which terminals are mapped to a given canonical dir.
    fn terminals_for_dir(inner: &Inner, dir: &Path) -> Vec<String> {
        inner
            .terminals
            .iter()
            .filter(|(_, d)| d.as_path() == dir)
            .map(|(t, _)| t.clone())
            .collect()
    }

    /// Start (or attach to) a watcher for `terminal_id` rooted at `dir`.
    pub fn watch_terminal(
        &self,
        app: &AppHandle,
        terminal_id: String,
        dir: String,
    ) -> Result<(), String> {
        let canonical =
            std::fs::canonicalize(&dir).map_err(|e| format!("cannot resolve {dir}: {e}"))?;
        if !canonical.is_dir() {
            return Err(format!("{} is not a directory", canonical.display()));
        }

        let mut inner = self.inner.lock().unwrap();

        // If this terminal was watching a different dir, detach it first.
        if let Some(prev) = inner.terminals.get(&terminal_id).cloned() {
            if prev != canonical {
                drop_terminal_locked(&mut inner, &terminal_id);
            } else {
                return Ok(()); // already watching the right dir
            }
        }

        inner
            .terminals
            .insert(terminal_id.clone(), canonical.clone());

        // Reuse an existing watcher for this dir if present.
        if inner.watchers.contains_key(&canonical) {
            return Ok(());
        }

        // Build a gitignore matcher rooted at the dir (best-effort).
        let gitignore = build_gitignore(&canonical);

        let app_for_cb = app.clone();
        let state_for_cb = self.clone();
        let dir_for_cb = canonical.clone();

        let mut debouncer = new_debouncer(
            DEBOUNCE,
            None,
            move |result: Result<Vec<DebouncedEvent>, Vec<notify::Error>>| {
                let Ok(events) = result else { return };
                state_for_cb.handle_events(&app_for_cb, &dir_for_cb, events);
            },
        )
        .map_err(|e| format!("watcher init failed: {e}"))?;

        debouncer
            .watcher()
            .watch(&canonical, RecursiveMode::Recursive)
            .map_err(|e| format!("watch failed: {e}"))?;

        inner.watchers.insert(
            canonical.clone(),
            WatchEntry {
                debouncer,
                gitignore,
                dirty: HashSet::new(),
            },
        );
        Ok(())
    }

    /// Stop watching for a terminal (e.g. its frame closed). Drops the OS
    /// watcher only when no other terminal references the same dir.
    pub fn unwatch_terminal(&self, terminal_id: String) {
        let mut inner = self.inner.lock().unwrap();
        drop_terminal_locked(&mut inner, &terminal_id);
    }

    /// Clear accumulated dirty state for a terminal's dir (e.g. after the user
    /// reviews the diff). Affects all terminals sharing that dir.
    pub fn clear_dirty(&self, terminal_id: String) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(dir) = inner.terminals.get(&terminal_id).cloned() {
            if let Some(entry) = inner.watchers.get_mut(&dir) {
                entry.dirty.clear();
            }
        }
    }

    fn handle_events(&self, app: &AppHandle, dir: &Path, events: Vec<DebouncedEvent>) {
        let mut inner = self.inner.lock().unwrap();
        let Some(entry) = inner.watchers.get_mut(dir) else {
            return;
        };

        // Per-batch, per-file change classification (latest kind wins). This
        // drives the attribution timeline. Separately we maintain the
        // accumulated dirty SET for the badge/inventory (existing behavior).
        let ts = now_millis();
        let mut batch: Vec<FileEvent> = Vec::new();
        let mut seen_in_batch: HashMap<String, usize> = HashMap::new();
        let mut dirty_grew = false;

        for ev in &events {
            let kind = classify_kind(&ev.kind);
            for path in &ev.paths {
                if let Some(rel) = accept_path(path, dir, entry.gitignore.as_ref()) {
                    if entry.dirty.insert(rel.clone()) {
                        dirty_grew = true;
                    }
                    match seen_in_batch.get(&rel) {
                        Some(&idx) => batch[idx].kind = kind.to_string(),
                        None => {
                            seen_in_batch.insert(rel.clone(), batch.len());
                            batch.push(FileEvent {
                                path: rel,
                                kind: kind.to_string(),
                                ts,
                            });
                        }
                    }
                }
            }
        }

        // Nothing meaningful in this window.
        if batch.is_empty() {
            return;
        }

        let count = entry.dirty.len();
        let mut paths: Vec<String> = entry.dirty.iter().cloned().collect();
        paths.sort();
        paths.truncate(50);

        let terminals = Self::terminals_for_dir(&inner, dir);
        drop(inner);

        for tid in terminals {
            // Per-file attributed event stream (new): always fires when files
            // changed this window, so re-edits of the same file are recorded.
            let _ = app.emit(
                &format!("terminal://{tid}/fs-event"),
                FsEventBatch {
                    terminal_id: tid.clone(),
                    events: batch.clone(),
                },
            );
            // Dirty-set surface (existing): only when the set actually grew.
            if dirty_grew {
                let _ = app.emit(
                    &format!("terminal://{tid}/fs-dirty"),
                    DirtyPayload {
                        terminal_id: tid.clone(),
                        count,
                        paths: paths.clone(),
                    },
                );
            }
        }
    }
}

fn drop_terminal_locked(inner: &mut Inner, terminal_id: &str) {
    let Some(dir) = inner.terminals.remove(terminal_id) else {
        return;
    };
    // If no remaining terminal references this dir, drop the watcher.
    let still_used = inner.terminals.values().any(|d| d == &dir);
    if !still_used {
        inner.watchers.remove(&dir); // dropping the Debouncer stops the OS watch
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

/// Decide whether a changed path is a meaningful source change. Returns the
/// path relative to `root` (for display) if accepted, else None.
fn accept_path(path: &Path, root: &Path, gitignore: Option<&Gitignore>) -> Option<String> {
    // Reject anything under a denied directory, or denied file names.
    for comp in path.components() {
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
    // Reject build/cache output marked with CACHEDIR.TAG (cargo target dirs and
    // their random-suffixed temp variants, etc.) that DENY_DIRS' exact-name
    // match can't catch.
    if under_cachedir_tag(path, root) {
        return None;
    }
    // Honor .gitignore when available. Use matched_path_or_any_parents (NOT
    // plain `matched`): a file inside an ignored directory (e.g.
    // .playwright-mcp/x.png) is only caught when the parent-dir rule is applied
    // to its ancestors, which `matched` does not do — it tests the literal path.
    if let Some(gi) = gitignore {
        let is_dir = path.is_dir();
        if gi.matched_path_or_any_parents(path, is_dir).is_ignore() {
            return None;
        }
    }
    let rel = path.strip_prefix(root).unwrap_or(path);
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
    fn classifies_event_kinds() {
        use notify::event::{CreateKind, ModifyKind, RemoveKind};
        assert_eq!(
            classify_kind(&EventKind::Create(CreateKind::File)),
            "create"
        );
        assert_eq!(
            classify_kind(&EventKind::Remove(RemoveKind::File)),
            "delete"
        );
        assert_eq!(
            classify_kind(&EventKind::Modify(ModifyKind::Data(
                notify::event::DataChange::Content
            ))),
            "modify"
        );
        assert_eq!(classify_kind(&EventKind::Any), "modify");
    }
}
