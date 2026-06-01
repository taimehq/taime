//! Tauri IPC commands — the Rust↔React bridge.

use base64::Engine;
use tauri::{AppHandle, State};

use crate::backend::BackendState;
use crate::config::ResolvedConfig;
use crate::fs_watch::FsWatchState;
use crate::pty::{PtyManager, SessionInfo};
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
// Rust-owned PTY (Claude path). Output streams as `pty://{id}/data` (base64)
// and `pty://{id}/exit`. CAO/tmux remains the default + fallback for now.
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
) -> Result<String, String> {
    // Mirror CAO's claude launch: --dangerously-skip-permissions enables
    // "bypass permissions" mode (no per-tool prompts). The recurring "Yes, I
    // accept" dialog is suppressed by skipDangerousModePermissionPrompt:true in
    // ~/.claude/settings.json, which CAO already writes.
    pty.spawn(
        &claude_binary(),
        &["--dangerously-skip-permissions".to_string()],
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

/// Reattach a view; returns base64 scrollback to replay into a fresh terminal.
#[tauri::command]
pub fn pty_reattach_view(pty: State<'_, PtyManager>, session_id: String) -> Result<String, String> {
    let bytes = pty.reattach_view(&session_id)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
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
