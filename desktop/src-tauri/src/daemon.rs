//! App-side thin client to the detached `taime-session-daemon`.
//!
//! The webview can't open a Unix socket, so the app's Rust side connects to the
//! daemon and bridges its byte stream to a per-session Tauri `Channel` (the same
//! binary transport Step 0a established). On first use the app **spawns the
//! daemon detached** (it outlives the app) or **adopts** an already-running one
//! discovered at the deterministic socket path. Control ops (spawn/list/kill)
//! use short-lived connections; an attach uses a long-lived connection whose
//! incoming frames are pumped to the channel and whose outbound control
//! (input/resize/ack/detach) flows on the same connection.

use std::collections::HashMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use tauri::ipc::{Channel, InvokeResponseBody};
use taime_protocol::{
    cap, decode_server, encode_client, parse_frame, paths, AgentSpawnSpec, ClientMsg, Frame,
    ServerMsg, WorktreeInfo, MAGIC, MAX_FRAME_LEN, PROTOCOL_VERSION,
};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, Mutex};
use tokio_util::codec::{length_delimited, Framed, LengthDelimitedCodec};

type Conn = Framed<UnixStream, LengthDelimitedCodec>;

fn framed(stream: UnixStream) -> Conn {
    length_delimited::Builder::new()
        .max_frame_length(MAX_FRAME_LEN)
        .new_framed(stream)
}

/// Whether a handshake error is the daemon rejecting our `Hello` (magic/version/
/// token mismatch) — the signal that a stale, pre-upgrade daemon is running.
fn is_protocol_reject(err: &str) -> bool {
    err.contains("handshake rejected")
}

/// Per-attached-session handle: the outbound control channel + the offset
/// accounting needed to translate the frontend's processed-byte count into the
/// absolute `AckBytes` offset the daemon expects.
struct AttachHandle {
    input: mpsc::Sender<ClientMsg>,
    /// Absolute byte offset the grid repaint reflected (`seq_n`).
    seq_n: Arc<AtomicU64>,
    /// Length of the repaint message (frontend counts it in its processed total).
    repaint_len: Arc<AtomicU64>,
    ready: Arc<AtomicBool>,
}

pub struct DaemonClient {
    socket: PathBuf,
    daemon_bin: Option<PathBuf>,
    req_counter: AtomicU64,
    attaches: Mutex<HashMap<String, AttachHandle>>,
    /// Serializes stale-daemon replacement so two ops hitting the protocol-mismatch
    /// path at once don't each spawn a replacement.
    restart_lock: Mutex<()>,
}

impl DaemonClient {
    pub fn new(daemon_bin: Option<PathBuf>) -> Self {
        let socket = paths::default_socket_path().unwrap_or_else(|_| PathBuf::from("/tmp/taime.sock"));
        DaemonClient {
            socket,
            daemon_bin,
            req_counter: AtomicU64::new(1),
            attaches: Mutex::new(HashMap::new()),
            restart_lock: Mutex::new(()),
        }
    }

    fn next_req(&self) -> u64 {
        self.req_counter.fetch_add(1, Ordering::SeqCst)
    }

    /// Best-effort startup warm-up: ensure a daemon is running (spawn it if
    /// needed) so connect-only reads (workspace probe, list, graph) succeed
    /// immediately instead of returning their empty fallback before the first
    /// launch. Errors are ignored — the lazy path still spawns on first real use.
    pub async fn warm_up(&self) {
        if let Err(e) = self.ensure_running().await {
            eprintln!("[taime] daemon warm-up skipped: {e}");
        }
    }

