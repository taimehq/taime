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

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use taime_protocol::{encode_data, encode_repaint, encode_server, ServerMsg, SessionSummary, SpawnSpec};
use tokio::sync::mpsc;

use crate::attribution::Attribution;
use crate::providers::{Cleanup, DaemonSessionSpec, Prepared};
use crate::{emulator, repaint};

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
}

struct SessionInner {
    id: String,
    program: String,
    cwd: String,
    attribution_key: Option<String>,
    /// Provider id (`claude_code`/`codex`/…) when spawned via the registry — the
    /// adapter to use for status inference (Phase 4). `None` for low-level spawns.
    #[allow(dead_code)]
    provider: Option<String>,
    /// Enters to send after a bracketed paste (CAO `paste_enter_count`); carried
    /// for the Phase-5 idle-gated stdin delivery.
    #[allow(dead_code)]
    paste_enter_count: u8,
    /// Spawn-time MCP injection to undo when the process exits (best-effort).
    cleanup: Cleanup,
    created_at_unix: u64,
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
    pub fn spawn(id: String, spec: &SpawnSpec) -> Result<Session, String> {
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
        Self::spawn_inner(id, &dspec, Cleanup::default(), None)
    }

    /// Spawn a registry-`Prepared` agent: the daemon-built command + MCP injection
    /// (the Phase-1 all-provider path). `provider` is the adapter id for status.
    pub fn spawn_prepared(
        id: String,
        prepared: Prepared,
        provider: Option<String>,
    ) -> Result<Session, String> {
        Self::spawn_inner(id, &prepared.spec, prepared.cleanup, provider)
    }

    /// Shared PTY setup for both spawn paths.
    fn spawn_inner(
        id: String,
        spec: &DaemonSessionSpec,
        cleanup: Cleanup,
        provider: Option<String>,
    ) -> Result<Session, String> {
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
            paste_enter_count: spec.paste_enter_count,
            cleanup,
            created_at_unix,
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
            }),
            resume: Condvar::new(),
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            child: Mutex::new(child),
            dead: AtomicBool::new(false),
        });

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
        if let Some(turn) = st.attr.checkpoint(off) {
            st.turn_dirty = false;
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
        if let Some(turn) = st.attr.quiet(off) {
            st.turn_dirty = false;
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

    pub fn summary(&self) -> SessionSummary {
        let st = self.inner.state.lock().unwrap();
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
            protocol_version: taime_protocol::PROTOCOL_VERSION,
        }
    }
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
            let turns = st.attr.feed(chunk, end);
            st.out_offset = end;
            st.last_output = Instant::now();
            st.turn_dirty = true;
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
