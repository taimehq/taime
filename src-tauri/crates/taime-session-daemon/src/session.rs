//! A single PTY session: owns the `portable-pty` master/child + the authoritative
//! `wezterm-term` emulator + the attribution tap, and streams output to at most
//! one attached client connection.
//!
//! Concurrency mirrors the in-app `PtyManager`'s proven discipline, generalized
//! across the process boundary:
//!   * a **blocking reader thread** reads the PTY master (no lock held across the
//!     read), then under one `state` mutex feeds the emulator + attribution,
//!     assigns the byte offset, and — if attached — enqueues a data frame;
//!   * **control** (attach/detach/resize/input/ack/checkpoint/kill) runs on the
//!     async connection task and locks the same `state` mutex briefly (never
//!     across an `.await`);
//!   * **backpressure**: the reader parks on a `Condvar` while
//!     `sent_offset - acked_offset` exceeds the high watermark; an `AckBytes`
//!     that drains below the low watermark wakes it. Parking the reader fills the
//!     kernel PTY buffer and the agent blocks on write — natural flow control.
//!
//! The attach handoff is exactly-once across the cut: under the `state` lock we
//! snapshot `seq_n = out_offset`, render the grid repaint, enqueue
//! `AttachOk(seq_n)` + the repaint into the (fresh, FIFO) client channel, then
//! mark the session attached — so the reader's first live data frame
//! (`start_offset >= seq_n`) is ordered strictly after the repaint.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use taime_protocol::{
    encode_data, encode_repaint, encode_server, AgentStatus, ServerMsg, SessionSummary, SpawnSpec,
    TurnInfo, MAX_TERM_DIM,
};
use tokio::sync::mpsc;

use crate::attribution::Attribution;
use crate::providers::{Cleanup, DaemonSessionSpec, GridView, Prepared, Provider};
use crate::store::Store;
use crate::{emulator, repaint};

/// Cap on the dirty-path set pushed to the app per `FsDirty` (mirrors the app
/// badge's old truncation).
const FS_DIRTY_CAP: usize = 50;

/// SHARED-mode only (review items 4/11): a shared agent's watcher is rooted at the
/// user's real working tree, so a change is only honestly the agent's while it is
/// actively producing output. We attribute a change if the agent produced output
/// within this window of it (covers the agent's own file-write/flush + debounce
/// lag after a turn's visible work); edits while the agent is genuinely idle — the
/// user's or another agent's — fall outside it and are dropped, never recorded
/// under the agent's key. Short on purpose: misattributing the user's edits is the
/// worse error, and real user-idle edits come minutes later, not seconds.
const SHARED_ATTRIBUTION_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// Whether a SHARED agent should attribute a change observed now: only once it has
/// actually produced output (`has_output`) AND is mid-turn or within
/// [`SHARED_ATTRIBUTION_GRACE`] of its last output (covers its own write/flush +
/// debounce lag after a turn). The `has_output` gate closes the cold-start window:
/// a freshly-spawned agent's `last_output` is its spawn instant, so without it any
/// edit the USER makes in the shared tree during the provider's startup latency
/// (before the agent emits a byte) would be misattributed — the worse error. Idle
/// longer than the grace ⇒ the change is the user's (or another agent's) and is
/// dropped. Pure so the window is unit-testable without spawning a PTY. Isolated
/// agents bypass this entirely — they own their worktree, so changes are theirs.
fn shared_records_now(has_output: bool, turn_open: bool, since_last_output: std::time::Duration) -> bool {
    has_output && (turn_open || since_last_output < SHARED_ATTRIBUTION_GRACE)
}

/// Backpressure watermarks (bytes in flight = sent − acked). Generous for the
/// daemon's two-hop path (socket + Tauri channel + xterm); 64 KiB would stall a
/// bursty TUI. Tunable.
const HIGH_WATERMARK: u64 = 2 * 1024 * 1024;
const LOW_WATERMARK: u64 = 512 * 1024;

/// The single attached client's stream state.
struct AttachedClient {
    /// Frames to this connection's socket writer task (FIFO). **Unbounded** so
    /// the reader can enqueue under the state lock without blocking — flow
    /// control is the ack watermark (which pauses the reader), not this channel.
    /// Sending under the lock keeps data and out-of-band TurnBoundary frames
    /// strictly ordered, and lets `kill()` always reach the EOF path.
    out: mpsc::UnboundedSender<Bytes>,
    /// Connection id, so detach/ack only affect the client that attached.
    conn_id: u64,
    /// Highest byte offset enqueued to this client.
    sent_offset: u64,
    /// Highest byte offset the client has processed (xterm write-callback).
    acked_offset: u64,
}

struct SessionState {
    term: wezterm_term::Terminal,
    attr: Attribution,
    out_offset: u64,
    attached: Option<AttachedClient>,
    paused: bool,
    rows: u16,
    cols: u16,
    last_output: Instant,
    /// Output seen since the last turn boundary (gates the quiet-window close).
    turn_dirty: bool,
    /// Wall-clock open time of the in-flight turn (set on the first output
    /// after a boundary, cleared at close). The durable turn record's REAL
    /// `started_at` — attribution is the thesis; `now, now` was meaningless.
    turn_started_unix: Option<u64>,
    /// Last status pushed to the attached client (Phase 4 push) — push only on
    /// change.
    last_status: Option<AgentStatus>,
    /// Paths the daemon's fs-watcher saw change since the current turn opened;
    /// drained into each closing turn's `fs_dirty_paths` (Phase 6).
    fs_turn_dirty: BTreeSet<String>,
    /// Accumulated dirty paths since the last review/clear — the full set pushed
    /// to the app on `FsDirty` (the badge/inventory).
    fs_all_dirty: BTreeSet<String>,
    /// When a graceful `kill()` sent SIGTERM to the group, so the gc tick can
    /// escalate to SIGKILL after a grace if the agent ignores it (review H1).
    kill_sigterm_at: Option<Instant>,
}

