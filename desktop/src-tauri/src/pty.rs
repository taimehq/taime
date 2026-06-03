//! Rust-owned PTY manager for the Claude terminal path.
//!
//! This is the foundation of moving Claude Code off CAO/tmux/WebSocket onto a
//! PTY that Tauri owns directly. The CAO path is untouched and remains the
//! fallback; this manager is scoped to `claude` only for now. It is also the
//! interim home of the **binary transport** (Step 0a) and **backpressure +
//! boundary-snapshot replay** (Step 0b); the standalone session daemon (Step 2)
//! supersedes it per-CLI as it reaches parity.
//!
//! Ownership model (the important rule): a closed UI frame is NOT a killed
//! agent.
//!   * `close_view`  — detach: stop emitting to the frontend; the process +
//!     reader keep running and output keeps accumulating in a scrollback buffer.
//!   * `attach`       — register a per-session sink, replay the (boundary-bounded)
//!     scrollback as the first message, then stream live output.
//!   * `kill_session` — explicitly terminate the process; the reader then exits.
//!
//! Transport (Step 0a): output is delivered as **raw bytes** through a per-session
//! [`SessionSink`]; the Tauri layer (`commands.rs`) wires that sink to a binary
//! `Channel<InvokeResponseBody>` (no base64, no JSON number-arrays). Tests wire a
//! collector. Data is delivered as [`PtyEvent::Data`]; process exit as
//! [`PtyEvent::Exit`] (only while attached — a detached exit is reconciled by the
//! `pty_list` poll, matching close_view's "no events while detached").
//!
//! Backpressure (Step 0b): the sink consumer acks the **highest processed byte
//! offset** (xterm's write-callback) via [`PtyManager::ack`]. When
//! `sent_offset - acked_offset` exceeds [`HIGH_WATERMARK`] the reader **pauses**
//! reading the PTY master (the kernel PTY buffer fills and the agent blocks on
//! write — natural flow control); it resumes below [`LOW_WATERMARK`]. Pausing
//! only applies while attached — a detached session never stalls its agent.
//!
//! Replay (Step 0b): the retained scrollback is trimmed at full-screen clear /
//! alt-screen boundaries, so reattach replays ~one screenful (bounded) instead of
//! a raw 1 MiB slice that could start mid-escape and desync.
//!
//! Lifecycle hardening (baked in, not retrofitted):
//!   * the reader thread holds NO manager lock while blocking on a PTY read;
//!   * the reader exits cleanly on EOF/error and emits a single Exit event;
//!   * `shutdown_all` kills every child on app exit so nothing leaks.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

/// Cap on retained scrollback per session (bytes) — a backstop beneath the
/// boundary-trim. Reattach replays this so a reopened view can repaint; the
/// agent's own redraw-on-resize fills the rest.
const MAX_BUFFER: usize = 1024 * 1024;

/// Backpressure high-water mark: pause reading the PTY when this many bytes are
/// in flight (sent to the client but not yet processed by xterm). Tunable — the
/// plan notes 64 KiB is too low (TUI bursts + webview jitter would stall agents);
/// these are deliberately generous for the single-hop in-app path. The daemon
/// (Step 2) exposes them as config.
const HIGH_WATERMARK: u64 = 2 * 1024 * 1024;
/// Resume reading once in-flight drops below this.
const LOW_WATERMARK: u64 = 512 * 1024;

#[derive(Debug, Clone)]
pub enum PtyEvent {
    Data(Vec<u8>),
    Exit(Option<i32>),
}

/// Per-session output sink. The Tauri layer supplies one that sends raw bytes
/// over a binary `Channel`; tests supply a collector. Cloneable + thread-safe so
/// the reader thread can hold it.
pub type SessionSink = Arc<dyn Fn(PtyEvent) + Send + Sync>;

/// Mutable per-session stream state, guarded by one mutex so attach/detach/ack
/// and the reader's append+emit can't interleave to create a gap, a duplicate,
/// or a lost backpressure signal. The companion `Condvar` parks the reader while
/// paused for backpressure.
struct Stream {
    /// Retained scrollback for replay, trimmed at screen-reset boundaries and
    /// capped at `MAX_BUFFER`.
    buffer: Vec<u8>,
    /// Current sink, or `None` when detached (`close_view`). "Attached" == Some.
    sink: Option<SessionSink>,
    /// Cumulative bytes delivered to the *current* sink since `attach`
    /// (including the replay snapshot). Resets on each attach.
    sent_offset: u64,
    /// Highest processed offset the client has acked (xterm write-callback).
    acked_offset: u64,
    /// True while the reader is parked for backpressure.
    paused: bool,
}

