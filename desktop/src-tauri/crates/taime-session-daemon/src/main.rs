//! taime-session-daemon — a long-lived, detached Rust binary that owns the PTY
//! masters + an authoritative `wezterm-term` emulator and outlives the Tauri app.
//!
//! Spawned detached by the app (`setsid`, stdio → /dev/null, dropped without
//! wait/kill), it advertises a per-user Unix socket whose path the app re-derives
//! deterministically. On relaunch the app adopts the still-running daemon and its
//! sessions. See `docs/terminal-architecture-plan.md` (Step 2).

mod attribution;
mod conn;
mod diff;
mod emulator;
mod listener;
mod manager;
mod mcp;
mod providers;
mod repaint;
mod runtime;
mod session;
mod store;
mod worktree;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use manager::Manager;
use runtime::Paths;

/// Quiet-window threshold for attribution turn boundaries.
const QUIET_WINDOW: Duration = Duration::from_millis(700);
/// Shut down after this long with no sessions and no connected client.
const IDLE_GRACE: Duration = Duration::from_secs(30);

/// `--mcp-stdio` mode: the per-agent MCP stdio shim. Bridges the CLI's MCP client
/// (newline-delimited JSON-RPC on stdin/stdout, per the MCP stdio transport) to
/// the daemon's MCP dispatcher over the control socket, authenticated by
/// `$TAIME_MCP_TOKEN`. The Phase-1 per-provider injection points the agent's MCP
/// server command at `<this binary> --mcp-stdio`.
fn run_mcp_shim() -> anyhow::Result<()> {
    use bytes::BytesMut;
    use std::io::{BufRead, Read, Write};
    use std::os::unix::net::UnixStream;
    use taime_protocol::{
        cap, decode_server, encode_client, parse_frame, paths, ClientMsg, Frame, ServerMsg, MAGIC,
        PROTOCOL_VERSION,
    };

    fn write_frame(s: &mut UnixStream, payload: &[u8]) -> std::io::Result<()> {
        s.write_all(&(payload.len() as u32).to_be_bytes())?;
        s.write_all(payload)?;
        s.flush()
    }
    fn read_frame(s: &mut UnixStream) -> std::io::Result<BytesMut> {
        let mut len = [0u8; 4];
        s.read_exact(&mut len)?;
        let mut buf = vec![0u8; u32::from_be_bytes(len) as usize];
        s.read_exact(&mut buf)?;
        Ok(BytesMut::from(&buf[..]))
    }

    let token = std::env::var("TAIME_MCP_TOKEN").unwrap_or_default();
    let socket = paths::default_socket_path()?;
    let mut stream = UnixStream::connect(&socket)?;

    // Handshake (same posture as the app client).
    let attach_token = std::fs::read_to_string(paths::token_path(&socket)).unwrap_or_default();
    let hello = encode_client(&ClientMsg::Hello {
        magic: MAGIC,
        protocol_version: PROTOCOL_VERSION,
        capabilities: cap::CURRENT,
        attach_token,
    })
    .map_err(|e| anyhow::anyhow!("encode hello: {e}"))?;
    write_frame(&mut stream, &hello)?;
    let _ = read_frame(&mut stream)?; // HelloOk / HelloRejected

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut req_id: u64 = 1;
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let payload = encode_client(&ClientMsg::McpRequest { req_id, token: token.clone(), json: line })
            .map_err(|e| anyhow::anyhow!("encode mcp request: {e}"))?;
        write_frame(&mut stream, &payload)?;
        req_id += 1;
        // Read until the matching McpResponse arrives (no other frames on this
        // connection — the shim never attaches a session).
        loop {
            let frame = read_frame(&mut stream)?;
            if let Ok(Frame::Control(body)) = parse_frame(frame) {
                if let Ok(ServerMsg::McpResponse { json, .. }) = decode_server(&body) {
                    if !json.is_empty() {
                        writeln!(stdout, "{json}")?;
                        stdout.flush()?;
                    }
                    break;
                }
            }
        }
    }
    Ok(())
}

fn parse_socket_arg() -> Option<PathBuf> {
    parse_flag_value("--socket")
}

