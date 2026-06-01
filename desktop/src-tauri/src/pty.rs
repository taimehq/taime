//! Rust-owned PTY manager for the Claude terminal path.
//!
//! This is the foundation of moving Claude Code off CAO/tmux/WebSocket onto a
//! PTY that Tauri owns directly. The CAO path is untouched and remains the
//! fallback; this manager is scoped to `claude` only for now.
//!
//! Ownership model (the important rule): a closed UI frame is NOT a killed
//! agent.
//!   * `close_view`  — stop emitting to the frontend; the process + reader keep
//!     running and output keeps accumulating in a scrollback buffer.
//!   * `reattach_view` — resume emitting and hand back the buffered scrollback
//!     so a fresh terminal can repaint current state.
//!   * `kill_session` — explicitly terminate the process; the reader then exits.
//!
//! Lifecycle hardening (baked in, not retrofitted):
//!   * the reader thread holds NO manager lock while blocking on a PTY read;
//!   * the reader exits cleanly on EOF/error and emits a single Exit event;
//!   * `shutdown_all` kills every child on app exit so nothing leaks.
//!
//! The manager is deliberately Tauri-agnostic: output is delivered through an
//! `EventSink` callback. The Tauri layer (`commands.rs`) supplies a sink that
//! emits `pty://{id}/data` / `pty://{id}/exit`; tests supply a collector.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

/// Cap on retained scrollback per session (bytes). Reattach replays this so a
/// reopened view can repaint; the agent's own redraw-on-resize fills the rest.
const MAX_BUFFER: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub enum PtyEvent {
    Data(Vec<u8>),
    Exit(Option<i32>),
}

/// Output sink: `(session_id, event)`. One sink serves all sessions; the
/// implementation decides how to route per id (emit a Tauri event, collect, …).
pub type EventSink = Arc<dyn Fn(&str, PtyEvent) + Send + Sync>;

struct Session {
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    attached: Arc<AtomicBool>,
    buffer: Arc<Mutex<Vec<u8>>>,
    cwd: String,
}

#[derive(Clone)]
pub struct PtyManager {
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    counter: Arc<AtomicU64>,
    sink: EventSink,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub id: String,
    pub cwd: String,
    pub attached: bool,
    pub alive: bool,
}