impl Stream {
    /// Append a freshly-read chunk to the replay buffer, trimming at the latest
    /// screen-reset boundary and enforcing the byte cap.
    fn append(&mut self, chunk: &[u8]) {
        let prev_len = self.buffer.len();
        self.buffer.extend_from_slice(chunk);
        // Look for a screen-reset boundary in the new bytes (plus a small overlap
        // so a boundary split across the chunk edge is still found). Trim
        // everything before the *latest* boundary: those bytes are no longer
        // visible, so replaying them would only risk a mid-escape desync.
        let scan_from = prev_len.saturating_sub(BOUNDARY_MAX_LEN - 1);
        if let Some(rel) = last_screen_reset(&self.buffer[scan_from..]) {
            let cut = scan_from + rel;
            if cut > 0 {
                self.buffer.drain(0..cut);
            }
        }
        if self.buffer.len() > MAX_BUFFER {
            let drop_n = self.buffer.len() - MAX_BUFFER;
            self.buffer.drain(0..drop_n);
        }
    }
}

/// Longest boundary marker we scan for (for the cross-chunk overlap window):
/// `\x1b[?1049h` is 8 bytes.
const BOUNDARY_MAX_LEN: usize = 8;

/// Screen-reset escape sequences. After any of these the visible screen is
/// repainted from scratch, so the replay buffer can start here.
const SCREEN_RESETS: &[&[u8]] = &[
    b"\x1b[2J",      // erase entire screen
    b"\x1b[3J",      // erase scrollback
    b"\x1b[?1049h",  // enter alt-screen (xterm DEC 1049)
    b"\x1b[?1047h",  // enter alt-screen (DEC 1047)
    b"\x1b[?47h",    // enter alt-screen (legacy)
    b"\x1bc",        // RIS — full reset
];