struct SessionInner {
    id: String,
    program: String,
    cwd: String,
    /// Whether this agent runs in a SHARED worktree (the user's real tree) rather
    /// than its own isolated checkout. Shared mode gates fs-event attribution on
    /// the agent being mid-turn + de-dups across co-watchers (review items 4/11):
    /// the watcher's cwd is shared with the user and other agents, so a change is
    /// only honestly the agent's while it's actively working. Isolated agents own
    /// their whole worktree — every change there is unambiguously theirs.
    shared: bool,
    attribution_key: Option<String>,
    /// Provider id (`claude_code`/`codex`/…) when spawned via the registry.
    /// `None` for low-level spawns.
    provider: Option<String>,
    /// The provider adapter, used to infer `status` from the live grid (Phase 4).
    /// `None` for low-level spawns.
    adapter: Option<Box<dyn Provider>>,
    /// Enters to send after a bracketed paste (CAO `paste_enter_count`); carried
    /// for the Phase-5 idle-gated stdin delivery.
    #[allow(dead_code)]
    paste_enter_count: u8,
    /// Spawn-time MCP injection to undo when the process exits (best-effort).
    cleanup: Cleanup,
    /// Process-group id of the agent (== leader pid; the child setsid()'d). Used
    /// by `kill`/`kill_all`/the gc reaper to signal the whole group (review H1).
    pid: Option<u32>,
    /// Single-shot guard so exit finalization (reap + cleanup + `Exited`) runs
    /// exactly once whether the reader hits EOF or the gc reaper sees the exit
    /// first (review M14/M15).
    finalized: AtomicBool,
    created_at_unix: u64,
    /// Durable store handle (Phase 6) — the session records its own fs-change
    /// activity here so attribution accrues even with the app closed.
    store: Option<Arc<Store>>,
    /// The per-session source-tree watcher (RAII: dropping it stops the OS watch).
    watcher: Mutex<Option<crate::fswatch::SessionWatcher>>,
    state: Mutex<SessionState>,
    resume: Condvar,
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    dead: AtomicBool,
}

/// A live session. Cheap to clone (`Arc`).
#[derive(Clone)]
pub struct Session {
    inner: Arc<SessionInner>,
}

impl Session {
    /// Spawn a low-level [`SpawnSpec`] (the Claude-only Step-2 path; no provider
    /// recipe, no MCP cleanup). Kept for back-compat with `ClientMsg::Spawn`.
    pub fn spawn(
        id: String,
        spec: &SpawnSpec,
        store: Option<Arc<Store>>,
        shared: bool,
    ) -> Result<Session, String> {
        let dspec = DaemonSessionSpec {
            prog: spec.prog.clone(),
            args: spec.args.clone(),
            cwd: spec.cwd.clone(),
            env: spec.env.clone(),
            env_remove: Vec::new(),
            rows: spec.rows,
            cols: spec.cols,
            attribution_key: spec.agent_id.clone(),
            paste_enter_count: 1,
        };
        Self::spawn_inner(id, &dspec, Cleanup::default(), None, store, shared)
    }

    /// Spawn a registry-`Prepared` agent: the daemon-built command + MCP injection
    /// (the Phase-1 all-provider path). `adapter` is the provider adapter, used
    /// for status inference; its `id()` is recorded as the session's provider.
    pub fn spawn_prepared(
        id: String,
        prepared: Prepared,
        adapter: Option<Box<dyn Provider>>,
        store: Option<Arc<Store>>,
        shared: bool,
    ) -> Result<Session, String> {
        Self::spawn_inner(id, &prepared.spec, prepared.cleanup, adapter, store, shared)
    }

    /// Shared PTY setup for both spawn paths.
    fn spawn_inner(
        id: String,
        spec: &DaemonSessionSpec,
        cleanup: Cleanup,
        adapter: Option<Box<dyn Provider>>,
        store: Option<Arc<Store>>,
        shared: bool,
    ) -> Result<Session, String> {
        let provider = adapter.as_ref().map(|a| a.id().to_string());
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: spec.rows,
                cols: spec.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("openpty failed: {e}"))?;

