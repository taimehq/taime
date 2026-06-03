//! Tauri IPC commands — the Rust↔React bridge.

use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, State};

use crate::backend::BackendState;
use crate::config::ResolvedConfig;
use crate::fs_watch::FsWatchState;
use crate::AppStateHandle;

/// Frontend calls this on boot to discover where the backend lives.
/// (config.ts → getConfig())
#[tauri::command]
pub fn get_api_url(state: State<'_, AppStateHandle>) -> ResolvedConfig {
    state.supervisor.cfg.clone()
}

/// Alias of `get_api_url` under the name the Step 2 terminal-canvas spec uses
/// (`get_backend_routing`). Resolves the host/port the frontend uses to build
/// the PTY WebSocket URL. Kept distinct so either command name works.
#[tauri::command]
pub fn get_backend_routing(state: State<'_, AppStateHandle>) -> ResolvedConfig {
    state.supervisor.cfg.clone()
}

/// One-shot pull of the current supervisor status. Live updates arrive via the
/// `backend://status` event.
#[tauri::command]
pub fn get_backend_status(state: State<'_, AppStateHandle>) -> BackendState {
    state.supervisor.snapshot()
}

/// Start watching `dir` for the given terminal; dirty-state events are emitted
/// as `terminal://{id}/fs-dirty`.
#[tauri::command]
pub fn watch_terminal(
    app: AppHandle,
    fs: State<'_, FsWatchState>,
    terminal_id: String,
    dir: String,
) -> Result<(), String> {
    fs.watch_terminal(&app, terminal_id, dir)
}

/// Stop watching for the given terminal (e.g. when its frame closes).
#[tauri::command]
pub fn unwatch_terminal(fs: State<'_, FsWatchState>, terminal_id: String) {
    fs.unwatch_terminal(terminal_id);
}

/// Clear accumulated dirty state for a terminal (e.g. after the user reviews).
#[tauri::command]
pub fn clear_dirty(fs: State<'_, FsWatchState>, terminal_id: String) {
    fs.clear_dirty(terminal_id);
}

// ---------------------------------------------------------------------------
// Claude terminal transport: the detached `taime-session-daemon` (the ONE Rust
// PTY path). The daemon owns the PTY + an authoritative wezterm-term grid and
// survives app crashes; output streams as RAW BYTES over a per-session binary
// `Channel<InvokeResponseBody>` (no base64). The app's `DaemonClient` bridges the
// socket to the per-session channel. CAO/tmux remains for the other CLIs and as
// the launch-failure fallback for Claude until the daemon ships bundled.
// ---------------------------------------------------------------------------

use crate::daemon::DaemonClient;
use taime_protocol::{AgentProfile, AgentSpawnSpec, SessionSummary, WorktreeInfo};

/// Provision (or resolve) an isolated git worktree for a daemon agent (Phase 3) —
/// the daemon-owned replacement for CAO's `/worktrees/provision`. Returns the
/// worktree info (snake_case fields, incl. `terminal_key` = attribution id).
#[tauri::command]
pub async fn daemon_provision_worktree(
    daemon: State<'_, DaemonClient>,
    project_root: String,
    provider: String,
    isolate: bool,
) -> Result<WorktreeInfo, String> {
    daemon.provision_worktree(project_root, provider, isolate).await
}

/// A high-level spawn for ANY provider through the daemon's registry (Phase 1).
/// The daemon owns the launch recipe + MCP injection; the app just names the
/// provider + default profile. `model`/`permission_mode` flow into the profile.
fn default_agent_spec(
    provider: String,
    cwd: Option<String>,
    rows: Option<u16>,
    cols: Option<u16>,
    attribution_key: Option<String>,
    model: Option<String>,
    permission_mode: Option<String>,
) -> AgentSpawnSpec {
    AgentSpawnSpec {
        provider,
        profile: AgentProfile {
            name: "default".to_string(),
            model,
            permission_mode,
            ..Default::default()
        },
        cwd,
        rows: rows.unwrap_or(24),
        cols: cols.unwrap_or(80),
        attribution_key,
        seed_prompt: None,
        env: vec![],
    }
}