/// Return the byte offset *after* the latest screen-reset sequence in `hay`, or
/// `None` if there is no boundary. Trimming to this offset keeps only what is
/// still visible.
fn last_screen_reset(hay: &[u8]) -> Option<usize> {
    let mut best: Option<usize> = None;
    for pat in SCREEN_RESETS {
        // Find the last occurrence of this pattern.
        let mut start = 0;
        while let Some(pos) = find_subslice(&hay[start..], pat) {
            let abs = start + pos;
            let end = abs + pat.len();
            best = Some(best.map_or(end, |b| b.max(end)));
            start = abs + 1;
        }
    }
    best
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

struct Session {
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    stream: Arc<(Mutex<Stream>, Condvar)>,
    cwd: String,
}

#[derive(Clone)]
pub struct PtyManager {
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    counter: Arc<AtomicU64>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub id: String,
    pub cwd: String,
    pub attached: bool,
    pub alive: bool,
}

impl Default for PtyManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PtyManager {
    pub fn new() -> Self {
        PtyManager {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            counter: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Spawn `prog` in a PTY and start its reader thread. Returns the session id.
    pub fn spawn(
        &self,
        prog: &str,
        args: &[String],
        cwd: Option<&str>,
        extra_env: &[(String, String)],
        rows: u16,
        cols: u16,
    ) -> Result<String, String> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("openpty failed: {e}"))?;

        let mut cmd = CommandBuilder::new(prog);
        for a in args {
            cmd.arg(a);
        }
        if let Some(dir) = cwd {
            cmd.cwd(dir);
        }
        // Inherit the parent environment so claude finds PATH/HOME/auth, then
        // force a sane TERM for its TUI and apply any caller overrides.
        for (k, v) in std::env::vars() {
            cmd.env(k, v);
        }
        cmd.env("TERM", "xterm-256color");
        for (k, v) in extra_env {
            cmd.env(k, v);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("spawn '{prog}' failed: {e}"))?;
        // Drop the slave so EOF propagates to the reader when the child exits.
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("clone reader failed: {e}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("take writer failed: {e}"))?;

        let id = format!("pty-{:x}", self.counter.fetch_add(1, Ordering::SeqCst));
        // Start DETACHED: output buffers until a view attaches. Attach atomically
        // registers the sink + replays the buffer under the stream lock the reader
        // emits under, so there is no gap and no duplicate across the handoff.
        let stream = Arc::new((
            Mutex::new(Stream {
                buffer: Vec::new(),
                sink: None,
                sent_offset: 0,
                acked_offset: 0,
                paused: false,
            }),
            Condvar::new(),
        ));

        let session = Session {
            master: Arc::new(Mutex::new(pair.master)),
            writer: Arc::new(Mutex::new(writer)),
            child: Arc::new(Mutex::new(child)),
            stream: stream.clone(),
            cwd: cwd.unwrap_or("").to_string(),
        };
        self.sessions.lock().unwrap().insert(id.clone(), session);

        // Reader thread: owns only Arc clones (NO manager lock held across reads).
        let tid = id.clone();
        let sessions = self.sessions.clone();
        thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; 8192];
            loop {
                // Backpressure: park here while paused (attached + over the high
                // watermark). Releases the lock while waiting; the ack path wakes
                // us. NB: we are not holding the lock during the blocking read.
                {
                    let (lock, cv) = &*stream;
                    let mut st = lock.lock().unwrap();
                    while st.paused {
                        st = cv.wait(st).unwrap();
                    }
                }
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let chunk = &buf[..n];
                        let (lock, _cv) = &*stream;
                        let mut st = lock.lock().unwrap();
                        // Append+emit under the SAME lock so attach's
                        // (register-sink + snapshot + replay) can't interleave to
                        // create a gap or a duplicate.
                        st.append(chunk);
                        if let Some(sink) = st.sink.clone() {
                            sink(PtyEvent::Data(chunk.to_vec()));
                            st.sent_offset += n as u64;
                            // Engage backpressure if the client is falling behind.
                            if st.sent_offset.saturating_sub(st.acked_offset) > HIGH_WATERMARK {
                                st.paused = true;
                            }
                        }
                    }
                }
            }
            // Process exited (EOF): reap the child, drop the session so pty_list
            // never reports a dead session, THEN emit Exit (running→exited→gone).
            if let Some(s) = sessions.lock().unwrap().remove(&tid) {
                let _ = s.child.lock().unwrap().wait();
            }
            let (lock, _cv) = &*stream;
            let sink = lock.lock().unwrap().sink.clone();
            if let Some(sink) = sink {
                sink(PtyEvent::Exit(None));
            }
        });

        Ok(id)
    }

    pub fn write_input(&self, id: &str, data: &[u8]) -> Result<(), String> {
        let writer = {
            let map = self.sessions.lock().unwrap();
            map.get(id).map(|s| s.writer.clone())
        }
        .ok_or_else(|| format!("no session {id}"))?;
        let mut w = writer.lock().unwrap();
        w.write_all(data).map_err(|e| format!("write failed: {e}"))?;
        w.flush().map_err(|e| format!("flush failed: {e}"))?;
        Ok(())
    }

    pub fn resize(&self, id: &str, rows: u16, cols: u16) -> Result<(), String> {
        let master = {
            let map = self.sessions.lock().unwrap();
            map.get(id).map(|s| s.master.clone())
        }
        .ok_or_else(|| format!("no session {id}"))?;
        let res = master.lock().unwrap().resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        res.map_err(|e| format!("resize failed: {e}"))
    }

    /// Detach the view: keep the process + reader alive, stop emitting. Clears
    /// any backpressure pause so the reader keeps buffering while detached (a
    /// detached agent must never stall waiting for an absent client to ack).
    pub fn close_view(&self, id: &str) {
        let stream = {
            let map = self.sessions.lock().unwrap();
            map.get(id).map(|s| s.stream.clone())
        };
        if let Some(stream) = stream {
            let (lock, cv) = &*stream;
            let mut st = lock.lock().unwrap();
            st.sink = None;
            st.paused = false;
            cv.notify_all();
        }
    }

    /// Attach a sink: register it, replay the (boundary-bounded) scrollback as the
    /// first message, then stream live output. Replaces the old
    /// `reattach_view`-returns-bytes path so replay + live share one ordered pipe.
    pub fn attach(&self, id: &str, sink: SessionSink) -> Result<(), String> {
        let stream = {
            let map = self.sessions.lock().unwrap();
            map.get(id).map(|s| s.stream.clone())
        }
        .ok_or_else(|| format!("no session {id}"))?;
        let (lock, cv) = &*stream;
        let mut st = lock.lock().unwrap();
        // Fresh client → reset the offset accounting; the client also counts from
        // zero at mount, so sent/acked stay aligned.
        st.sent_offset = 0;
        st.acked_offset = 0;
        st.paused = false;
        let snapshot = st.buffer.clone();
        st.sink = Some(sink.clone());
        if !snapshot.is_empty() {
            let n = snapshot.len() as u64;
            sink(PtyEvent::Data(snapshot));
            st.sent_offset += n;
            if st.sent_offset.saturating_sub(st.acked_offset) > HIGH_WATERMARK {
                st.paused = true;
            }
        }
        cv.notify_all();
        Ok(())
    }

    /// Backpressure ack: record the highest processed offset and resume the reader
    /// if it had paused and in-flight has drained below the low watermark.
    pub fn ack(&self, id: &str, offset: u64) {
        let stream = {
            let map = self.sessions.lock().unwrap();
            map.get(id).map(|s| s.stream.clone())
        };
        if let Some(stream) = stream {
            let (lock, cv) = &*stream;
            let mut st = lock.lock().unwrap();
            if offset > st.acked_offset {
                st.acked_offset = offset;
            }
            if st.paused && st.sent_offset.saturating_sub(st.acked_offset) < LOW_WATERMARK {
                st.paused = false;
                cv.notify_all();
            }
        }
    }

    /// Explicitly terminate the process and drop the session.
    pub fn kill_session(&self, id: &str) {
        let session = self.sessions.lock().unwrap().remove(id);
        if let Some(s) = session {
            let _ = s.child.lock().unwrap().kill();
            let _ = s.child.lock().unwrap().wait();
            let (lock, cv) = &*s.stream;
            let mut st = lock.lock().unwrap();
            st.sink = None;
            st.paused = false;
            cv.notify_all();
        }
    }

    pub fn list_sessions(&self) -> Vec<SessionInfo> {
        let map = self.sessions.lock().unwrap();
        map.iter()
            .map(|(id, s)| {
                let alive = matches!(s.child.lock().unwrap().try_wait(), Ok(None));
                let attached = s.stream.0.lock().unwrap().sink.is_some();
                SessionInfo {
                    id: id.clone(),
                    cwd: s.cwd.clone(),
                    attached,
                    alive,
                }
            })
            .collect()
    }

    /// Kill every session — called on app shutdown so no child leaks.
    pub fn shutdown_all(&self) {
        let ids: Vec<String> = self.sessions.lock().unwrap().keys().cloned().collect();
        for id in ids {
            self.kill_session(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// A sink that forwards events over a channel for assertions.
    fn channel_sink() -> (SessionSink, mpsc::Receiver<PtyEvent>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let sink: SessionSink = Arc::new(move |ev: PtyEvent| {
            let _ = tx.lock().unwrap().send(ev);
        });
        (sink, rx)
    }

    fn collect_data(rx: &mpsc::Receiver<PtyEvent>, timeout: Duration) -> Vec<u8> {
        let mut out = Vec::new();
        let deadline = std::time::Instant::now() + timeout;
        while let Ok(ev) =
            rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
        {
            match ev {
                PtyEvent::Data(d) => out.extend_from_slice(&d),
                PtyEvent::Exit(_) => break,
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
        }
        out
    }

    #[test]
    fn spawn_buffers_output_for_attach() {
        // Spawn starts detached: output is buffered, not emitted live, and an
        // attach replays it (so a freshly-opened view repaints). Use a long-lived
        // `cat` so the session is still alive at attach.
        let mgr = PtyManager::new();
        let id = mgr.spawn("cat", &[], None, &[], 24, 80).expect("spawn");
        mgr.write_input(&id, b"TAIME_OK\n").expect("write");
        std::thread::sleep(Duration::from_millis(300));
        let (sink, rx) = channel_sink();
        mgr.attach(&id, sink).expect("attach");
        let replay = collect_data(&rx, Duration::from_millis(300));
        assert!(
            String::from_utf8_lossy(&replay).contains("TAIME_OK"),
            "attach should replay buffered output, got: {:?}",
            String::from_utf8_lossy(&replay)
        );
        mgr.kill_session(&id);
    }

    #[test]
    fn write_input_echoes_back_live_after_attach() {
        let mgr = PtyManager::new();
        let (sink, rx) = channel_sink();
        // `cat` echoes stdin back to the PTY.
        let id = mgr.spawn("cat", &[], None, &[], 24, 80).expect("spawn");
        mgr.attach(&id, sink).expect("attach"); // attach so output emits live
        mgr.write_input(&id, b"ping-pong\n").expect("write");
        let out = collect_data(&rx, Duration::from_secs(3));
        assert!(
            String::from_utf8_lossy(&out).contains("ping-pong"),
            "got: {:?}",
            String::from_utf8_lossy(&out)
        );
        mgr.resize(&id, 40, 120).expect("resize should not error");
        mgr.kill_session(&id);
    }

    #[test]
    fn close_view_keeps_process_then_attach_replays() {
        let mgr = PtyManager::new();
        let (sink, rx) = channel_sink();
        let id = mgr.spawn("cat", &[], None, &[], 24, 80).expect("spawn");
        mgr.attach(&id, sink).expect("attach"); // attach the view first

        // Detach the view, then write — process stays alive, output is buffered
        // but NOT emitted while detached.
        mgr.close_view(&id);
        while rx.try_recv().is_ok() {} // drain any echo from before detach
        mgr.write_input(&id, b"while-detached\n").expect("write");
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            rx.try_recv().is_err(),
            "no events should be emitted while detached"
        );

        // Reattach replays the buffered scrollback (so a fresh view can repaint).
        let (sink2, rx2) = channel_sink();
        mgr.attach(&id, sink2).expect("reattach");
        let replay = collect_data(&rx2, Duration::from_millis(300));
        assert!(
            String::from_utf8_lossy(&replay).contains("while-detached"),
            "reattach replay should include buffered output"
        );

        let alive = mgr.list_sessions().iter().any(|s| s.id == id && s.alive);
        assert!(alive, "process should still be alive after close_view");

        mgr.kill_session(&id);
        assert!(!mgr.list_sessions().iter().any(|s| s.id == id));
    }

    #[test]
    fn missing_binary_errors_cleanly() {
        let mgr = PtyManager::new();
        let res = mgr.spawn("definitely-not-a-real-binary-xyz", &[], None, &[], 24, 80);
        assert!(res.is_err(), "missing binary should return Err, not panic");
    }

    #[test]
    fn exited_session_is_removed_from_list() {
        let mgr = PtyManager::new();
        let id = mgr
            .spawn("sh", &["-c".into(), "exit 0".into()], None, &[], 24, 80)
            .expect("spawn");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while mgr.list_sessions().iter().any(|s| s.id == id) && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !mgr.list_sessions().iter().any(|s| s.id == id),
            "an exited session must be removed from the manager (no dead sessions in list)"
        );
    }

    #[test]
    fn replay_trims_to_last_screen_reset_boundary() {
        // The boundary-snapshot replay (Step 0b): bytes before the latest
        // screen-reset escape are dropped, so reattach replays ~one screenful and
        // never starts mid-escape on a long session.
        let mut st = Stream {
            buffer: Vec::new(),
            sink: None,
            sent_offset: 0,
            acked_offset: 0,
            paused: false,
        };
        st.append(b"old scrollback that should be dropped");
        st.append(b"\x1b[2Jfresh screen contents");
        let s = String::from_utf8_lossy(&st.buffer);
        assert!(
            !s.contains("old scrollback"),
            "pre-clear bytes must be trimmed, got: {s:?}"
        );
        assert!(s.contains("fresh screen contents"));
        assert!(s.starts_with("fresh screen contents"), "trim to AFTER the reset");
    }

    #[test]
    fn boundary_scan_handles_split_across_chunks() {
        // A reset sequence split across two appends is still found (overlap scan).
        let mut st = Stream {
            buffer: Vec::new(),
            sink: None,
            sent_offset: 0,
            acked_offset: 0,
            paused: false,
        };
        st.append(b"keep-out\x1b[?10");
        st.append(b"49hkeep-in");
        let s = String::from_utf8_lossy(&st.buffer);
        assert!(!s.contains("keep-out"), "split boundary must trim, got: {s:?}");
        assert_eq!(&s[..], "keep-in");
    }
}
