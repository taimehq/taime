//! Python backend (cao-server) lifecycle supervision.
//!
//! Responsibilities:
//!   * spawn `cao-server` with the resolved host/port + CORS/host allowlists
//!     so the Tauri webview origin is accepted,
//!   * health-check `GET /health` and restart on crash (managed mode),
//!   * or, in external mode, just watch an already-running backend,
//!   * emit `backend://status` events so the UI updates live,
//!   * shut the child down gracefully (SIGTERM → SIGKILL) on app exit.

use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::config::ResolvedConfig;

const HEALTH_POLL: Duration = Duration::from_secs(2);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
const SPAWN_BACKOFF: Duration = Duration::from_secs(3);
const SHUTDOWN_GRACE_MS: u64 = 3000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendState {
    /// starting | healthy | down | restarting | external | external_down
    pub status: String,
    pub detail: String,
    pub external: bool,
    pub pid: Option<u32>,
    pub api_url: String,
}

impl BackendState {
    fn new(cfg: &ResolvedConfig) -> Self {
        let (status, detail) = if cfg.external_backend {
            ("external", "Waiting for external backend…")
        } else {
            ("starting", "Starting backend…")
        };
        BackendState {
            status: status.to_string(),
            detail: detail.to_string(),
            external: cfg.external_backend,
            pid: None,
            api_url: cfg.api_url.clone(),
        }
    }
}

/// Cloneable handle stored in Tauri state and used by commands + the exit hook.
#[derive(Clone)]
pub struct SupervisorHandle {
    pub cfg: ResolvedConfig,
    pub state: Arc<Mutex<BackendState>>,
    child: Arc<Mutex<Option<Child>>>,
}

impl SupervisorHandle {
    pub fn snapshot(&self) -> BackendState {
        self.state.lock().unwrap().clone()
    }

