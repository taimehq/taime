//! Grok Build CLI adapter (`grok`).
//!
//! Originally CAO-parity (`grok --always-approve` + `--model`, nothing else —
//! CAO never wired grok's restriction/prompt/MCP surface). Now full parity with
//! the other providers, using grok 0.2.32's native mechanisms (each verified
//! against the installed binary's flags + embedded docs):
//!   * `--always-approve` (base_args), `--model`;
//!   * system prompt appended via `--rules <RULES>` ("extra rules to append to
//!     the system prompt" — works in the TUI, unlike `--system-prompt-override`'s
//!     replace semantics we don't want);
//!   * tool restriction via repeatable `--deny <ToolPrefix>` permission rules
//!     (Claude Code-style prefixes; works in the TUI and "always wins" — deny is
//!     checked before approval modes, so `--always-approve` cannot override it.
//!     NOT `--disallowed-tools`, which is headless-only and silently ignored in
//!     the TUI);
//!   * MCP merged into `~/.grok/config.toml` (`[mcp_servers.<name>]`, the shape
//!     `grok mcp add` writes), format-preserving via `toml_edit`, with
//!     `CAO_TERMINAL_ID` stamped into each server's env, and removed on exit —
//!     which makes grok agents orchestrator-capable (the `taime --mcp-stdio`
//!     shim + `TAIME_MCP_TOKEN` inject like every other provider).
//!
//! `paste_enter_count` is 1 (verified single-Enter submit).
//! Status heuristics ported from `grok_cli.py:32-75,177-223`.

// Status heuristics + patterns are wired into the protocol + UI in Phase 4.
#![allow(dead_code)]

use regex::Regex;
use std::path::PathBuf;
use std::sync::LazyLock;

use taime_protocol::{AgentProfile, AgentStatus, McpServerConfig};

use super::config::{self, ProviderDefaults};
use super::{
    with_terminal_id, ApprovalPrompt, Cleanup, CleanupAction, DaemonSessionSpec, GridView,
    LaunchOpts, Prepared, Provider,
};

pub struct GrokProvider {
    defaults: ProviderDefaults,
    /// Test override of `~/.grok/config.toml` (the MCP merge target).
    config_path: Option<PathBuf>,
}

impl GrokProvider {
    pub fn new(defaults: ProviderDefaults) -> Self {
        GrokProvider { defaults, config_path: None }
    }
}

/// Tools are restricted iff a non-empty allow-list lacks the `*` wildcard.
fn tools_restricted(profile: &AgentProfile) -> bool {
    !profile.allowed_tools.is_empty() && !profile.allowed_tools.iter().any(|t| t == "*")
}

/// `~/.grok/config.toml` (the MCP merge target — where `grok mcp add` writes).
fn grok_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".grok").join("config.toml"))
}

impl Provider for GrokProvider {
    fn id(&self) -> &str {
        "grok_cli"
    }

    fn build(&self, profile: &AgentProfile, opts: &LaunchOpts) -> std::io::Result<Prepared> {
        let mut args: Vec<String> = self.defaults.base_args.clone();
        if let Some(model) = &profile.model {
            args.push(self.defaults.model_flag.clone());
            args.push(model.clone());
        }
        if let Some(sp) = profile.system_prompt.as_deref().filter(|s| !s.is_empty()) {
            args.push("--rules".into());
            args.push(sp.to_string());
        }
        // Tool restriction: one `--deny <ToolPrefix>` per blocked prefix (a bare
        // prefix matches all invocations of that type). Deny is checked before
        // approval modes, so the `--always-approve` base arg cannot override it.
        if tools_restricted(profile) {
            for prefix in super::tool_mapping::get_disallowed_tools("grok_cli", &profile.allowed_tools)
            {
                args.push("--deny".into());
                args.push(prefix);
            }
        }

        let mut cleanup = Cleanup::default();
        inject_mcp(
            self.config_path.clone().or_else(grok_config_path),
            &mut cleanup,
            &profile.mcp_servers,
            opts.terminal_id(),
        )?;

        let spec = DaemonSessionSpec {
            prog: self.defaults.binary.clone(),
            args,
            cwd: opts.cwd.clone(),
            env: Vec::new(),
            env_remove: Vec::new(),
            rows: opts.rows,
            cols: opts.cols,
            attribution_key: opts.attribution_key.clone(),
            paste_enter_count: self.paste_enter_count(),
        };
        Ok(Prepared { spec, cleanup })
    }

    fn paste_enter_count(&self) -> u8 {
        1
    }

