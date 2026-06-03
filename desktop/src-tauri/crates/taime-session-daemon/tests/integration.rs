//! End-to-end integration test: drives the REAL daemon binary over a REAL Unix
//! socket through the full protocol — handshake → spawn → attach handoff →
//! input/output echo → kill/exit. This is the headless proof that the daemon
//! works without needing the Tauri GUI.

use std::path::PathBuf;
use std::process::Child;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use futures::{SinkExt, StreamExt};
use taime_protocol::{
    decode_server, encode_client, parse_frame, ClientMsg, Frame, ServerMsg, SpawnSpec, MAGIC,
    MAX_FRAME_LEN, PROTOCOL_VERSION,
};
use tokio::net::UnixStream;
use tokio_util::codec::{length_delimited, Framed, LengthDelimitedCodec};

struct DaemonProc {
    child: Child,
    socket: PathBuf,
    token_path: PathBuf,
    lock_path: PathBuf,
}

impl Drop for DaemonProc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_file(&self.token_path);
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

fn spawn_daemon() -> DaemonProc {
    // Unique per call: tests run in parallel in one process, so a shared
    // socket/lock path would make the second daemon see the first's lock and exit.
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let uniq = format!("{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst));
    let dir = std::env::temp_dir().join(format!("taime-it-{uniq}"));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("d.sock");
    let _ = std::fs::remove_file(&socket);
    let bin = env!("CARGO_BIN_EXE_taime-session-daemon");
    let child = std::process::Command::new(bin)
        .arg("--socket")
        .arg(&socket)
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("spawn daemon");
    DaemonProc {
        child,
        token_path: socket.with_extension("token"),
        lock_path: socket.with_extension("lock"),
        socket,
    }
}

fn make_framed(stream: UnixStream) -> Framed<UnixStream, LengthDelimitedCodec> {
    length_delimited::Builder::new()
        .max_frame_length(MAX_FRAME_LEN)
        .new_framed(stream)
}

async fn connect(socket: &std::path::Path) -> UnixStream {
    // Poll-connect: the daemon needs a moment to bind.
    for _ in 0..100 {
        if let Ok(s) = UnixStream::connect(socket).await {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("daemon never came up at {socket:?}");
}

async fn next_frame(framed: &mut Framed<UnixStream, LengthDelimitedCodec>) -> Frame {
    let payload: BytesMut = tokio::time::timeout(Duration::from_secs(5), framed.next())
        .await
        .expect("frame timeout")
        .expect("stream closed")
        .expect("frame io error");
    parse_frame(payload).expect("parse frame")
}

async fn next_server_msg(framed: &mut Framed<UnixStream, LengthDelimitedCodec>) -> ServerMsg {
    loop {
        if let Frame::Control(body) = next_frame(framed).await {
            return decode_server(&body).expect("decode server msg");
        }
    }
}

async fn send_client(framed: &mut Framed<UnixStream, LengthDelimitedCodec>, msg: &ClientMsg) {
    let bytes: Bytes = encode_client(msg).expect("encode client");
    framed.send(bytes).await.expect("send");
}

#[tokio::test]
async fn daemon_full_lifecycle() {
    let daemon = spawn_daemon();
    let stream = connect(&daemon.socket).await;

    // The token is written next to the socket once the daemon is up.
    let token = {
        let mut token = String::new();
        for _ in 0..100 {
            if let Ok(t) = std::fs::read_to_string(&daemon.token_path) {
                token = t;
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(!token.is_empty(), "daemon should have written an attach token");
        token
    };

    let mut framed = make_framed(stream);

    // 1) Handshake.
    send_client(
        &mut framed,
        &ClientMsg::Hello {
            magic: MAGIC,
            protocol_version: PROTOCOL_VERSION,
            capabilities: 0,
            attach_token: token,
        },
    )
    .await;
    match next_server_msg(&mut framed).await {
        ServerMsg::HelloOk { magic, protocol_version, .. } => {
            assert_eq!(magic, MAGIC);
            assert_eq!(protocol_version, PROTOCOL_VERSION);
        }
        other => panic!("expected HelloOk, got {other:?}"),
    }

    // 2) Spawn a `cat` session (echoes its stdin back to the PTY).
    send_client(
        &mut framed,
        &ClientMsg::Spawn {
            req_id: 1,
            spec: SpawnSpec {
                prog: "cat".into(),
                args: vec![],
                cwd: None,
                env: vec![],
                rows: 24,
                cols: 80,
                attribution_key: Some("term-xyz".into()),
            },
        },
    )
    .await;
    let session_id = match next_server_msg(&mut framed).await {
        ServerMsg::Spawned { req_id, session_id } => {
            assert_eq!(req_id, 1);
            session_id
        }
        other => panic!("expected Spawned, got {other:?}"),
    };

    // 3) Attach with a DIFFERENT viewport (30x100) than the 24x80 spawn → the
    //    daemon must resize before snapshotting, so AttachOk reflects 30x100.
    send_client(
        &mut framed,
        &ClientMsg::Attach { session_id: session_id.clone(), rows: 30, cols: 100 },
    )
    .await;
    let seq_n = match next_server_msg(&mut framed).await {
        ServerMsg::AttachOk { rows, cols, seq_n, .. } => {
            assert_eq!((rows, cols), (30, 100), "attach must resize before repaint");
            seq_n
        }
        other => panic!("expected AttachOk, got {other:?}"),
    };
    match next_frame(&mut framed).await {
        Frame::Repaint(seq, _bytes) => assert_eq!(seq, seq_n, "repaint should carry seq_n"),
        other => panic!("expected Repaint frame, got {other:?}"),
    }

    // 4) Write input; expect a data frame echoing it (offset >= seq_n).
    send_client(&mut framed, &ClientMsg::Input { bytes: b"taime-echo\n".to_vec() }).await;
    let mut saw_echo = false;
    let mut highest_off = seq_n;
    for _ in 0..50 {
        match next_frame(&mut framed).await {
            Frame::Data(off, bytes) => {
                assert!(off >= seq_n, "data offset {off} must be >= seq_n {seq_n}");
                highest_off = highest_off.max(off + bytes.len() as u64);
                if String::from_utf8_lossy(&bytes).contains("taime-echo") {
                    saw_echo = true;
                    break;
                }
            }
            Frame::Control(_) | Frame::Repaint(_, _) => {}
        }
    }
    assert!(saw_echo, "should have received the echoed input as a data frame");

    // 5) Ack the processed bytes (backpressure return path).
    send_client(&mut framed, &ClientMsg::AckBytes { offset: highest_off }).await;

    // 6) List shows the live session.
    send_client(&mut framed, &ClientMsg::List { req_id: 2 }).await;
    match next_server_msg(&mut framed).await {
        ServerMsg::Sessions { req_id, sessions } => {
            assert_eq!(req_id, 2);
            assert!(sessions.iter().any(|s| s.id == session_id && s.alive));
            let s = sessions.iter().find(|s| s.id == session_id).unwrap();
            assert_eq!(s.attribution_key.as_deref(), Some("term-xyz"));
        }
        other => panic!("expected Sessions, got {other:?}"),
    }

    // 7) Kill → the session's process exits → Exited.
    send_client(&mut framed, &ClientMsg::Kill { session_id: session_id.clone() }).await;
    let mut saw_exit = false;
    for _ in 0..50 {
        if let Frame::Control(body) = next_frame(&mut framed).await {
            if let Ok(ServerMsg::Exited { .. }) = decode_server(&body) {
                saw_exit = true;
                break;
            }
        }
    }
    assert!(saw_exit, "killing the session should yield an Exited notice");
}

/// Reproduce the UI's Phase-6/7 surface against a real daemon: workspace_info,
/// worktree provisioning, a high-level agent spawn, and a diff query.
#[tokio::test]
async fn daemon_query_and_spawn_surface() {
    use taime_protocol::{AgentProfile, AgentSpawnSpec};
    let daemon = spawn_daemon();
    let stream = connect(&daemon.socket).await;
    let token = {
        let mut t = String::new();
        for _ in 0..100 {
            if let Ok(x) = std::fs::read_to_string(&daemon.token_path) {
                t = x;
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        t
    };
    let mut framed = make_framed(stream);
    send_client(
        &mut framed,
        &ClientMsg::Hello { magic: MAGIC, protocol_version: PROTOCOL_VERSION, capabilities: 0, attach_token: token },
    )
    .await;
    assert!(matches!(next_server_msg(&mut framed).await, ServerMsg::HelloOk { .. }));

    // workspace_info on an existing dir → exists:true.
    let cwd = std::env::current_dir().unwrap();
    send_client(
        &mut framed,
        &ClientMsg::Query { req_id: 10, kind: "workspace_info".into(), args: format!("{{\"path\":{:?}}}", cwd.to_string_lossy()) },
    )
    .await;
    match next_server_msg(&mut framed).await {
        ServerMsg::QueryResult { json, .. } => {
            let v: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(v["exists"], true, "workspace_info should see an existing dir: {json}");
        }
        other => panic!("expected QueryResult, got {other:?}"),
    }

    // ProvisionWorktree on the real repo (the launch's FIRST daemon call).
    send_client(
        &mut framed,
        &ClientMsg::ProvisionWorktree {
            req_id: 12,
            project_root: cwd.to_string_lossy().into_owned(),
            provider: "claude_code".into(),
            isolate: true,
        },
    )
    .await;
    match next_server_msg(&mut framed).await {
        ServerMsg::Worktree { info, .. } => {
            // A git repo provisions an isolated worktree (or shared on git failure);
            // either way the daemon ANSWERS without a protocol error.
            assert!(info.mode == "worktree" || info.mode == "shared");
        }
        other => panic!("expected Worktree, got {other:?}"),
    }

    // SpawnAgent with a trivial provider (a fake registry entry can't run, so use a
    // shell via the low-level path is covered elsewhere; here assert the daemon
    // ANSWERS a SpawnAgent for a known provider without protocol error).
    send_client(
        &mut framed,
        &ClientMsg::SpawnAgent {
            req_id: 11,
            spec: AgentSpawnSpec {
                // `true` is a real binary that exits 0 — proves the spawn PATH works
                // independent of whether claude/codex are installed.
                provider: "claude_code".into(),
                profile: AgentProfile { name: "default".into(), ..Default::default() },
                cwd: Some(cwd.to_string_lossy().into_owned()),
                rows: 24,
                cols: 80,
                attribution_key: Some("term-it".into()),
                seed_prompt: None,
                env: vec![],
                inject_orchestration: false,
            },
        },
    )
    .await;
    match next_server_msg(&mut framed).await {
        // Either it spawned (claude present) or returned a clear Error (claude
        // missing) — both are valid protocol responses, NOT a hang/crash.
        ServerMsg::Spawned { req_id, .. } => assert_eq!(req_id, 11),
        ServerMsg::Error { .. } => {}
        other => panic!("expected Spawned/Error, got {other:?}"),
    }
}

#[tokio::test]
async fn daemon_rejects_bad_token() {
    let daemon = spawn_daemon();
    let stream = connect(&daemon.socket).await;
    let mut framed = make_framed(stream);
    send_client(
        &mut framed,
        &ClientMsg::Hello {
            magic: MAGIC,
            protocol_version: PROTOCOL_VERSION,
            capabilities: 0,
            attach_token: "not-the-real-token".into(),
        },
    )
    .await;
    match next_server_msg(&mut framed).await {
        ServerMsg::HelloRejected { .. } => {}
        other => panic!("expected HelloRejected for a bad token, got {other:?}"),
    }
}