    /// Gracefully stop a managed child: SIGTERM, wait, then SIGKILL.
    pub fn shutdown(&self) {
        let mut guard = self.child.lock().unwrap();
        let Some(child) = guard.as_mut() else {
            return;
        };
        let pid = child.id();

        #[cfg(unix)]
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
        #[cfg(not(unix))]
        let _ = pid;

        let steps = SHUTDOWN_GRACE_MS / 100;
        for _ in 0..steps {
            if let Ok(Some(_)) = child.try_wait() {
                *guard = None;
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = child.kill();
        let _ = child.wait();
        *guard = None;
    }
}

fn spawn_child(cfg: &ResolvedConfig) -> std::io::Result<Child> {
    let cmd_str = cfg
        .backend_cmd
        .clone()
        .unwrap_or_else(|| "cao-server".to_string());
    let mut parts = cmd_str.split_whitespace();
    let prog = parts.next().unwrap_or("cao-server");

    let mut command = Command::new(prog);
    for extra in parts {
        command.arg(extra);
    }
    command.arg("--host").arg(&cfg.host);
    command.arg("--port").arg(cfg.port.to_string());

    // Backend reads these as overrides (constants.py).
    command.env("CAO_API_HOST", &cfg.host);
    command.env("CAO_API_PORT", cfg.port.to_string());
    // Allow the Tauri webview origin(s) through CORS + host/WS allowlists.
    command.env(
        "CAO_CORS_ORIGINS",
        "tauri://localhost,http://tauri.localhost,https://tauri.localhost,http://localhost:1420,http://127.0.0.1:1420",
    );
    command.env(
        "CAO_ALLOWED_HOSTS",
        format!("{},localhost,127.0.0.1", cfg.host),
    );
    command.env("CAO_WS_ALLOWED_CLIENTS", "127.0.0.1,::1,localhost");

    // Inherit stdio so cao-server logs appear in the `tauri dev` console.
    command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    command.spawn()
}

async fn is_healthy(client: &reqwest::Client, api_url: &str) -> bool {
    match client
        .get(format!("{api_url}/health"))
        .timeout(HEALTH_TIMEOUT)
        .send()
        .await
    {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}

/// Start supervision and return the handle. Spawns a background task.
pub fn start(app: &AppHandle, cfg: ResolvedConfig) -> SupervisorHandle {
    let handle = SupervisorHandle {
        cfg: cfg.clone(),
        state: Arc::new(Mutex::new(BackendState::new(&cfg))),
        child: Arc::new(Mutex::new(None)),
    };

    let app = app.clone();
    let state = handle.state.clone();
    let child = handle.child.clone();

    tauri::async_runtime::spawn(async move {
        let client = reqwest::Client::new();
        let mut last_emitted = String::new();

        // Helper: write state + emit only on status change.
        let publish = |status: &str, detail: &str, pid: Option<u32>| {
            let new = {
                let mut g = state.lock().unwrap();
                g.status = status.to_string();
                g.detail = detail.to_string();
                if pid.is_some() {
                    g.pid = pid;
                }
                if status == "down" || status == "external_down" {
                    // keep pid for diagnostics; managed restart updates it
                }
                g.clone()
            };
            new
        };

        // In managed mode, if a healthy backend already occupies our port (an
        // orphan from a previous non-graceful exit, or a manually started
        // cao-server), adopt it instead of spawning a duplicate that can't bind.
        let mut adopted = false;

        loop {
            if cfg.external_backend {
                let ok = is_healthy(&client, &cfg.api_url).await;
                let (s, d) = if ok {
                    ("external", "Connected to external backend")
                } else {
                    ("external_down", "External backend not reachable")
                };
                let snap = publish(s, d, None);
                if snap.status != last_emitted {
                    last_emitted = snap.status.clone();
                    let _ = app.emit("backend://status", snap);
                }
                tokio::time::sleep(HEALTH_POLL).await;
                continue;
            }

            // Managed mode.

            // Reap an exited child so a dead handle never masquerades as "we own
            // a live backend" — otherwise a child that died (e.g. failed to bind
            // because an external cao-server holds the port) would trap us in a
            // respawn loop and permanently disable adoption below.
            {
                let mut g = child.lock().unwrap();
                if let Some(c) = g.as_mut() {
                    if matches!(c.try_wait(), Ok(Some(_))) {
                        *g = None;
                    }
                }
            }
            let have_live_child = { child.lock().unwrap().is_some() };

            // Adopt an already-running healthy backend rather than fighting over
            // the port. Re-checked every time we lack a live child (not just once
            // at boot), so a single transient health miss can't strand us spawning
            // duplicates against a port an external cao-server already owns.
            if !have_live_child && is_healthy(&client, &cfg.api_url).await {
                if !adopted {
                    adopted = true;
                    let snap = publish(
                        "healthy",
                        "Adopted a backend already running on this port",
                        None,
                    );
                    if snap.status != last_emitted {
                        last_emitted = snap.status.clone();
                        let _ = app.emit("backend://status", snap);
                    }
                }
                tokio::time::sleep(HEALTH_POLL).await;
                continue;
            }
            // We either own a live child or nothing healthy holds the port.
            adopted = false;

            // Ensure a live child exists (the handle is None once reaped above).
            let needs_spawn = { child.lock().unwrap().is_none() };

            if needs_spawn {
                let had_child = { child.lock().unwrap().is_some() };
                let (s, d) = if had_child {
                    ("restarting", "Backend exited — restarting…")
                } else {
                    ("starting", "Starting backend…")
                };
                let snap = publish(s, d, None);
                if snap.status != last_emitted {
                    last_emitted = snap.status.clone();
                    let _ = app.emit("backend://status", snap);
                }

                match spawn_child(&cfg) {
                    Ok(c) => {
                        let pid = c.id();
                        *child.lock().unwrap() = Some(c);
                        let snap = publish(
                            "starting",
                            "Backend launched; waiting for health…",
                            Some(pid),
                        );
                        // pid changed — always emit
                        last_emitted = snap.status.clone();
                        let _ = app.emit("backend://status", snap);
                    }
                    Err(e) => {
                        let snap = publish(
                            "down",
                            &format!("Failed to launch backend: {e}. Is `cao-server` on PATH?"),
                            None,
                        );
                        if snap.status != last_emitted {
                            last_emitted = snap.status.clone();
                            let _ = app.emit("backend://status", snap);
                        }
                        tokio::time::sleep(SPAWN_BACKOFF).await;
                        continue;
                    }
                }
            }

            let ok = is_healthy(&client, &cfg.api_url).await;
            let alive = {
                let mut g = child.lock().unwrap();
                match g.as_mut() {
                    Some(c) => !matches!(c.try_wait(), Ok(Some(_))),
                    None => false,
                }
            };
            let (s, d) = match (ok, alive) {
                (true, _) => ("healthy", "Backend healthy"),
                (false, true) => ("starting", "Backend launched; waiting for health…"),
                (false, false) => ("down", "Backend process is not running"),
            };
            let snap = publish(s, d, None);
            if snap.status != last_emitted {
                last_emitted = snap.status.clone();
                let _ = app.emit("backend://status", snap);
            }

            tokio::time::sleep(HEALTH_POLL).await;
        }
    });

    handle
}