        let mut cmd = CommandBuilder::new(&spec.prog);
        for a in &spec.args {
            cmd.arg(a);
        }
        if let Some(dir) = &spec.cwd {
            cmd.cwd(dir);
        }
        // Inherit the daemon's env (seeded from the app at detached-spawn), force
        // a sane TERM for the TUI, drop provider-requested vars (e.g. Claude's
        // CLAUDE* to avoid a "nested session"), then apply caller overrides.
        for (k, v) in std::env::vars() {
            cmd.env(k, v);
        }
        cmd.env("TERM", "xterm-256color");
        for k in &spec.env_remove {
            cmd.env_remove(k);
        }
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        // Stamp every agent child with its session id (review H1/H2 backstop).
        // Env is inherited across `setsid`, so grandchildren carry it too; a
        // boot-time process-table sweep uses it to find + kill orphan groups a
        // crashed daemon left behind. Applied last so nothing overrides it.
        cmd.env("TAIME_SESSION_ID", &id);

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("spawn '{}' failed: {e}", spec.prog))?;
        // portable-pty's pre_exec calls `setsid()`, so the child leads its own
        // session + process group (pgid == pid). Capture the pid now so kill can
        // signal the whole GROUP — every helper the CLI forks (node workers,
        // ripgrep, git, language servers, MCP shims) — not just the leader pid,
        // which is all `Child::kill` reaches and would orphan the rest (review H1).
        let pid = child.process_id();
        drop(pair.slave); // EOF propagates to the reader on child exit

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("clone reader failed: {e}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("take writer failed: {e}"))?;

        let created_at_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let inner = Arc::new(SessionInner {
            id: id.clone(),
            program: spec.prog.clone(),
            cwd: spec.cwd.clone().unwrap_or_default(),
            shared,
            attribution_key: spec.attribution_key.clone(),
            provider,
            adapter,
            paste_enter_count: spec.paste_enter_count,
            cleanup,
            pid,
            finalized: AtomicBool::new(false),
            created_at_unix,
            store,
            watcher: Mutex::new(None),
            state: Mutex::new(SessionState {
                term: emulator::build_terminal(spec.rows, spec.cols),
                attr: Attribution::new(id.clone()),
                out_offset: 0,
                attached: None,
                paused: false,
                rows: spec.rows,
                cols: spec.cols,
                last_output: Instant::now(),
                turn_dirty: false,
                turn_started_unix: None,
                last_status: None,
                fs_turn_dirty: BTreeSet::new(),
                fs_all_dirty: BTreeSet::new(),
                kill_sigterm_at: None,
            }),
            resume: Condvar::new(),
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            child: Mutex::new(child),
            dead: AtomicBool::new(false),
        });

        start_fs_watch(&inner);
        spawn_reader(inner.clone(), reader);
        Ok(Session { inner })
    }

    /// Attach `out` (the connection's outbound frame channel) as the streaming
    /// client. Enqueues `AttachOk(seq_n)` + the grid repaint, then marks attached
    /// so the reader's first live data frame is ordered after the repaint.
    pub fn attach(
        &self,
        conn_id: u64,
        rows: u16,
        cols: u16,
        out: mpsc::UnboundedSender<Bytes>,
    ) -> Result<(), String> {
        // Handoff step 1 — Resize FIRST (no lock held across it; `resize` takes
        // master-then-state in that order to avoid inversion), so the grid repaint
        // below reflects the client's actual viewport rather than the spawn-time
        // size. A no-op when the size already matches.
        if rows != 0 && cols != 0 {
            let _ = self.resize(rows, cols);
        }
        let mut st = lock_state(&self.inner);
        let seq_n = st.out_offset;
        let (rows, cols) = (st.rows, st.cols);
        let alt = st.term.is_alt_screen_active();
        let repaint_bytes = repaint::serialize(&st.term);

        let attach_ok = encode_server(&ServerMsg::AttachOk { rows, cols, seq_n, alt_screen: alt })
            .map_err(|e| format!("encode AttachOk: {e}"))?;
        out.send(attach_ok).map_err(|e| format!("attach send: {e}"))?;
        out.send(encode_repaint(seq_n, &repaint_bytes))
            .map_err(|e| format!("repaint send: {e}"))?;

        // Replay the current accumulated dirty set so a (re)attaching client's
        // badge is correct immediately, rather than lagging until the next fs
        // change (Phase 6: FsDirty is otherwise only pushed on change, to the one
        // attached client).
        if !st.fs_all_dirty.is_empty() {
            let mut paths: Vec<String> = st.fs_all_dirty.iter().cloned().collect();
            paths.truncate(FS_DIRTY_CAP);
            if let Ok(frame) = encode_server(&ServerMsg::FsDirty { paths }) {
                let _ = out.send(frame);
            }
        }

        st.attached = Some(AttachedClient {
            out,
            conn_id,
            sent_offset: seq_n,
            acked_offset: seq_n,
        });
        st.paused = false;
        self.inner.resume.notify_all();
        Ok(())
    }

    /// Detach a specific connection (close_view): keep the agent running.
    pub fn detach(&self, conn_id: u64) {
        let mut st = lock_state(&self.inner);
        if st.attached.as_ref().map(|c| c.conn_id) == Some(conn_id) {
            st.attached = None;
        }
        st.paused = false;
        self.inner.resume.notify_all();
    }

    /// Backpressure ack from `conn_id`: record the processed offset and wake the
    /// reader if in-flight drained below the low watermark.
    pub fn ack(&self, conn_id: u64, offset: u64) {
        let mut guard = lock_state(&self.inner);
        let st = &mut *guard; // reborrow so disjoint fields can be touched together
        let mut unpause = false;
        if let Some(c) = st.attached.as_mut() {
            if c.conn_id == conn_id {
                // Clamp to sent: a client cannot have processed more than we sent.
                // This keeps the watermark a true bound on the unbounded outbound
                // channel: the client only acks bytes it processed, which it can
                // only process after RECEIVING them from the socket, so
                // acked <= received <= flushed <= sent — hence sent - acked is >=
                // the channel's unflushed depth. The clamp stops a buggy/over-acking
                // client from disabling backpressure and growing the channel.
                let offset = offset.min(c.sent_offset);
                if offset > c.acked_offset {
                    c.acked_offset = offset;
                }
                unpause = st.paused && c.sent_offset.saturating_sub(c.acked_offset) < LOW_WATERMARK;
            }
        }
        if unpause {
            st.paused = false;
            self.inner.resume.notify_all();
        }
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), String> {
        // Clamp the viewport (review L23): rows/cols are u16, but a pathological
        // request would make the per-cell repaint approach the frame cap. Every
        // attach/resize flows through here, so one clamp covers all paths.
        let rows = rows.min(MAX_TERM_DIM);
        let cols = cols.min(MAX_TERM_DIM);
        self.inner
            .master
            .lock()
            .unwrap()
            .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| format!("resize failed: {e}"))?;
        let mut st = lock_state(&self.inner);
        emulator::resize(&mut st.term, rows, cols);
        st.rows = rows;
        st.cols = cols;
        Ok(())
    }

    pub fn input(&self, bytes: &[u8]) -> Result<(), String> {
        let mut w = self.inner.writer.lock().unwrap();
        w.write_all(bytes).map_err(|e| format!("write failed: {e}"))?;
        w.flush().map_err(|e| format!("flush failed: {e}"))
    }

    /// App-driven attribution boundary (strongest signal).
    pub fn checkpoint(&self) {
        let mut st = lock_state(&self.inner);
        let off = st.out_offset;
        if let Some(mut turn) = st.attr.checkpoint(off) {
            st.turn_dirty = false;
            drain_turn_paths(&mut st, &mut turn);
            let started = st.turn_started_unix.take().unwrap_or_else(now_unix_secs);
            persist_turn(&self.inner, &turn, started);
            if let (Some(c), Ok(frame)) = (
                st.attached.as_ref(),
                encode_server(&ServerMsg::TurnBoundary { turn }),
            ) {
                let _ = c.out.send(frame);
            }
        }
    }

    /// Quiet-window check (called by the manager tick). Closes the current turn
    /// if output went idle past `threshold`.
    pub fn quiet_check(&self, threshold: std::time::Duration) {
        let mut st = lock_state(&self.inner);
        if !st.turn_dirty || st.last_output.elapsed() < threshold {
            return;
        }
        let off = st.out_offset;
        if let Some(mut turn) = st.attr.quiet(off) {
            st.turn_dirty = false;
            drain_turn_paths(&mut st, &mut turn);
            let started = st.turn_started_unix.take().unwrap_or_else(now_unix_secs);
            persist_turn(&self.inner, &turn, started);
            if let (Some(c), Ok(frame)) = (
                st.attached.as_ref(),
                encode_server(&ServerMsg::TurnBoundary { turn }),
            ) {
                let _ = c.out.send(frame);
            }
        }
    }

    /// Graceful kill: SIGTERM the whole process group (review H1 — reaches every
    /// helper the CLI forked, not just the leader pid a single-pid `Child::kill`
    /// would orphan), arm the gc reaper's SIGKILL escalation, and wake a parked
    /// reader so it observes EOF. Non-blocking. The session stays in the manager
    /// map until the gc reaper confirms the exit, so a CLI that ignores SIGTERM is
    /// still force-killed after a grace and then removed.
    pub fn kill(&self) {
        signal_group(&self.inner, libc::SIGTERM);
        let mut st = lock_state(&self.inner);
        st.kill_sigterm_at.get_or_insert_with(Instant::now);
        st.paused = false;
        drop(st);
        self.inner.resume.notify_all();
    }

    /// gc reaper: escalate an unanswered `kill()` to a group SIGKILL once SIGTERM
    /// has gone `grace` unacknowledged (review H1).
    pub fn escalate_kill_if_due(&self, grace: std::time::Duration) {
        let due = {
            let st = lock_state(&self.inner);
            st.kill_sigterm_at.map(|t| t.elapsed() >= grace).unwrap_or(false)
        };
        if due && self.is_alive() {
            signal_group(&self.inner, libc::SIGKILL);
        }
    }

    /// gc reaper (review M15): if the child has exited, finalize once and return
    /// true. Reaps a session whose reader is parked on backpressure (and so can't
    /// observe EOF) or whose natural exit the reader hasn't reached yet. Finalize
    /// sets `dead` before waking the reader, so the woken reader converges to its
    /// own EOF path (a guarded no-op finalize).
    pub fn reap_if_exited(&self) -> bool {
        let exited = matches!(lock_child(&self.inner).try_wait(), Ok(Some(_)));
        if exited {
            finalize_exit(&self.inner);
            self.inner.resume.notify_all();
        }
        exited
    }

    /// Shutdown step 1: SIGTERM the whole group (review H1/H2). The signal handler
    /// `process::exit`s and can't wait for the gc reaper, so it does its own
    /// terminate → grace → SIGKILL + finalize across all sessions; this is the
    /// terminate.
    pub fn shutdown_terminate(&self) {
        signal_group(&self.inner, libc::SIGTERM);
    }

    /// Shutdown step 2: SIGKILL any straggler, then reap + run provider cleanup
    /// synchronously so injected MCP config never leaks past an abrupt exit
    /// (review H2). Idempotent (the `finalized` guard).
    pub fn shutdown_finalize(&self) {
        if self.is_alive() {
            signal_group(&self.inner, libc::SIGKILL);
        }
        finalize_exit(&self.inner);
    }

    pub fn is_alive(&self) -> bool {
        !self.inner.dead.load(Ordering::SeqCst)
    }

    /// Infer the live status via the provider adapter + grid snapshot (Phase 4).
    /// `None` for a low-level spawn (no adapter) or a dead session. Caller holds
    /// the state lock.
    fn infer_status(&self, st: &SessionState) -> Option<AgentStatus> {
        if !self.is_alive() {
            return None;
        }
        // Review M9: before ANY output, the grid is empty because the CLI hasn't
        // drawn its banner yet (Node/Ink cold start can exceed the 250 ms tick),
        // NOT because it crashed. Report "unknown" (None) until real bytes arrive
        // so a fresh agent never flashes a spurious ERROR badge. Once output has
        // been seen (out_offset > 0), an empty grid genuinely means a dead/cleared
        // screen and the adapter's empty→ERROR mapping applies.
        if st.out_offset == 0 {
            return None;
        }
        self.inner.adapter.as_ref().map(|a| {
            let lines = emulator::snapshot_visible_text(&st.term);
            a.status(&GridView::new(&lines))
        })
    }

    /// Push a `StatusChanged` to the attached client when the inferred status
    /// changes (Phase 4 push). Called on the manager tick — cheap (one regex pass
    /// over the grid) and only emits on a transition.
    pub fn push_status_if_changed(&self) {
        let mut st = lock_state(&self.inner);
        let status = self.infer_status(&st);
        if status == st.last_status {
            return;
        }
        st.last_status = status;
        if let (Some(s), Some(c)) = (status, st.attached.as_ref()) {
            if let Ok(frame) = encode_server(&ServerMsg::StatusChanged { status: s }) {
                let _ = c.out.send(frame);
            }
        }
    }

    /// Ingest a batch of filesystem changes from this session's watcher (Phase 6):
    /// accumulate the dirty sets, record each change to the durable store, and —
    /// if the dirty set grew — push the full set to the attached client so the app
    /// badge updates without polling. Runs on the debouncer's thread.
    pub fn note_fs_changes(&self, changes: Vec<crate::fswatch::FsChange>) {
        if changes.is_empty() {
            return;
        }
        // SHARED-mode authorship gating (review items 4/11). An isolated agent owns
        // its whole worktree, so every change there is unambiguously its work and
        // is recorded as-is. A SHARED agent's watcher is rooted at the user's real
        // tree, shared with the user and any co-watching agents — so we must not
        // blindly stamp every change with this agent's key:
        //  (a) Mid-turn gate: only attribute while the agent was recently producing
        //      output. Outside that window the change is the user's (or another
        //      agent's); we DROP it entirely rather than misattribute (the worse
        //      error). This also silences an idle co-watcher recording the active
        //      agent's edits. (Edits an isolated agent makes OUTSIDE its worktree
        //      are never seen here — the watcher is rooted at the worktree — and we
        //      drop them by omission rather than guess; see `start_fs_watch`.)
        //  (b) Co-watcher de-dup: among simultaneously-active shared agents in one
        //      root, only the first to observe a path records it, so a shared edit
        //      is attributed once, never double-counted as phantom contention.
        let changes = if self.inner.shared {
            let recently_active = {
                let st = lock_state(&self.inner);
                shared_records_now(
                    st.out_offset > 0,
                    st.turn_started_unix.is_some(),
                    st.last_output.elapsed(),
                )
            };
            if !recently_active {
                return;
            }
            let now = Instant::now();
            let kept: Vec<crate::fswatch::FsChange> = changes
                .into_iter()
                .filter(|c| {
                    crate::fswatch::claim_shared_change(&self.inner.cwd, &c.path, &self.inner.id, now)
                })
                .collect();
            if kept.is_empty() {
                return;
            }
            kept
        } else {
            changes
        };
        let (grew, snapshot, out) = {
            let mut st = lock_state(&self.inner);
            let mut grew = false;
            for c in &changes {
                st.fs_turn_dirty.insert(c.path.clone());
                if st.fs_all_dirty.insert(c.path.clone()) {
                    grew = true;
                }
            }
            let mut paths: Vec<String> = st.fs_all_dirty.iter().cloned().collect();
            paths.truncate(FS_DIRTY_CAP);
            let out = if grew { st.attached.as_ref().map(|c| c.out.clone()) } else { None };
            (grew, paths, out)
        };
        // Durable per-file attribution (best-effort), outside the state lock. One
        // transaction for the whole debounced batch (review L8) instead of N
        // autocommits contending with the gc tick on the single write connection.
        if let (Some(store), Some(key)) = (&self.inner.store, &self.inner.attribution_key) {
            let ts = now_unix_secs();
            let rows: Vec<(String, String, String)> = changes
                .iter()
                .map(|c| (gen_event_id(), c.path.clone(), c.kind.to_string()))
                .collect();
            let _ = store.record_fs_events(key, ts, &rows);
            // A new dirty path means changes beyond what any review covered:
            // invalidate the standing ack daemon-side, so the merge gate's
            // "reviewed" is always current — the daemon owns this invalidation
            // rather than trusting the app to clear it. (Re-writes of
            // already-dirty paths keep the ack, matching the app's
            // path-set re-arm semantics.)
            if grew {
                let _ = store.clear_reviewed(key);
            }
        }
        // Push the full dirty set to the app (Phase 6 fs-dirty push).
        if grew {
            if let Some(out) = out {
                if let Ok(frame) = encode_server(&ServerMsg::FsDirty { paths: snapshot }) {
                    let _ = out.send(frame);
                }
            }
        }
    }

    /// Snapshot of the cumulative fs-dirty set (Task rollups / detail surface).
    /// Read-only — never mutates the badge or in-flight turn sets.
    pub fn fs_dirty_paths(&self) -> Vec<String> {
        let st = lock_state(&self.inner);
        st.fs_all_dirty.iter().cloned().collect()
    }

    /// Clear the accumulated dirty set (the user reviewed the diff). Resets both
    /// the badge set and the in-flight turn set so future pushes start fresh.
    pub fn clear_fs_dirty(&self) {
        let mut st = lock_state(&self.inner);
        st.fs_all_dirty.clear();
        st.fs_turn_dirty.clear();
    }

    /// Whether the agent is ready to receive an injected message: IDLE or
    /// COMPLETED (the idle-gated delivery property — never mid-turn; matches
    /// CAO's `check_and_send_pending_messages`).
    pub fn is_ready_for_delivery(&self) -> bool {
        let st = lock_state(&self.inner);
        matches!(self.infer_status(&st), Some(AgentStatus::Idle | AgentStatus::Completed))
    }

    /// The Agent ID this session was spawned with (internally still the
    /// `attribution_key` field) — the inbox addresses messages to it.
    pub fn attribution_key(&self) -> Option<String> {
        self.inner.attribution_key.clone()
    }

    /// The provider id (`assign` inherits the parent's provider for its worker).
    pub fn provider(&self) -> Option<String> {
        self.inner.provider.clone()
    }

    /// The session's working directory (the worktree root `assign` forks from).
    pub fn cwd(&self) -> String {
        self.inner.cwd.clone()
    }

    pub fn summary(&self) -> SessionSummary {
        let st = lock_state(&self.inner);
        // A dead session reports no live status so the app's exit handling
        // (daemon_list `alive=false`) drives the badge.
        let status = self.infer_status(&st);
        SessionSummary {
            id: self.inner.id.clone(),
            cwd: self.inner.cwd.clone(),
            program: self.inner.program.clone(),
            alive: self.is_alive(),
            attached: st.attached.is_some(),
            rows: st.rows,
            cols: st.cols,
            created_at_unix: self.inner.created_at_unix,
            agent_id: self.inner.attribution_key.clone(),
            provider: self.inner.provider.clone(),
            status,
            protocol_version: taime_protocol::PROTOCOL_VERSION,
            // Membership lives on the worktree row, not the live session —
            // Manager::list() fills this from the store (the single fill site).
            task_id: None,
            // Role/profile likewise filled by Manager::list() from the roles map.
            role: None,
        }
    }
}

