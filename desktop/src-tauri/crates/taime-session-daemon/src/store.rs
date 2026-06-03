//! Durable orchestration store (CAO-replacement Phase 3).
//!
//! A single SQLite database in the **app-data dir** (`dirs::data_dir()/taime/`,
//! e.g. `~/Library/Application Support/taime/taime.sqlite` on macOS — the
//! CAO-equivalent home), via `rusqlite` (bundled SQLite, so the daemon is
//! self-contained). The schema **mirrors CAO's** (`clients/database.py`) so the
//! Phase-7 `--import-cao` is a row copy, plus a daemon-native `daemon_sessions`
//! table for the PTY session lifecycle and a `taime_meta` table for the
//! idempotent-import marker.
//!
//! Location split (the plan's invariant): durable state lives HERE in app-data;
//! the **runtime dir** (`$TMPDIR/taime/`) stays transient (socket/lock/token) —
//! a temp dir can be GC'd, and the inbox/worktrees/turns must outlive it.
//!
//! Persistence is **best-effort**: if the DB can't open, the daemon still runs
//! (every call goes through `Manager`'s `Option<Store>`); a launch never fails
//! because of the store.

use std::path::PathBuf;
use std::sync::Mutex;

use rusqlite::Connection;
use taime_protocol::WorktreeInfo;

/// The durable store. `Connection` is `Send` but not `Sync`; the `Mutex` makes
/// `Store` `Sync` so it can live behind the shared `Arc<Manager>`. Write volume
/// is low (one row per spawn/exit), so a single guarded connection is ample.
pub struct Store {
    conn: Mutex<Connection>,
}

/// `dirs::data_dir()/taime/` — created if absent. The CAO-equivalent durable home.
pub fn data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("taime"))
}

/// A persisted daemon session row (history + crash-survival metadata).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub pty_session_id: String,
    pub provider: Option<String>,
    pub attribution_key: Option<String>,
    pub cwd: Option<String>,
    pub program: String,
    pub created_at_unix: u64,
    pub status: String,
}

impl Store {
    /// Open (creating the dir + file) and run the schema migration.
    pub fn open() -> rusqlite::Result<Store> {
        let dir = data_dir().ok_or_else(|| {
            rusqlite::Error::InvalidParameterName("no data dir".into())
        })?;
        std::fs::create_dir_all(&dir).map_err(|e| {
            rusqlite::Error::InvalidParameterName(format!("create data dir: {e}"))
        })?;
        let conn = Connection::open(dir.join("taime.sqlite"))?;
        // WAL for concurrent readers + a single writer; durable across crashes.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Store { conn: Mutex::new(conn) };
        store.migrate()?;
        Ok(store)
    }

    /// Open at an explicit path (tests).
    #[cfg(test)]
    pub fn open_at(path: &std::path::Path) -> rusqlite::Result<Store> {
        let conn = Connection::open(path)?;
        let store = Store { conn: Mutex::new(conn) };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(SCHEMA)
    }

