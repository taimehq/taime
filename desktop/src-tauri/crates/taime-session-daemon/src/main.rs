//! taime-session-daemon — a long-lived, detached Rust binary that owns the PTY
//! masters + an authoritative `wezterm-term` emulator and outlives the Tauri app.
//!
//! Spawned detached by the app (`setsid`, stdio → /dev/null, dropped without
//! wait/kill), it advertises a per-user Unix socket whose path the app re-derives
//! deterministically. On relaunch the app adopts the still-running daemon and its
//! sessions. See `docs/terminal-architecture-plan.md` (Step 2).

mod attribution;
mod conn;
mod emulator;
mod listener;
mod manager;
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

fn parse_socket_arg() -> Option<PathBuf> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--socket" {
            return args.next().map(PathBuf::from);
        }
    }
    None
}

fn cleanup(paths: &Paths) {
    let _ = std::fs::remove_file(&paths.socket);
    let _ = std::fs::remove_file(&paths.token);
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