/// Persist a closed attribution turn to the durable store (best-effort) so the
/// flagship turn substrate — including its files-touched — survives restarts and
/// feeds the graph/attribution surface even with the app closed. `started_at`
/// is the turn's real wall-clock open (tracked in `SessionState`), so the
/// record supports chronological analysis of agent work bursts.
fn persist_turn(inner: &SessionInner, turn: &TurnInfo, started_at_unix: u64) {
    if let (Some(store), Some(key)) = (&inner.store, &inner.attribution_key) {
        let _ = store.record_turn(
            &gen_event_id(),
            key,
            turn.epoch,
            started_at_unix,
            now_unix_secs(),
            &turn.fs_dirty_paths,
        );
    }
}

/// Move the fs paths seen during the now-closing turn into its `fs_dirty_paths`
/// (sorted) and reset the per-turn set. The accumulated badge set is untouched.
fn drain_turn_paths(st: &mut SessionState, turn: &mut TurnInfo) {
    if st.fs_turn_dirty.is_empty() {
        return;
    }
    // BTreeSet already yields sorted; collect then clear.
    turn.fs_dirty_paths = st.fs_turn_dirty.iter().cloned().collect();
    st.fs_turn_dirty.clear();
}

/// Start the per-session source-tree watcher on the agent's cwd (best-effort: a
/// non-dir cwd or watcher-init failure just means no fs attribution). The
/// callback holds a `Weak` to avoid a ref-cycle that would keep the session — and
/// thus the OS watch — alive forever; on session drop the watcher drops with it.
///
/// Scope decision (review item 11 — out-of-worktree edits): the watch is rooted at
/// the agent's cwd (its worktree). An isolated agent that writes OUTSIDE its
/// worktree via an absolute path produces no event here, so such edits are NOT
/// attributed to it. This is deliberate: the worktree boundary is what *proves*
/// authorship, and the watcher has no way to know an out-of-worktree write was
/// this agent's rather than the user's — so we DROP it (invisible) rather than
/// guess and silently misattribute. Surfacing escaped writes as "changes outside
/// any worktree" (a project-root escape watcher flagged while an agent is
/// PROCESSING) is a separate Review feature, intentionally not built here.
fn start_fs_watch(inner: &Arc<SessionInner>) {
    if inner.cwd.is_empty() {
        return;
    }
    let dir = std::path::PathBuf::from(&inner.cwd);
    if !dir.is_dir() {
        return;
    }
    let weak = Arc::downgrade(inner);
    let watcher = crate::fswatch::spawn_watcher(&dir, move |batch| {
        if let Some(strong) = weak.upgrade() {
            Session { inner: strong }.note_fs_changes(batch);
        }
    });
    match watcher {
        Ok(w) => *inner.watcher.lock().unwrap() = Some(w),
        Err(e) => eprintln!("[taime-daemon] fs-watch disabled for {}: {e}", inner.id),
    }
}

