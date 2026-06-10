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

/// Create (and optionally `git init`) a brand-new project folder for the "Start
/// something new" flow, then return its [`WorkspaceInfo`]. App-side (a direct fs
/// call, like `workspace_info`) so the dialog can *generate* a workspace from a
/// typed / not-yet-existing path without a daemon round-trip. Idempotent:
/// `create_dir_all` on an existing dir is fine, and `git init` is skipped when
/// the path is already inside a repo.
///
/// Callers default `git_init` to false: the founding agent runs its own
/// `git init` + first commit so genesis stays attributed to its turn (the
/// flagship thesis) — this command just guarantees the folder exists.
#[tauri::command]
pub fn workspace_init(path: String, git_init: bool) -> Result<WorkspaceInfo, String> {
    init_workspace_dir(std::path::Path::new(&path), git_init).map_err(|e| e.to_string())?;
    Ok(workspace_info(path))
}

fn init_workspace_dir(p: &std::path::Path, git_init: bool) -> std::io::Result<()> {
    std::fs::create_dir_all(p)?;
    if git_init {
        // Don't re-init a path already inside a repo (idempotent / never clobbers).
        let inside =
            git_probe(p, &["rev-parse", "--is-inside-work-tree"]).as_deref() == Some("true");
        if !inside {
            let _ = std::process::Command::new("git").current_dir(p).args(["init"]).output();
        }
    }
    Ok(())
}

/// Permanently delete a directory and all its contents — the "delete workspace
/// (also remove from disk)" flow. DESTRUCTIVE + irreversible. The UI gates this
/// behind an explicit checkbox AND a typed folder-name confirmation; these
/// backend guards are belt-and-suspenders so a bug can never wipe the filesystem
/// root, the home directory (or an ancestor of it), or a shallow top-level dir,
/// regardless of what the caller sends.
#[tauri::command]
pub fn delete_directory(path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    if !p.is_dir() {
        return Err(format!("not a directory: {path}"));
    }
    // Resolve symlinks / `..` so the guards apply to the REAL target.
    let canon = std::fs::canonicalize(p).map_err(|e| format!("resolve path: {e}"))?;
    guard_deletable(&canon)?;
    std::fs::remove_dir_all(&canon).map_err(|e| format!("delete failed: {e}"))
}

/// Refuse to delete catastrophic paths no matter what the UI sent.
fn guard_deletable(canon: &std::path::Path) -> Result<(), String> {
    if canon.parent().is_none() {
        return Err("refusing to delete the filesystem root".into());
    }
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
        if let Ok(home_c) = std::fs::canonicalize(&home) {
            if canon == home_c {
                return Err("refusing to delete the home directory".into());
            }
            // `canon` is an ancestor of home (e.g. "/Users", "/") → never.
            if home_c.starts_with(canon) {
                return Err("refusing to delete a parent of the home directory".into());
            }
        }
    }
    // Require some depth so a top-level dir ("/Users", "/tmp"'s parent, …) can
    // never be removed even if the home checks are unavailable.
    let depth = canon
        .components()
        .filter(|c| matches!(c, std::path::Component::Normal(_)))
        .count();
    if depth < 2 {
        return Err("refusing to delete a top-level directory".into());
    }
    Ok(())
}


// ---------------------------------------------------------------------------
// Claude terminal transport: the detached `taime-session-daemon` (the ONE Rust
// PTY path). The daemon owns the PTY + an authoritative wezterm-term grid and
// survives app crashes; output streams as RAW BYTES over a per-session binary
// `Channel<InvokeResponseBody>` (no base64). The app's `DaemonClient` bridges the
// socket to the per-session channel. The daemon is the ONLY transport — every
// supported CLI launches through it (CAO/tmux are deleted).
// ---------------------------------------------------------------------------

use crate::daemon::DaemonClient;
use taime_protocol::{AgentProfile, AgentSpawnSpec, SessionSummary, WorktreeInfo};

/// Connect-only daemon liveness probe: true iff a live daemon answered the
/// handshake. Never spawns one — distinguishes "daemon answered" from
/// "fallback used" (`daemon_query` returns its fallback when no daemon is up).
#[tauri::command]
pub async fn daemon_ping(daemon: State<'_, DaemonClient>) -> Result<bool, String> {
    Ok(daemon.ping().await)
}

/// Whether a background poll saw an incompatible/unresponsive daemon (review M2).
/// The UI shows a "restart backend (stops agents)" prompt instead of the poll
/// silently replacing it.
#[tauri::command]
pub async fn daemon_incompatible(daemon: State<'_, DaemonClient>) -> Result<bool, String> {
    Ok(daemon.incompatible_seen())
}

/// User-consented backend replacement (review M2): the explicit action behind the
/// incompatible-daemon prompt. Stops the old daemon's agents and spawns the
/// current binary.
#[tauri::command]
pub async fn daemon_restart(daemon: State<'_, DaemonClient>) -> Result<(), String> {
    daemon.force_restart().await
}

/// Provision (or resolve) an isolated git worktree for a daemon agent (Phase 3) —
/// the daemon-owned replacement for CAO's `/worktrees/provision`. Returns the
/// worktree info (snake_case fields, incl. `agent_id` = the Agent ID).
#[tauri::command]
pub async fn daemon_provision_worktree(
    daemon: State<'_, DaemonClient>,
    project_root: String,
    provider: String,
    isolate: bool,
    task_id: Option<String>,
) -> Result<WorktreeInfo, String> {
    daemon.provision_worktree(project_root, provider, isolate, task_id).await
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
    agent_id: Option<String>,
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
        agent_id,
        seed_prompt: None,
        env: vec![],
        // Plain agents don't get the orchestration tools; an "orchestrator" launch
        // sets this so the agent can call list_agents/send_message/handoff/assign.
        inject_orchestration,
    }
}

