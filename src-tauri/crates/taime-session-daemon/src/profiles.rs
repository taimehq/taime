//! TOML-backed agent profiles (`~/.taime/agents/*.toml`) — the named roles a
//! user can launch, or an orchestrator can `assign` by name. Each file is one
//! profile; its file stem is the profile name. Two built-ins (`default`,
//! `orchestrator`) always exist so the launcher's base options resolve even with
//! no files on disk; a file of the same name overrides the built-in.
//!
//! A profile holds only the **data** worth authoring (system prompt, model,
//! tool allow-list, …) — the subset of [`AgentProfile`] that varies per role.
//! Behavior (provider command shape, MCP wiring) stays in the adapters. The
//! daemon resolves a name → [`ProfileDef`] at spawn and fills any field the app
//! didn't set, so attribution/orchestration are coherent even headless.

use serde::Deserialize;
use taime_protocol::AgentProfile;

/// One profile's overridable fields (a subset of [`AgentProfile`]). Every field
/// is optional so a file can specify just what it needs; unknown keys are
/// ignored so files carry forward across daemon versions.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProfileDef {
    /// One-line role description (shown in the launcher).
    #[serde(default)]
    pub description: String,
    /// System prompt injected per provider (claude `--append-system-prompt`, …).
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// Claude permission mode (`default`/`acceptEdits`/`plan`/`bypassPermissions`).
    #[serde(default)]
    pub permission_mode: Option<String>,
    /// CAO-vocabulary allowed tools; `["*"]` or empty ⇒ unrestricted.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Claude `--agent <name>` thin-wrapper (delegates to Claude's agent store).
    #[serde(default)]
    pub native_agent: Option<String>,
    /// Codex `--profile <name>` (codex's own profile system).
    #[serde(default)]
    pub codex_profile: Option<String>,
    /// Inject the daemon's MCP orchestration tools (assign/handoff/…) so this
    /// agent can drive a team. The built-in `orchestrator` sets this.
    #[serde(default)]
    pub orchestrator: bool,
}

/// A resolved profile: its name + fields + where it came from (`builtin`/`file`).
pub struct Profile {
    pub name: String,
    pub def: ProfileDef,
    pub source: &'static str,
}

/// The loaded set of profiles (built-ins overlaid with `~/.taime/agents/*.toml`).
pub struct ProfileStore {
    profiles: Vec<Profile>,
}

impl ProfileStore {
    /// Built-ins overlaid with the user's `~/.taime/agents/` files. A file with
    /// the same stem as a built-in replaces it (and reports `source = "file"`).
    /// Malformed files are skipped (logged) rather than failing every spawn.
    pub fn load() -> ProfileStore {
        let mut profiles = builtins();
        if let Some(home) = dirs::home_dir() {
            let dir = home.join(".taime").join("agents");
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                        continue;
                    }
                    let Some(name) = path.file_stem().and_then(|s| s.to_str()) else { continue };
                    let text = match std::fs::read_to_string(&path) {
                        Ok(t) => t,
                        Err(e) => {
                            eprintln!("[taime-daemon] skipping unreadable profile {path:?}: {e}");
                            continue;
                        }
                    };
                    match toml::from_str::<ProfileDef>(&text) {
                        Ok(def) => {
                            // Override a same-named built-in, else append.
                            let prof = Profile { name: name.to_string(), def, source: "file" };
                            match profiles.iter_mut().find(|p| p.name == prof.name) {
                                Some(slot) => *slot = prof,
                                None => profiles.push(prof),
                            }
                        }
                        Err(e) => {
                            eprintln!("[taime-daemon] ignoring malformed profile {path:?}: {e}");
                        }
                    }
                }
            }
        }
        ProfileStore { profiles }
    }

    /// `{name, description, source}` for each profile — the launcher's list.
    pub fn infos_json(&self) -> String {
        let list: Vec<serde_json::Value> = self
            .profiles
            .iter()
            .map(|p| {
                serde_json::json!({
                    "name": p.name,
                    "description": p.def.description,
                    "source": p.source,
                })
            })
            .collect();
        serde_json::json!(list).to_string()
    }

    /// Resolve a profile by name (None ⇒ unknown; the spawn keeps app defaults).
    pub fn resolve(&self, name: &str) -> Option<&ProfileDef> {
        self.profiles.iter().find(|p| p.name == name).map(|p| &p.def)
    }
}

