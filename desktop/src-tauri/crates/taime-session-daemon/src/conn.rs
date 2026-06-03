//! Per-connection handler: versioned handshake, then dispatch the control set.
//! Each connection has an outbound frame channel drained by a writer task; the
//! session streams data frames into that same channel when this connection is
//! the attached client.

use std::sync::Arc;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use taime_protocol::{
    cap, decode_client, encode_server, parse_frame, versions_compatible, ClientMsg, Frame,
    ServerMsg, MAGIC, MAX_FRAME_LEN, PROTOCOL_VERSION,
};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio_util::codec::{LengthDelimitedCodec, length_delimited};

use crate::listener::token_matches;
use crate::manager::Manager;
use crate::session::Session;

fn framed(stream: UnixStream) -> tokio_util::codec::Framed<UnixStream, LengthDelimitedCodec> {
    length_delimited::Builder::new()
        .max_frame_length(MAX_FRAME_LEN)
        .new_framed(stream)
}

/// Handle one client connection until it closes. `token` is the daemon's current
/// attach token (already gated by the peer-uid check at accept time).
pub async fn handle(stream: UnixStream, manager: Arc<Manager>, token: String) {
    manager.conn_opened();
    let conn_id = manager.next_conn_id();

    let (mut sink, mut reader) = framed(stream).split();
    // Unbounded so the session reader can enqueue under its state lock without
    // blocking; flow control is the ack watermark, which pauses the reader.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Bytes>();

    // Writer task: outbound frames → socket. Ends when out_tx is dropped.
    let writer = tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            if sink.send(frame).await.is_err() {
                break;
            }
        }
    });

    let mut attached: Option<Session> = None;
    let mut hello_ok = false;

    while let Some(frame) = reader.next().await {
        let payload = match frame {
            Ok(p) => p,
            Err(_) => break,
        };
        let body = match parse_frame(payload) {
            Ok(Frame::Control(b)) => b,
            Ok(_) => continue, // app→daemon never sends data/repaint frames
            Err(_) => break,
        };
        let msg = match decode_client(&body) {
            Ok(m) => m,
            Err(_) => break,
        };

        // First message must be a valid Hello.
        if !hello_ok {
            match msg {
                ClientMsg::Hello { magic, protocol_version, attach_token, .. } => {
                    let ok = magic == MAGIC
                        && versions_compatible(protocol_version, PROTOCOL_VERSION)
                        && token_matches(&token, &attach_token);
                    if ok {
                        hello_ok = true;
                        send(
                            &out_tx,
                            &ServerMsg::HelloOk {
                                magic: MAGIC,
                                protocol_version: PROTOCOL_VERSION,
                                capabilities: cap::CURRENT,
                                daemon_version: env!("CARGO_PKG_VERSION").to_string(),
                            },
                        )
                        .await;
                    } else {
                        send(
                            &out_tx,
                            &ServerMsg::HelloRejected {
                                reason: "magic/version/token mismatch".into(),
                            },
                        )
                        .await;
                        break;
                    }
                }
                _ => break, // protocol violation: control before handshake
            }
            continue;
        }

        manager.touch();
        match msg {
            ClientMsg::Hello { .. } => { /* ignore duplicate handshake */ }
            ClientMsg::Spawn { req_id, spec } => match manager.spawn(spec) {
                Ok(session_id) => send(&out_tx, &ServerMsg::Spawned { req_id, session_id }).await,
                Err(e) => send(&out_tx, &ServerMsg::Error { message: e }).await,
            },
            ClientMsg::SpawnAgent { req_id, spec } => match manager.spawn_agent(spec) {
                Ok(session_id) => send(&out_tx, &ServerMsg::Spawned { req_id, session_id }).await,
                Err(e) => send(&out_tx, &ServerMsg::Error { message: e }).await,
            },
            ClientMsg::ProvisionWorktree { req_id, project_root, provider, isolate } => {
                // git worktree shells out (blocking); keep it off the async worker.
                let mgr = manager.clone();
                match tokio::task::spawn_blocking(move || {
                    mgr.provision_worktree(project_root, provider, isolate)
                })
                .await
                {
                    Ok(info) => send(&out_tx, &ServerMsg::Worktree { req_id, info }).await,
                    Err(_) => {
                        send(&out_tx, &ServerMsg::Error { message: "worktree provisioning failed".into() })
                            .await
                    }
                }
            }
            ClientMsg::List { req_id } => {
                send(&out_tx, &ServerMsg::Sessions { req_id, sessions: manager.list() }).await
            }
            ClientMsg::Attach { session_id, rows, cols } => match manager.get(&session_id) {
                Some(session) => {
                    // Detach any previously attached session on this connection.
                    if let Some(prev) = &attached {
                        prev.detach(conn_id);
                    }
                    if let Err(e) = session.attach(conn_id, rows, cols, out_tx.clone()) {
                        send(&out_tx, &ServerMsg::Error { message: e }).await;
                    } else {
                        attached = Some(session);
                    }
                }
                None => {
                    send(&out_tx, &ServerMsg::Error { message: format!("no session {session_id}") })
                        .await
                }
            },
            ClientMsg::Detach => {
                if let Some(s) = attached.take() {
                    s.detach(conn_id);
                }
            }
            ClientMsg::Input { bytes } => {
                if let Some(s) = &attached {
                    let _ = s.input(&bytes);
                }
            }
            ClientMsg::Resize { rows, cols } => {
                if let Some(s) = &attached {
                    let _ = s.resize(rows, cols);
                }
            }
            ClientMsg::AckBytes { offset } => {
                if let Some(s) = &attached {
                    s.ack(conn_id, offset);
                }
            }
            ClientMsg::Kill { session_id } => manager.kill(&session_id),
            ClientMsg::Checkpoint { .. } => {
                if let Some(s) = &attached {
                    s.checkpoint();
                }
            }
            ClientMsg::Heartbeat => { /* manager.touch() already ran */ }
            ClientMsg::GetHistory { .. } => {
                // Reserved; unimplemented in Step 2.
                send(&out_tx, &ServerMsg::Error { message: "history not implemented".into() }).await
            }
            ClientMsg::RefreshEnv { .. } => { /* existing sessions keep spawn-time env */ }
            ClientMsg::FdFollows { .. } => {
                // Reserved for Step 2.5 (SCM_RIGHTS); not implemented.
                send(&out_tx, &ServerMsg::Error { message: "fd passing not implemented".into() })
                    .await
            }
        }
    }

    // Connection closed: detach (keep the agent running), drain the writer.
    if let Some(s) = &attached {
        s.detach(conn_id);
    }
    drop(out_tx);
    let _ = writer.await;
    manager.conn_closed();
}

async fn send(out_tx: &mpsc::UnboundedSender<Bytes>, msg: &ServerMsg) {
    if let Ok(frame) = encode_server(msg) {
        let _ = out_tx.send(frame); // unbounded send is non-blocking + sync
    }
}