pub(crate) fn now_unix_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Lock the session `state` tolerating poison (review M14). The reader thread is
/// the sole reaper; if a control path panics under this lock (e.g. the emulator
/// on hostile escape-heavy output), a bare `.unwrap()` on the next lock would
/// panic the reader too — the child would never be reaped (zombie), `dead` would
/// stay false, and per-session GC + idle-shutdown would break. Recovering the
/// guard keeps the reaper alive; a single corrupted frame is the worst case.
fn lock_state(inner: &SessionInner) -> std::sync::MutexGuard<'_, SessionState> {
    inner.state.lock().unwrap_or_else(|e| e.into_inner())
}

/// Lock the `child` handle tolerating poison (same rationale as [`lock_state`] —
/// the reaper must never be blocked by a poisoned lock).
fn lock_child(inner: &SessionInner) -> std::sync::MutexGuard<'_, Box<dyn Child + Send + Sync>> {
    inner.child.lock().unwrap_or_else(|e| e.into_inner())
}

/// Send `sig` to the agent's whole process group (review H1). `pgid == leader
/// pid` because the child `setsid()`'d at spawn, so this reaches every helper the
/// CLI forked into the group — the processes a single-pid `Child::kill` orphans.
#[cfg(unix)]
fn signal_group(inner: &SessionInner, sig: libc::c_int) {
    if let Some(pid) = inner.pid {
        if pid > 1 {
            // SAFETY: killpg(2) with a pid we own (same uid); errors (group gone)
            // are ignored — best-effort teardown.
            unsafe {
                let _ = libc::killpg(pid as libc::pid_t, sig);
            }
        }
    }
}
#[cfg(not(unix))]
fn signal_group(_inner: &SessionInner, _sig: i32) {}

