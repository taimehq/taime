//! Codex adapter (`codex`).
//!
//! Command parity with CAO's `_build_codex_command`:
//!   * `--yolo` by default, `--profile <p>` for a restricted profile, always with
//!     `--no-alt-screen --disable shell_snapshot` (base_args);
//!   * `--model`;
//!   * system prompt via `-c developer_instructions="<escaped>"` (TOML-escaped),
//!     prefixed with a tool constraint when restricted;
//!   * MCP via per-field `-c mcp_servers.<name>.…` overrides + an `env_vars` list
//!     that inherits `CAO_TERMINAL_ID` from the child env + a 600s tool timeout.
//!
//! Unlike CAO (which types the command into a shell in a fresh tmux pane and warms
//! it up with `echo ready`), the daemon spawns `codex` directly as the PTY child —
//! so the fresh-shell exit quirk the warm-up worked around does not apply.

// Status heuristics + patterns are wired into the protocol + UI in Phase 4.
#![allow(dead_code)]

use regex::Regex;
use std::sync::LazyLock;

use taime_protocol::{AgentProfile, AgentStatus, McpServerConfig};

use super::config::ProviderDefaults;
use super::{ApprovalPrompt, Cleanup, DaemonSessionSpec, GridView, LaunchOpts, Prepared, Provider};

pub struct CodexProvider {
    defaults: ProviderDefaults,
}

impl CodexProvider {
    pub fn new(defaults: ProviderDefaults) -> Self {
        CodexProvider { defaults }
    }
}

fn tools_restricted(profile: &AgentProfile) -> bool {
    !profile.allowed_tools.is_empty() && !profile.allowed_tools.iter().any(|t| t == "*")
}

