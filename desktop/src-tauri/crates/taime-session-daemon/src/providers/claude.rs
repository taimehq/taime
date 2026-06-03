//! Claude Code adapter (`claude`).
//!
//! Command parity with CAO's `_build_claude_command`:
//!   * native-agent thin wrapper, or full-profile decomposition;
//!   * `--dangerously-skip-permissions` by default, `--permission-mode <m>` when a
//!     restricted profile names one;
//!   * `--model`, `--append-system-prompt <escaped>`, `--disallowedTools …`;
//!   * MCP via an inline `--mcp-config <json>` carrying each server + a stamped
//!     `CAO_TERMINAL_ID` (no temp file → no cleanup);
//!   * unset inherited `CLAUDE*` env (except the bedrock/vertex/foundry/effort
//!     allowlist) so a daemon launched from inside Claude isn't a "nested session".
//!
//! Status heuristics ported from `claude_code.py:321-413` but run on the
//! emulator's already-de-ANSI'd grid.

// The status/approval/extract heuristics + their regex patterns are the Phase-1
// abstraction; Phase 4 wires them into the wire protocol + UI. Until then they're
// exercised only by this file's unit tests.
#![allow(dead_code)]

use regex::Regex;
use std::path::Path;

use taime_protocol::{AgentProfile, AgentStatus, McpServerConfig};

use super::config::ProviderDefaults;
use super::{
    with_terminal_id, ApprovalPrompt, Cleanup, DaemonSessionSpec, GridView, LaunchOpts, Prepared,
    Provider,
};

/// Inherited `CLAUDE*` vars that are SAFE to keep (auth + user preference).
const CLAUDE_ENV_ALLOWLIST: &[&str] = &[
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_SKIP_BEDROCK_AUTH",
    "CLAUDE_CODE_SKIP_VERTEX_AUTH",
    "CLAUDE_CODE_SKIP_FOUNDRY_AUTH",
    "CLAUDE_CODE_EFFORT_LEVEL",
];

pub struct ClaudeProvider {
    defaults: ProviderDefaults,
}

impl ClaudeProvider {
    pub fn new(defaults: ProviderDefaults) -> Self {
        ClaudeProvider { defaults }
    }

    /// Prefer the known install at `~/.local/bin/claude`, else the configured name.
    fn resolve_binary(&self) -> String {
        if self.defaults.binary == "claude" {
            if let Some(home) = std::env::var_os("HOME") {
                let p = Path::new(&home).join(".local/bin/claude");
                if p.exists() {
                    return p.to_string_lossy().into_owned();
                }
            }
        }
        self.defaults.binary.clone()
    }
}

/// Tools are restricted iff a non-empty allow-list lacks the `*` wildcard.
fn tools_restricted(profile: &AgentProfile) -> bool {
    !profile.allowed_tools.is_empty() && !profile.allowed_tools.iter().any(|t| t == "*")
}

/// CAO's `--append-system-prompt` escaping (backslash, then newline→literal `\n`).
fn escape_system_prompt(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n")
}

/// Inherited `CLAUDE*` env keys to drop for the child (all but the allowlist).
fn claude_env_remove() -> Vec<String> {
    std::env::vars()
        .map(|(k, _)| k)
        .filter(|k| k.starts_with("CLAUDE") && !CLAUDE_ENV_ALLOWLIST.contains(&k.as_str()))
        .collect()
}

impl Provider for ClaudeProvider {
    fn id(&self) -> &str {
        "claude_code"
    }

    fn build(&self, profile: &AgentProfile, opts: &LaunchOpts) -> std::io::Result<Prepared> {
        let mut args: Vec<String> = Vec::new();

        if let Some(native) = &profile.native_agent {
            // Thin wrapper: Claude owns config; just pass the permission mode +
            // native agent name.
            let mode = profile.permission_mode.clone().unwrap_or_else(|| "default".into());
            args.push("--permission-mode".into());
            args.push(mode);
            args.push("--agent".into());
            args.push(native.clone());
        } else {
            // Full profile decomposition.
            if tools_restricted(profile) {
                if let Some(mode) = &profile.permission_mode {
                    args.push("--permission-mode".into());
                    args.push(mode.clone());
                } else {
                    args.push("--dangerously-skip-permissions".into());
                }
            } else {
                args.push("--dangerously-skip-permissions".into());
            }
            if let Some(model) = &profile.model {
                args.push(self.defaults.model_flag.clone());
                args.push(model.clone());
            }
            if let Some(sp) = profile.system_prompt.as_deref().filter(|s| !s.is_empty()) {
                args.push("--append-system-prompt".into());
                args.push(escape_system_prompt(sp));
            }
            // NOTE(phase1): per-tool `--disallowedTools` derivation needs the CAO
            // tool_mapping table; restricted Claude profiles still go through CAO
            // until that table is ported. Default (unrestricted) launches are
            // unaffected.
        }
        args.extend(self.defaults.base_args.iter().cloned());

        let mut spec = DaemonSessionSpec {
            prog: self.resolve_binary(),
            args,
            cwd: opts.cwd.clone(),
            env: Vec::new(),
            env_remove: claude_env_remove(),
            rows: opts.rows,
            cols: opts.cols,
            attribution_key: opts.attribution_key.clone(),
            paste_enter_count: self.paste_enter_count(),
        };

        let cleanup = inject_mcp(&mut spec, &profile.mcp_servers, opts.terminal_id());
        Ok(Prepared { spec, cleanup })
    }