/// Generic daemon query RPC (Phase 6 route-layer migration) — the daemon-backed
/// replacement for the CAO REST surface (diff/hunks/attribution/contention/
/// worktree/agents). Returns a JSON value in the frontend's existing shape.
/// `fallback: None` is strict mode: a dead daemon is an error ("daemon
/// unreachable"), never a well-typed empty value masquerading as data — the
/// review/trust surfaces depend on this distinction.
#[tauri::command]
pub async fn daemon_query(
    daemon: State<'_, DaemonClient>,
    kind: String,
    args: serde_json::Value,
    fallback: Option<String>,
) -> Result<serde_json::Value, String> {
    let json = daemon.query(kind, args.to_string(), fallback.as_deref()).await?;
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
    agent_id: Option<String>,
    inject_orchestration: Option<bool>,
    profile: Option<String>,
) -> Result<String, String> {
    let spec = default_agent_spec(
        provider,
        cwd,
        rows,
        cols,
        agent_id,
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

/// Terminal input. `String` is correct at this boundary, deliberately: the only
/// input source is xterm.js `onData`, which yields JS strings — serde carries
/// any JS string as valid UTF-8, and the PTY receives those bytes verbatim
/// (`into_bytes` never re-encodes). Arbitrary non-UTF-8 bytes can't originate
/// from the webview; if a future non-xterm source needs them, add a raw-body
/// variant rather than widening this one.
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

/// Load an image FILE into the macOS system clipboard (as image data), so a
/// dragged image can be ingested by a CLI that reads clipboard images on paste
/// (e.g. Claude Code's Ctrl+V image paste → `[Image #N]`). A web terminal can't
/// pipe image bytes through the PTY, so the clipboard is the channel; the caller
/// then sends the agent its paste trigger.
#[cfg(target_os = "macos")]
#[tauri::command]
pub fn set_clipboard_image_from_path(path: String) -> Result<(), String> {
    // Defense in depth: this path is interpolated into an AppleScript source
    // string. The escaping below handles `"` and `\`, but a newline (or other
    // control char) would terminate the string literal and let the remainder
    // of the path execute as a fresh AppleScript statement (e.g. `do shell
    // script`). Reject control characters outright, and a leading '-' so the
    // value can never be misread as an osascript flag.
    if path.chars().any(|c| c.is_control()) {
        return Err("invalid path: contains control characters".to_string());
    }
    if path.starts_with('-') {
        return Err("invalid path: leading '-'".to_string());
    }
    let p = std::path::Path::new(&path);
    if !p.is_file() {
        return Err(format!("not a file: {path}"));
    }
    // AppleScript image class for the clipboard, by extension. Only known image
    // extensions are accepted (mirrors IMAGE_EXTS in src/lib/terminalInput.ts);
    // anything else is refused before osascript is ever invoked.
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let class = match ext.as_str() {
        "jpg" | "jpeg" => "«class JPEG»",
        "gif" => "«class GIFf»",
        "tif" | "tiff" => "«class TIFF»",
        "png" | "webp" | "bmp" | "heic" | "svg" => "«class PNGf»",
        _ => return Err(format!("unsupported image extension: {ext:?}")),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_init_creates_dir_and_optional_git() {
        let base = std::env::temp_dir().join(format!("taime-wsinit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let proj = base.join("nested").join("new-project");

        // git_init=false: just the directory (the agent does its own git init).
        init_workspace_dir(&proj, false).unwrap();
        assert!(proj.is_dir(), "nested dir created");
        assert!(!proj.join(".git").exists(), "no repo when git_init=false");

        // Idempotent + git_init=true initializes a repo.
        init_workspace_dir(&proj, true).unwrap();
        assert!(proj.join(".git").exists(), "git init created a repo");

        // Re-running with git_init=true is a no-op (already inside a repo).
        init_workspace_dir(&proj, true).unwrap();
        assert!(proj.join(".git").exists());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn delete_directory_removes_a_nested_temp_dir() {
        // The ONLY real deletion in these tests is a temp dir we just created —
        // never a real system path. (Dangerous paths are checked via the pure
        // `guard_deletable` below, which never touches the filesystem, so a
        // guard regression can't make `cargo test` wipe the dev's machine.)
        let base = std::env::temp_dir().join(format!("taime-del-{}", std::process::id()));
        let proj = base.join("proj");
        std::fs::create_dir_all(proj.join("sub")).unwrap();
        std::fs::write(proj.join("f.txt"), "x").unwrap();

        assert!(delete_directory(proj.to_string_lossy().to_string()).is_ok());
        assert!(!proj.exists());

        // A non-directory / missing path is an error, not a delete.
        assert!(delete_directory(base.join("nope").to_string_lossy().to_string()).is_err());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn guard_refuses_root_home_and_shallow_paths() {
        use std::path::Path;
        // PURE guard checks — no `remove_dir_all` is ever invoked here, so this
        // test is incapable of deleting anything even if a guard were wrong.
        assert!(guard_deletable(Path::new("/")).is_err(), "filesystem root");
        assert!(guard_deletable(Path::new("/Users")).is_err(), "shallow top-level dir");
        if let Some(home) = std::env::var_os("HOME") {
            if let Ok(home_c) = std::fs::canonicalize(home) {
                assert!(guard_deletable(&home_c).is_err(), "home directory");
            }
        }
        // A normal deep project path passes the guard.
        assert!(guard_deletable(Path::new("/Users/someone/projects/app")).is_ok());
    }
}