/// Launch any supported CLI (`claude_code`/`codex`/`gemini_cli`/`grok_cli`) via
/// the daemon's provider registry with the default (unrestricted) profile.
#[tauri::command]
pub async fn daemon_spawn_agent(
    daemon: State<'_, DaemonClient>,
    provider: String,
    cwd: Option<String>,
    rows: Option<u16>,
    cols: Option<u16>,
    model: Option<String>,
    permission_mode: Option<String>,
    attribution_key: Option<String>,
) -> Result<String, String> {
    let spec = default_agent_spec(provider, cwd, rows, cols, attribution_key, model, permission_mode);
    daemon.spawn_agent(spec).await
}

// SECURITY: the default profile is unrestricted (`--dangerously-skip-permissions`
// / `--yolo` / `--always-approve` per provider) — the deliberate model for driving
// the CLIs unattended in the workspace. Claude now launches via
// `daemon_spawn_agent` with provider="claude_code" (the daemon's adapter owns the
// binary + args), so the old Claude-specific `daemon_spawn_claude` is gone.

#[tauri::command]
pub async fn daemon_attach(
    daemon: State<'_, DaemonClient>,
    session_id: String,
    rows: u16,
    cols: u16,
    on_data: Channel<InvokeResponseBody>,
) -> Result<(), String> {
    daemon.attach(session_id, rows, cols, on_data).await
}

#[tauri::command]
pub async fn daemon_write(
    daemon: State<'_, DaemonClient>,
    session_id: String,
    data: String,
) -> Result<(), String> {
    daemon.write(&session_id, data.into_bytes()).await;
    Ok(())
}

#[tauri::command]
pub async fn daemon_resize(
    daemon: State<'_, DaemonClient>,
    session_id: String,
    rows: u16,
    cols: u16,
) -> Result<(), String> {
    daemon.resize(&session_id, rows, cols).await;
    Ok(())
}

#[tauri::command]
pub async fn daemon_ack(
    daemon: State<'_, DaemonClient>,
    session_id: String,
    offset: u64,
) -> Result<(), String> {
    daemon.ack(&session_id, offset).await;
    Ok(())
}

#[tauri::command]
pub async fn daemon_checkpoint(
    daemon: State<'_, DaemonClient>,
    session_id: String,
    cause: String,
) -> Result<(), String> {
    daemon.checkpoint(&session_id, cause).await;
    Ok(())
}

#[tauri::command]
pub async fn daemon_close_view(
    daemon: State<'_, DaemonClient>,
    session_id: String,
) -> Result<(), String> {
    daemon.detach(&session_id).await;
    Ok(())
}

#[tauri::command]
pub async fn daemon_kill(daemon: State<'_, DaemonClient>, session_id: String) -> Result<(), String> {
    daemon.kill(session_id).await
}

#[tauri::command]
pub async fn daemon_list(daemon: State<'_, DaemonClient>) -> Result<Vec<SessionSummary>, String> {
    daemon.list().await
}

/// Whether the daemon transport is usable: the daemon binary is resolvable
/// (so we can spawn it) or one is already running. The frontend uses this to
/// route Claude through the daemon when available, falling back to CAO when not
/// (e.g. an unbundled build) — so Claude launches never hard-fail.
#[tauri::command]
pub async fn daemon_available(daemon: State<'_, DaemonClient>) -> Result<bool, String> {
    Ok(daemon.available().await)
}

/// Load an image FILE into the macOS system clipboard (as image data), so a
/// dragged image can be ingested by a CLI that reads clipboard images on paste
/// (e.g. Claude Code's Ctrl+V image paste → `[Image #N]`). A web terminal can't
/// pipe image bytes through the PTY, so the clipboard is the channel; the caller
/// then sends the agent its paste trigger.
#[cfg(target_os = "macos")]
#[tauri::command]
pub fn set_clipboard_image_from_path(path: String) -> Result<(), String> {
    // AppleScript image class for the clipboard, by extension.
    let ext = std::path::Path::new(&path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let class = match ext.as_str() {
        "jpg" | "jpeg" => "«class JPEG»",
        "gif" => "«class GIFf»",
        "tif" | "tiff" => "«class TIFF»",
        _ => "«class PNGf»", // png + default
    };
    let esc = path.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!("set the clipboard to (read (POSIX file \"{esc}\") as {class})");
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map_err(|e| format!("osascript failed: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
pub fn set_clipboard_image_from_path(_path: String) -> Result<(), String> {
    Err("clipboard image set is only implemented on macOS".to_string())
}
