//! Tauri IPC commands — the Rust↔React bridge.

use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::State;

/// Probe a candidate project dir for the workspace picker. Done **app-side** (a
/// direct git read) rather than via the daemon, so the badge resolves on first
/// paint regardless of whether the daemon has warmed up yet (no boot race, no
/// "missing" flash).
#[derive(serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct WorkspaceInfo {
    path: String,
    exists: bool,
    is_git: bool,
    repo_root: Option<String>,
    branch: Option<String>,
    head_short: Option<String>,
}

fn git_probe(cwd: &std::path::Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").current_dir(cwd).args(args).output().ok()?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!s.is_empty()).then_some(s)
    } else {
        None
    }
}

#[tauri::command]
pub fn workspace_info(path: String) -> WorkspaceInfo {
    let p = std::path::Path::new(&path);
    if !p.is_dir() {
        return WorkspaceInfo { path, exists: false, is_git: false, repo_root: None, branch: None, head_short: None };
    }
    let repo_root = git_probe(p, &["rev-parse", "--show-toplevel"]);
    WorkspaceInfo {
        is_git: repo_root.is_some(),
        branch: git_probe(p, &["rev-parse", "--abbrev-ref", "HEAD"]),
        head_short: git_probe(p, &["rev-parse", "--short", "HEAD"]),
        repo_root,
        exists: true,
        path,
    }
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
#[allow(clippy::too_many_arguments)]
fn default_agent_spec(
    provider: String,
    cwd: Option<String>,
    rows: Option<u16>,
    cols: Option<u16>,
    attribution_key: Option<String>,
    model: Option<String>,
    permission_mode: Option<String>,
    inject_orchestration: bool,
    profile: String,
) -> AgentSpawnSpec {
    AgentSpawnSpec {
        provider,
        profile: AgentProfile {
            // The daemon resolves this name against its profile store
            // (~/.taime/agents/*.toml + built-ins) and fills system_prompt/model/
            // tools when we leave them unset.
            name: profile,
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
        // Plain agents don't get the orchestration tools; an "orchestrator" launch
        // sets this so the agent can call list_agents/send_message/handoff/assign.
        inject_orchestration,
    }
}

/// Generic daemon query RPC (Phase 6 route-layer migration) — the daemon-backed
/// replacement for the CAO REST surface (diff/hunks/attribution/contention/
/// worktree/sessions). Returns a JSON value in the frontend's existing shape.
#[tauri::command]
pub async fn daemon_query(
    daemon: State<'_, DaemonClient>,
    kind: String,
    args: serde_json::Value,
    fallback: Option<String>,
) -> Result<serde_json::Value, String> {
    let fb = fallback.unwrap_or_else(|| "null".to_string());
    let json = daemon.query(kind, args.to_string(), &fb).await?;
    serde_json::from_str(&json).map_err(|e| format!("parse query result: {e}"))
}

/// The daemon-side activity graph (agents + inter-agent assign/handoff/message
/// edges), read from the durable store (Phase 6). Complete even with the UI
/// closed; the frontend route switch to this lands with the diff move.
#[tauri::command]
pub async fn daemon_activity_graph(
    daemon: State<'_, DaemonClient>,
) -> Result<serde_json::Value, String> {
    let json = daemon.activity_graph().await?;
    serde_json::from_str(&json).map_err(|e| format!("parse graph: {e}"))
}

/// Enqueue an inbox message for a live daemon agent (Phase 5 message bus) — the
/// ops/app entry; the daemon delivers it into the receiver's stdin when idle.
#[tauri::command]
pub async fn daemon_send_message(
    daemon: State<'_, DaemonClient>,
    sender: String,
    receiver: String,
    message: String,
) -> Result<i64, String> {
    daemon.send_message(sender, receiver, message).await
}

/// Launch any supported CLI (`claude_code`/`codex`/`gemini_cli`/`grok_cli`) via
/// the daemon's provider registry with the default (unrestricted) profile.
#[allow(clippy::too_many_arguments)]
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
    inject_orchestration: Option<bool>,
    profile: Option<String>,
) -> Result<String, String> {
    let spec = default_agent_spec(
        provider,
        cwd,
        rows,
        cols,
        attribution_key,
        model,
        permission_mode,
        inject_orchestration.unwrap_or(false),
        profile.unwrap_or_else(|| "default".to_string()),
    );
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