/// System prompt for the built-in orchestrator so the agent actually KNOWS it can
/// delegate (otherwise it only finds the tools by chance via `tools/list`).
const ORCHESTRATOR_PROMPT: &str = "\
You are the team lead (orchestrator) in Taime. You have MCP tools (server \"taime\") \
to coordinate a team of other coding agents:\n\
- assign(message, role?, tools?): spawn a worker sub-agent in its own git worktree \
to do a task; it reports back to you and Taime notifies you when it exits. Give it a \
specialist role: \"product-builder\" (build from scratch), \"feature-builder\" (add \
a feature), \"bug-fixer\" (fix a bug), \"security-reviewer\" (security review), \
\"researcher\" (investigate a question + report, read-only — use this for any \
research/investigation you need, including web searches).\n\
- list_agents(): see the live team (id, role, status).\n\
- send_message(to, body) / broadcast(body, role?): message teammates (delivered \
when they're idle).\n\
- request(to, body) / reply(interaction_id, body): ask a teammate a question and \
get a correlated answer.\n\
- handoff(to, summary): transfer the active task to another agent.\n\
- share(key, value) / get(key): a shared blackboard for plans/results.\n\n\
You ORCHESTRATE — you do NOT do hands-on work yourself. Delegate ALL execution AND \
investigation to workers via `assign`: writing code, scaffolding, running commands, \
reading files, and research (including web searches) are workers' jobs, never yours. \
If you need information to plan, assign a worker to find it and report back (via \
`share`) — don't go find out yourself. Delegate ONLY through `assign`: your own \
Task/subagent tool is DISABLED, so `assign` is the only way to spawn help — and it \
makes every teammate a tracked agent visible in Taime (an in-process subagent would \
be invisible). Your own actions are limited to talking with the user and using the \
coordination tools above. Decompose the work, run independent \
pieces in parallel, use `list_agents` to track progress, and integrate the results — \
you are the integrator: own the final result. In each `assign` message, tell the worker \
HOW to report back: post its result to the shared blackboard with `share(\"<key>\", \
value)` (give it a clear key); Taime notifies you when the worker finishes, then you \
`get(\"<key>\")` to collect it. Your workers have these SAME Taime team tools — they \
report through `share` / `send_message` to you, never a native subagent channel.";

const PRODUCT_BUILDER_PROMPT: &str = "\
You build new products/projects from scratch. Scaffold a clean, conventional project \
structure for the chosen stack, implement a working MVP, and add a README with run \
instructions. Favor simple, idiomatic choices and working software over breadth. \
Verify it builds/runs before declaring done.";

const FEATURE_BUILDER_PROMPT: &str = "\
You add features to an existing codebase. First study the surrounding code and match \
its patterns, naming, and conventions. Implement the feature with the smallest sensible \
change, add or update tests, and verify they pass. Don't refactor unrelated code.";

const BUG_FIXER_PROMPT: &str = "\
You fix bugs. Reproduce the issue first, find the ROOT cause (not just the symptom), \
make the minimal correct fix, and add a regression test that fails before and passes \
after. State the root cause briefly.";

const SECURITY_REVIEWER_PROMPT: &str = "\
You perform security reviews. Identify REAL vulnerabilities (injection, auth flaws, \
secrets, unsafe deserialization, path traversal, SSRF, etc.), each rated by severity \
with file:line, an exploit scenario, and a concrete fix. Prefer precision over volume — \
don't invent issues. Default to read-only: propose fixes, don't apply them unless asked.";

const RESEARCHER_PROMPT: &str = "\
You investigate and REPORT — you do not modify code or make changes. Given a question \
or area, research it (read docs/code, search the web, compare options), then report \
concise, sourced findings plus a clear recommendation to whoever assigned you. Prefer \
primary sources, flag uncertainty, and keep it actionable. Your deliverable is \
information, not edits. DELIVER it through your Taime team tools — `share(\"<key>\", \
<your findings>)` to the blackboard and/or `send_message` to the agent that assigned \
you — NOT a native subagent/team channel. If you were given a key, use it.";

