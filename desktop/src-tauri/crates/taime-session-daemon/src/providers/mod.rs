//! Provider adapters — one per coding CLI, behind a [`Provider`] trait + a
//! TOML-backed [`Registry`]. This is the Phase-1 generalization of the daemon's
//! Claude-only spawn into "launch any agent with the same capabilities CAO gave
//! it", with the recipe living **daemon-side** so Phase-5 headless `assign` can
//! spawn workers without the app.
//!
//! The split mirrors the plan: TOML holds the **data** that's reasonable to
//! override per machine (binary path, base args, model flag, extra env —
//! [`config`]); the trait holds the **behavior** that can't be data (command
//! shape, per-provider MCP injection mechanism + cleanup, status/approval
//! heuristics, paste-enter count). A data-only provider is served from TOML; a
//! quirky one overrides trait methods — no `match provider { … }` scattered
//! through the daemon.
//!
//! What each provider's `build()` produces is a [`Prepared`]: the concrete PTY
//! [`DaemonSessionSpec`] the session layer spawns, plus a data-driven [`Cleanup`]
//! the session runs on exit (delete temp MCP config, restore a mutated
//! settings.json, rm a per-terminal workspace). The read-only behavior methods
//! ([`Provider::status`], [`Provider::approval_prompt`],
//! [`Provider::extract_response`], [`Provider::idle_pattern`]) operate on a
//! [`GridView`] — the emulator's already-de-ANSI'd visible screen — and are
//! defined here (ported from CAO's provider heuristics) so Phase 4 only has to
//! *wire* them into a status push, not invent them.

use std::path::PathBuf;

use taime_protocol::{AgentProfile, AgentSpawnSpec, AgentStatus};

mod claude;
mod codex;
mod config;
mod gemini;
mod grok;
mod status_util;

pub use config::ProvidersConfig;

/// The concrete PTY launch the daemon executes for an agent — the generalization
/// of the wire `SpawnSpec` to any binary, plus the provider behavior the PTY
/// layer needs (env removal, paste-enter count).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DaemonSessionSpec {
    pub prog: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    /// Env overrides/additions applied on top of the inherited daemon env.
    pub env: Vec<(String, String)>,
    /// Inherited env vars to DROP before applying `env` (e.g. Claude unsets
    /// `CLAUDE*` to avoid a "nested session" error).
    pub env_remove: Vec<String>,
    pub rows: u16,
    pub cols: u16,
    pub attribution_key: Option<String>,
    /// Enters to send after a bracketed paste (CAO `paste_enter_count`): Claude's
    /// Ink TUI needs 2, others 1. Carried here so the session/input layer (Phase 5
    /// delivery) doesn't need to re-resolve the provider.
    pub paste_enter_count: u8,
}

/// A spawn-time side effect to undo when the session exits. Data-driven (not
/// closures) so it is `Send` + unit-testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanupAction {
    /// Delete a temp file written at spawn (e.g. a temp MCP config json). Not yet
    /// constructed (Claude injects MCP inline); reserved for the temp-file
    /// strategy + Phase-5 daemon-MCP config files.
    #[allow(dead_code)]
    RemoveFile(PathBuf),
    /// Remove named keys from `mcpServers` in a JSON settings file, deleting the
    /// `mcpServers` object if it becomes empty (gemini `~/.gemini/settings.json`).
    RemoveJsonMcpServers { path: PathBuf, names: Vec<String> },
    /// Recursively remove a per-terminal workspace dir (gemini `GEMINI.md` home).
    RemoveDir(PathBuf),
}

/// The accumulated undo for a spawn. Run once, best-effort, on session exit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cleanup {
    pub actions: Vec<CleanupAction>,
}

impl Cleanup {
    pub fn push(&mut self, action: CleanupAction) {
        self.actions.push(action);
    }

