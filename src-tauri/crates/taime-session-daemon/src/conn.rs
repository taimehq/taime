//! Per-connection handler: versioned handshake, then dispatch the control set.
//! Each connection has an outbound frame channel drained by a writer task; the
//! session streams data frames into that same channel when this connection is
//! the attached client.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
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

/// A connected peer must complete the `Hello` handshake within this window
/// (review H5/M7): a same-user process that connects and stays silent must not
/// park a task forever, and must never count toward `active_conns` (which would
/// pin the daemon awake past idle-shutdown).
const HELLO_DEADLINE: Duration = Duration::from_secs(5);

/// Cap on un-drained RPC-response bytes queued for one connection (review M16).
/// The data path is bounded separately by the ack watermark; this bounds the
/// CONTROL-response path (`McpResponse`/`QueryResult`/`Graph`/`Sessions` — a big
/// git diff can be multi-MB) so a client that pipelines requests and stops
/// reading can't grow daemon RSS without bound. Over the cap → close the
/// connection (the session, if any, just detaches and keeps running).
const MAX_OUTBOUND_RESPONSE_BYTES: u64 = 128 * 1024 * 1024;

fn framed(stream: UnixStream) -> tokio_util::codec::Framed<UnixStream, LengthDelimitedCodec> {
    length_delimited::Builder::new()
        .max_frame_length(MAX_FRAME_LEN)
        .new_framed(stream)
}

/// The byte-accounted sender for RPC responses on a connection: tracks how many
/// queued bytes the writer hasn't flushed yet so the dispatch loop can close a
/// connection whose peer has stopped reading (review M16).
struct Responder {
    tx: mpsc::UnboundedSender<Bytes>,
    pending: Arc<AtomicU64>,
}

