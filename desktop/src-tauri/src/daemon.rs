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
    cap, decode_server, encode_client, parse_frame, paths, ClientMsg, Frame, ServerMsg, SpawnSpec,
    MAGIC, MAX_FRAME_LEN, PROTOCOL_VERSION,
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
}

impl DaemonClient {
    pub fn new(daemon_bin: Option<PathBuf>) -> Self {
        let socket = paths::default_socket_path().unwrap_or_else(|_| PathBuf::from("/tmp/taime.sock"));
        DaemonClient {
            socket,
            daemon_bin,
            req_counter: AtomicU64::new(1),
            attaches: Mutex::new(HashMap::new()),
        }
    }

    fn next_req(&self) -> u64 {
        self.req_counter.fetch_add(1, Ordering::SeqCst)
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

    /// Spawn-if-needed, then connect + handshake. Used by ops that REQUIRE a
    /// daemon (spawn, attach).
    async fn connect_handshake(&self) -> Result<Conn, String> {
        self.ensure_running().await?;
        let stream = UnixStream::connect(&self.socket)
            .await
            .map_err(|e| format!("connect: {e}"))?;
        self.handshake(framed(stream)).await
    }

    /// Connect-only handshake: returns `Ok(None)` if no daemon is reachable.
    /// Does NOT spawn one — used by enumeration/kill so we never boot a daemon
    /// just to list (or to kill a session that's already gone with it).
    async fn try_connect_handshake(&self) -> Result<Option<Conn>, String> {
        match UnixStream::connect(&self.socket).await {
            Ok(stream) => self.handshake(framed(stream)).await.map(Some),
            Err(_) => Ok(None),
        }
    }

    pub async fn spawn_session(&self, spec: SpawnSpec) -> Result<String, String> {
        let req_id = self.next_req();
        let mut conn = self.connect_handshake().await?;
        send(&mut conn, &ClientMsg::Spawn { req_id, spec }).await?;
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
        }),
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

/// Resolve the daemon binary: prefer a sibling of the app executable (dev +
/// next-to-exe bundles), else `None` (caller surfaces "not found").
pub fn resolve_daemon_bin() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let candidate = dir.join("taime-session-daemon");
    if candidate.exists() {
        return Some(candidate);
    }
    // Bundled macOS apps: the binary may live in ../Resources.
    let resources = dir.parent().map(|p| p.join("Resources").join("taime-session-daemon"));
    match resources {
        Some(p) if p.exists() => Some(p),
        _ => None,
    }
}
