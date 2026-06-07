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
    TurnInfo,
};
use tokio::sync::mpsc;

use crate::attribution::Attribution;
use crate::providers::{Cleanup, DaemonSessionSpec, GridView, Prepared, Provider};
use crate::store::Store;
use crate::{emulator, repaint};

/// Cap on the dirty-path set pushed to the app per `FsDirty` (mirrors the app
/// badge's old truncation).
const FS_DIRTY_CAP: usize = 50;

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
}

struct SessionInner {
    id: String,
    program: String,
    cwd: String,
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
    ) -> Result<Session, String> {
        let dspec = DaemonSessionSpec {
            prog: spec.prog.clone(),
            args: spec.args.clone(),
            cwd: spec.cwd.clone(),
            env: spec.env.clone(),
            env_remove: Vec::new(),
            rows: spec.rows,
            cols: spec.cols,
            attribution_key: spec.attribution_key.clone(),
            paste_enter_count: 1,
        };
        Self::spawn_inner(id, &dspec, Cleanup::default(), None, store)
    }

    /// Spawn a registry-`Prepared` agent: the daemon-built command + MCP injection
    /// (the Phase-1 all-provider path). `adapter` is the provider adapter, used
    /// for status inference; its `id()` is recorded as the session's provider.
    pub fn spawn_prepared(
        id: String,
        prepared: Prepared,
        adapter: Option<Box<dyn Provider>>,
        store: Option<Arc<Store>>,
    ) -> Result<Session, String> {
        Self::spawn_inner(id, &prepared.spec, prepared.cleanup, adapter, store)
    }

    /// Shared PTY setup for both spawn paths.
    fn spawn_inner(
        id: String,
        spec: &DaemonSessionSpec,
        cleanup: Cleanup,
        adapter: Option<Box<dyn Provider>>,
        store: Option<Arc<Store>>,
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

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("spawn '{}' failed: {e}", spec.prog))?;
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
            attribution_key: spec.attribution_key.clone(),
            provider,
            adapter,
            paste_enter_count: spec.paste_enter_count,
            cleanup,
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
        let mut st = self.inner.state.lock().unwrap();
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
        let mut st = self.inner.state.lock().unwrap();
        if st.attached.as_ref().map(|c| c.conn_id) == Some(conn_id) {
            st.attached = None;
        }
        st.paused = false;
        self.inner.resume.notify_all();
    }

    /// Backpressure ack from `conn_id`: record the processed offset and wake the
    /// reader if in-flight drained below the low watermark.
    pub fn ack(&self, conn_id: u64, offset: u64) {
        let mut guard = self.inner.state.lock().unwrap();
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
        self.inner
            .master
            .lock()
            .unwrap()
            .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| format!("resize failed: {e}"))?;
        let mut st = self.inner.state.lock().unwrap();
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
        let mut st = self.inner.state.lock().unwrap();
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
        let mut st = self.inner.state.lock().unwrap();
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

    pub fn kill(&self) {
        // Signal the child; DON'T clear `attached` or reap here. We wake a parked
        // reader (backpressure) so it observes EOF, emits `Exited` to the attached
        // client, and reaps the child on its own EOF path — otherwise an explicit
        // kill while the reader is parked would never deliver `Exited`. The reader
        // never blocks in `send` (the outbound channel is unbounded), so once
        // unparked it always reaches the EOF cleanup.
        let _ = self.inner.child.lock().unwrap().kill();
        let mut st = self.inner.state.lock().unwrap();
        st.paused = false;
        self.inner.resume.notify_all();
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
        self.inner.adapter.as_ref().map(|a| {
            let lines = emulator::snapshot_visible_text(&st.term);
            a.status(&GridView::new(&lines))
        })
    }

    /// Push a `StatusChanged` to the attached client when the inferred status
    /// changes (Phase 4 push). Called on the manager tick — cheap (one regex pass
    /// over the grid) and only emits on a transition.
    pub fn push_status_if_changed(&self) {
        let mut st = self.inner.state.lock().unwrap();
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
        let (grew, snapshot, out) = {
            let mut st = self.inner.state.lock().unwrap();
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
        // Durable per-file attribution (best-effort), outside the state lock.
        if let (Some(store), Some(key)) = (&self.inner.store, &self.inner.attribution_key) {
            let ts = now_unix_secs();
            for c in &changes {
                let _ = store.record_fs_event(&gen_event_id(), key, &c.path, c.kind, ts);
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
        let st = self.inner.state.lock().unwrap();
        st.fs_all_dirty.iter().cloned().collect()
    }

    /// Clear the accumulated dirty set (the user reviewed the diff). Resets both
    /// the badge set and the in-flight turn set so future pushes start fresh.
    pub fn clear_fs_dirty(&self) {
        let mut st = self.inner.state.lock().unwrap();
        st.fs_all_dirty.clear();
        st.fs_turn_dirty.clear();
    }

    /// Whether the agent is ready to receive an injected message: IDLE or
    /// COMPLETED (the idle-gated delivery property — never mid-turn; matches
    /// CAO's `check_and_send_pending_messages`).
    pub fn is_ready_for_delivery(&self) -> bool {
        let st = self.inner.state.lock().unwrap();
        matches!(self.infer_status(&st), Some(AgentStatus::Idle | AgentStatus::Completed))
    }

    /// The attribution key (CAO terminal id) this session was spawned with — the
    /// inbox addresses messages to it.
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
        let st = self.inner.state.lock().unwrap();
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
            attribution_key: self.inner.attribution_key.clone(),
            provider: self.inner.provider.clone(),
            status,
            protocol_version: taime_protocol::PROTOCOL_VERSION,
            // Membership lives on the worktree row, not the live session —
            // Manager::list() fills this from the store (the single fill site).
            task_id: None,
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

fn now_unix_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
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
            {
                let mut st = inner.state.lock().unwrap();
                while st.paused {
                    st = inner.resume.wait(st).unwrap();
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
            let mut guard = inner.state.lock().unwrap();
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
            // REAL start: the first closed turn opened at `opened_at`; any
            // back-to-back turns closed within this same chunk opened now.
            let mut started = opened_at;
            for turn in &turns {
                persist_turn(&inner, turn, started);
                started = now_secs;
            }
            // Trailing output after the last boundary opens the next turn now;
            // with no boundary in this chunk the in-flight turn keeps its open.
            st.turn_started_unix =
                if turns.is_empty() { Some(opened_at) } else { Some(now_secs) };
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

        // EOF: reap, mark dead, notify the attached client.
        let code = inner
            .child
            .lock()
            .unwrap()
            .wait()
            .ok()
            .map(|s| s.exit_code() as i32);
        inner.dead.store(true, Ordering::SeqCst);
        // Undo spawn-time MCP injection (temp config / settings.json / workspace).
        inner.cleanup.run();
        let st = inner.state.lock().unwrap();
        if let Some(c) = st.attached.as_ref() {
            if let Ok(frame) = encode_server(&ServerMsg::Exited { code }) {
                let _ = c.out.send(frame);
            }
        }
    });
}

#[cfg(test)]
impl Session {
    /// The accumulated badge dirty set (test inspection).
    pub fn fs_all_dirty_snapshot(&self) -> Vec<String> {
        self.inner.state.lock().unwrap().fs_all_dirty.iter().cloned().collect()
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
            attribution_key: Some("term-fsw".into()),
        };
        let session = Session::spawn("pty-fsw".into(), &spec, Some(store.clone())).unwrap();

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
}