    /// Best-effort teardown: every action is independent; a failure in one is
    /// logged and skipped (a teardown must never panic the reader thread).
    pub fn run(&self) {
        for action in &self.actions {
            match action {
                CleanupAction::RemoveFile(p) => {
                    let _ = std::fs::remove_file(p);
                }
                CleanupAction::RemoveDir(p) => {
                    let _ = std::fs::remove_dir_all(p);
                }
                CleanupAction::RemoveJsonMcpServers { path, names } => {
                    let _ = config::remove_json_mcp_servers(path, names);
                }
            }
        }
    }
}

/// Everything needed to spawn + later tear down one agent session.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub spec: DaemonSessionSpec,
    pub cleanup: Cleanup,
}

/// Non-profile launch parameters (the `opts` of the plan's `command(profile,
/// opts)`): viewport, cwd, attribution key (doubles as `CAO_TERMINAL_ID`), and a
/// reserved seed prompt. Extra env overrides are applied by the registry after
/// the provider builds its command.
#[derive(Debug, Clone, Default)]
pub struct LaunchOpts {
    pub cwd: Option<String>,
    pub rows: u16,
    pub cols: u16,
    pub attribution_key: Option<String>,
    /// Reserved: the first prompt to seed once the agent is ready (Phase-5
    /// `assign`/`handoff` seeding); the app drives initial input today.
    #[allow(dead_code)]
    pub seed_prompt: Option<String>,
}

impl LaunchOpts {
    /// `CAO_TERMINAL_ID` — the attribution key, or "" when unset.
    fn terminal_id(&self) -> &str {
        self.attribution_key.as_deref().unwrap_or("")
    }
}

/// A read-only snapshot of an agent's visible screen. The emulator has already
/// interpreted ANSI, so these are plain text rows (trailing blanks trimmed) —
/// the status/approval heuristics run on clean text, simpler than CAO's
/// regex-on-raw-bytes approach.
//
// `dead_code`: the status subsystem (GridView + the trait's status/approval/
// extract methods) is defined in Phase 1 and wired into a `SessionSummary.status`
// + push event in Phase 4; until then it's exercised only by unit tests.
#[allow(dead_code)]
pub struct GridView<'a> {
    pub lines: &'a [String],
}

#[allow(dead_code)]
impl<'a> GridView<'a> {
    pub fn new(lines: &'a [String]) -> Self {
        GridView { lines }
    }

    /// The whole screen as one `\n`-joined string.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// The last `n` non-blank lines (the footer/prompt area most heuristics scan).
    pub fn nonblank_tail(&self, n: usize) -> Vec<&str> {
        let nb: Vec<&str> = self
            .lines
            .iter()
            .map(|s| s.as_str())
            .filter(|s| !s.trim().is_empty())
            .collect();
        let start = nb.len().saturating_sub(n);
        nb[start..].to_vec()
    }
}

/// A detected approval/permission prompt (the `WAITING_USER_ANSWER` signal).
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // surfaced in the Phase-4 status push.
pub struct ApprovalPrompt {
    /// The matched prompt text, for surfacing in the UI (Phase 4/5).
    pub hint: String,
}

/// One adapter per CLI. `build()` is the spawn recipe (command + MCP injection +
/// cleanup); the rest is read-only behavior over the live grid.
//
// `dead_code`: `status`/`approval_prompt`/`extract_response`/`idle_pattern` are
// the Phase-1 abstraction wired into the wire protocol + UI in Phase 4/5; for now
// only unit tests call them.
#[allow(dead_code)]
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;

    /// Build the concrete PTY launch + teardown for `profile` + `opts`. Does any
    /// spawn-time IO (temp MCP config, settings.json merge, workspace + GEMINI.md)
    /// and records the matching [`Cleanup`]. Internally factors into base-command
    /// construction and per-strategy MCP injection.
    fn build(&self, profile: &AgentProfile, opts: &LaunchOpts) -> std::io::Result<Prepared>;

    /// Enters to send after a bracketed paste (CAO `paste_enter_count`).
    fn paste_enter_count(&self) -> u8 {
        2
    }

    /// Infer lifecycle status from the live grid (wired in Phase 4).
    fn status(&self, view: &GridView) -> AgentStatus;

    /// Detect an approval/permission prompt (the `WAITING_USER_ANSWER` heuristic).
    fn approval_prompt(&self, _view: &GridView) -> Option<ApprovalPrompt> {
        None
    }

    /// Extract the agent's last response for handoff/attribution, if exposed.
    fn extract_response(&self, _text: &str) -> Option<String> {
        None
    }

    /// Regex that marks "ready for input" in a plain-text log tail — the
    /// idle-gated delivery primitive (CAO `get_idle_pattern_for_log`; Phase 5).
    fn idle_pattern(&self) -> &str;
}

