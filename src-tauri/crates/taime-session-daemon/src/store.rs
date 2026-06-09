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

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension};
use taime_protocol::WorktreeInfo;

/// The CAO tables to import, each with its explicit column list (robust against
/// schema-order drift between CAO's SQLAlchemy DDL and our `CREATE TABLE`).
const IMPORT_TABLES: &[(&str, &str)] = &[
    ("terminals", "id, tmux_session, tmux_window, provider, agent_profile, allowed_tools, shell_command, last_active"),
    ("inbox", "id, sender_id, receiver_id, message, status, created_at"),
    ("memory_metadata", "id, key, memory_type, scope, scope_id, file_path, tags, source_provider, source_terminal_id, token_estimate, created_at, updated_at"),
    ("flows", "name, file_path, schedule, agent_profile, provider, script, last_run, next_run, enabled"),
    ("taime_worktrees", "terminal_id, session_name, project_root, repo_root, worktree_path, branch, base_sha, mode, provider, member_of, created_at"),
    ("taime_agent_turns", "id, terminal_id, session_name, turn_index, started_at, ended_at, start_snapshot, end_snapshot, files_touched"),
    ("taime_activity_events", "id, ts, kind, terminal_id, session_name, agent_profile, provider, target_terminal_id, path, change_kind, turn_id, snapshot_sha, meta"),
];

/// Result of a `--import-cao` run.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ImportStats {
    /// True if the import ran; false if it was already done (idempotent no-op).
    pub ran: bool,
    /// Rows copied per table (only tables present in the CAO db).
    pub copied: Vec<(String, usize)>,
}

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

/// A persisted inbox message (Phase 5). `created_at` is a unix-seconds string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxMessage {
    pub id: i64,
    pub sender_id: String,
    pub receiver_id: String,
    pub message: String,
    pub status: String,
    pub created_at: String,
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

/// A full `taime_worktrees` row — the complete worktree surface for the app's
/// `getWorktree` read (mirrors the protocol `WorktreeInfo` fields).
#[derive(Debug, Clone)]
pub struct WorktreeRow {
    pub terminal_id: String,
    pub project_root: Option<String>,
    pub repo_root: Option<String>,
    pub worktree_path: String,
    pub branch: Option<String>,
    pub base_sha: Option<String>,
    pub mode: Option<String>,
    pub provider: Option<String>,
    pub member_of: Option<String>,
    /// Task membership (`None` ⇒ Uncategorized). The durable Task anchor.
    pub task_id: Option<String>,
}

/// A schedule row (the `flows` table; user-facing term is "Schedule").
#[derive(Debug, Clone)]
pub struct ScheduleRow {
    pub name: String,
    pub file_path: String,
    pub schedule: String,
    pub agent_profile: String,
    pub provider: String,
    pub script: Option<String>,
    pub prompt: Option<String>,
    pub last_run: Option<u64>,
    pub next_run: Option<u64>,
    pub enabled: bool,
    /// Workspace the fire runs in (cwd + worktree root). `None` = daemon cwd,
    /// no task targeting possible (tasks are workspace-scoped).
    pub workspace_root: Option<String>,
    /// Explicit task behavior: `None`/"" = uncategorized, "fixed" = attach to
    /// `task_id`, "per_run" = create a fresh task per fire (explicit opt-in).
    pub task_mode: Option<String>,
    pub task_id: Option<String>,
}

/// A workflow run row.
#[derive(Debug, Clone)]
pub struct WorkflowRunRow {
    pub id: String,
    pub workflow_name: String,
    pub status: String,
    pub started_at: Option<u64>,
    pub ended_at: Option<u64>,
    pub error: Option<String>,
    /// Task this run executes inside (`None` ⇒ Uncategorized). Node agents
    /// inherit it at provision.
    pub task_id: Option<String>,
}

/// A Task row — a named, workspace-scoped unit of user intent. Owns grouping,
/// lifecycle, and review aggregation; never raw attribution (Agent-ID anchored).
#[derive(Debug, Clone)]
pub struct TaskRow {
    pub id: String,
    pub workspace_root: String,
    pub title: String,
    pub description: String,
    /// `open` | `in_review` | `done` | `archived` (validated by the manager).
    pub status: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub archived_at: Option<u64>,
}

