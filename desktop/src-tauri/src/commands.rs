//! Tauri IPC commands — the Rust↔React bridge.

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
