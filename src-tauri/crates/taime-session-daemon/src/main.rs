//! taime-session-daemon — a long-lived, detached Rust binary that owns the PTY
//! masters + an authoritative `wezterm-term` emulator and outlives the Tauri app.
//!
//! Spawned detached by the app (`setsid`, stdio → /dev/null, dropped without
//! wait/kill), it advertises a per-user Unix socket whose path the app re-derives
//! deterministically. On relaunch the app adopts the still-running daemon and its
//! sessions. See `docs/terminal-architecture-plan.md` (Step 2).
//!
//! Diagnostics: when spawned detached, stderr is re-pointed from /dev/null at
//! `<data_dir>/taime/daemon.log` (see `redirect_stderr_to_log`).

mod attribution;
mod conn;
mod diff;
mod emulator;
mod fswatch;
mod listener;
mod manager;
mod mcp;
mod profiles;
mod providers;
mod reap;
mod schedules;
mod workflow;
mod workflow_engine;
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
        let n = u32::from_be_bytes(len) as usize;
        // Same cap as the main codec — a garbage/hostile peer must not make the
        // shim allocate unbounded memory from a forged length prefix.
        if n > taime_protocol::MAX_FRAME_LEN {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("frame length {n} exceeds cap"),
            ));
        }
        let mut buf = vec![0u8; n];
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
    // A rejected handshake (version/token mismatch after a partial upgrade)
    // must be LOUD: silently proceeding leaves the agent's MCP tools failing
    // with no diagnostic anywhere.
    let first = read_frame(&mut stream)?;
    if let Ok(Frame::Control(body)) = parse_frame(first) {
        if let Ok(ServerMsg::HelloRejected { reason }) = decode_server(&body) {
            eprintln!(
                "[taime-mcp] daemon rejected handshake: {reason} \
                 (app/daemon protocol mismatch? relaunch the agent)"
            );
            std::process::exit(1);
        }
    }

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
    let _ = std::fs::remove_file(&paths.lock);
}

/// Rotation cap for `daemon.log`: at this size the boot path renames it to
/// `daemon.log.1` (replacing the previous one), so the pair is bounded at
/// ~2× this. Rotating only at boot is enough of a bound — idle shutdown makes
/// daemon restarts routine.
const LOG_ROTATE_BYTES: u64 = 4 * 1024 * 1024;