/// Handle one client connection until it closes. `token` is the daemon's current
/// attach token (already gated by the peer-uid check at accept time).
pub async fn handle(stream: UnixStream, manager: Arc<Manager>, token: String) {
    manager.touch();
    let conn_id = manager.next_conn_id();

    let (mut sink, mut reader) = framed(stream).split();
    // Two outbound channels, both unbounded so a producer never blocks under a
    // lock: `data_tx` carries the session's stream (Data/Repaint + its ordered
    // control pushes) — flow-controlled by the ack watermark; `resp_tx` carries
    // this connection's RPC responses — byte-accounted via `pending` so a peer
    // that stops reading is closed rather than growing RSS (review M16).
    let (data_tx, mut data_rx) = mpsc::unbounded_channel::<Bytes>();
    let (resp_tx, mut resp_rx) = mpsc::unbounded_channel::<Bytes>();
    let pending = Arc::new(AtomicU64::new(0));
    let out_tx = Responder { tx: resp_tx, pending: pending.clone() };

    // Writer task: drain both channels → socket. Decrements `pending` as each
    // response frame is flushed, so the counter reflects un-written backlog.
    let writer_pending = pending.clone();
    let writer = tokio::spawn(async move {
        loop {
            // `Some(..) =` patterns: when a channel closes it yields None, the
            // pattern fails, and select disables that branch — so a closed data
            // channel does NOT abandon queued responses (e.g. a HelloRejected sent
            // just before the connection drops). `else` fires only once BOTH are
            // closed.
            tokio::select! {
                biased;
                Some(f) = data_rx.recv() => {
                    if sink.send(f).await.is_err() {
                        break;
                    }
                }
                Some(f) = resp_rx.recv() => {
                    let len = f.len() as u64;
                    let r = sink.send(f).await;
                    writer_pending.fetch_sub(len, Ordering::Relaxed);
                    if r.is_err() {
                        break;
                    }
                }
                else => break,
            }
        }
    });

    // First frame must be a valid Hello within HELLO_DEADLINE (review H5/M7). We
    // do NOT count this connection toward `active_conns` until the handshake
    // succeeds, so a silent/squatting same-user peer can neither hang here nor
    // pin the daemon awake.
    let hello_ok = match tokio::time::timeout(HELLO_DEADLINE, reader.next()).await {
        Ok(Some(Ok(payload))) => handle_hello(payload, &token, &out_tx).await,
        _ => false, // timed out / closed / codec error — drop, never counted
    };
    if !hello_ok {
        drop(out_tx);
        drop(data_tx);
        let _ = writer.await;
        return;
    }
    manager.conn_opened();

    let mut attached: Option<Session> = None;

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
            ClientMsg::ProvisionWorktree { req_id, project_root, provider, isolate, task_id } => {
                // git worktree shells out (blocking); keep it off the async worker.
                let mgr = manager.clone();
                match tokio::task::spawn_blocking(move || {
                    mgr.provision_worktree(project_root, provider, isolate, task_id)
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
            ClientMsg::SendMessage { req_id, sender, receiver, message } => {
                match manager.enqueue_message(sender.clone(), receiver.clone(), message) {
                    Ok(id) => {
                        // Record the message as a graph edge too (parity with the
                        // MCP send_message path), so the ops/app message path is
                        // attributed in the activity graph.
                        manager.record_edge("message", &sender, &receiver);
                        send(&out_tx, &ServerMsg::MessageQueued { req_id, id }).await
                    }
                    Err(e) => send(&out_tx, &ServerMsg::Error { message: e }).await,
                }
            }
            ClientMsg::McpRequest { req_id, token, json } => {
                // assign() shells out to git (worktree) — keep it off the async
                // worker. The dispatcher authenticates the caller from `token`.
                let mgr = manager.clone();
                let resp = tokio::task::spawn_blocking(move || mgr.handle_mcp(&token, &json))
                    .await
                    .unwrap_or_else(|_| {
                        r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"internal error"}}"#
                            .to_string()
                    });
                send(&out_tx, &ServerMsg::McpResponse { req_id, json: resp }).await;
            }
            ClientMsg::GetGraph { req_id } => {
                // Legacy daemon-wide graph; the workspace-scoped Team drawer goes
                // through the `graph` query arm with a `workspace_root`.
                let json = manager.activity_graph_json(None);
                send(&out_tx, &ServerMsg::Graph { req_id, json }).await;
            }
            ClientMsg::Query { req_id, kind, args } => {
                // Diffs shell out to git — keep them off the async worker.
                let mgr = manager.clone();
                let json = tokio::task::spawn_blocking(move || mgr.query(&kind, &args))
                    .await
                    .unwrap_or_else(|_| r#"{"error":"query panicked"}"#.to_string());
                send(&out_tx, &ServerMsg::QueryResult { req_id, json }).await;
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
                    // The session streams onto the DATA channel (watermark-paced),
                    // not the byte-capped response channel.
                    if let Err(e) = session.attach(conn_id, rows, cols, data_tx.clone()) {
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
        // Backpressure on a peer that stops reading its responses (review M16):
        // once un-flushed response bytes exceed the cap, close — the writer can't
        // keep up, so further work would only grow RSS. An attached session just
        // detaches below and keeps running.
        if pending.load(Ordering::Relaxed) > MAX_OUTBOUND_RESPONSE_BYTES {
            eprintln!("[taime-daemon] connection exceeded response backlog cap; closing");
            break;
        }
    }

    // Connection closed: detach (keep the agent running), drain the writer.
    if let Some(s) = &attached {
        s.detach(conn_id);
    }
    drop(out_tx);
    drop(data_tx);
    let _ = writer.await;
    manager.conn_closed();
}

async fn send(out_tx: &Responder, msg: &ServerMsg) {
    if let Ok(frame) = encode_server(msg) {
        // Account the queued bytes BEFORE sending; the writer subtracts them once
        // the frame is flushed (review M16). Unbounded send is non-blocking.
        out_tx.pending.fetch_add(frame.len() as u64, Ordering::Relaxed);
        let _ = out_tx.tx.send(frame);
    }
}

/// Validate the first frame as a `Hello` and reply `HelloOk`/`HelloRejected`.
/// Returns true iff the handshake passed (magic + compatible protocol + matching
/// attach token). Anything else (wrong frame, decode error, non-Hello control) is
/// a silent reject so a probe/garbage peer learns nothing and is dropped.
async fn handle_hello(
    payload: BytesMut,
    token: &str,
    out_tx: &Responder,
) -> bool {
    let body = match parse_frame(payload) {
        Ok(Frame::Control(b)) => b,
        _ => return false,
    };
    let msg = match decode_client(&body) {
        Ok(m) => m,
        Err(_) => return false,
    };
    match msg {
        ClientMsg::Hello { magic, protocol_version, attach_token, .. } => {
            let ok = magic == MAGIC
                && versions_compatible(protocol_version, PROTOCOL_VERSION)
                && token_matches(token, &attach_token);
            if ok {
                send(
                    out_tx,
                    &ServerMsg::HelloOk {
                        magic: MAGIC,
                        protocol_version: PROTOCOL_VERSION,
                        capabilities: cap::CURRENT,
                        daemon_version: env!("CARGO_PKG_VERSION").to_string(),
                    },
                )
                .await;
                true
            } else {
                send(
                    out_tx,
                    &ServerMsg::HelloRejected { reason: "magic/version/token mismatch".into() },
                )
                .await;
                false
            }
        }
        _ => false, // control before handshake
    }
}