/// The provider registry: TOML-backed defaults + the built-in adapters. One per
/// daemon, cheap to construct (config is parsed once at load).
pub struct Registry {
    config: ProvidersConfig,
}

impl Default for Registry {
    fn default() -> Self {
        Self::load()
    }
}

impl Registry {
    /// Load `~/.taime/providers.toml` over the built-in defaults.
    pub fn load() -> Self {
        Registry { config: ProvidersConfig::load() }
    }

    /// Construct the adapter for `id`, seeded with its (possibly user-overridden)
    /// TOML defaults. Unknown ids return `None`.
    fn provider(&self, id: &str) -> Option<Box<dyn Provider>> {
        let defaults = self.config.get(id);
        match id {
            "claude_code" => Some(Box::new(claude::ClaudeProvider::new(defaults))),
            "codex" => Some(Box::new(codex::CodexProvider::new(defaults))),
            "gemini_cli" => Some(Box::new(gemini::GeminiProvider::new(defaults))),
            "grok_cli" => Some(Box::new(grok::GrokProvider::new(defaults))),
            _ => None,
        }
    }

    /// Build the concrete launch + MCP cleanup for a high-level spawn request.
    /// `extra` env overrides from the request are applied last.
    pub fn build(&self, req: &AgentSpawnSpec) -> Result<Prepared, String> {
        let provider = self
            .provider(&req.provider)
            .ok_or_else(|| format!("unknown provider '{}'", req.provider))?;
        let opts = LaunchOpts {
            cwd: req.cwd.clone(),
            rows: req.rows,
            cols: req.cols,
            attribution_key: req.attribution_key.clone(),
            seed_prompt: req.seed_prompt.clone(),
        };
        let mut prepared = provider
            .build(&req.profile, &opts)
            .map_err(|e| format!("build '{}': {e}", req.provider))?;
        // Final env precedence (lowest → highest): TOML `[providers.<id>].env`,
        // then the provider's own env (e.g. codex's `CAO_TERMINAL_ID`), then the
        // caller's request `env` overrides.
        let mut env: Vec<(String, String)> = self.config.get(&req.provider).env.into_iter().collect();
        env.sort(); // deterministic order from the HashMap
        env.extend(std::mem::take(&mut prepared.spec.env));
        env.extend(req.env.iter().cloned());
        prepared.spec.env = env;
        prepared.spec.attribution_key = req.attribution_key.clone();
        Ok(prepared)
    }

    /// The adapter for a provider id, for status/idle queries (Phase 4/5).
    pub fn adapter(&self, id: &str) -> Option<Box<dyn Provider>> {
        self.provider(id)
    }
}

/// Shared helper: stamp `CAO_TERMINAL_ID` into an MCP server's env so the server
/// can resolve handoff/assign context. The daemon-stamped id is the **spoof-proof
/// identity**, so it OVERWRITES any value the profile's server env may carry
/// (matching CAO, which assigns `env["CAO_TERMINAL_ID"]` unconditionally).
fn with_terminal_id(env: &[(String, String)], terminal_id: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> =
        env.iter().filter(|(k, _)| k != "CAO_TERMINAL_ID").cloned().collect();
    out.push(("CAO_TERMINAL_ID".to_string(), terminal_id.to_string()));
    out
}

// Re-exported for the Phase-5 log-tail idle check (raw stream, not the grid).
#[allow(unused_imports)]
pub(crate) use status_util::strip_ansi;

#[cfg(test)]
mod tests {
    use super::*;