    fn status(&self, view: &GridView) -> AgentStatus {
        let text = view.text();
        if text.trim().is_empty() {
            return AgentStatus::Processing;
        }
        let tail = view.nonblank_tail(6);
        let has_footer = tail.iter().any(|l| IDLE_FOOTER_RE.is_match(l));

        // Approval: scan the footer area, excluding permanent chrome (so the
        // "always-approve" mode bar doesn't false-match).
        let prompt_area: String = tail
            .iter()
            .filter(|l| !CHROME_RE.is_match(l))
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        if WAITING_RE.is_match(&prompt_area) {
            return AgentStatus::WaitingUserAnswer;
        }
        if PROCESSING_RE.is_match(&text) || PLAN_RE.is_match(&text) {
            return AgentStatus::Processing;
        }
        if TURN_COMPLETE_RE.is_match(&text) {
            return AgentStatus::Completed;
        }
        // ERROR scoped to the chrome-filtered prompt area and checked AFTER
        // completed (review L5): an agent quoting "Error:" / a Traceback (or, with
        // the dropped bare "failed to" alternative, normal prose like "failed to
        // compile") must not beat the legitimate Completed/Idle state.
        if ERROR_RE.is_match(&prompt_area) {
            return AgentStatus::Error;
        }
        if has_footer {
            return AgentStatus::Idle;
        }
        AgentStatus::Processing
    }

    fn approval_prompt(&self, view: &GridView) -> Option<ApprovalPrompt> {
        let prompt_area: String = view
            .nonblank_tail(6)
            .iter()
            .filter(|l| !CHROME_RE.is_match(l))
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        if WAITING_RE.is_match(&prompt_area) {
            return Some(ApprovalPrompt { hint: "approve change (y/n)".into() });
        }
        None
    }

    fn extract_response(&self, text: &str) -> Option<String> {
        let body: Vec<&str> = text
            .lines()
            .filter(|l| !l.trim().is_empty() && !EXTRACT_CHROME_RE.is_match(l))
            .collect();
        if body.is_empty() {
            return None;
        }
        let start = body.len().saturating_sub(200);
        let joined = body[start..].join("\n").trim().to_string();
        if joined.is_empty() {
            None
        } else {
            Some(joined)
        }
    }

    fn idle_pattern(&self) -> &str {
        r"Shift\+Tab:\s*mode|Ctrl\+\.\s*:\s*shortcuts"
    }
}

/// MCP injection: merge each server into `~/.grok/config.toml` as a
/// `[mcp_servers.<name>]` section and record a removal cleanup.
/// `CAO_TERMINAL_ID` is stamped into each server's env. Fail closed: a profile
/// whose MCP can't be written must not launch without its tools (an
/// orchestrator without the `taime` server would be silently inert).
fn inject_mcp(
    path: Option<PathBuf>,
    cleanup: &mut Cleanup,
    servers: &[McpServerConfig],
    terminal_id: &str,
) -> std::io::Result<()> {
    if servers.is_empty() {
        return Ok(());
    }
    let Some(path) = path else {
        return Ok(());
    };
    let stamped: Vec<McpServerConfig> = servers
        .iter()
        .map(|s| McpServerConfig {
            name: s.name.clone(),
            command: s.command.clone(),
            args: s.args.clone(),
            env: with_terminal_id(&s.env, terminal_id),
        })
        .collect();
    config::merge_toml_mcp_servers(&path, &stamped)?;
    cleanup.push(CleanupAction::RemoveTomlMcpServers {
        path,
        names: servers.iter().map(|s| s.name.clone()).collect(),
    });
    Ok(())
}