/// TOML string escaping for `-c key="value"` overrides.
fn escape_toml(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

impl Provider for CodexProvider {
    fn id(&self) -> &str {
        "codex"
    }

    fn build(&self, profile: &AgentProfile, opts: &LaunchOpts) -> std::io::Result<Prepared> {
        let restricted = tools_restricted(profile);
        let mut args: Vec<String> = Vec::new();

        match (&profile.codex_profile, restricted) {
            (Some(cp), true) => {
                args.push("--profile".into());
                args.push(cp.clone());
            }
            _ => args.push("--yolo".into()),
        }
        args.extend(self.defaults.base_args.iter().cloned());

        if let Some(model) = &profile.model {
            args.push(self.defaults.model_flag.clone());
            args.push(model.clone());
        }

        // developer_instructions = [tool constraint if restricted] + system prompt.
        let mut instructions = String::new();
        if restricted {
            // NOTE(phase1): CAO prepends a fuller SECURITY_PROMPT; the tool
            // constraint is the load-bearing part. Restricted codex profiles still
            // route through CAO until the full prompt is ported.
            let tools = profile.allowed_tools.join(", ");
            instructions.push_str(&format!(
                "You only have access to these tools: {tools}\n\n"
            ));
        }
        if let Some(sp) = profile.system_prompt.as_deref().filter(|s| !s.is_empty()) {
            instructions.push_str(sp);
        }
        if !instructions.is_empty() {
            args.push("-c".into());
            args.push(format!("developer_instructions=\"{}\"", escape_toml(&instructions)));
        }

        let env_remove = std::env::vars()
            .map(|(k, _)| k)
            .filter(|k| k.starts_with("CODEX_"))
            .collect();

        let mut spec = DaemonSessionSpec {
            prog: self.defaults.binary.clone(),
            args,
            cwd: opts.cwd.clone(),
            env: Vec::new(),
            env_remove,
            rows: opts.rows,
            cols: opts.cols,
            attribution_key: opts.attribution_key.clone(),
            paste_enter_count: self.paste_enter_count(),
        };

        inject_mcp(&mut spec, &profile.mcp_servers, opts.terminal_id());
        Ok(Prepared { spec, cleanup: Cleanup::default() })
    }

    fn status(&self, view: &GridView) -> AgentStatus {
        let text = view.text();
        if text.trim().is_empty() {
            return AgentStatus::Error;
        }
        if text.contains("allow Codex to work in this folder") {
            return AgentStatus::WaitingUserAnswer;
        }
        if APPROVE_RE.is_match(&text) {
            return AgentStatus::WaitingUserAnswer;
        }
        if ERROR_RE.is_match(&text) {
            return AgentStatus::Error;
        }
        if PROGRESS_RE.is_match(&text) {
            return AgentStatus::Processing;
        }
        let tail = view.nonblank_tail(5).join("\n");
        let has_idle = IDLE_PROMPT_RE.is_match(&tail);
        if has_idle {
            if ASSISTANT_RE.is_match(&text) {
                return AgentStatus::Completed;
            }
            return AgentStatus::Idle;
        }
        AgentStatus::Processing
    }

    fn approval_prompt(&self, view: &GridView) -> Option<ApprovalPrompt> {
        let text = view.text();
        if text.contains("allow Codex to work in this folder") {
            return Some(ApprovalPrompt { hint: "trust this folder".into() });
        }
        if APPROVE_RE.is_match(&text) {
            return Some(ApprovalPrompt { hint: "approve command (y/n)".into() });
        }
        None
    }

    fn idle_pattern(&self) -> &str {
        r"(?:❯|›|codex>)"
    }
}

/// MCP injection: per-field `-c mcp_servers.<name>.*` overrides. `CAO_TERMINAL_ID`
/// is added to each server's `env_vars` and set in the child process env (codex
/// inherits `env_vars` from the environment), matching CAO. No file → no cleanup.
fn inject_mcp(spec: &mut DaemonSessionSpec, servers: &[McpServerConfig], terminal_id: &str) {
    if servers.is_empty() {
        return;
    }
    for s in servers {
        let prefix = format!("mcp_servers.{}", s.name);
        push_c(&mut spec.args, format!("{prefix}.command=\"{}\"", escape_toml(&s.command)));
        let arr = s
            .args
            .iter()
            .map(|a| format!("\"{}\"", escape_toml(a)))
            .collect::<Vec<_>>()
            .join(", ");
        push_c(&mut spec.args, format!("{prefix}.args=[{arr}]"));

        let mut env_keys: Vec<String> = Vec::new();
        for (k, v) in &s.env {
            push_c(&mut spec.args, format!("{prefix}.env.{k}=\"{}\"", escape_toml(v)));
            env_keys.push(k.clone());
        }
        if !env_keys.iter().any(|k| k == "CAO_TERMINAL_ID") {
            env_keys.push("CAO_TERMINAL_ID".to_string());
        }
        let vars = env_keys
            .iter()
            .map(|k| format!("\"{k}\""))
            .collect::<Vec<_>>()
            .join(", ");
        push_c(&mut spec.args, format!("{prefix}.env_vars=[{vars}]"));
        push_c(&mut spec.args, format!("{prefix}.tool_timeout_sec=600.0"));
    }
    // CAO_TERMINAL_ID must be in the child env for codex's env_vars inheritance.
    spec.env.push(("CAO_TERMINAL_ID".to_string(), terminal_id.to_string()));
}

fn push_c(args: &mut Vec<String>, kv: String) {
    args.push("-c".into());
    args.push(kv);
}

// --- Heuristic patterns (ported from codex.py:17-65) ---
static IDLE_PROMPT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:❯|›|codex>)").unwrap());
static APPROVE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^(?:Approve|Allow)\b.*\b(?:y/n|yes/no|yes|no)\b").unwrap()
});
static ERROR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^(?:Error:|ERROR:|Traceback \(most recent call last\):|panic:)").unwrap()
});
static PROGRESS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"•.*\(\d+s\s*•\s*esc to interrupt\)").unwrap());
static ASSISTANT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^(?:(?:assistant|codex|agent)\s*:|\s*•)").unwrap());

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> ProviderDefaults {
        ProviderDefaults {
            binary: "codex".into(),
            base_args: vec!["--no-alt-screen".into(), "--disable".into(), "shell_snapshot".into()],
            model_flag: "--model".into(),
            env: Default::default(),
        }
    }
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
    fn default_is_yolo_inline() {
        let p = CodexProvider::new(defaults());
        let a = p.build(&profile(), &opts()).unwrap().spec.args;
        assert_eq!(a[0], "--yolo");
        assert!(a.windows(2).any(|w| w == ["--disable", "shell_snapshot"]));
        assert!(a.contains(&"--no-alt-screen".to_string()));
    }

    #[test]
    fn mcp_overrides_and_env_inheritance() {
        let mut prof = profile();
        prof.mcp_servers = vec![McpServerConfig { name: "cao".into(), command: "cao-mcp-server".into(), args: vec!["--stdio".into()], env: vec![("FOO".into(), "bar".into())] }];
        let p = CodexProvider::new(defaults());
        let prepared = p.build(&prof, &opts()).unwrap();
        let joined = prepared.spec.args.join(" ");
        assert!(joined.contains("mcp_servers.cao.command=\"cao-mcp-server\""));
        assert!(joined.contains("mcp_servers.cao.args=[\"--stdio\"]"));
        assert!(joined.contains("mcp_servers.cao.env.FOO=\"bar\""));
        assert!(joined.contains("CAO_TERMINAL_ID"));
        assert!(joined.contains("tool_timeout_sec=600.0"));
        assert!(prepared.spec.env.iter().any(|(k, v)| k == "CAO_TERMINAL_ID" && v == "t1"));
    }

    #[test]
    fn status_transitions() {
        let p = CodexProvider::new(defaults());
        assert_eq!(p.status(&GridView::new(&grid(&["allow Codex to work in this folder?"]))), AgentStatus::WaitingUserAnswer);
        assert_eq!(p.status(&GridView::new(&grid(&["• Working (3s • esc to interrupt)", "› "]))), AgentStatus::Processing);
        assert_eq!(p.status(&GridView::new(&grid(&["welcome", "› "]))), AgentStatus::Idle);
        assert_eq!(p.status(&GridView::new(&grid(&["• here is the result", "› "]))), AgentStatus::Completed);
        assert_eq!(p.status(&GridView::new(&grid(&["Error: boom"]))), AgentStatus::Error);
    }
}