    fn default_req(provider: &str) -> AgentSpawnSpec {
        AgentSpawnSpec {
            provider: provider.to_string(),
            profile: AgentProfile { name: "default".into(), ..Default::default() },
            cwd: Some("/tmp/wt".into()),
            rows: 24,
            cols: 80,
            attribution_key: Some("term-1".into()),
            seed_prompt: None,
            env: vec![],
            inject_orchestration: false,
        }
    }

    #[test]
    fn registry_builds_each_default_provider() {
        let reg = Registry::load();
        for p in ["claude_code", "codex", "gemini_cli", "grok_cli"] {
            let prepared = reg.build(&default_req(p)).unwrap_or_else(|e| panic!("{p}: {e}"));
            assert!(!prepared.spec.prog.is_empty(), "{p} has a program");
            assert_eq!(prepared.spec.cwd.as_deref(), Some("/tmp/wt"));
            assert_eq!(prepared.spec.attribution_key.as_deref(), Some("term-1"));
        }
    }

    #[test]
    fn unknown_provider_errors() {
        let reg = Registry::load();
        assert!(reg.build(&default_req("nope")).is_err());
    }

    #[test]
    fn agent_status_json_matches_cao_status_vocab() {
        // The frontend StatusBadge keys on these exact strings; the daemon→app
        // status field must serialize to them (serde rename_all).
        use taime_protocol::AgentStatus;
        let cases = [
            (AgentStatus::Idle, "\"IDLE\""),
            (AgentStatus::Processing, "\"PROCESSING\""),
            (AgentStatus::WaitingUserAnswer, "\"WAITING_USER_ANSWER\""),
            (AgentStatus::Completed, "\"COMPLETED\""),
            (AgentStatus::Error, "\"ERROR\""),
        ];
        for (status, json) in cases {
            assert_eq!(serde_json::to_string(&status).unwrap(), json);
        }
    }

    #[test]
    fn default_profiles_are_unrestricted_launches() {
        let reg = Registry::load();
        // Claude default → skip-permissions; codex default → --yolo; gemini → --yolo;
        // grok → --always-approve. (Parity with CAO's default/unrestricted path.)
        let claude = reg.build(&default_req("claude_code")).unwrap();
        assert!(claude.spec.args.iter().any(|a| a == "--dangerously-skip-permissions"));
        let codex = reg.build(&default_req("codex")).unwrap();
        assert!(codex.spec.args.iter().any(|a| a == "--yolo"));
        assert!(codex.spec.args.iter().any(|a| a == "--no-alt-screen"));
        let gemini = reg.build(&default_req("gemini_cli")).unwrap();
        assert!(gemini.spec.args.iter().any(|a| a == "--yolo"));
        let grok = reg.build(&default_req("grok_cli")).unwrap();
        assert!(grok.spec.args.iter().any(|a| a == "--always-approve"));
        assert_eq!(grok.spec.paste_enter_count, 1);
        assert_eq!(claude.spec.paste_enter_count, 2);
    }

    #[test]
    fn terminal_id_is_authoritative_overwrites_profile_value() {
        // A profile-supplied CAO_TERMINAL_ID must be overwritten by the daemon's
        // spoof-proof id (CAO assigns it unconditionally).
        let env = vec![
            ("FOO".to_string(), "bar".to_string()),
            ("CAO_TERMINAL_ID".to_string(), "spoofed".to_string()),
        ];
        let out = with_terminal_id(&env, "real-tid");
        let tid: Vec<&String> = out
            .iter()
            .filter(|(k, _)| k == "CAO_TERMINAL_ID")
            .map(|(_, v)| v)
            .collect();
        assert_eq!(tid, vec!["real-tid"], "exactly one, authoritative id");
        assert!(out.iter().any(|(k, v)| k == "FOO" && v == "bar"), "other env preserved");
    }

    #[test]
    fn extra_env_is_applied_last() {
        let reg = Registry::load();
        let mut req = default_req("grok_cli");
        req.env = vec![("MY_KEY".into(), "v".into())];
        let prepared = reg.build(&req).unwrap();
        assert!(prepared
            .spec
            .env
            .iter()
            .any(|(k, v)| k == "MY_KEY" && v == "v"));
    }
}