/// Exit finalization, single-shot via the `finalized` guard (review M14/M15):
/// reap the leader (collects the zombie; `wait()` returns the cached status if the
/// gc reaper's `try_wait` already collected it), mark dead, run the durable
/// provider cleanup + drop its ledger row, then push `Exited` to any attached
/// client. Safe to call from the reader's EOF path, the gc reaper, and shutdown.
fn finalize_exit(inner: &SessionInner) {
    if inner.finalized.swap(true, Ordering::SeqCst) {
        return; // already finalized
    }
    let code = lock_child(inner).wait().ok().map(|s| s.exit_code() as i32);
    inner.dead.store(true, Ordering::SeqCst);
    // Undo spawn-time MCP injection (temp config / settings.json / workspace) and
    // drop the durable cleanup-ledger row now that teardown has actually run.
    inner.cleanup.run();
    if let Some(store) = &inner.store {
        let _ = store.delete_cleanup(&inner.id);
    }
    let st = lock_state(inner);
    if let Some(c) = st.attached.as_ref() {
        if let Ok(frame) = encode_server(&ServerMsg::Exited { code }) {
            let _ = c.out.send(frame);
        }
    }
}

/// A random hex id for an fs activity-event row.
fn gen_event_id() -> String {
    format!("{:016x}{:016x}", rand::random::<u64>(), rand::random::<u64>())
}

