//! Grok Build CLI adapter (`grok`).
//!
//! Command parity with CAO's `_build_grok_command`: `grok --always-approve`
//! (base_args) + `--model`. v1 injects no system prompt and no MCP (Grok exposes
//! `--agent <file>` but no inline system-prompt flag, and no MCP config) — the
//! adapter mirrors that. `paste_enter_count` is 1 (verified single-Enter submit).
//!
//! Status heuristics ported from `grok_cli.py:32-75,177-223`.

// Status heuristics + patterns are wired into the protocol + UI in Phase 4.
#![allow(dead_code)]

use regex::Regex;
use std::sync::LazyLock;

use taime_protocol::{AgentProfile, AgentStatus};

use super::config::ProviderDefaults;
use super::{ApprovalPrompt, Cleanup, DaemonSessionSpec, GridView, LaunchOpts, Prepared, Provider};

pub struct GrokProvider {
    defaults: ProviderDefaults,
}

impl GrokProvider {
    pub fn new(defaults: ProviderDefaults) -> Self {
        GrokProvider { defaults }
    }
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
        // v1: no system-prompt flag, no MCP (matches CAO).
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
        Ok(Prepared { spec, cleanup: Cleanup::default() })
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
        if ERROR_RE.is_match(&text) {
            return AgentStatus::Error;
        }
        if PROCESSING_RE.is_match(&text) || PLAN_RE.is_match(&text) {
            return AgentStatus::Processing;
        }
        if TURN_COMPLETE_RE.is_match(&text) {
            return AgentStatus::Completed;
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
    Regex::new(r"(?i)(?:^|\n)\s*(?:Error:|ERROR:|panic:|rate limit|quota exceeded|authentication failed|failed to)")
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
        let a = p.build(&prof, &opts()).unwrap().spec.args;
        assert_eq!(a, vec!["--always-approve", "--model", "grok-code"]);
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