    fn paste_enter_count(&self) -> u8 {
        2
    }

    fn status(&self, view: &GridView) -> AgentStatus {
        let text = view.text();
        if text.trim().is_empty() {
            return AgentStatus::Error;
        }
        let tail = view.nonblank_tail(20).join("\n");

        // PROCESSING first (work-in-flight beats a stale completed marker).
        if SPINNER_RE.is_match(&tail) {
            return AgentStatus::Processing;
        }
        // WAITING: the Ink selection widget — but not the auto-handled startup
        // trust/bypass dialogs.
        if WAITING_RE.is_match(&text)
            && !text.contains("Yes, I trust this folder")
            && !text.contains("Yes, I accept")
        {
            return AgentStatus::WaitingUserAnswer;
        }
        let has_response = RESPONSE_RE.is_match(&text);
        let has_idle = IDLE_PROMPT_RE.is_match(&tail);
        if has_response && has_idle {
            return AgentStatus::Completed;
        }
        if has_idle {
            return AgentStatus::Idle;
        }
        AgentStatus::Error
    }

    fn approval_prompt(&self, view: &GridView) -> Option<ApprovalPrompt> {
        let text = view.text();
        if WAITING_RE.is_match(&text)
            && !text.contains("Yes, I trust this folder")
            && !text.contains("Yes, I accept")
        {
            return Some(ApprovalPrompt { hint: "selection (↑/↓ to navigate)".into() });
        }
        None
    }

    fn extract_response(&self, text: &str) -> Option<String> {
        // Everything after the LAST response marker, up to a start-of-line idle
        // prompt or a separator line.
        let idx = text.rfind('⏺')?;
        let after = &text[idx + '⏺'.len_utf8()..];
        let mut out: Vec<&str> = Vec::new();
        for line in after.lines() {
            if SEPARATOR_RE.is_match(line) || SOL_IDLE_RE.is_match(line) {
                break;
            }
            out.push(line);
        }
        let joined = out.join("\n").trim().to_string();
        if joined.is_empty() {
            None
        } else {
            Some(joined)
        }
    }

    fn idle_pattern(&self) -> &str {
        r"[>❯][\s\u{a0}]"
    }
}

/// MCP injection: inline `--mcp-config <json>` with `CAO_TERMINAL_ID` stamped into
/// each server's env. No temp file → empty cleanup.
fn inject_mcp(spec: &mut DaemonSessionSpec, servers: &[McpServerConfig], terminal_id: &str) -> Cleanup {
    if servers.is_empty() {
        return Cleanup::default();
    }
    let mut map = serde_json::Map::new();
    for s in servers {
        let env = with_terminal_id(&s.env, terminal_id);
        let env_obj: serde_json::Map<String, serde_json::Value> =
            env.into_iter().map(|(k, v)| (k, serde_json::Value::String(v))).collect();
        map.insert(
            s.name.clone(),
            serde_json::json!({ "command": s.command, "args": s.args, "env": env_obj }),
        );
    }
    let mcp = serde_json::json!({ "mcpServers": map });
    spec.args.push("--mcp-config".into());
    spec.args.push(mcp.to_string());
    Cleanup::default()
}