/// The built-in roles, always present in the launcher even with no `~/.taime/agents`
/// files: the two base roles plus a starter team-lead + specialist individuals for
/// the common jobs (build from scratch, add a feature, fix a bug, security review).
fn builtins() -> Vec<Profile> {
    let individual = |name: &str, description: &str, prompt: &str| Profile {
        name: name.to_string(),
        def: ProfileDef {
            description: description.to_string(),
            system_prompt: Some(prompt.to_string()),
            ..Default::default()
        },
        source: "builtin",
    };
    vec![
        Profile {
            name: "default".to_string(),
            def: ProfileDef {
                description: "Plain agent — no orchestration tools.".to_string(),
                ..Default::default()
            },
            source: "builtin",
        },
        Profile {
            name: "orchestrator".to_string(),
            def: ProfileDef {
                description: "Team lead — plans and delegates to specialist workers.".to_string(),
                system_prompt: Some(ORCHESTRATOR_PROMPT.to_string()),
                orchestrator: true,
                // HARD lock-down: read + list only. This blocks Bash / Edit /
                // Write / WebSearch / WebFetch AND Claude's own `Task` subagent
                // (see tool_mapping), so the orchestrator can't build, research,
                // or spawn invisible in-process helpers — the ONLY way it can
                // delegate is Taime's tracked `assign`. Its MCP orchestration
                // tools and the question picker aren't in the map, so they stay.
                allowed_tools: vec!["fs_read".to_string(), "fs_list".to_string()],
                ..Default::default()
            },
            source: "builtin",
        },
        individual(
            "product-builder",
            "Builds a new product/project from scratch.",
            PRODUCT_BUILDER_PROMPT,
        ),
        individual(
            "feature-builder",
            "Adds a feature to an existing codebase.",
            FEATURE_BUILDER_PROMPT,
        ),
        individual(
            "bug-fixer",
            "Reproduces and fixes a bug with a regression test.",
            BUG_FIXER_PROMPT,
        ),
        individual(
            "security-reviewer",
            "Reviews code for security vulnerabilities.",
            SECURITY_REVIEWER_PROMPT,
        ),
        individual(
            "researcher",
            "Investigates a question and reports findings (read-only).",
            RESEARCHER_PROMPT,
        ),
    ]
}

/// Overlay a resolved profile onto a spawn's [`AgentProfile`]: fill any field the
/// caller left unset (app-provided values win), and flip on orchestration when
/// the profile is a supervisor. Returns whether orchestration should be injected.
pub fn apply_to(def: &ProfileDef, p: &mut AgentProfile, inject_orchestration: &mut bool) {
    if p.system_prompt.is_none() {
        p.system_prompt = def.system_prompt.clone();
    }
    if p.model.is_none() {
        p.model = def.model.clone();
    }
    if p.permission_mode.is_none() {
        p.permission_mode = def.permission_mode.clone();
    }
    if p.allowed_tools.is_empty() {
        p.allowed_tools = def.allowed_tools.clone();
    }
    if p.native_agent.is_none() {
        p.native_agent = def.native_agent.clone();
    }
    if p.codex_profile.is_none() {
        p.codex_profile = def.codex_profile.clone();
    }
    if def.orchestrator {
        *inject_orchestration = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_always_present_and_orchestrator_flagged() {
        let store = ProfileStore::load();
        assert!(store.resolve("default").is_some());
        let orch = store.resolve("orchestrator").expect("orchestrator built-in");
        assert!(orch.orchestrator, "orchestrator built-in injects tools");
        // It must also be TOLD it can delegate — otherwise it only finds the tools
        // by chance and the user sees "nothing happened".
        let sp = orch.system_prompt.as_deref().unwrap_or("");
        assert!(sp.contains("assign") && sp.contains("orchestrator"), "orchestrator seed prompt");
        // Hard lock-down: tool-restricted (non-empty allow-list, no wildcard) so it
        // can only read + coordinate — never build/research/spawn-Task itself.
        assert!(
            !orch.allowed_tools.is_empty() && !orch.allowed_tools.iter().any(|t| t == "*"),
            "orchestrator is tool-restricted"
        );
        // The infos surface is a JSON array containing the base roles + starter
        // specialists, each (except plain default) carrying a real system prompt.
        let v: serde_json::Value = serde_json::from_str(&store.infos_json()).unwrap();
        let names: Vec<&str> = v.as_array().unwrap().iter().filter_map(|p| p["name"].as_str()).collect();
        for n in [
            "default",
            "orchestrator",
            "product-builder",
            "feature-builder",
            "bug-fixer",
            "security-reviewer",
            "researcher",
        ] {
            assert!(names.contains(&n), "missing built-in role {n}");
        }
        assert!(store.resolve("security-reviewer").unwrap().system_prompt.is_some());
    }

    #[test]
    fn apply_fills_unset_fields_and_keeps_app_overrides() {
        let def = ProfileDef {
            system_prompt: Some("you are a reviewer".into()),
            model: Some("from-profile".into()),
            allowed_tools: vec!["fs_read".into()],
            orchestrator: true,
            ..Default::default()
        };
        let mut p = AgentProfile {
            name: "reviewer".into(),
            model: Some("app-override".into()), // app value must win
            ..Default::default()
        };
        let mut inject = false;
        apply_to(&def, &mut p, &mut inject);
        assert_eq!(p.system_prompt.as_deref(), Some("you are a reviewer"));
        assert_eq!(p.model.as_deref(), Some("app-override")); // not clobbered
        assert_eq!(p.allowed_tools, vec!["fs_read".to_string()]);
        assert!(inject, "orchestrator profile flips injection on");
    }

    #[test]
    fn unknown_profile_resolves_to_none() {
        let store = ProfileStore::load();
        assert!(store.resolve("does-not-exist-xyz").is_none());
    }
}