/// The value following `flag` in argv (e.g. `--socket <path>`), if present.
fn parse_flag_value(flag: &str) -> Option<PathBuf> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == flag {
            return args.next().map(PathBuf::from);
        }
    }
    None
}

/// `--import-cao <cao.sqlite>`: one-time, idempotent import of a CAO SQLite db
/// into the daemon's app-data store (Phase 7), then exit.
fn run_import_cao(cao_path: &std::path::Path) -> anyhow::Result<()> {
    let store = store::Store::open().map_err(|e| anyhow::anyhow!("open store: {e}"))?;
    let stats = store
        .import_cao(cao_path)
        .map_err(|e| anyhow::anyhow!("import: {e}"))?;
    if stats.ran {
        eprintln!("[taime-daemon] imported CAO db {cao_path:?}:");
        for (table, n) in &stats.copied {
            eprintln!("  {table}: {n} rows");
        }
    } else {
        eprintln!("[taime-daemon] CAO already imported (no-op); marker present");
    }
    Ok(())
}

fn cleanup(paths: &Paths) {
    let _ = std::fs::remove_file(&paths.socket);
    let _ = std::fs::remove_file(&paths.token);
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Per-agent MCP stdio shim mode (Phase 5 transport): bridge the CLI's MCP
    // client to the daemon's tool dispatcher. Runs instead of the daemon.
    if std::env::args().any(|a| a == "--mcp-stdio") {
        return run_mcp_shim();
    }

    // One-time CAO state migration (Phase 7): `--import-cao <cao.sqlite>`.
    if let Some(cao_path) = parse_flag_value("--import-cao") {
        return run_import_cao(&cao_path);
    }

    let paths = match parse_socket_arg() {
        Some(p) => runtime::for_socket(p)?,
        None => runtime::default_paths()?,
    };

    // Liveness lock: if another daemon already holds it, exit — the app should
    // adopt that one over the advertised socket.
    let _lock = match runtime::acquire_lock(&paths.lock)? {
        Some(g) => g,
        None => {
            eprintln!("[taime-daemon] another daemon holds the lock; exiting");
            return Ok(());
        }
    };

    // Write the token BEFORE binding so that "socket is connectable" implies
    // "token is on disk" — otherwise a client that connects the instant bind
    // completes could read an empty token and get HelloRejected.
    let token = listener::write_token(&paths.token)?;
    let listener = listener::bind_secure(&paths.socket).await?;
    let manager = Arc::new(Manager::new());
    eprintln!("[taime-daemon] listening on {:?}", paths.socket);

    // Signal handler: unlink socket + token, kill children, exit.
    {
        let paths_sig = Paths {
            dir: paths.dir.clone(),
            socket: paths.socket.clone(),
            token: paths.token.clone(),
            lock: paths.lock.clone(),
        };
        let mgr = manager.clone();
        tokio::spawn(async move {
            let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("SIGTERM handler");
            let mut int = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                .expect("SIGINT handler");
            tokio::select! {
                _ = term.recv() => {}
                _ = int.recv() => {}
            }
            mgr.kill_all();
            cleanup(&paths_sig);
            std::process::exit(0);
        });
    }

    // GC + idle-shutdown tick.
    {
        let mgr = manager.clone();
        let paths_gc = Paths {
            dir: paths.dir.clone(),
            socket: paths.socket.clone(),
            token: paths.token.clone(),
            lock: paths.lock.clone(),
        };
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_millis(250));
            loop {
                ticker.tick().await;
                if mgr.gc_tick(QUIET_WINDOW, IDLE_GRACE) {
                    eprintln!("[taime-daemon] idle with no sessions; shutting down");
                    cleanup(&paths_gc);
                    std::process::exit(0);
                }
            }
        });
    }

    // Accept loop.
    loop {
        let (stream, _addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("[taime-daemon] accept error: {e}");
                continue;
            }
        };
        // Primary access gate: same-user only.
        if listener::validate_same_user(&stream).is_err() {
            drop(stream);
            continue;
        }
        let mgr = manager.clone();
        let tok = token.clone();
        tokio::spawn(async move {
            conn::handle(stream, mgr, tok).await;
        });
    }
}