impl PtyManager {
    pub fn new(sink: EventSink) -> Self {
        PtyManager {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            counter: Arc::new(AtomicU64::new(0)),
            sink,
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
        // Start DETACHED: output buffers until a view reattaches. This lets a
        // view subscribe, then reattach to get a clean replay with no gap and no
        // duplicate (reattach atomically flips attached + snapshots the buffer
        // under the same lock the reader emits under).
        let attached = Arc::new(AtomicBool::new(false));
        let buffer = Arc::new(Mutex::new(Vec::<u8>::new()));

        let session = Session {
            master: Arc::new(Mutex::new(pair.master)),
            writer: Arc::new(Mutex::new(writer)),
            child: Arc::new(Mutex::new(child)),
            attached: attached.clone(),
            buffer: buffer.clone(),
            cwd: cwd.unwrap_or("").to_string(),
        };
        self.sessions.lock().unwrap().insert(id.clone(), session);

        // Reader thread: owns only Arc clones (NO manager lock held across reads).
        let sink = self.sink.clone();
        let tid = id.clone();
        let sessions = self.sessions.clone();
        thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let chunk = &buf[..n];
                        // Append + emit under the SAME buffer lock so reattach's
                        // (set-attached + snapshot) can't interleave to create a
                        // gap or a duplicate.
                        let mut b = buffer.lock().unwrap();
                        b.extend_from_slice(chunk);
                        if b.len() > MAX_BUFFER {
                            let drop_n = b.len() - MAX_BUFFER;
                            b.drain(0..drop_n);
                        }
                        if attached.load(Ordering::SeqCst) {
                            (sink)(&tid, PtyEvent::Data(chunk.to_vec()));
                        }
                    }
                }
            }
            // Process exited (EOF): reap the child, drop the session so pty_list
            // never reports a dead session, THEN emit Exit (running→exited→gone).
            if let Some(s) = sessions.lock().unwrap().remove(&tid) {
                let _ = s.child.lock().unwrap().wait();
            }
            (sink)(&tid, PtyEvent::Exit(None));
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
        w.write_all(data)
            .map_err(|e| format!("write failed: {e}"))?;
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

    /// Detach the view: keep the process + reader alive, stop emitting.
    pub fn close_view(&self, id: &str) {
        if let Some(a) = self
            .sessions
            .lock()
            .unwrap()
            .get(id)
            .map(|s| s.attached.clone())
        {
            a.store(false, Ordering::SeqCst);
        }
    }

    /// Reattach the view: resume emitting and return the scrollback to replay.
    pub fn reattach_view(&self, id: &str) -> Result<Vec<u8>, String> {
        let map = self.sessions.lock().unwrap();
        let s = map.get(id).ok_or_else(|| format!("no session {id}"))?;
        // Flip attached + snapshot under the buffer lock, atomic w.r.t. the
        // reader's append+emit (no gap, no duplicate across the handoff).
        let b = s.buffer.lock().unwrap();
        s.attached.store(true, Ordering::SeqCst);
        let snap = b.clone();
        drop(b);
        Ok(snap)
    }

    /// Explicitly terminate the process and drop the session.
    pub fn kill_session(&self, id: &str) {
        let session = self.sessions.lock().unwrap().remove(id);
        if let Some(s) = session {
            let _ = s.child.lock().unwrap().kill();
            let _ = s.child.lock().unwrap().wait();
            s.attached.store(false, Ordering::SeqCst);
        }
    }

    pub fn list_sessions(&self) -> Vec<SessionInfo> {
        let map = self.sessions.lock().unwrap();
        let out: Vec<SessionInfo> = map
            .iter()
            .map(|(id, s)| {
                let alive = matches!(s.child.lock().unwrap().try_wait(), Ok(None));
                SessionInfo {
                    id: id.clone(),
                    cwd: s.cwd.clone(),
                    attached: s.attached.load(Ordering::SeqCst),
                    alive,
                }
            })
            .collect();
        out
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

    /// A sink that forwards (id, event) over a channel for assertions.
    fn channel_sink() -> (EventSink, mpsc::Receiver<(String, PtyEvent)>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let sink: EventSink = Arc::new(move |id: &str, ev: PtyEvent| {
            let _ = tx.lock().unwrap().send((id.to_string(), ev));
        });
        (sink, rx)
    }

    fn collect_data(rx: &mpsc::Receiver<(String, PtyEvent)>, timeout: Duration) -> Vec<u8> {
        let mut out = Vec::new();
        let deadline = std::time::Instant::now() + timeout;
        while let Ok((_, ev)) =
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
    fn spawn_buffers_output_for_reattach() {
        // Spawn starts detached: output is buffered, not emitted live, and a
        // reattach replays it (so a freshly-opened view repaints). Use a
        // long-lived `cat` so the session is still alive at reattach (a
        // fast-exiting process is reaped + removed, which is a separate test).
        let (sink, _rx) = channel_sink();
        let mgr = PtyManager::new(sink);
        let id = mgr.spawn("cat", &[], None, &[], 24, 80).expect("spawn");
        mgr.write_input(&id, b"TAIME_OK\n").expect("write");
        std::thread::sleep(Duration::from_millis(300));
        let replay = mgr.reattach_view(&id).expect("reattach");
        assert!(
            String::from_utf8_lossy(&replay).contains("TAIME_OK"),
            "reattach should replay buffered output, got: {:?}",
            String::from_utf8_lossy(&replay)
        );
        mgr.kill_session(&id);
    }

    #[test]
    fn write_input_echoes_back_live_after_attach() {
        let (sink, rx) = channel_sink();
        let mgr = PtyManager::new(sink);
        // `cat` echoes stdin back to the PTY.
        let id = mgr.spawn("cat", &[], None, &[], 24, 80).expect("spawn");
        mgr.reattach_view(&id).expect("attach"); // attach so output emits live
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
    fn close_view_keeps_process_then_reattach_replays() {
        let (sink, rx) = channel_sink();
        let mgr = PtyManager::new(sink);
        let id = mgr.spawn("cat", &[], None, &[], 24, 80).expect("spawn");
        mgr.reattach_view(&id).expect("attach"); // attach the view first

        // Detach the view, then write — process stays alive, output is buffered
        // but NOT emitted while detached.
        mgr.close_view(&id);
        while rx.try_recv().is_ok() {} // drain any echo from before detach
        mgr.write_input(&id, b"while-detached\n").expect("write");
        std::thread::sleep(Duration::from_millis(300));
        // Nothing should have been emitted while detached.
        assert!(
            rx.try_recv().is_err(),
            "no events should be emitted while detached"
        );

        // Reattach replays the buffered scrollback (so a fresh view can repaint).
        let replay = mgr.reattach_view(&id).expect("reattach");
        assert!(
            String::from_utf8_lossy(&replay).contains("while-detached"),
            "reattach replay should include buffered output"
        );

        // process is still alive
        let alive = mgr.list_sessions().iter().any(|s| s.id == id && s.alive);
        assert!(alive, "process should still be alive after close_view");

        mgr.kill_session(&id);
        // After kill it should be gone from the list.
        assert!(!mgr.list_sessions().iter().any(|s| s.id == id));
    }

    #[test]
    fn missing_binary_errors_cleanly() {
        let (sink, _rx) = channel_sink();
        let mgr = PtyManager::new(sink);
        let res = mgr.spawn("definitely-not-a-real-binary-xyz", &[], None, &[], 24, 80);
        assert!(res.is_err(), "missing binary should return Err, not panic");
    }

    #[test]
    fn exited_session_is_removed_from_list() {
        let (sink, _rx) = channel_sink();
        let mgr = PtyManager::new(sink);
        let id = mgr
            .spawn("sh", &["-c".into(), "exit 0".into()], None, &[], 24, 80)
            .expect("spawn");
        // Wait for the process to exit + the reader to reap and drop it.
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
}