/// The blocking PTY reader thread.
fn spawn_reader(inner: Arc<SessionInner>, mut reader: Box<dyn Read + Send>) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            // Park while paused for backpressure (releases the lock while waiting).
            // The gc reaper sets `dead` + `notify_all` if the child exits while
            // we're parked (review M15), so break out on death to converge to the
            // EOF reap below instead of waiting forever on a dead agent.
            {
                let mut st = lock_state(&inner);
                while st.paused {
                    st = inner.resume.wait(st).unwrap_or_else(|e| e.into_inner());
                    if inner.dead.load(Ordering::SeqCst) {
                        break;
                    }
                }
            }
            let n = match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let chunk = &buf[..n];

            // Feed the emulator + attribution and enqueue the data frame (then any
            // turn boundaries from THIS chunk) — all under the lock via the
            // unbounded sender, so ordering is strict relative to out-of-band
            // checkpoint/quiet TurnBoundary frames and the send never blocks.
            let mut guard = lock_state(&inner);
            let st = &mut *guard; // reborrow for disjoint-field access
            st.term.advance_bytes(chunk);
            let start = st.out_offset;
            let end = start + n as u64;
            let now_secs = now_unix_secs();
            // The wall-clock open of the turn this chunk belongs to: the stored
            // open time, or — first output after a boundary — this instant.
            let opened_at = st.turn_started_unix.unwrap_or(now_secs);
            let mut turns = st.attr.feed(chunk, end);
            st.out_offset = end;
            st.last_output = Instant::now();
            st.turn_dirty = true;
            // Attribute fs changes accumulated during this turn to its boundary.
            if let Some(turn) = turns.last_mut() {
                drain_turn_paths(st, turn);
            }
            // Persist every closed turn (durable, regardless of attach) with its
            // REAL start. (`feed` currently coalesces to at most one boundary
            // per chunk; the loop stays robust if that ever changes — later
            // boundaries in the same chunk would have opened ~now.)
            let mut started = opened_at;
            for turn in &turns {
                persist_turn(&inner, turn, started);
                started = now_secs;
            }
            // After a boundary, the NEXT turn opens at the next OUTPUT — not at
            // the boundary itself. Stamping `now` here would over-report by the
            // whole idle-at-prompt gap; clearing instead means the next chunk's
            // first bytes stamp the open (at worst sub-second late, never
            // minutes early). No boundary ⇒ the in-flight turn keeps its open.
            st.turn_started_unix = if turns.is_empty() { Some(opened_at) } else { None };
            if let Some(c) = st.attached.as_mut() {
                let mut ok = c.out.send(encode_data(start, chunk)).is_ok();
                for turn in turns {
                    if !ok {
                        break;
                    }
                    if let Ok(f) = encode_server(&ServerMsg::TurnBoundary { turn }) {
                        ok = c.out.send(f).is_ok();
                    }
                }
                c.sent_offset = end;
                if !ok {
                    // The client we were sending to is gone: detach (only this
                    // client — we hold the lock, so it cannot have been swapped).
                    st.attached = None;
                    st.paused = false;
                } else if end.saturating_sub(c.acked_offset) > HIGH_WATERMARK {
                    st.paused = true;
                }
            }
        }

        // EOF: reap + cleanup + notify, single-shot (the gc reaper may have
        // beaten us to it — review M14/M15).
        finalize_exit(&inner);
    });
}