// --- Heuristic patterns (ported from claude_code.py:28-52, de-ANSI'd) ---
use std::sync::LazyLock;
static SPINNER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[✶✢✽✻✳·][^\n]*\x{2026}").unwrap());
static RESPONSE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"⏺\s").unwrap());
static IDLE_PROMPT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[>❯][\s\x{a0}]").unwrap());
static WAITING_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"↑/↓ to navigate").unwrap());
static SEPARATOR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x{2500}{20,}").unwrap());
static SOL_IDLE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*[>❯][\s\x{a0}]").unwrap());

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> AgentProfile {
        AgentProfile { name: "default".into(), ..Default::default() }
    }
    fn opts() -> LaunchOpts {
        LaunchOpts { cwd: Some("/tmp/wt".into()), rows: 24, cols: 80, attribution_key: Some("t1".into()), seed_prompt: None }
    }
    fn grid(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn default_profile_uses_skip_permissions() {
        let p = ClaudeProvider::new(ProviderDefaults { binary: "claude".into(), base_args: vec![], model_flag: "--model".into(), env: Default::default() });
        let prepared = p.build(&profile(), &opts()).unwrap();
        assert!(prepared.spec.args.contains(&"--dangerously-skip-permissions".to_string()));
        assert_eq!(prepared.spec.paste_enter_count, 2);
    }

    #[test]
    fn restricted_profile_with_mode_uses_permission_mode() {
        let mut prof = profile();
        prof.allowed_tools = vec!["read".into()];
        prof.permission_mode = Some("acceptEdits".into());
        let p = ClaudeProvider::new(ProviderDefaults { binary: "claude".into(), base_args: vec![], model_flag: "--model".into(), env: Default::default() });
        let prepared = p.build(&prof, &opts()).unwrap();
        let a = &prepared.spec.args;
        assert!(a.windows(2).any(|w| w == ["--permission-mode", "acceptEdits"]));
        assert!(!a.contains(&"--dangerously-skip-permissions".to_string()));
    }

    #[test]
    fn model_and_system_prompt_are_injected() {
        let mut prof = profile();
        prof.model = Some("claude-opus-4-8".into());
        prof.system_prompt = Some("line1\nline2".into());
        let p = ClaudeProvider::new(ProviderDefaults { binary: "claude".into(), base_args: vec![], model_flag: "--model".into(), env: Default::default() });
        let a = p.build(&prof, &opts()).unwrap().spec.args;
        assert!(a.windows(2).any(|w| w == ["--model", "claude-opus-4-8"]));
        let i = a.iter().position(|x| x == "--append-system-prompt").unwrap();
        assert_eq!(a[i + 1], "line1\\nline2");
    }

    #[test]
    fn mcp_servers_become_inline_json_with_terminal_id() {
        let mut prof = profile();
        prof.mcp_servers = vec![McpServerConfig { name: "cao".into(), command: "cao-mcp-server".into(), args: vec!["--stdio".into()], env: vec![] }];
        let p = ClaudeProvider::new(ProviderDefaults { binary: "claude".into(), base_args: vec![], model_flag: "--model".into(), env: Default::default() });
        let a = p.build(&prof, &opts()).unwrap().spec.args;
        let i = a.iter().position(|x| x == "--mcp-config").unwrap();
        let json: serde_json::Value = serde_json::from_str(&a[i + 1]).unwrap();
        assert_eq!(json["mcpServers"]["cao"]["command"], "cao-mcp-server");
        assert_eq!(json["mcpServers"]["cao"]["env"]["CAO_TERMINAL_ID"], "t1");
    }

    #[test]
    fn status_processing_completed_idle() {
        let p = ClaudeProvider::new(ProviderDefaults { binary: "claude".into(), base_args: vec![], model_flag: "--model".into(), env: Default::default() });
        let processing = grid(&["✶ Thinking…", "some text"]);
        assert_eq!(p.status(&GridView::new(&processing)), AgentStatus::Processing);

        let completed = grid(&["⏺ Here is the answer", "more", "❯ "]);
        assert_eq!(p.status(&GridView::new(&completed)), AgentStatus::Completed);

        let idle = grid(&["welcome", "❯ "]);
        assert_eq!(p.status(&GridView::new(&idle)), AgentStatus::Idle);

        let waiting = grid(&["Do you want to proceed?", "  1. Yes", "  2. No", "↑/↓ to navigate"]);
        assert_eq!(p.status(&GridView::new(&waiting)), AgentStatus::WaitingUserAnswer);
    }

    #[test]
    fn extract_response_after_marker() {
        let p = ClaudeProvider::new(ProviderDefaults { binary: "claude".into(), base_args: vec![], model_flag: "--model".into(), env: Default::default() });
        let text = "garbage\n⏺ The result is 42\nmore detail\n❯ ";
        assert_eq!(p.extract_response(text).as_deref(), Some("The result is 42\nmore detail"));
    }
}