/// Re-point stderr at `<data_dir>/taime/daemon.log` — but ONLY when it currently
/// points at /dev/null (the app's detached spawn dup2's all stdio there). Every
/// `eprintln!` diagnostic in the daemon was literally discarded in production
/// (the corrupt-store review finding: persistence could die with no trace
/// anywhere). A foreground run (terminal) or the test harness (pipe) keeps its
/// stderr — the redirect exists to stop diagnostics being thrown away, not to
/// move them away from someone already watching.
fn redirect_stderr_to_log() {
    use std::os::unix::io::AsRawFd;

    // SAFETY: fstat/stat write into locally-owned zeroed buffers; the path
    // literal is NUL-terminated.
    let stderr_is_devnull = unsafe {
        let mut err_st: libc::stat = std::mem::zeroed();
        let mut null_st: libc::stat = std::mem::zeroed();
        libc::fstat(libc::STDERR_FILENO, &mut err_st) == 0
            && libc::stat(c"/dev/null".as_ptr(), &mut null_st) == 0
            && err_st.st_dev == null_st.st_dev
            && err_st.st_ino == null_st.st_ino
    };
    if !stderr_is_devnull {
        return;
    }
    // Best-effort throughout: stderr is /dev/null here, so a failure to set up
    // the log has nowhere to report — just keep the discard behavior.
    let Some(dir) = store::data_dir() else { return };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("daemon.log");
    if std::fs::metadata(&path).map(|m| m.len() > LOG_ROTATE_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(&path, dir.join("daemon.log.1"));
    }
    let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    // O_APPEND + dup2 onto stderr; dropping `file` closes only its own fd (the
    // stderr slot keeps the duplicate). Append mode means a racing second
    // daemon (other socket path) interleaves lines instead of clobbering.
    // SAFETY: dup2 of a freshly opened, owned fd onto a standard fd slot.
    if unsafe { libc::dup2(file.as_raw_fd(), libc::STDERR_FILENO) } == -1 {
        return;
    }
    eprintln!(
        "[taime-daemon] ---- boot {} (v{}, pid {}) ----",
        chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%z"),
        env!("CARGO_PKG_VERSION"),
        std::process::id()
    );
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

    // Daemon proper from here on. Before anything can fail, give a discarded
    // stderr a real destination — including the lock-conflict exit below.
    redirect_stderr_to_log();

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
    // Let the manager hand its own Arc to background driver threads (the workflow
    // engine), then ingest schedule + workflow files.
    manager.init_self();
    manager.load_schedules_on_start();
    manager.seed_example_workflows();
    manager.load_workflow_files();
    // Boot reconciliation (review H1/H2): kill orphan agent process groups left
    // by a previous daemon that died without reaping them, and replay any provider
    // -cleanup ledger rows (injected gemini/grok MCP config) it never tore down.
    // We hold the liveness lock, so nothing live is touched.
    reap::sweep_orphan_agents();
    manager.reconcile_cleanups_on_boot();
    // Startup maintenance, off the accept path: a ONE-TIME collapse of the
    // pre-existing per-agent worktree pile into refs/taime/archive/* (archive
    // -then-reclaim rollout, guarded so it runs once), the steady-state retention
    // sweep, then aged-history pruning.
    {
        let m = manager.clone();
        tokio::task::spawn_blocking(move || {
            m.collapse_worktrees_once();
            m.sweep_worktrees();
            m.prune_history();
        });
    }
    eprintln!("[taime-daemon] listening on {:?}", paths.socket);

    // Signal handler: terminate every agent's process GROUP + run their provider
    // cleanup SYNCHRONOUSLY (review H1/H2 — `kill_all` now SIGTERMs the group,
    // grace, SIGKILLs stragglers, then reaps + tears down injected config before
    // we exit), unlink socket/token/lock, then exit.
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
            let mut n: u64 = 0;
            loop {
                ticker.tick().await;
                // The maintenance body (dead-session reap, the live reaper, quiet-
                // window attribution, idle-gated delivery — which does SQLite + a
                // blocking PTY write — and the idle-shutdown decision) runs on a
                // blocking thread, SINGLE-FLIGHT (review M6): if a previous tick is
                // still running (e.g. blocked on a wedged child's stdin), skip this
                // one rather than piling up blocked jobs every 250 ms.
                if mgr.try_begin_gc() {
                    let m = mgr.clone();
                    let p = Paths {
                        dir: paths_gc.dir.clone(),
                        socket: paths_gc.socket.clone(),
                        token: paths_gc.token.clone(),
                        lock: paths_gc.lock.clone(),
                    };
                    tokio::task::spawn_blocking(move || {
                        if m.gc_tick(QUIET_WINDOW, IDLE_GRACE) {
                            eprintln!("[taime-daemon] idle with no sessions; shutting down");
                            cleanup(&p);
                            std::process::exit(0);
                        }
                        m.end_gc();
                    });
                }
                // Schedules: check due cron schedules every ~30s, OFF the hot tick
                // (sqlite + an optional shell gate) so a slow gate never stalls the
                // 250 ms delivery/quiet-window loop.
                n = n.wrapping_add(1);
                if n % 120 == 0 {
                    let m = mgr.clone();
                    tokio::task::spawn_blocking(move || m.check_schedules());
                }
                // Worktree GC every ~10min (shells out to git → spawn_blocking).
                if n % 2400 == 0 {
                    let m = mgr.clone();
                    tokio::task::spawn_blocking(move || m.sweep_worktrees());
                }
                // History retention pruning daily.
                if n % 345_600 == 0 {
                    let m = mgr.clone();
                    tokio::task::spawn_blocking(move || m.prune_history());
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