#[cfg(test)]
impl Session {
    /// The accumulated badge dirty set (test inspection).
    pub fn fs_all_dirty_snapshot(&self) -> Vec<String> {
        lock_state(&self.inner).fs_all_dirty.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::Duration;

    /// A live end-to-end check that the daemon-owned watcher records a real file
    /// change to the store AND into the badge dirty set.
    #[test]
    fn fs_watch_records_changes_to_store_and_dirty_set() {
        let dir = std::env::temp_dir().join(format!("taime-fsw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let store = Arc::new(Store::open_at(Path::new(":memory:")).unwrap());
        let spec = SpawnSpec {
            // A long-lived process so the watcher stays up during the test.
            prog: "sleep".into(),
            args: vec!["10".into()],
            cwd: Some(dir.to_string_lossy().into_owned()),
            env: vec![],
            rows: 24,
            cols: 80,
            agent_id: Some("term-fsw".into()),
        };
        // isolated (shared=false): the worktree boundary proves authorship, so the
        // watcher records unconditionally — no mid-turn gate.
        let session = Session::spawn("pty-fsw".into(), &spec, Some(store.clone()), false).unwrap();

        // Create a source file; the debounced watcher should pick it up.
        std::fs::write(dir.join("hello.txt"), "hi").unwrap();

        let mut recorded = false;
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(100));
            if store
                .fs_path_touches("term-fsw")
                .unwrap()
                .iter()
                .any(|(p, _)| p == "hello.txt")
            {
                recorded = true;
                break;
            }
        }
        session.kill();
        let dirty = session.fs_all_dirty_snapshot();
        let _ = std::fs::remove_dir_all(&dir);

        assert!(recorded, "watcher should record the new file to the store");
        assert!(dirty.iter().any(|p| p == "hello.txt"), "dirty set: {dirty:?}");
    }

    #[test]
    fn shared_attribution_gate_records_only_while_recently_active() {
        // has_output=true, mid-turn → always attribute, regardless of the clock.
        assert!(shared_records_now(true, true, Duration::from_secs(3600)));
        // has_output=true, idle but within the grace window (write/flush lag) → yes.
        assert!(shared_records_now(true, false, SHARED_ATTRIBUTION_GRACE / 2));
        // Idle past the grace window → the change is the user's (or another
        // agent's); drop it rather than misattribute.
        assert!(!shared_records_now(true, false, SHARED_ATTRIBUTION_GRACE + Duration::from_secs(1)));
        // Cold start: NO output yet (out_offset == 0) → never attribute, even though
        // last_output (the spawn instant) is "recent". Closes the spawn-window
        // false-positive where the user's edits would be stamped as the agent's.
        assert!(!shared_records_now(false, false, Duration::from_millis(1)));
        assert!(!shared_records_now(false, true, Duration::from_millis(1)));
    }

    #[test]
    fn shared_session_records_its_edit_while_active() {
        // A SHARED agent (cwd = the user's real tree) still records its OWN edits
        // while recently active — the gate drops only idle/cold-start changes. The
        // agent prints output first (so has_output is true and it's within the
        // grace window) before editing, so the edit is attributed.
        let dir = std::env::temp_dir().join(format!("taime-fsw-shared-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let store = Arc::new(Store::open_at(Path::new(":memory:")).unwrap());
        let spec = SpawnSpec {
            // Emit output (so out_offset > 0 clears the cold-start gate), then stay
            // alive so the watcher keeps running.
            prog: "sh".into(),
            args: vec!["-c".into(), "printf ready; sleep 10".into()],
            cwd: Some(dir.to_string_lossy().into_owned()),
            env: vec![],
            rows: 24,
            cols: 80,
            agent_id: Some("term-shared".into()),
        };
        // shared = true: the authorship gate is active.
        let session = Session::spawn("pty-shared".into(), &spec, Some(store.clone()), true).unwrap();

        // Let the "ready" output land (out_offset > 0) BEFORE editing, so the gate
        // sees the agent as active when the file's debounced fs event fires.
        std::thread::sleep(Duration::from_millis(500));
        std::fs::write(dir.join("work.txt"), "agent output").unwrap();

        let mut recorded = false;
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(100));
            if store
                .fs_path_touches("term-shared")
                .unwrap()
                .iter()
                .any(|(p, _)| p == "work.txt")
            {
                recorded = true;
                break;
            }
        }
        session.kill();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(recorded, "an active shared agent should record its own edit");
    }

    /// Regression for review H1: `kill()` must terminate the agent's whole process
    /// GROUP, not just the leader pid. We spawn `sh` that backgrounds a long
    /// `sleep` under `nohup` (so the helper IGNORES SIGHUP — modelling a CLI
    /// helper that survives the controlling-tty hangup), records its pid, then
    /// `wait`s. After `kill()` the helper must be gone: the old leader-only kill
    /// (SIGHUP to the leader pid → tty SIGHUP cascade, which `nohup` shrugs off)
    /// would orphan it; only a group SIGTERM (the fix) reaches it.
    #[cfg(unix)]
    #[test]
    fn kill_terminates_the_whole_process_group_not_just_the_leader() {
        fn alive(pid: i32) -> bool {
            // kill(pid, 0): 0 => exists, ESRCH => gone. (A reaped zombie is gone.)
            unsafe { libc::kill(pid, 0) == 0 }
        }

        let dir = std::env::temp_dir().join(format!("taime-killpg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pidfile = dir.join("helper.pid");

        // Background a long-lived helper into the session's process group, publish
        // its pid, then keep the leader alive by waiting on it.
        let script = format!(
            "nohup sleep 120 >/dev/null 2>&1 & printf %s \"$!\" > '{}'; wait",
            pidfile.display()
        );
        let spec = SpawnSpec {
            prog: "sh".into(),
            args: vec!["-c".into(), script],
            cwd: Some(dir.to_string_lossy().into_owned()),
            env: vec![],
            rows: 24,
            cols: 80,
            agent_id: Some("term-killpg".into()),
        };
        let session = Session::spawn("pty-killpg".into(), &spec, None, false).unwrap();

        // Wait for the helper pid to be published.
        let mut helper_pid = None;
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(50));
            if let Ok(s) = std::fs::read_to_string(&pidfile) {
                if let Ok(p) = s.trim().parse::<i32>() {
                    helper_pid = Some(p);
                    break;
                }
            }
        }
        let helper_pid = helper_pid.expect("helper published its pid");
        assert!(alive(helper_pid), "helper should be running before kill");

        // SIGTERM the whole group; `sleep` dies on SIGTERM immediately.
        session.kill();

        let mut gone = false;
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(50));
            if !alive(helper_pid) {
                gone = true;
                break;
            }
        }
        // Belt-and-suspenders cleanup if the assert is about to fail.
        if !gone {
            unsafe {
                libc::kill(helper_pid, libc::SIGKILL);
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(gone, "helper (pid {helper_pid}) survived kill() — process group was not signaled");
    }
}