// --- Heuristic patterns (ported from grok_cli.py:32-75) ---
static IDLE_FOOTER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Shift\+Tab:\s*mode|Ctrl\+\.\s*:\s*shortcuts").unwrap());
static TURN_COMPLETE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Turn completed in\s+[\d.]+s").unwrap());
static PROCESSING_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)Esc to interrupt|Thinking|Working|Generating|Running").unwrap()
});
static PLAN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)Plan mode|Planning|subagent|sub-agent|worktree task|delegating").unwrap()
});
static WAITING_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\(y/N\)|\(Y/n\)|\by/n\b|\byes/no\b|(?:Allow|Approve|Proceed|Apply this change)\b[^\n]*\?")
        .unwrap()
});
static CHROME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"always-approve|Grok Build|Shift\+Tab:|Ctrl\+\.").unwrap());
static ERROR_RE: LazyLock<Regex> = LazyLock::new(|| {
    // `failed to` dropped (review L5) — too loose: matched normal prose like
    // "failed to compile". Kept: explicit error/panic/quota/auth markers.
    Regex::new(r"(?i)(?:^|\n)\s*(?:Error:|ERROR:|panic:|rate limit|quota exceeded|authentication failed)")
        .unwrap()
});
static EXTRACT_CHROME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"Shift\+Tab:\s*mode|Ctrl\+\.\s*:\s*shortcuts|Grok Build|always-approve|Turn completed in|Thought for|New worktree|Resume session|^\s*Quit\s|Tip:|Beta\s*$",
    )
    .unwrap()
});

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> ProviderDefaults {
        ProviderDefaults {
            binary: "grok".into(),
            base_args: vec!["--always-approve".into()],
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
    fn default_is_always_approve_with_model() {
        let mut prof = profile();
        prof.model = Some("grok-code".into());
        let p = GrokProvider::new(defaults());
        let prepared = p.build(&prof, &opts()).unwrap();
        assert_eq!(prepared.spec.args, vec!["--always-approve", "--model", "grok-code"]);
        assert!(prepared.cleanup.actions.is_empty());
    }

    #[test]
    fn system_prompt_appends_via_rules() {
        let mut prof = profile();
        prof.system_prompt = Some("you are the reviewer".into());
        let p = GrokProvider::new(defaults());
        let a = p.build(&prof, &opts()).unwrap().spec.args;
        assert_eq!(a, vec!["--always-approve", "--rules", "you are the reviewer"]);
    }

    #[test]
    fn restricted_profile_appends_sorted_deny_rules() {
        let mut prof = profile();
        prof.allowed_tools = vec!["fs_read".into(), "@taime".into()];
        let p = GrokProvider::new(defaults());
        let a = p.build(&prof, &opts()).unwrap().spec.args;
        // fs_read leaves Bash/Edit/Grep/Write blocked — exact ordered pairs.
        assert_eq!(
            a,
            vec![
                "--always-approve",
                "--deny",
                "Bash",
                "--deny",
                "Edit",
                "--deny",
                "Grep",
                "--deny",
                "Write"
            ]
        );
    }

    #[test]
    fn unrestricted_profiles_get_no_deny_rules() {
        let p = GrokProvider::new(defaults());
        for tools in [vec![], vec!["*".to_string()]] {
            let mut prof = profile();
            prof.allowed_tools = tools;
            let a = p.build(&prof, &opts()).unwrap().spec.args;
            assert_eq!(a, vec!["--always-approve"]);
        }
    }

    #[test]
    fn mcp_merges_into_config_toml_with_cleanup() {
        let dir = std::env::temp_dir().join(format!("taime-grok-mcp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "[ui]\nyolo = false\n").unwrap();

        let p = GrokProvider { defaults: defaults(), config_path: Some(path.clone()) };
        let mut prof = profile();
        prof.mcp_servers = vec![McpServerConfig {
            name: "taime".into(),
            command: "/bin/taime-session-daemon".into(),
            args: vec!["--mcp-stdio".into()],
            env: vec![("TAIME_MCP_TOKEN".into(), "tok".into())],
        }];
        let prepared = p.build(&prof, &opts()).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("[ui]\nyolo = false\n"), "user config preserved");
        assert!(content.contains("[mcp_servers.taime]"));
        assert!(content.contains("args = [\"--mcp-stdio\"]"));
        assert!(content.contains("TAIME_MCP_TOKEN = \"tok\""));
        // The daemon-stamped spoof-proof id rides along.
        assert!(content.contains("CAO_TERMINAL_ID = \"t1\""));
        assert!(prepared.cleanup.actions.iter().any(|a| matches!(
            a,
            CleanupAction::RemoveTomlMcpServers { path: p2, names }
                if p2 == &path && names == &vec!["taime".to_string()]
        )));

        // Running the cleanup restores the user's config exactly.
        prepared.cleanup.run();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[ui]\nyolo = false\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn idle_footer_is_idle_not_waiting_on_chrome() {
        let p = GrokProvider::new(defaults());
        // "always-approve" chrome must NOT trigger WAITING.
        let idle = grid(&["some output", "always-approve  Shift+Tab: mode  Ctrl+.: shortcuts"]);
        assert_eq!(p.status(&GridView::new(&idle)), AgentStatus::Idle);
    }

    #[test]
    fn real_approval_prompt_waits() {
        let p = GrokProvider::new(defaults());
        let waiting = grid(&["Apply this change? (y/N)"]);
        assert_eq!(p.status(&GridView::new(&waiting)), AgentStatus::WaitingUserAnswer);
    }

    #[test]
    fn processing_and_completed() {
        let p = GrokProvider::new(defaults());
        assert_eq!(p.status(&GridView::new(&grid(&["Thinking… esc to interrupt"]))), AgentStatus::Processing);
        let completed = grid(&["Turn completed in 3.2s", "Shift+Tab: mode  Ctrl+.: shortcuts"]);
        assert_eq!(p.status(&GridView::new(&completed)), AgentStatus::Completed);
    }
}
