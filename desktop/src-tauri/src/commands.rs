//! Tauri IPC commands — the Rust↔React bridge.

use std::sync::Arc;

use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, State};

use crate::backend::BackendState;
use crate::config::ResolvedConfig;
use crate::fs_watch::FsWatchState;
use crate::pty::{PtyEvent, PtyManager, SessionInfo, SessionSink};
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
// Rust-owned PTY (Claude path). Output streams as RAW BYTES over a per-session
// binary `Channel<InvokeResponseBody>` (no base64, no JSON number-arrays): data
// chunks arrive as `InvokeResponseBody::Raw` (ArrayBuffer on the JS side); a
// process-exit notice arrives as a small `InvokeResponseBody::Json` control
// object on the same ordered channel. CAO/tmux remains the default + fallback.
// ---------------------------------------------------------------------------

/// Resolve the claude binary: prefer the known install, else rely on PATH.
fn claude_binary() -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let p = std::path::Path::new(&home).join(".local/bin/claude");
        if p.exists() {
            return p.to_string_lossy().to_string();
        }
    }
    "claude".to_string()
}

#[tauri::command]
pub fn pty_spawn_claude(
    pty: State<'_, PtyManager>,
    cwd: Option<String>,
    rows: Option<u16>,
    cols: Option<u16>,
    permission_mode: Option<String>,
) -> Result<String, String> {
    // Permission mode mirrors CAO's _build_claude_command: a caller-supplied
    // `--permission-mode <mode>` (e.g. "default", "acceptEdits", "plan") when
    // given, else the bypass default ("bypass permissions on" — no per-tool
    // prompts) that the rest of Taime/CAO uses for unattended orchestration.
    // SECURITY: bypass runs tools without prompts; it's the deliberate model
    // for driving real CLIs in the user's workspace, and is overridable here
    // rather than hardcoded. The recurring "Yes, I accept" dialog is suppressed
    // by skipDangerousModePermissionPrompt in ~/.claude/settings.json (written
    // by CAO; not touched here, so the user can still revoke bypass there).
    let args: Vec<String> = match permission_mode {
        Some(mode) if !mode.is_empty() => vec!["--permission-mode".to_string(), mode],
        _ => vec!["--dangerously-skip-permissions".to_string()],
    };
    pty.spawn(
        &claude_binary(),
        &args,
        cwd.as_deref(),
        &[],
        rows.unwrap_or(24),
        cols.unwrap_or(80),
    )
}

#[tauri::command]
pub fn pty_write(
    pty: State<'_, PtyManager>,
    session_id: String,
    data: String,
) -> Result<(), String> {
    pty.write_input(&session_id, data.as_bytes())
}

#[tauri::command]
pub fn pty_resize(
    pty: State<'_, PtyManager>,
    session_id: String,
    rows: u16,
    cols: u16,
) -> Result<(), String> {
    pty.resize(&session_id, rows, cols)
}

/// Detach the view (does NOT kill the process).
#[tauri::command]
pub fn pty_close_view(pty: State<'_, PtyManager>, session_id: String) {
    pty.close_view(&session_id);
}

/// Attach a view to a session. `on_data` is a per-session binary channel: the
/// retained scrollback replays as the first `Raw` message, then live output
/// streams as `Raw` chunks; a process exit arrives as a `Json` control object
/// `{ "type": "exit", "code": <i32|null> }` on the same channel. Using
/// `Channel<InvokeResponseBody>` + `Raw` is what forces the efficient ArrayBuffer
/// path — a `Channel<&[u8]>`/`Channel<Vec<u8>>` would serialize to a JSON
/// number-array instead. `send` errors (webview gone) detach the session.
#[tauri::command]
pub fn pty_attach(
    pty: State<'_, PtyManager>,
    session_id: String,
    on_data: Channel<InvokeResponseBody>,
) -> Result<(), String> {
    let sink: SessionSink = Arc::new(move |ev| match ev {
        PtyEvent::Data(bytes) => {
            let _ = on_data.send(InvokeResponseBody::Raw(bytes));
        }
        PtyEvent::Exit(code) => {
            let payload = serde_json::json!({ "type": "exit", "code": code }).to_string();
            let _ = on_data.send(InvokeResponseBody::Json(payload));
        }
    });
    pty.attach(&session_id, sink)
}

/// Backpressure ack: the highest byte offset the client has *processed* (xterm's
/// `write(data, cb)` callback fired), batched ~once per frame on the JS side.
/// The manager pauses reading the PTY master when sent−acked exceeds the high
/// watermark, resuming below the low watermark.
#[tauri::command]
pub fn pty_ack(pty: State<'_, PtyManager>, session_id: String, offset: u64) {
    pty.ack(&session_id, offset);
}

/// Explicitly terminate the process.
#[tauri::command]
pub fn pty_kill(pty: State<'_, PtyManager>, session_id: String) {
    pty.kill_session(&session_id);
}

#[tauri::command]
pub fn pty_list(pty: State<'_, PtyManager>) -> Vec<SessionInfo> {
    pty.list_sessions()
}

// ---------------------------------------------------------------------------
// Session daemon (Step 2). Same binary `Channel` transport as the in-app path,
// but the bytes originate in the detached `taime-session-daemon` (which owns the
// PTY + an authoritative wezterm-term grid and survives app crashes). The app's
// `DaemonClient` bridges the socket to the per-session channel.
// ---------------------------------------------------------------------------

use crate::daemon::DaemonClient;
use taime_protocol::{SessionSummary, SpawnSpec};

/// Claude argv, shared with the in-app path (see `pty_spawn_claude`).
fn claude_args(permission_mode: Option<String>) -> Vec<String> {
    match permission_mode {
        Some(mode) if !mode.is_empty() => vec!["--permission-mode".to_string(), mode],
        _ => vec!["--dangerously-skip-permissions".to_string()],
    }
}

#[tauri::command]
pub async fn daemon_spawn_claude(
    daemon: State<'_, DaemonClient>,
    cwd: Option<String>,
    rows: Option<u16>,
    cols: Option<u16>,
    permission_mode: Option<String>,
    attribution_key: Option<String>,
) -> Result<String, String> {
    let spec = SpawnSpec {
        prog: claude_binary(),
        args: claude_args(permission_mode),
        cwd,
        env: vec![],
        rows: rows.unwrap_or(24),
        cols: cols.unwrap_or(80),
        attribution_key,
    };
    daemon.spawn_session(spec).await
}

#[tauri::command]
pub async fn daemon_attach(
    daemon: State<'_, DaemonClient>,
    session_id: String,
    on_data: Channel<InvokeResponseBody>,
) -> Result<(), String> {
    daemon.attach(session_id, on_data).await
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