    /// Ensure a daemon is reachable: adopt a running one, else spawn it detached.
    async fn ensure_running(&self) -> Result<(), String> {
        if UnixStream::connect(&self.socket).await.is_ok() {
            return Ok(()); // adopt the running daemon
        }
        let bin = self
            .daemon_bin
            .as_ref()
            .ok_or_else(|| "session daemon binary not found".to_string())?;
        spawn_detached(bin, &self.socket).map_err(|e| format!("spawn daemon: {e}"))?;
        for _ in 0..150 {
            if UnixStream::connect(&self.socket).await.is_ok() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Err("session daemon did not come up".into())
    }

    async fn handshake(&self, mut conn: Conn) -> Result<Conn, String> {
        let token = std::fs::read_to_string(paths::token_path(&self.socket)).unwrap_or_default();
        let hello = ClientMsg::Hello {
            magic: MAGIC,
            protocol_version: PROTOCOL_VERSION,
            capabilities: cap::CURRENT,
            attach_token: token,
        };
        send(&mut conn, &hello).await?;
        match read_server(&mut conn).await? {
            ServerMsg::HelloOk { .. } => Ok(conn),
            ServerMsg::HelloRejected { reason } => Err(format!("handshake rejected: {reason}")),
            other => Err(format!("unexpected handshake reply: {other:?}")),
        }
    }

    /// Connect to the (already-running) daemon and handshake. No spawn.
    async fn connect_once(&self) -> Result<Conn, String> {
        let stream = UnixStream::connect(&self.socket)
            .await
            .map_err(|e| format!("connect: {e}"))?;
        self.handshake(framed(stream)).await
    }

    /// Spawn-if-needed, then connect + handshake. Used by ops that REQUIRE a
    /// daemon (spawn, attach). If the daemon we reach speaks a different protocol
    /// (e.g. a pre-upgrade daemon still running — `HelloRejected`), replace it with
    /// the current binary and retry ONCE, so an app upgrade self-heals instead of
    /// requiring a manual `pkill`.
    async fn connect_handshake(&self) -> Result<Conn, String> {
        self.ensure_running().await?;
        match self.connect_once().await {
            Err(e) if is_protocol_reject(&e) => {
                eprintln!("[taime] daemon protocol mismatch ({e}); replacing stale daemon");
                self.restart_daemon().await?;
                self.connect_once().await
            }
            other => other,
        }
    }

    /// Connect-only handshake: returns `Ok(None)` if no daemon is reachable.
    /// Does NOT spawn one — used by enumeration/kill so we never boot a daemon
    /// just to list. A stale (wrong-protocol) daemon IS replaced, though, so the
    /// next poll heals it instead of erroring every tick.
    async fn try_connect_handshake(&self) -> Result<Option<Conn>, String> {
        if UnixStream::connect(&self.socket).await.is_err() {
            return Ok(None); // no daemon — caller uses its fallback
        }
        match self.connect_once().await {
            Ok(c) => Ok(Some(c)),
            Err(e) if is_protocol_reject(&e) => {
                eprintln!("[taime] daemon protocol mismatch ({e}); replacing stale daemon");
                // If we can't replace it (no binary), fall back rather than error.
                match self.restart_daemon().await {
                    Ok(()) => self.connect_once().await.map(Some),
                    Err(_) => Ok(None),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// The pid the live daemon recorded in its lock file (read only after a
    /// confirmed-live handshake, so it's the current lock holder — no PID reuse).
    fn read_lock_pid(&self) -> Option<i32> {
        std::fs::read_to_string(paths::lock_path(&self.socket)).ok()?.trim().parse::<i32>().ok()
    }

    /// Replace a stale/incompatible daemon: SIGTERM it (its handler kills its old
    /// agents + unlinks its runtime files), remove any leftover socket/token/lock,
    /// then spawn the current binary fresh and wait for it to come up.
    async fn restart_daemon(&self) -> Result<(), String> {
        let _guard = self.restart_lock.lock().await;
        // A concurrent op may have already replaced it while we waited — if the
        // daemon now handshakes cleanly, we're done.
        if self.connect_once().await.is_ok() {
            return Ok(());
        }
        if let Some(pid) = self.read_lock_pid() {
            // SAFETY: a plain kill(2); pid is the live daemon we just handshook.
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
        // Let it release the socket (its SIGTERM handler unlinks + exits).
        for _ in 0..50 {
            if UnixStream::connect(&self.socket).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Belt-and-suspenders: free the path so a fresh bind succeeds even if the
        // stale daemon is wedged (it then orphans and idle-shuts-down).
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_file(paths::token_path(&self.socket));
        let _ = std::fs::remove_file(paths::lock_path(&self.socket));

        let bin = self
            .daemon_bin
            .as_ref()
            .ok_or_else(|| "session daemon binary not found".to_string())?;
        spawn_detached(bin, &self.socket).map_err(|e| format!("respawn daemon: {e}"))?;
        for _ in 0..150 {
            if UnixStream::connect(&self.socket).await.is_ok() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Err("replacement daemon did not come up".into())
    }

    /// Connect-only liveness probe: true iff a live, protocol-compatible daemon
    /// answered the handshake. NEVER spawns (or replaces) a daemon — a pure
    /// probe, so the frontend can tell "daemon answered" from "fallback used"
    /// (`query()` returns its JSON fallback when no daemon is connectable,
    /// which otherwise masks a dead daemon).
    pub async fn ping(&self) -> bool {
        self.connect_once().await.is_ok()
    }

    /// Provision (or resolve) an isolated git worktree for an agent (Phase 3):
    /// the daemon mints the attribution key, runs `git worktree`, persists the
    /// row, and returns the worktree info (or shared-mode fallback).
    pub async fn provision_worktree(
        &self,
        project_root: String,
        provider: String,
        isolate: bool,
        task_id: Option<String>,
    ) -> Result<WorktreeInfo, String> {
        let req_id = self.next_req();
        let mut conn = self.connect_handshake().await?;
        send(
            &mut conn,
            &ClientMsg::ProvisionWorktree { req_id, project_root, provider, isolate, task_id },
        )
        .await?;
        match read_server(&mut conn).await? {
            ServerMsg::Worktree { info, .. } => Ok(info),
            ServerMsg::Error { message } => Err(message),
            other => Err(format!("unexpected worktree reply: {other:?}")),
        }
    }

    /// Generic daemon query RPC (Phase 6 route layer): `kind` + JSON `args` →
    /// a JSON string in the frontend's shape. Connect-only (returns `fallback`
    /// without spawning a daemon when none is running).
    pub async fn query(&self, kind: String, args: String, fallback: &str) -> Result<String, String> {
        let mut conn = match self.try_connect_handshake().await? {
            Some(c) => c,
            None => return Ok(fallback.to_string()),
        };
        let req_id = self.next_req();
        send(&mut conn, &ClientMsg::Query { req_id, kind, args }).await?;
        match read_server(&mut conn).await? {
            ServerMsg::QueryResult { json, .. } => Ok(json),
            ServerMsg::Error { message } => Err(message),
            other => Err(format!("unexpected query reply: {other:?}")),
        }
    }

    /// The daemon-side activity graph (agents + inter-agent edges) as a JSON
    /// string. Connect-only: returns an empty graph (without spawning a daemon)
    /// when none is running.
    pub async fn activity_graph(&self) -> Result<String, String> {
        let mut conn = match self.try_connect_handshake().await? {
            Some(c) => c,
            None => return Ok(r#"{"agents":[],"edges":[]}"#.to_string()),
        };
        let req_id = self.next_req();
        send(&mut conn, &ClientMsg::GetGraph { req_id }).await?;
        match read_server(&mut conn).await? {
            ServerMsg::Graph { json, .. } => Ok(json),
            ServerMsg::Error { message } => Err(message),
            other => Err(format!("unexpected graph reply: {other:?}")),
        }
    }

    /// Enqueue an inbox message for a live agent (Phase 5 message bus). The
    /// daemon delivers it into the receiver's stdin when it next goes idle.
    /// Returns the monotonic inbox id.
    pub async fn send_message(
        &self,
        sender: String,
        receiver: String,
        message: String,
    ) -> Result<i64, String> {
        let req_id = self.next_req();
        let mut conn = self.connect_handshake().await?;
        send(&mut conn, &ClientMsg::SendMessage { req_id, sender, receiver, message }).await?;
        match read_server(&mut conn).await? {
            ServerMsg::MessageQueued { id, .. } => Ok(id),
            ServerMsg::Error { message } => Err(message),
            other => Err(format!("unexpected send reply: {other:?}")),
        }
    }

    /// High-level agent spawn: the daemon's provider registry builds the command
    /// + MCP injection (the Phase-1 all-CLI path). Returns the session id.
    pub async fn spawn_agent(&self, spec: AgentSpawnSpec) -> Result<String, String> {
        let req_id = self.next_req();
        let mut conn = self.connect_handshake().await?;
        send(&mut conn, &ClientMsg::SpawnAgent { req_id, spec }).await?;
        match read_server(&mut conn).await? {
            ServerMsg::Spawned { session_id, .. } => Ok(session_id),
            ServerMsg::Error { message } => Err(message),
            other => Err(format!("unexpected spawn reply: {other:?}")),
        }
    }

    /// Enumerate the daemon's live sessions for discovery/adoption + liveness.
    /// Returns empty (without spawning) when no daemon is running.
    pub async fn list(&self) -> Result<Vec<taime_protocol::SessionSummary>, String> {
        let mut conn = match self.try_connect_handshake().await? {
            Some(c) => c,
            None => return Ok(Vec::new()),
        };
        let req_id = self.next_req();
        send(&mut conn, &ClientMsg::List { req_id }).await?;
        match read_server(&mut conn).await? {
            ServerMsg::Sessions { sessions, .. } => Ok(sessions),
            other => Err(format!("unexpected list reply: {other:?}")),
        }
    }

    pub async fn kill(&self, session_id: String) -> Result<(), String> {
        self.attaches.lock().await.remove(&session_id);
        // Connect-only: if no daemon is running the session is already gone.
        if let Some(mut conn) = self.try_connect_handshake().await? {
            send(&mut conn, &ClientMsg::Kill { session_id }).await?;
        }
        Ok(())
    }

    /// Attach `session_id` and pump its frames to `channel`. Stores a handle so
    /// `write`/`resize`/`ack`/`detach` can reach the same connection.
    pub async fn attach(
        &self,
        session_id: String,
        rows: u16,
        cols: u16,
        channel: Channel<InvokeResponseBody>,
    ) -> Result<(), String> {
        let mut conn = self.connect_handshake().await?;
        send(
            &mut conn,
            &ClientMsg::Attach { session_id: session_id.clone(), rows, cols },
        )
        .await?;

        let (input_tx, mut input_rx) = mpsc::channel::<ClientMsg>(256);
        let seq_n = Arc::new(AtomicU64::new(0));
        let repaint_len = Arc::new(AtomicU64::new(0));
        let ready = Arc::new(AtomicBool::new(false));

        let (seq2, rl2, ready2) = (seq_n.clone(), repaint_len.clone(), ready.clone());
        tauri::async_runtime::spawn(async move {
            let (mut sink, mut stream) = conn.split();
            loop {
                tokio::select! {
                    out = input_rx.recv() => {
                        // A closed input channel (handle dropped on detach / re-attach)
                        // is terminal — BREAK, don't `continue`, or this arm would
                        // busy-spin at 100% CPU (recv() resolves to None on every poll).
                        let Some(out) = out else { break };
                        if let Ok(bytes) = encode_client(&out) {
                            if sink.send(bytes).await.is_err() {
                                break;
                            }
                        }
                    }
                    frame = stream.next() => {
                        let Some(Ok(payload)) = frame else { break };
                        match parse_frame(payload) {
                            Ok(Frame::Data(_off, bytes)) => {
                                if channel.send(InvokeResponseBody::Raw(bytes.to_vec())).is_err() {
                                    break; // webview gone
                                }
                            }
                            Ok(Frame::Repaint(seq, bytes)) => {
                                seq2.store(seq, Ordering::SeqCst);
                                rl2.store(bytes.len() as u64, Ordering::SeqCst);
                                ready2.store(true, Ordering::SeqCst);
                                if channel.send(InvokeResponseBody::Raw(bytes.to_vec())).is_err() {
                                    break;
                                }
                            }
                            Ok(Frame::Control(body)) => {
                                if let Ok(msg) = decode_server(&body) {
                                    if !forward_control(&channel, &seq2, msg) {
                                        break; // Exited / channel gone
                                    }
                                }
                            }
                            Err(_) => break,
                        }
                    }
                }
            }
        });

        self.attaches.lock().await.insert(
            session_id,
            AttachHandle { input: input_tx, seq_n, repaint_len, ready },
        );
        Ok(())
    }

    async fn send_to_session(&self, session_id: &str, msg: ClientMsg) {
        let input = self.attaches.lock().await.get(session_id).map(|h| h.input.clone());
        if let Some(input) = input {
            let _ = input.send(msg).await;
        }
    }

    pub async fn write(&self, session_id: &str, bytes: Vec<u8>) {
        self.send_to_session(session_id, ClientMsg::Input { bytes }).await;
    }

    pub async fn resize(&self, session_id: &str, rows: u16, cols: u16) {
        self.send_to_session(session_id, ClientMsg::Resize { rows, cols }).await;
    }

    /// App-driven attribution checkpoint (the strongest boundary signal): the app
    /// knows when the user submitted a command. Routed to the attached session.
    pub async fn checkpoint(&self, session_id: &str, cause: String) {
        self.send_to_session(session_id, ClientMsg::Checkpoint { cause }).await;
    }

    /// Translate the frontend's cumulative processed-byte count (`processed`,
    /// which includes the repaint bytes) into the daemon's absolute byte offset:
    /// `seq_n + (processed - repaint_len)`.
    pub async fn ack(&self, session_id: &str, processed: u64) {
        let abs = {
            let map = self.attaches.lock().await;
            match map.get(session_id) {
                Some(h) if h.ready.load(Ordering::SeqCst) => {
                    let seq = h.seq_n.load(Ordering::SeqCst);
                    let rl = h.repaint_len.load(Ordering::SeqCst);
                    Some(seq + processed.saturating_sub(rl))
                }
                _ => None,
            }
        };
        if let Some(offset) = abs {
            self.send_to_session(session_id, ClientMsg::AckBytes { offset }).await;
        }
    }

    pub async fn detach(&self, session_id: &str) {
        self.send_to_session(session_id, ClientMsg::Detach).await;
        self.attaches.lock().await.remove(session_id);
    }
}

/// Forward a daemon control message to the frontend channel as a small JSON
/// control object. Returns false to stop pumping (process exited / channel gone).
fn forward_control(
    channel: &Channel<InvokeResponseBody>,
    seq_n: &AtomicU64,
    msg: ServerMsg,
) -> bool {
    let json = match msg {
        ServerMsg::AttachOk { rows, cols, seq_n: s, alt_screen } => {
            seq_n.store(s, Ordering::SeqCst);
            serde_json::json!({ "type": "attach_ok", "rows": rows, "cols": cols, "altScreen": alt_screen })
        }
        ServerMsg::Exited { code } => {
            let _ = channel.send(InvokeResponseBody::Json(
                serde_json::json!({ "type": "exit", "code": code }).to_string(),
            ));
            return false;
        }
        ServerMsg::TurnBoundary { turn } => serde_json::json!({
            "type": "turn",
            "epoch": turn.epoch,
            "startOffset": turn.start_offset,
            "endOffset": turn.end_offset,
            "startedCause": format!("{:?}", turn.started_cause),
            "endedCause": format!("{:?}", turn.ended_cause),
            "commandExit": turn.command_exit,
            // The daemon fills this from its per-session fs-watcher (Phase 6); the
            // app's live per-frame turn history shows which files the turn touched.
            "fsDirtyPaths": turn.fs_dirty_paths,
        }),
        ServerMsg::StatusChanged { status } => {
            serde_json::json!({ "type": "status", "status": status })
        }
        ServerMsg::FsDirty { paths } => serde_json::json!({ "type": "fs_dirty", "paths": paths }),
        ServerMsg::Error { message } => serde_json::json!({ "type": "error", "message": message }),
        _ => return true,
    };
    channel.send(InvokeResponseBody::Json(json.to_string())).is_ok()
}

async fn send(conn: &mut Conn, msg: &ClientMsg) -> Result<(), String> {
    let bytes = encode_client(msg).map_err(|e| format!("encode: {e}"))?;
    conn.send(bytes).await.map_err(|e| format!("send: {e}"))
}

async fn read_server(conn: &mut Conn) -> Result<ServerMsg, String> {
    loop {
        let payload = conn
            .next()
            .await
            .ok_or_else(|| "connection closed".to_string())?
            .map_err(|e| format!("read: {e}"))?;
        if let Ok(Frame::Control(body)) = parse_frame(payload) {
            return decode_server(&body).map_err(|e| format!("decode: {e}"));
        }
    }
}

/// Spawn the daemon fully detached (`setsid`, stdio → /dev/null, dropped without
/// wait/kill) so it outlives this app. NOT a Tauri sidecar.
fn spawn_detached(daemon_bin: &Path, socket: &Path) -> std::io::Result<u32> {
    use std::fs::OpenOptions;
    use std::os::unix::io::AsRawFd;

    let devnull_in = OpenOptions::new().read(true).open("/dev/null")?;
    let devnull_out = OpenOptions::new().write(true).open("/dev/null")?;
    let (in_fd, out_fd) = (devnull_in.as_raw_fd(), devnull_out.as_raw_fd());

    let mut cmd = std::process::Command::new(daemon_bin);
    cmd.arg("--socket").arg(socket);
    unsafe {
        // SAFETY: only async-signal-safe calls (setsid/dup2) — no alloc/lock/env.
        cmd.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::dup2(in_fd, libc::STDIN_FILENO) == -1
                || libc::dup2(out_fd, libc::STDOUT_FILENO) == -1
                || libc::dup2(out_fd, libc::STDERR_FILENO) == -1
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    let pid = child.id();
    // Drop the Child WITHOUT wait/kill: std's Child does not kill on drop, so the
    // daemon is orphaned to launchd and survives our exit/crash.
    drop(child);
    Ok(pid)
}

/// Resolve the daemon binary across dev + bundle layouts: a sibling of the app
/// exe (dev `target/<profile>/`, and macOS `.app/Contents/MacOS/`), or the
/// macOS bundle Resources dir (`bundle.resources` may flatten or nest under
/// `binaries/`). `None` → the caller surfaces "session daemon binary not found".
pub fn resolve_daemon_bin() -> Option<PathBuf> {
    const BIN: &str = "taime-session-daemon";
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let mut candidates = vec![dir.join(BIN)];
    if let Some(contents) = dir.parent() {
        let res = contents.join("Resources");
        candidates.push(res.join(BIN));
        candidates.push(res.join("binaries").join(BIN));
    }
    candidates.into_iter().find(|p| p.exists())
}