    /// Record a freshly-spawned agent session (status `running`). Best-effort:
    /// errors are returned for the caller to log, never to fail the spawn.
    pub fn record_session(&self, row: &SessionRow) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO daemon_sessions \
               (pty_session_id, provider, attribution_key, cwd, program, created_at_unix, status) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT(pty_session_id) DO UPDATE SET status = excluded.status",
            rusqlite::params![
                row.pty_session_id,
                row.provider,
                row.attribution_key,
                row.cwd,
                row.program,
                row.created_at_unix as i64,
                row.status,
            ],
        )?;
        Ok(())
    }

    /// Update a session's status (e.g. `exited` when reaped).
    pub fn set_session_status(&self, pty_session_id: &str, status: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE daemon_sessions SET status = ?2 WHERE pty_session_id = ?1",
            rusqlite::params![pty_session_id, status],
        )?;
        Ok(())
    }

    /// Persist (or update) a provisioned worktree row, keyed by `terminal_key`
    /// (the CAO terminal id). Mirrors CAO's `taime_worktrees` upsert so the
    /// Phase-7 import is a row copy. `created_at` stored as a unix-seconds string.
    pub fn upsert_worktree(
        &self,
        info: &WorktreeInfo,
        provider: &str,
        created_at_unix: u64,
    ) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO taime_worktrees \
               (terminal_id, project_root, repo_root, worktree_path, branch, base_sha, mode, provider, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
             ON CONFLICT(terminal_id) DO UPDATE SET \
               worktree_path = excluded.worktree_path, branch = excluded.branch, \
               base_sha = excluded.base_sha, mode = excluded.mode",
            rusqlite::params![
                info.terminal_key,
                info.project_root,
                info.repo_root,
                info.worktree_path,
                info.branch,
                info.base_sha,
                info.mode,
                provider,
                created_at_unix.to_string(),
            ],
        )?;
        Ok(())
    }

    /// All recorded sessions, newest first (history / detached-panel backfill).
    #[allow(dead_code)] // consumed by the Phase-6 history/route layer.
    pub fn list_sessions(&self) -> rusqlite::Result<Vec<SessionRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT pty_session_id, provider, attribution_key, cwd, program, created_at_unix, status \
             FROM daemon_sessions ORDER BY created_at_unix DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(SessionRow {
                    pty_session_id: r.get(0)?,
                    provider: r.get(1)?,
                    attribution_key: r.get(2)?,
                    cwd: r.get(3)?,
                    program: r.get(4)?,
                    created_at_unix: r.get::<_, i64>(5)? as u64,
                    status: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

/// Schema: CAO's tables verbatim (for the Phase-7 import) + daemon-native tables.
/// `IF NOT EXISTS` everywhere so `migrate()` is idempotent across daemon restarts.
const SCHEMA: &str = r#"
-- ---- CAO-mirrored (clients/database.py) — populated by --import-cao + the
-- ---- daemon's attribution writes (Phase 5/6) ----
CREATE TABLE IF NOT EXISTS terminals (
    id TEXT PRIMARY KEY,
    tmux_session TEXT NOT NULL,
    tmux_window TEXT NOT NULL,
    provider TEXT NOT NULL,
    agent_profile TEXT,
    allowed_tools TEXT,
    shell_command TEXT,
    last_active TEXT
);
CREATE TABLE IF NOT EXISTS inbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    sender_id TEXT NOT NULL,
    receiver_id TEXT NOT NULL,
    message TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TEXT
);
CREATE TABLE IF NOT EXISTS memory_metadata (
    id TEXT PRIMARY KEY,
    key TEXT NOT NULL,
    memory_type TEXT NOT NULL,
    scope TEXT NOT NULL,
    scope_id TEXT,
    file_path TEXT NOT NULL,
    tags TEXT NOT NULL DEFAULT '',
    source_provider TEXT,
    source_terminal_id TEXT,
    token_estimate INTEGER,
    created_at TEXT,
    updated_at TEXT,
    UNIQUE(key, scope, scope_id)
);
CREATE TABLE IF NOT EXISTS flows (
    name TEXT PRIMARY KEY,
    file_path TEXT NOT NULL,
    schedule TEXT NOT NULL,
    agent_profile TEXT NOT NULL,
    provider TEXT NOT NULL,
    script TEXT,
    last_run TEXT,
    next_run TEXT,
    enabled INTEGER DEFAULT 1
);
CREATE TABLE IF NOT EXISTS taime_worktrees (
    terminal_id TEXT PRIMARY KEY,
    session_name TEXT,
    project_root TEXT NOT NULL,
    repo_root TEXT,
    worktree_path TEXT NOT NULL,
    branch TEXT,
    base_sha TEXT,
    mode TEXT NOT NULL DEFAULT 'shared',
    provider TEXT,
    member_of TEXT,
    created_at TEXT
);
CREATE TABLE IF NOT EXISTS taime_agent_turns (
    id TEXT PRIMARY KEY,
    terminal_id TEXT NOT NULL,
    session_name TEXT,
    turn_index INTEGER NOT NULL DEFAULT 0,
    started_at TEXT,
    ended_at TEXT,
    start_snapshot TEXT,
    end_snapshot TEXT,
    files_touched TEXT
);
CREATE TABLE IF NOT EXISTS taime_activity_events (
    id TEXT PRIMARY KEY,
    ts TEXT,
    kind TEXT NOT NULL,
    terminal_id TEXT,
    session_name TEXT,
    agent_profile TEXT,
    provider TEXT,
    target_terminal_id TEXT,
    path TEXT,
    change_kind TEXT,
    turn_id TEXT,
    snapshot_sha TEXT,
    meta TEXT
);
CREATE INDEX IF NOT EXISTS idx_memory_scope ON memory_metadata(scope, scope_id);
CREATE INDEX IF NOT EXISTS idx_memory_updated ON memory_metadata(updated_at);
CREATE INDEX IF NOT EXISTS idx_memory_type ON memory_metadata(memory_type);
CREATE INDEX IF NOT EXISTS idx_inbox_receiver ON inbox(receiver_id, status);

-- ---- daemon-native ----
CREATE TABLE IF NOT EXISTS daemon_sessions (
    pty_session_id TEXT PRIMARY KEY,
    provider TEXT,
    attribution_key TEXT,
    cwd TEXT,
    program TEXT NOT NULL,
    created_at_unix INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'running'
);
CREATE TABLE IF NOT EXISTS taime_meta (
    key TEXT PRIMARY KEY,
    value TEXT
);
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str) -> SessionRow {
        SessionRow {
            pty_session_id: id.into(),
            provider: Some("claude_code".into()),
            attribution_key: Some("term-1".into()),
            cwd: Some("/tmp/wt".into()),
            program: "claude".into(),
            created_at_unix: 1000,
            status: "running".into(),
        }
    }

    #[test]
    fn schema_creates_all_cao_tables() {
        let store = Store::open_at(std::path::Path::new(":memory:")).unwrap();
        let conn = store.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap();
        let tables: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for t in [
            "terminals",
            "inbox",
            "memory_metadata",
            "flows",
            "taime_worktrees",
            "taime_agent_turns",
            "taime_activity_events",
            "daemon_sessions",
            "taime_meta",
        ] {
            assert!(tables.contains(&t.to_string()), "missing table {t}");
        }
    }

    #[test]
    fn record_then_list_and_mark_exited() {
        let store = Store::open_at(std::path::Path::new(":memory:")).unwrap();
        store.record_session(&row("pty-0")).unwrap();
        store.record_session(&row("pty-1")).unwrap();
        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(sessions.iter().all(|s| s.status == "running"));

        store.set_session_status("pty-0", "exited").unwrap();
        let after = store.list_sessions().unwrap();
        let p0 = after.iter().find(|s| s.pty_session_id == "pty-0").unwrap();
        assert_eq!(p0.status, "exited");
        assert_eq!(p0.provider.as_deref(), Some("claude_code"));
        assert_eq!(p0.attribution_key.as_deref(), Some("term-1"));
    }

    #[test]
    fn migrate_is_idempotent() {
        let store = Store::open_at(std::path::Path::new(":memory:")).unwrap();
        // Re-running migrate must not error (IF NOT EXISTS).
        store.migrate().unwrap();
        store.record_session(&row("pty-0")).unwrap();
        // Re-record same id updates status, doesn't duplicate.
        let mut r = row("pty-0");
        r.status = "exited".into();
        store.record_session(&r).unwrap();
        assert_eq!(store.list_sessions().unwrap().len(), 1);
    }
}