/// The latest state of one node within a run.
#[derive(Debug, Clone)]
pub struct NodeState {
    pub node_id: String,
    pub status: String,
    pub iteration: u32,
    pub agent_key: Option<String>,
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
        conn.execute_batch(SCHEMA)?;
        // Additive column migrations (ALTER … ADD COLUMN is not IF-NOT-EXISTS, so
        // ignore the "duplicate column" error to stay idempotent across versions).
        // `prompt` backs Schedules created in-app (the .md body, stored inline).
        let _ = conn.execute("ALTER TABLE flows ADD COLUMN prompt TEXT", []);
        // Tasks (v9): membership is a nullable task_id on the durable records —
        // the worktree row (agents) and the workflow run. NULL ⇒ Uncategorized.
        let _ = conn.execute("ALTER TABLE taime_worktrees ADD COLUMN task_id TEXT", []);
        let _ = conn.execute("ALTER TABLE taime_workflow_runs ADD COLUMN task_id TEXT", []);
        // Schedules gain a workspace target (prerequisite for task-scoped fires)
        // and an explicit task behavior: task_mode ∈ NULL/'' (uncategorized) |
        // 'fixed' (attach to task_id) | 'per_run' (create a task per fire).
        let _ = conn.execute("ALTER TABLE flows ADD COLUMN workspace_root TEXT", []);
        let _ = conn.execute("ALTER TABLE flows ADD COLUMN task_mode TEXT", []);
        let _ = conn.execute("ALTER TABLE flows ADD COLUMN task_id TEXT", []);
        // This index references the ALTER-added column, so it must run AFTER the
        // ALTERs (a pre-existing DB's SCHEMA batch ran before task_id existed).
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_worktrees_task ON taime_worktrees(task_id)",
            [],
        )?;
        // Wave-1 lexicon (v10): the worktree mode named after its own object
        // becomes `isolated` (`shared` unchanged). One-time rewrite here, plus
        // read-normalization in [`normalize_mode`] (belt and braces — e.g. a
        // post-open `--import-cao` can still land legacy rows).
        conn.execute(
            "UPDATE taime_worktrees SET mode = 'isolated' WHERE mode = 'worktree'",
            [],
        )?;
        // A freshly-started daemon owns NO live sessions yet, and it can't
        // re-attach to a previous daemon's PTY children (the master fd died with
        // it). So any `running` rows are stale orphans from a prior process: mark
        // them exited at startup so the UI shows them as ended + reviewable, not
        // phantom "running". New spawns set `running` AFTER this sweep.
        conn.execute(
            "UPDATE daemon_sessions SET status = 'exited' WHERE status = 'running'",
            [],
        )?;
        Ok(())
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

    /// Persist (or update) a provisioned worktree row, keyed by the Agent ID
    /// (the `terminal_id` column — DB name kept for import compatibility).
    /// Mirrors CAO's `taime_worktrees` upsert so the Phase-7 import is a row
    /// copy. `created_at` stored as a unix-seconds string.
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
               base_sha = excluded.base_sha, mode = excluded.mode, \
               created_at = excluded.created_at",
            rusqlite::params![
                info.agent_id,
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

    // ---- Inbox (Phase 5): persisted-by-default mailbox, FIFO per receiver ----

    /// Enqueue a message for `receiver_id` (status `pending`). Returns the
    /// monotonic message id. The daemon stamps `sender_id` (never a client field).
    pub fn enqueue_message(
        &self,
        sender_id: &str,
        receiver_id: &str,
        message: &str,
        created_at_unix: u64,
    ) -> rusqlite::Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO inbox (sender_id, receiver_id, message, status, created_at) \
             VALUES (?1, ?2, ?3, 'pending', ?4)",
            rusqlite::params![sender_id, receiver_id, message, created_at_unix.to_string()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// The distinct receivers that have at least one `pending` message (the
    /// delivery engine's work-list).
    pub fn receivers_with_pending(&self) -> rusqlite::Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT DISTINCT receiver_id FROM inbox WHERE status = 'pending'")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The oldest `pending` messages for `receiver_id` (FIFO by id), up to `limit`.
    pub fn pending_for(&self, receiver_id: &str, limit: i64) -> rusqlite::Result<Vec<InboxMessage>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, sender_id, receiver_id, message, status, created_at \
             FROM inbox WHERE receiver_id = ?1 AND status = 'pending' \
             ORDER BY id ASC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![receiver_id, limit], |r| {
                Ok(InboxMessage {
                    id: r.get(0)?,
                    sender_id: r.get(1)?,
                    receiver_id: r.get(2)?,
                    message: r.get(3)?,
                    status: r.get(4)?,
                    created_at: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Transition a message's status (`delivered` / `failed`). The
    /// `pending → delivered` gate makes delivery idempotent across a restart.
    pub fn set_message_status(&self, id: i64, status: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE inbox SET status = ?2 WHERE id = ?1",
            rusqlite::params![id, status],
        )?;
        Ok(())
    }

    /// One-time, idempotent import of a CAO SQLite db into this store (Phase 7).
    /// Re-runs no-op (a `taime_meta.migrated_from_cao` marker gates it). Per
    /// table: `INSERT OR IGNORE INTO <t> (cols) SELECT cols FROM cao.<t>`, with
    /// explicit columns; a missing/incompatible CAO table is skipped, not fatal.
    pub fn import_cao(&self, cao_path: &Path) -> rusqlite::Result<ImportStats> {
        let conn = self.conn.lock().unwrap();
        let already: Option<String> = conn
            .query_row(
                "SELECT value FROM taime_meta WHERE key = 'migrated_from_cao'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if already.is_some() {
            return Ok(ImportStats { ran: false, copied: Vec::new() });
        }
        conn.execute("ATTACH DATABASE ?1 AS cao", rusqlite::params![cao_path.to_string_lossy()])?;
        let mut copied = Vec::new();
        for (table, cols) in IMPORT_TABLES {
            // Skip tables absent from the CAO db.
            let exists: Option<String> = conn
                .query_row(
                    "SELECT name FROM cao.sqlite_master WHERE type='table' AND name = ?1",
                    rusqlite::params![table],
                    |r| r.get(0),
                )
                .optional()
                .unwrap_or(None);
            if exists.is_none() {
                continue;
            }
            let sql = format!("INSERT OR IGNORE INTO {table} ({cols}) SELECT {cols} FROM cao.{table}");
            match conn.execute(&sql, []) {
                Ok(n) => copied.push((table.to_string(), n)),
                Err(e) => eprintln!("[taime-daemon] import: skipped {table}: {e}"),
            }
        }
        conn.execute("DETACH DATABASE cao", [])?;
        conn.execute(
            "INSERT OR REPLACE INTO taime_meta (key, value) VALUES ('migrated_from_cao', ?1)",
            rusqlite::params![cao_path.to_string_lossy()],
        )?;
        Ok(ImportStats { ran: true, copied })
    }

    /// Record an inter-agent edge in the activity graph (`kind` ∈
    /// message|request|reply|handoff|assign): `source` → `target` by attribution
    /// key (the `taime_activity_events` row with a `target_terminal_id`).
    pub fn record_activity_edge(
        &self,
        id: &str,
        kind: &str,
        source: &str,
        target: &str,
        ts_unix: u64,
    ) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO taime_activity_events (id, ts, kind, terminal_id, target_terminal_id) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, ts_unix.to_string(), kind, source, target],
        )?;
        Ok(())
    }

    /// Record one attributed filesystem change (`kind = "fs"`) for an agent's
    /// terminal — the durable per-file activity timeline (Phase 6, daemon-owned
    /// so it accrues even with the app closed).
    pub fn record_fs_event(
        &self,
        id: &str,
        terminal_id: &str,
        path: &str,
        change_kind: &str,
        ts_unix: u64,
    ) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO taime_activity_events (id, ts, kind, terminal_id, path, change_kind) \
             VALUES (?1, ?2, 'fs', ?3, ?4, ?5)",
            rusqlite::params![id, ts_unix.to_string(), terminal_id, path, change_kind],
        )?;
        Ok(())
    }

    /// Per-path last-touch timestamp for one terminal — `(path, max_ts)`. Used to
    /// pick the "last contributor" per file in the attribution surface.
    pub fn fs_path_touches(&self, terminal_id: &str) -> rusqlite::Result<Vec<(String, u64)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT path, MAX(CAST(ts AS INTEGER)) FROM taime_activity_events \
             WHERE kind = 'fs' AND terminal_id = ?1 AND path IS NOT NULL GROUP BY path",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![terminal_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Files touched (fs events) by ≥2 distinct terminals — cross-agent contention,
    /// computed from the durable activity log (no git). `(path, [terminal_id…])`.
    pub fn fs_contention(&self, limit: usize) -> rusqlite::Result<Vec<(String, Vec<String>)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT path, GROUP_CONCAT(DISTINCT terminal_id) FROM taime_activity_events \
             WHERE kind = 'fs' AND path IS NOT NULL AND terminal_id IS NOT NULL \
             GROUP BY path HAVING COUNT(DISTINCT terminal_id) > 1 LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![limit as i64], |r| {
                let path: String = r.get(0)?;
                let terms: String = r.get(1)?;
                Ok((path, terms.split(',').map(String::from).collect::<Vec<_>>()))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ---- Phase-6 turn persistence ----

    /// Persist a closed attribution turn (durable so the flagship turn substrate
    /// survives app/daemon restarts). `files_touched` is stored as a JSON array.
    pub fn record_turn(
        &self,
        id: &str,
        terminal_id: &str,
        turn_index: u64,
        started_at_unix: u64,
        ended_at_unix: u64,
        files_touched: &[String],
    ) -> rusqlite::Result<()> {
        let files_json = serde_json::to_string(files_touched).unwrap_or_else(|_| "[]".to_string());
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO taime_agent_turns \
             (id, terminal_id, turn_index, started_at, ended_at, files_touched) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                id,
                terminal_id,
                turn_index as i64,
                started_at_unix.to_string(),
                ended_at_unix.to_string(),
                files_json
            ],
        )?;
        Ok(())
    }

    /// An agent's persisted turns, newest first: `(id, turn_index, started_at,
    /// ended_at, files_touched)` where files is the raw JSON-array string.
    #[allow(clippy::type_complexity)]
    pub fn agent_turns(
        &self,
        terminal_id: &str,
        limit: usize,
    ) -> rusqlite::Result<Vec<(String, u64, Option<String>, Option<String>, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, turn_index, started_at, ended_at, COALESCE(files_touched, '[]') \
             FROM taime_agent_turns WHERE terminal_id = ?1 ORDER BY turn_index DESC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![terminal_id, limit as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)? as u64,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Agents for the activity graph: `(attribution_key, provider, status)` from
    /// the recorded sessions (Phase 6). Sessions without an attribution key are
    /// keyed by their pty id.
    pub fn graph_agents(&self) -> rusqlite::Result<Vec<(String, Option<String>, String)>> {
        // Cap to the most recent agents so the team view shows the current/recent
        // team (live agents are newest, so they're always included) rather than
        // every agent ever spawned — the table persists across runs.
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT COALESCE(attribution_key, pty_session_id), provider, status \
             FROM daemon_sessions ORDER BY created_at_unix DESC LIMIT 40",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Inter-agent edges for the activity graph: `(kind, source, target)` from the
    /// recorded message/request/reply/handoff/assign events (Phase 6), oldest
    /// first. Bounded to the most recent window — this runs on every graph
    /// fetch, and the events table grows with normal use (one row per fs event
    /// per agent); an unbounded scan would degrade into multi-second queries.
    pub fn activity_edges(&self) -> rusqlite::Result<Vec<(String, String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT kind, terminal_id, target_terminal_id FROM ( \
               SELECT kind, terminal_id, target_terminal_id, ts \
               FROM taime_activity_events WHERE target_terminal_id IS NOT NULL \
               ORDER BY CAST(ts AS INTEGER) DESC LIMIT 2000 \
             ) ORDER BY CAST(ts AS INTEGER) ASC",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// GC-sweep candidates: ISOLATED worktree rows old enough to be safely
    /// considered (created before `cutoff_unix`). The age gate closes the
    /// provision→spawn TOCTOU — a row is persisted one IPC round-trip before
    /// its agent registers as a live session, and a sweep landing in that gap
    /// would see a fresh, clean, at-base checkout with no live agent and
    /// delete the cwd the agent is about to spawn into. Legacy rows with NULL
    /// created_at are treated as old.
    pub fn gc_candidate_worktrees(&self, cutoff_unix: u64) -> rusqlite::Result<Vec<WorktreeRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {WORKTREE_COLS} FROM taime_worktrees \
             WHERE mode IN ('isolated', 'worktree') \
               AND (created_at IS NULL OR CAST(created_at AS INTEGER) <= ?1)"
        ))?;
        let rows = stmt
            .query_map(rusqlite::params![cutoff_unix as i64], map_worktree)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Whether `key` is the agent of a currently-running workflow node run
    /// (the MCP recursion gate for `run_workflow`).
    pub fn is_active_node_agent(&self, key: &str) -> bool {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM taime_workflow_node_runs \
             WHERE agent_key = ?1 AND status = 'running'",
            rusqlite::params![key],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    /// Retention pruning for the append-only history tables (startup + daily).
    /// Windows: activity events + interactions + delivered inbox 30d; turns 90d
    /// (turns are the attribution substrate — keep the longest useful window).
    /// Worktree rows are NEVER pruned here (durable Agent-ID anchors).
    pub fn prune_history(&self, now_unix: u64) -> rusqlite::Result<usize> {
        let conn = self.conn.lock().unwrap();
        let d30 = now_unix.saturating_sub(30 * 86_400);
        let d90 = now_unix.saturating_sub(90 * 86_400);
        let mut n = conn.execute(
            "DELETE FROM taime_activity_events WHERE CAST(ts AS INTEGER) < ?1",
            rusqlite::params![d30 as i64],
        )?;
        n += conn.execute(
            "DELETE FROM taime_agent_turns WHERE CAST(ended_at AS INTEGER) < ?1",
            rusqlite::params![d90 as i64],
        )?;
        n += conn.execute(
            "DELETE FROM taime_interactions WHERE created_at < ?1",
            rusqlite::params![d30 as i64],
        )?;
        // Pending messages are never pruned — only consumed/failed ones age out.
        n += conn.execute(
            "DELETE FROM inbox WHERE status != 'pending' AND CAST(created_at AS INTEGER) < ?1",
            rusqlite::params![d30 as i64],
        )?;
        Ok(n)
    }

    /// The worktree `(path, base_sha, mode)` for an Agent ID — the daemon
    /// diff's context (Phase 6). `base_sha` is the fork point for an isolated
    /// worktree (diff against it shows all the agent's changes).
    pub fn worktree(
        &self,
        agent_id: &str,
    ) -> rusqlite::Result<Option<(String, Option<String>, String)>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT worktree_path, base_sha, mode FROM taime_worktrees WHERE terminal_id = ?1",
            rusqlite::params![agent_id],
            |r| Ok((r.get(0)?, r.get(1)?, normalize_mode_str(r.get(2)?))),
        )
        .optional()
    }

    /// The full worktree row for a terminal — the complete `WorktreeInfo`-shaped
    /// surface the app's `getWorktree` reads (branch chip, repo root, etc.).
    pub fn worktree_row(&self, terminal_id: &str) -> rusqlite::Result<Option<WorktreeRow>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            &format!("SELECT {WORKTREE_COLS} FROM taime_worktrees WHERE terminal_id = ?1"),
            rusqlite::params![terminal_id],
            map_worktree,
        )
        .optional()
    }

    // ---- Tasks (v9): workspace-scoped intent grouping over agents + runs ----

    /// Create a task (status `open`). The manager mints the id.
    pub fn create_task(
        &self,
        id: &str,
        workspace_root: &str,
        title: &str,
        description: &str,
        now_unix: u64,
    ) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO taime_tasks (id, workspace_root, title, description, status, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, 'open', ?5, ?5)",
            rusqlite::params![id, workspace_root, title, description, now_unix as i64],
        )?;
        Ok(())
    }

    pub fn get_task(&self, id: &str) -> rusqlite::Result<Option<TaskRow>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            &format!("SELECT {TASK_COLS} FROM taime_tasks WHERE id = ?1"),
            rusqlite::params![id],
            map_task,
        )
        .optional()
    }

    /// Tasks for a workspace, newest first. `include_archived=false` hides
    /// archived tasks (the default list view).
    pub fn list_tasks(
        &self,
        workspace_root: &str,
        include_archived: bool,
    ) -> rusqlite::Result<Vec<TaskRow>> {
        let conn = self.conn.lock().unwrap();
        let sql = if include_archived {
            format!(
                "SELECT {TASK_COLS} FROM taime_tasks WHERE workspace_root = ?1 \
                 ORDER BY created_at DESC"
            )
        } else {
            format!(
                "SELECT {TASK_COLS} FROM taime_tasks \
                 WHERE workspace_root = ?1 AND status != 'archived' \
                 ORDER BY created_at DESC"
            )
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params![workspace_root], map_task)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Update title/description/status (each optional). Status `archived` stamps
    /// `archived_at`; leaving `archived` clears it. `updated_at` always bumps.
    pub fn update_task(
        &self,
        id: &str,
        title: Option<&str>,
        description: Option<&str>,
        status: Option<&str>,
        now_unix: u64,
    ) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE taime_tasks SET \
               title = COALESCE(?2, title), \
               description = COALESCE(?3, description), \
               status = COALESCE(?4, status), \
               archived_at = CASE \
                 WHEN ?4 = 'archived' THEN ?5 \
                 WHEN ?4 IS NOT NULL THEN NULL \
                 ELSE archived_at END, \
               updated_at = ?5 \
             WHERE id = ?1",
            rusqlite::params![id, title, description, status, now_unix as i64],
        )?;
        Ok(n > 0)
    }

    /// Delete a task, demoting members to Uncategorized first (never touches
    /// runtimes, worktrees, or attribution — the partition rule).
    pub fn delete_task(&self, id: &str) -> rusqlite::Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE taime_worktrees SET task_id = NULL WHERE task_id = ?1",
            rusqlite::params![id],
        )?;
        tx.execute(
            "UPDATE taime_workflow_runs SET task_id = NULL WHERE task_id = ?1",
            rusqlite::params![id],
        )?;
        tx.execute("DELETE FROM taime_tasks WHERE id = ?1", rusqlite::params![id])?;
        tx.commit()
    }

    /// Assign (or unassign with `None`) an agent's worktree row to a task.
    pub fn set_worktree_task(
        &self,
        terminal_id: &str,
        task_id: Option<&str>,
    ) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE taime_worktrees SET task_id = ?2 WHERE terminal_id = ?1",
            rusqlite::params![terminal_id, task_id],
        )?;
        Ok(n > 0)
    }

    /// An agent's task membership (`None` ⇒ Uncategorized or unknown agent).
    pub fn task_of_worktree(&self, terminal_id: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT task_id FROM taime_worktrees WHERE terminal_id = ?1",
            rusqlite::params![terminal_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
    }

    /// Member agents (worktree rows) of a task, newest first.
    pub fn agents_for_task(&self, task_id: &str) -> rusqlite::Result<Vec<WorktreeRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {WORKTREE_COLS} FROM taime_worktrees WHERE task_id = ?1 \
             ORDER BY created_at DESC"
        ))?;
        let rows = stmt
            .query_map(rusqlite::params![task_id], map_worktree)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Workflow runs attached to a task, newest first.
    pub fn runs_for_task(&self, task_id: &str) -> rusqlite::Result<Vec<WorkflowRunRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {RUN_COLS} FROM taime_workflow_runs WHERE task_id = ?1 \
             ORDER BY started_at DESC"
        ))?;
        let rows =
            stmt.query_map(rusqlite::params![task_id], map_run)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Member-agent counts per task for one workspace (the task-list rollup),
    /// as `(task_id, agent_count)` pairs.
    pub fn task_agent_counts(&self, workspace_root: &str) -> rusqlite::Result<Vec<(String, i64)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT w.task_id, COUNT(*) FROM taime_worktrees w \
             JOIN taime_tasks t ON t.id = w.task_id \
             WHERE t.workspace_root = ?1 GROUP BY w.task_id",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![workspace_root], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// An agent's worktree attribution attributes: `(branch, mode, member_of)`,
    /// for the activity-graph + attribution surfaces.
    #[allow(clippy::type_complexity)]
    pub fn worktree_attrs(
        &self,
        agent_id: &str,
    ) -> rusqlite::Result<Option<(Option<String>, Option<String>, Option<String>)>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT branch, mode, member_of FROM taime_worktrees WHERE terminal_id = ?1",
            rusqlite::params![agent_id],
            |r| Ok((r.get(0)?, normalize_mode(r.get(1)?), r.get(2)?)),
        )
        .optional()
    }

    /// The workspace (`project_root`) an agent's worktree was cut from — the
    /// workspace grouping key for the `agents` surface.
    pub fn worktree_project_root(&self, agent_id: &str) -> rusqlite::Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT project_root FROM taime_worktrees WHERE terminal_id = ?1",
            rusqlite::params![agent_id],
            |r| r.get(0),
        )
        .optional()
    }

    // ---- Workspace teardown (the "delete workspace" flow) ----

    /// Every worktree row provisioned from `workspace_root` (all agents in the
    /// workspace, regardless of task membership) — the teardown set.
    pub fn worktrees_in_workspace(&self, workspace_root: &str) -> rusqlite::Result<Vec<WorktreeRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {WORKTREE_COLS} FROM taime_worktrees WHERE project_root = ?1"
        ))?;
        let rows = stmt
            .query_map(rusqlite::params![workspace_root], map_worktree)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Delete one worktree row (its agent is being torn down with the workspace).
    pub fn delete_worktree_row(&self, agent_id: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM taime_worktrees WHERE terminal_id = ?1",
            rusqlite::params![agent_id],
        )?;
        Ok(())
    }

    /// Delete every task in a workspace, detaching any workflow runs that pointed
    /// at them first. Returns the number of tasks deleted.
    pub fn delete_tasks_for_workspace(&self, workspace_root: &str) -> rusqlite::Result<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE taime_workflow_runs SET task_id = NULL WHERE task_id IN \
             (SELECT id FROM taime_tasks WHERE workspace_root = ?1)",
            rusqlite::params![workspace_root],
        )?;
        let n = tx.execute(
            "DELETE FROM taime_tasks WHERE workspace_root = ?1",
            rusqlite::params![workspace_root],
        )?;
        tx.commit()?;
        Ok(n)
    }

    // ---- Durable review acknowledgments (the flagship safe-context-switch
    // ---- guard's state, persisted so it survives a UI/daemon restart) ----

    /// Mark an agent's current changes acknowledged (idempotent upsert).
    pub fn mark_reviewed(&self, agent_id: &str, now_unix: u64) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO taime_reviews (agent_id, reviewed_at) VALUES (?1, ?2) \
             ON CONFLICT(agent_id) DO UPDATE SET reviewed_at = excluded.reviewed_at",
            rusqlite::params![agent_id, now_unix as i64],
        )?;
        Ok(())
    }

    /// Drop an agent's review ack — a fresh review cycle (its dirty set was
    /// reset), so the next change re-raises the guard.
    pub fn clear_reviewed(&self, agent_id: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM taime_reviews WHERE agent_id = ?1", rusqlite::params![agent_id])?;
        Ok(())
    }

    /// Every agent id with a standing review ack — hydrates the UI guard on boot
    /// so acknowledgments aren't lost across a restart.
    pub fn reviewed_agents(&self) -> rusqlite::Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT agent_id FROM taime_reviews")?;
        let rows =
            stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
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

    // ---- Phase-5 blackboard (shared scratchpad) ----

    /// Upsert a blackboard entry (last writer wins).
    pub fn blackboard_set(
        &self,
        key: &str,
        value: &str,
        author: &str,
        updated_at_unix: u64,
    ) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO taime_blackboard (key, value, author, updated_at) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, author = excluded.author, \
             updated_at = excluded.updated_at",
            rusqlite::params![key, value, author, updated_at_unix as i64],
        )?;
        Ok(())
    }

    /// Read a blackboard entry: `(value, author, updated_at_unix)`, or None.
    pub fn blackboard_get(
        &self,
        key: &str,
    ) -> rusqlite::Result<Option<(String, Option<String>, u64)>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT value, author, updated_at FROM taime_blackboard WHERE key = ?1",
            rusqlite::params![key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as u64)),
        )
        .optional()
    }

    // ---- Phase-5 request/reply correlation ----

    /// Record an open interaction (a `request` awaiting a `reply`).
    pub fn interaction_open(
        &self,
        interaction_id: &str,
        requester: &str,
        responder: &str,
        body: &str,
        created_at_unix: u64,
    ) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO taime_interactions \
             (interaction_id, requester, responder, body, reply, status, created_at) \
             VALUES (?1, ?2, ?3, ?4, NULL, 'pending', ?5)",
            rusqlite::params![interaction_id, requester, responder, body, created_at_unix as i64],
        )?;
        Ok(())
    }

    /// An interaction's `(requester, responder, status)`, or None if unknown.
    pub fn interaction_parties(
        &self,
        interaction_id: &str,
    ) -> rusqlite::Result<Option<(String, String, String)>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT requester, responder, status FROM taime_interactions WHERE interaction_id = ?1",
            rusqlite::params![interaction_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
    }

    /// Close an interaction with the responder's reply.
    pub fn interaction_answer(&self, interaction_id: &str, reply: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE taime_interactions SET reply = ?2, status = 'answered' WHERE interaction_id = ?1",
            rusqlite::params![interaction_id, reply],
        )?;
        Ok(())
    }

    // ---- Schedules (cron-triggered unattended agent runs; `flows` table) ----

    /// Create or replace a schedule (resets last_run; next_run is recomputed).
    pub fn upsert_schedule(&self, row: &ScheduleRow) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO flows \
             (name, file_path, schedule, agent_profile, provider, script, last_run, next_run, \
              enabled, prompt, workspace_root, task_mode, task_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![
                row.name,
                row.file_path,
                row.schedule,
                row.agent_profile,
                row.provider,
                row.script,
                row.last_run.map(|v| v.to_string()),
                row.next_run.map(|v| v.to_string()),
                row.enabled as i64,
                row.prompt,
                row.workspace_root,
                row.task_mode,
                row.task_id,
            ],
        )?;
        Ok(())
    }

    pub fn list_schedules(&self) -> rusqlite::Result<Vec<ScheduleRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!("SELECT {SCHEDULE_COLS} FROM flows ORDER BY name"))?;
        let rows = stmt.query_map([], map_schedule)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn get_schedule(&self, name: &str) -> rusqlite::Result<Option<ScheduleRow>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            &format!("SELECT {SCHEDULE_COLS} FROM flows WHERE name = ?1"),
            rusqlite::params![name],
            map_schedule,
        )
        .optional()
    }

    /// Enabled schedules whose `next_run` is due (≤ now). Powers the cron tick.
    pub fn due_schedules(&self, now_unix: u64) -> rusqlite::Result<Vec<ScheduleRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {SCHEDULE_COLS} FROM flows \
             WHERE enabled = 1 AND next_run IS NOT NULL AND CAST(next_run AS INTEGER) <= ?1"
        ))?;
        let rows = stmt
            .query_map(rusqlite::params![now_unix as i64], map_schedule)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn set_schedule_run(
        &self,
        name: &str,
        last_run_unix: u64,
        next_run_unix: Option<u64>,
    ) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE flows SET last_run = ?2, next_run = ?3 WHERE name = ?1",
            rusqlite::params![name, last_run_unix.to_string(), next_run_unix.map(|v| v.to_string())],
        )?;
        Ok(())
    }

    pub fn set_schedule_next(&self, name: &str, next_run_unix: Option<u64>) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE flows SET next_run = ?2 WHERE name = ?1",
            rusqlite::params![name, next_run_unix.map(|v| v.to_string())],
        )?;
        Ok(())
    }

    pub fn set_schedule_enabled(&self, name: &str, enabled: bool) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE flows SET enabled = ?2 WHERE name = ?1",
            rusqlite::params![name, enabled as i64],
        )?;
        Ok(())
    }

    pub fn delete_schedule(&self, name: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM flows WHERE name = ?1", rusqlite::params![name])?;
        Ok(())
    }

    /// Whether any schedule is enabled — the daemon stays alive (skips idle
    /// shutdown) while true, so cron schedules fire even with the app closed.
    pub fn has_enabled_schedules(&self) -> bool {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM flows WHERE enabled = 1", [], |r| r.get::<_, i64>(0))
            .map(|n: i64| n > 0)
            .unwrap_or(false)
    }

    // ---- Workflows (the loopable agent step-graph) ----

    pub fn upsert_workflow(
        &self,
        name: &str,
        file_path: Option<&str>,
        definition: &str,
        source: &str,
        created_at: u64,
    ) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO taime_workflows (name, file_path, definition, source, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![name, file_path, definition, source, created_at as i64],
        )?;
        Ok(())
    }

    /// `(definition_json, source, file_path)` for a workflow, or None.
    pub fn get_workflow(
        &self,
        name: &str,
    ) -> rusqlite::Result<Option<(String, String, Option<String>)>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT definition, COALESCE(source,'file'), file_path FROM taime_workflows WHERE name = ?1",
            rusqlite::params![name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
    }

    /// `(name, source, definition_json)` for every workflow.
    pub fn list_workflows(&self) -> rusqlite::Result<Vec<(String, String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT name, COALESCE(source,'file'), definition FROM taime_workflows ORDER BY name",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn delete_workflow(&self, name: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM taime_workflows WHERE name = ?1", rusqlite::params![name])?;
        Ok(())
    }

    pub fn create_run(
        &self,
        id: &str,
        workflow_name: &str,
        started_at: u64,
        task_id: Option<&str>,
    ) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO taime_workflow_runs (id, workflow_name, status, started_at, task_id) \
             VALUES (?1, ?2, 'running', ?3, ?4)",
            rusqlite::params![id, workflow_name, started_at as i64, task_id],
        )?;
        Ok(())
    }

    pub fn finish_run(
        &self,
        id: &str,
        status: &str,
        ended_at: u64,
        error: Option<&str>,
    ) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE taime_workflow_runs SET status = ?2, ended_at = ?3, error = ?4 WHERE id = ?1",
            rusqlite::params![id, status, ended_at as i64, error],
        )?;
        Ok(())
    }

    pub fn get_run(&self, id: &str) -> rusqlite::Result<Option<WorkflowRunRow>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            &format!("SELECT {RUN_COLS} FROM taime_workflow_runs WHERE id = ?1"),
            rusqlite::params![id],
            map_run,
        )
        .optional()
    }

    pub fn latest_run(&self, workflow_name: &str) -> rusqlite::Result<Option<WorkflowRunRow>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            &format!(
                "SELECT {RUN_COLS} FROM taime_workflow_runs \
                 WHERE workflow_name = ?1 ORDER BY started_at DESC LIMIT 1"
            ),
            rusqlite::params![workflow_name],
            map_run,
        )
        .optional()
    }

    /// Start a node-run row (status='running'); each loop iteration is a new row.
    pub fn insert_node_run(
        &self,
        id: &str,
        run_id: &str,
        node_id: &str,
        agent_key: Option<&str>,
        iteration: u32,
        started_at: u64,
    ) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO taime_workflow_node_runs \
             (id, run_id, node_id, agent_key, status, iteration, started_at) \
             VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?6)",
            rusqlite::params![id, run_id, node_id, agent_key, iteration as i64, started_at as i64],
        )?;
        Ok(())
    }

    pub fn finish_node_run(
        &self,
        id: &str,
        status: &str,
        output: Option<&str>,
        ended_at: u64,
    ) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE taime_workflow_node_runs SET status = ?2, output = ?3, ended_at = ?4 WHERE id = ?1",
            rusqlite::params![id, status, output, ended_at as i64],
        )?;
        Ok(())
    }

    /// How many times `node_id` has run in this run (the per-node loop guard).
    pub fn node_iteration_count(&self, run_id: &str, node_id: &str) -> u32 {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM taime_workflow_node_runs WHERE run_id = ?1 AND node_id = ?2",
            rusqlite::params![run_id, node_id],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n as u32)
        .unwrap_or(0)
    }

    /// The latest state of each node in a run (for the live graph).
    pub fn node_states(&self, run_id: &str) -> rusqlite::Result<Vec<NodeState>> {
        let conn = self.conn.lock().unwrap();
        // The row with the greatest started_at per node_id.
        let mut stmt = conn.prepare(
            "SELECT node_id, status, iteration, agent_key FROM taime_workflow_node_runs n \
             WHERE started_at = (SELECT MAX(started_at) FROM taime_workflow_node_runs \
                                 WHERE run_id = n.run_id AND node_id = n.node_id) \
             AND run_id = ?1",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![run_id], |r| {
                Ok(NodeState {
                    node_id: r.get(0)?,
                    status: r.get(1)?,
                    iteration: r.get::<_, i64>(2)? as u32,
                    agent_key: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

/// Column list for a `WorkflowRunRow` SELECT (kept in sync with [`map_run`]).
const RUN_COLS: &str = "id, workflow_name, status, started_at, ended_at, error, task_id";

fn map_run(r: &rusqlite::Row) -> rusqlite::Result<WorkflowRunRow> {
    Ok(WorkflowRunRow {
        id: r.get(0)?,
        workflow_name: r.get(1)?,
        status: r.get(2)?,
        started_at: r.get::<_, Option<i64>>(3)?.map(|v| v as u64),
        ended_at: r.get::<_, Option<i64>>(4)?.map(|v| v as u64),
        error: r.get(5)?,
        task_id: r.get(6)?,
    })
}

/// Column list for a `WorktreeRow` SELECT (kept in sync with [`map_worktree`]).
const WORKTREE_COLS: &str = "terminal_id, project_root, repo_root, worktree_path, branch, \
     base_sha, mode, provider, member_of, task_id";

/// Read-normalize a legacy worktree mode value: pre-v10 rows say `worktree`
/// where the lexicon (and every app-facing surface) says `isolated`. The
/// open-time migration rewrites rows; this catches anything that slips past it.
fn normalize_mode(mode: Option<String>) -> Option<String> {
    mode.map(normalize_mode_str)
}

fn normalize_mode_str(mode: String) -> String {
    if mode == "worktree" {
        "isolated".to_string()
    } else {
        mode
    }
}

fn map_worktree(r: &rusqlite::Row) -> rusqlite::Result<WorktreeRow> {
    Ok(WorktreeRow {
        terminal_id: r.get(0)?,
        project_root: r.get(1)?,
        repo_root: r.get(2)?,
        worktree_path: r.get(3)?,
        branch: r.get(4)?,
        base_sha: r.get(5)?,
        mode: normalize_mode(r.get(6)?),
        provider: r.get(7)?,
        member_of: r.get(8)?,
        task_id: r.get(9)?,
    })
}

/// Column list for a `TaskRow` SELECT (kept in sync with [`map_task`]).
const TASK_COLS: &str =
    "id, workspace_root, title, description, status, created_at, updated_at, archived_at";

fn map_task(r: &rusqlite::Row) -> rusqlite::Result<TaskRow> {
    Ok(TaskRow {
        id: r.get(0)?,
        workspace_root: r.get(1)?,
        title: r.get(2)?,
        description: r.get(3)?,
        status: r.get(4)?,
        created_at: r.get::<_, i64>(5)? as u64,
        updated_at: r.get::<_, i64>(6)? as u64,
        archived_at: r.get::<_, Option<i64>>(7)?.map(|v| v as u64),
    })
}

/// Column list for a `ScheduleRow` SELECT (kept in sync with [`map_schedule`]).
const SCHEDULE_COLS: &str = "name, file_path, schedule, agent_profile, provider, script, \
     last_run, next_run, enabled, prompt, workspace_root, task_mode, task_id";

fn map_schedule(r: &rusqlite::Row) -> rusqlite::Result<ScheduleRow> {
    let parse = |s: Option<String>| s.and_then(|x| x.trim().parse::<u64>().ok());
    Ok(ScheduleRow {
        name: r.get(0)?,
        file_path: r.get(1)?,
        schedule: r.get(2)?,
        agent_profile: r.get(3)?,
        provider: r.get(4)?,
        script: r.get(5)?,
        last_run: parse(r.get(6)?),
        next_run: parse(r.get(7)?),
        enabled: r.get::<_, i64>(8)? != 0,
        prompt: r.get(9)?,
        workspace_root: r.get(10)?,
        task_mode: r.get(11)?,
        task_id: r.get(12)?,
    })
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
-- Schedules (user-facing term) — cron-triggered unattended agent runs. `flows`
-- table name kept for CAO-import heritage; `prompt` added for in-app schedules.
CREATE TABLE IF NOT EXISTS flows (
    name TEXT PRIMARY KEY,
    file_path TEXT NOT NULL,
    schedule TEXT NOT NULL,
    agent_profile TEXT NOT NULL,
    provider TEXT NOT NULL,
    script TEXT,
    last_run TEXT,
    next_run TEXT,
    enabled INTEGER DEFAULT 1,
    prompt TEXT,
    workspace_root TEXT,
    task_mode TEXT,
    task_id TEXT
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
    created_at TEXT,
    task_id TEXT
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
-- Phase-5 shared blackboard: a small global key/value scratchpad agents post to
-- (`share`) and read (`get`). Last writer wins; `author` is the stamping caller.
CREATE TABLE IF NOT EXISTS taime_blackboard (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    author TEXT,
    updated_at INTEGER NOT NULL
);
-- Phase-5 request/reply correlation: `request` records a row keyed by a generated
-- interaction id; `reply` fills `reply` + flips `status` to 'answered'. The actual
-- message delivery rides the inbox; this table is the durable correlation thread.
CREATE TABLE IF NOT EXISTS taime_interactions (
    interaction_id TEXT PRIMARY KEY,
    requester TEXT NOT NULL,
    responder TEXT NOT NULL,
    body TEXT NOT NULL,
    reply TEXT,
    status TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
-- Workflows: the loopable agent step-graph. Definition is the JSON; runs +
-- node_runs track execution state for the live graph view.
CREATE TABLE IF NOT EXISTS taime_workflows (
    name TEXT PRIMARY KEY,
    file_path TEXT,
    definition TEXT NOT NULL,
    source TEXT,
    created_at INTEGER
);
CREATE TABLE IF NOT EXISTS taime_workflow_runs (
    id TEXT PRIMARY KEY,
    workflow_name TEXT NOT NULL,
    status TEXT NOT NULL,
    started_at INTEGER,
    ended_at INTEGER,
    error TEXT,
    task_id TEXT
);
CREATE TABLE IF NOT EXISTS taime_workflow_node_runs (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    agent_key TEXT,
    status TEXT NOT NULL,
    iteration INTEGER DEFAULT 0,
    output TEXT,
    started_at INTEGER,
    ended_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_wf_node_runs_run ON taime_workflow_node_runs(run_id);
CREATE INDEX IF NOT EXISTS idx_wf_runs_name ON taime_workflow_runs(workflow_name, started_at);

-- Tasks (v9): named, workspace-scoped units of user intent. A Task owns
-- membership/lifecycle/review-state/rollups — NEVER raw attribution (that stays
-- anchored to the Agent ID on taime_worktrees/turns/events). Membership is the
-- nullable task_id on member rows; deleting a task demotes members to
-- Uncategorized, archiving preserves them read-only.
CREATE TABLE IF NOT EXISTS taime_tasks (
    id TEXT PRIMARY KEY,
    workspace_root TEXT NOT NULL,
    title TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'open',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    archived_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_tasks_root ON taime_tasks(workspace_root, status);

-- Durable review acknowledgments — the flagship safe-context-switch guard's
-- state. Records which agents' current changes the user has acknowledged, so the
-- "I already reviewed this" fact survives a UI/daemon restart (it was
-- frontend-only `reviewedFrames` before, dying with the UI — the worst case for
-- a long-running / generative session). Keyed by Agent ID; an ack is dropped
-- when the agent's dirty set is reset (a fresh review cycle, via clear_dirty).
CREATE TABLE IF NOT EXISTS taime_reviews (
    agent_id TEXT PRIMARY KEY,
    reviewed_at INTEGER NOT NULL
);
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_crud_due_and_enabled() {
        let store = Store::open_at(std::path::Path::new(":memory:")).unwrap();
        let mk = |next: Option<u64>, enabled: bool| ScheduleRow {
            name: "nightly".into(),
            file_path: "/x/nightly.md".into(),
            schedule: "0 2 * * *".into(),
            agent_profile: "security-reviewer".into(),
            provider: "claude_code".into(),
            script: None,
            prompt: Some("review".into()),
            last_run: None,
            next_run: next,
            enabled,
            workspace_root: Some("/projects/app".into()),
            task_mode: Some("fixed".into()),
            task_id: Some("task-12345678".into()),
        };
        store.upsert_schedule(&mk(Some(100), true)).unwrap();
        // Task-targeting columns roundtrip through the flows table.
        let got = store.get_schedule("nightly").unwrap().unwrap();
        assert_eq!(got.workspace_root.as_deref(), Some("/projects/app"));
        assert_eq!(got.task_mode.as_deref(), Some("fixed"));
        assert_eq!(got.task_id.as_deref(), Some("task-12345678"));
        let list = store.list_schedules().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].agent_profile, "security-reviewer");
        assert_eq!(list[0].next_run, Some(100));
        assert!(store.has_enabled_schedules());
        // Due iff next_run <= now AND enabled.
        assert_eq!(store.due_schedules(200).unwrap().len(), 1);
        assert_eq!(store.due_schedules(50).unwrap().len(), 0);
        store.set_schedule_enabled("nightly", false).unwrap();
        assert_eq!(store.due_schedules(200).unwrap().len(), 0);
        assert!(!store.has_enabled_schedules());
        store.set_schedule_run("nightly", 200, Some(300)).unwrap();
        assert_eq!(store.get_schedule("nightly").unwrap().unwrap().last_run, Some(200));
        store.delete_schedule("nightly").unwrap();
        assert!(store.list_schedules().unwrap().is_empty());
    }

    #[test]
    fn workflow_run_persistence() {
        let store = Store::open_at(std::path::Path::new(":memory:")).unwrap();
        let def = r#"{"name":"wf","entry":"a","nodes":[{"id":"a","prompt":"p"}],"edges":[]}"#;
        store.upsert_workflow("wf", None, def, "generated", 1).unwrap();
        assert_eq!(store.list_workflows().unwrap().len(), 1);
        assert!(store.get_workflow("wf").unwrap().is_some());

        store.create_run("run1", "wf", 10, Some("task-12345678")).unwrap();
        assert_eq!(
            store.get_run("run1").unwrap().unwrap().task_id.as_deref(),
            Some("task-12345678")
        );
        store.insert_node_run("nr1", "run1", "a", Some("agent1"), 1, 11).unwrap();
        let states = store.node_states("run1").unwrap();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].status, "running");
        assert_eq!(states[0].agent_key.as_deref(), Some("agent1"));

        store.finish_node_run("nr1", "completed", Some("PASS"), 12).unwrap();
        // A second iteration of the same node — node_states returns the LATEST.
        store.insert_node_run("nr2", "run1", "a", Some("agent2"), 2, 13).unwrap();
        assert_eq!(store.node_iteration_count("run1", "a"), 2);
        let states = store.node_states("run1").unwrap();
        assert_eq!(states.len(), 1, "one state per node_id (latest)");
        assert_eq!(states[0].iteration, 2);

        store.finish_run("run1", "completed", 14, None).unwrap();
        assert_eq!(store.latest_run("wf").unwrap().unwrap().status, "completed");
        store.delete_workflow("wf").unwrap();
        assert!(store.list_workflows().unwrap().is_empty());
    }

    #[test]
    fn migrate_upgrades_a_pre_v9_db() {
        // A REAL upgrade: build the pre-v9 table shapes (no task_id /
        // workspace_root / task_mode columns) in a file DB, then open it — the
        // ALTERs must actually run (not be swallowed as duplicates) and the
        // ordering-sensitive idx_worktrees_task must build AFTER them.
        let dir = std::env::temp_dir().join(format!("taime-mig-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pre-v9.sqlite");
        let _ = std::fs::remove_file(&path);
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE taime_worktrees (terminal_id TEXT PRIMARY KEY, session_name TEXT, \
                   project_root TEXT NOT NULL, repo_root TEXT, worktree_path TEXT NOT NULL, \
                   branch TEXT, base_sha TEXT, mode TEXT NOT NULL DEFAULT 'shared', \
                   provider TEXT, member_of TEXT, created_at TEXT);
                 CREATE TABLE flows (name TEXT PRIMARY KEY, file_path TEXT NOT NULL, \
                   schedule TEXT NOT NULL, agent_profile TEXT NOT NULL, provider TEXT NOT NULL, \
                   script TEXT, last_run TEXT, next_run TEXT, enabled INTEGER DEFAULT 1, prompt TEXT);
                 CREATE TABLE taime_workflow_runs (id TEXT PRIMARY KEY, workflow_name TEXT NOT NULL, \
                   status TEXT NOT NULL, started_at INTEGER, ended_at INTEGER, error TEXT);
                 INSERT INTO taime_worktrees (terminal_id, project_root, worktree_path, mode) \
                   VALUES ('old-agent', '/p', '/wt', 'worktree');",
            )
            .unwrap();
        }
        let store = Store::open_at(&path).unwrap();
        // The pre-existing row survives with task_id = NULL (Uncategorized)…
        let row = store.worktree_row("old-agent").unwrap().unwrap();
        assert_eq!(row.task_id, None);
        // …its legacy mode value is migrated to the v10 lexicon on open…
        assert_eq!(row.mode.as_deref(), Some("isolated"));
        // …and the upgraded columns are fully usable end-to-end.
        store.create_task("task-up", "/p", "Upgraded", "", 1).unwrap();
        assert!(store.set_worktree_task("old-agent", Some("task-up")).unwrap());
        assert_eq!(store.task_of_worktree("old-agent").as_deref(), Some("task-up"));
        store.create_run("r-up", "wf", 2, Some("task-up")).unwrap();
        assert_eq!(store.runs_for_task("task-up").unwrap().len(), 1);
        let idx: i64 = {
            let conn = store.conn.lock().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_worktrees_task'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(idx, 1, "ALTER-dependent index must exist after upgrade");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tasks_crud_membership_and_demote() {
        let store = Store::open_at(std::path::Path::new(":memory:")).unwrap();
        store.create_task("task-1", "/p", "Fix login bug", "", 100).unwrap();
        store.create_task("task-2", "/p", "Add feature", "desc", 110).unwrap();
        store.create_task("task-3", "/other", "Elsewhere", "", 120).unwrap();

        // Workspace-scoped, newest first; other workspaces never bleed in.
        let tasks = store.list_tasks("/p", false).unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].id, "task-2");

        // Membership = nullable task_id on the worktree row (the durable anchor).
        let info = WorktreeInfo {
            agent_id: "agent-a".into(),
            project_root: "/p".into(),
            repo_root: None,
            worktree_path: "/p".into(),
            branch: None,
            base_sha: None,
            mode: "shared".into(),
            error: None,
        };
        store.upsert_worktree(&info, "claude_code", 100).unwrap();
        assert!(store.set_worktree_task("agent-a", Some("task-1")).unwrap());
        assert_eq!(store.task_of_worktree("agent-a").as_deref(), Some("task-1"));
        assert_eq!(store.agents_for_task("task-1").unwrap().len(), 1);
        assert_eq!(store.task_agent_counts("/p").unwrap(), vec![("task-1".to_string(), 1)]);

        // Archive: stamps archived_at, hides from the default list, PRESERVES
        // membership (read-only history). Un-archiving clears the stamp.
        assert!(store.update_task("task-1", None, None, Some("archived"), 200).unwrap());
        let t = store.get_task("task-1").unwrap().unwrap();
        assert_eq!((t.status.as_str(), t.archived_at), ("archived", Some(200)));
        assert_eq!(store.list_tasks("/p", false).unwrap().len(), 1);
        assert_eq!(store.list_tasks("/p", true).unwrap().len(), 2);
        assert_eq!(store.task_of_worktree("agent-a").as_deref(), Some("task-1"));
        assert!(store.update_task("task-1", None, None, Some("open"), 300).unwrap());
        assert_eq!(store.get_task("task-1").unwrap().unwrap().archived_at, None);

        // Runs attach to tasks too.
        store.create_run("r1", "wf", 10, Some("task-1")).unwrap();
        assert_eq!(store.runs_for_task("task-1").unwrap().len(), 1);

        // Delete DEMOTES members + detaches runs — never deletes their rows.
        store.delete_task("task-1").unwrap();
        assert!(store.get_task("task-1").unwrap().is_none());
        assert_eq!(store.task_of_worktree("agent-a"), None);
        assert!(store.worktree_row("agent-a").unwrap().is_some(), "agent row survives");
        assert_eq!(store.get_run("r1").unwrap().unwrap().task_id, None);

        // Unknown ids are reported, not silently absorbed.
        assert!(!store.update_task("task-x", None, None, Some("open"), 1).unwrap());
        assert!(!store.set_worktree_task("nobody", Some("task-2")).unwrap());
    }

    #[test]
    fn reviews_persist_idempotently_and_clear() {
        let store = Store::open_at(std::path::Path::new(":memory:")).unwrap();
        assert!(store.reviewed_agents().unwrap().is_empty());

        store.mark_reviewed("agent-a", 100).unwrap();
        store.mark_reviewed("agent-a", 200).unwrap(); // upsert, not a duplicate
        store.mark_reviewed("agent-b", 150).unwrap();
        let mut ids = store.reviewed_agents().unwrap();
        ids.sort();
        assert_eq!(ids, vec!["agent-a".to_string(), "agent-b".to_string()]);

        // A fresh review cycle drops the ack so new changes re-raise the guard.
        store.clear_reviewed("agent-a").unwrap();
        assert_eq!(store.reviewed_agents().unwrap(), vec!["agent-b".to_string()]);
        // Clearing an unknown id is a harmless no-op.
        store.clear_reviewed("nobody").unwrap();
        assert_eq!(store.reviewed_agents().unwrap(), vec!["agent-b".to_string()]);
    }

    #[test]
    fn workspace_teardown_is_scoped_to_the_workspace() {
        let store = Store::open_at(std::path::Path::new(":memory:")).unwrap();
        let mk = |id: &str, root: &str| WorktreeInfo {
            agent_id: id.into(),
            project_root: root.into(),
            repo_root: None,
            worktree_path: root.into(),
            branch: None,
            base_sha: None,
            mode: "shared".into(),
            error: None,
        };
        store.upsert_worktree(&mk("a", "/ws"), "claude_code", 1).unwrap();
        store.upsert_worktree(&mk("b", "/ws"), "claude_code", 2).unwrap();
        store.upsert_worktree(&mk("c", "/other"), "claude_code", 3).unwrap();
        store.create_task("t1", "/ws", "One", "", 1).unwrap();
        store.create_task("t2", "/ws", "Two", "", 2).unwrap();
        store.create_task("t3", "/other", "Other", "", 3).unwrap();

        // Listing is workspace-scoped.
        let ws = store.worktrees_in_workspace("/ws").unwrap();
        assert_eq!(ws.len(), 2);
        assert!(ws.iter().all(|w| w.project_root.as_deref() == Some("/ws")));

        // Deleting rows + tasks affects ONLY the target workspace.
        store.delete_worktree_row("a").unwrap();
        store.delete_worktree_row("b").unwrap();
        assert_eq!(store.worktrees_in_workspace("/ws").unwrap().len(), 0);
        assert!(store.worktree_row("c").unwrap().is_some(), "/other agent survives");

        assert_eq!(store.delete_tasks_for_workspace("/ws").unwrap(), 2);
        assert_eq!(store.list_tasks("/ws", true).unwrap().len(), 0);
        assert_eq!(store.list_tasks("/other", true).unwrap().len(), 1, "/other tasks survive");
    }

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
    fn inbox_enqueue_fifo_and_status_gate() {
        let store = Store::open_at(std::path::Path::new(":memory:")).unwrap();
        let id1 = store.enqueue_message("a", "b", "first", 10).unwrap();
        let id2 = store.enqueue_message("a", "b", "second", 11).unwrap();
        let _other = store.enqueue_message("a", "c", "for-c", 12).unwrap();
        assert!(id2 > id1, "monotonic ids");

        assert_eq!(store.receivers_with_pending().unwrap().len(), 2); // b, c

        // FIFO: oldest first.
        let next = store.pending_for("b", 1).unwrap();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].message, "first");
        assert_eq!(next[0].id, id1);

        // Deliver the first → the second becomes head; b stays a pending receiver.
        store.set_message_status(id1, "delivered").unwrap();
        let next = store.pending_for("b", 1).unwrap();
        assert_eq!(next[0].message, "second");

        store.set_message_status(id2, "delivered").unwrap();
        // b drained; only c remains.
        assert_eq!(store.receivers_with_pending().unwrap(), vec!["c".to_string()]);
    }

    #[test]
    fn import_cao_copies_rows_and_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("taime-import-{}", unsafe { libc::getpid() }));
        std::fs::create_dir_all(&dir).unwrap();
        let cao_path = dir.join("cao.sqlite");
        {
            let cao = Connection::open(&cao_path).unwrap();
            cao.execute_batch(
                "CREATE TABLE terminals (id TEXT PRIMARY KEY, tmux_session TEXT, tmux_window TEXT, provider TEXT, agent_profile TEXT, allowed_tools TEXT, shell_command TEXT, last_active TEXT);
                 INSERT INTO terminals (id, tmux_session, tmux_window, provider) VALUES ('t1','s','w','claude_code');
                 CREATE TABLE taime_worktrees (terminal_id TEXT PRIMARY KEY, session_name TEXT, project_root TEXT, repo_root TEXT, worktree_path TEXT, branch TEXT, base_sha TEXT, mode TEXT, provider TEXT, member_of TEXT, created_at TEXT);
                 INSERT INTO taime_worktrees (terminal_id, project_root, worktree_path, mode) VALUES ('t1','/p','/wt','worktree');",
            )
            .unwrap();
        }

        let store = Store::open_at(&dir.join("daemon.sqlite")).unwrap();
        let stats = store.import_cao(&cao_path).unwrap();
        assert!(stats.ran);
        {
            let conn = store.conn.lock().unwrap();
            let terms: i64 = conn.query_row("SELECT COUNT(*) FROM terminals", [], |r| r.get(0)).unwrap();
            let wts: i64 = conn.query_row("SELECT COUNT(*) FROM taime_worktrees", [], |r| r.get(0)).unwrap();
            assert_eq!(terms, 1);
            assert_eq!(wts, 1);
        }
        // The import lands AFTER the open-time mode migration, so the legacy
        // 'worktree' value reaches reads only via normalization (belt/braces).
        let row = store.worktree_row("t1").unwrap().unwrap();
        assert_eq!(row.mode.as_deref(), Some("isolated"));

        // Re-run is a no-op (the marker gates it).
        let again = store.import_cao(&cao_path).unwrap();
        assert!(!again.ran);

        let _ = std::fs::remove_dir_all(&dir);
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
