//! **Schedules** — cron-triggered, unattended Agent runs (the CAO "Flow" feature
//! under the finalized Taime lexicon). A schedule is a markdown file with a YAML
//! front-matter header in `~/.taime/schedules/*.md`:
//!
//! ```text
//! ---
//! name: nightly-review
//! schedule: "0 2 * * *"        # 5-field POSIX cron (Sun=0)
//! profile: security-reviewer
//! provider: claude_code
//! script: ./health-check.sh    # optional gate; non-zero exit = skip this run
//! ---
//! Review yesterday's changes and open issues for anything risky.
//! ```
//!
//! Canonical keys are `profile` and `workspace_root`; the legacy spellings
//! `agent_profile` and `workspace` are accepted forever as aliases (user-authored
//! files never break). `task_mode: uncategorized` is accepted as the explicit
//! spelling of the absent default. The serializer ([`to_markdown`]) emits only
//! the canonical spellings.
//!
//! The body (after the closing `---`) is the prompt fed to the agent when the cron
//! fires. `[[var]]` placeholders in the prompt are substituted from a fixed,
//! injection-safe allowlist. The daemon's tick checks due schedules and fires them
//! headless. (Storage lives in the `flows` table — CAO heritage; user-facing term
//! is "Schedule".)

use std::collections::HashMap;

/// A parsed schedule definition (front-matter + prompt body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleDef {
    pub name: String,
    pub schedule: String,
    /// The launch Profile (front-matter `profile:`; legacy alias `agent_profile:`).
    pub agent_profile: String,
    pub provider: String,
    pub script: Option<String>,
    pub prompt: String,
    /// Workspace the fire runs in (front-matter `workspace_root:`; legacy alias
    /// `workspace:`). Without one the agent runs in the daemon's cwd and task
    /// targeting is unavailable.
    pub workspace_root: Option<String>,
    /// Explicit task behavior (front-matter `task_mode:`): absent or explicit
    /// `uncategorized` = uncategorized (stored as `None`), `fixed` = attach runs
    /// to `task_id`, `per_run` = create a task per fire.
    pub task_mode: Option<String>,
    /// The target task for `task_mode: fixed` (front-matter `task_id:`).
    pub task_id: Option<String>,
}

/// Parse a schedule markdown file (YAML-ish front-matter + body). Hand-rolled (no
/// serde_yaml dep): `---`-delimited `key: value` lines, with a `script: |` literal
/// block. Returns a clear error string on malformed input.
pub fn parse_schedule(text: &str) -> Result<ScheduleDef, String> {
    let text = text.trim_start_matches('\u{feff}');
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return Err("missing front-matter: file must start with `---`".to_string());
    }
    let mut fields: HashMap<String, String> = HashMap::new();
    let mut script_block: Option<String> = None;
    let mut closed = false;
    let mut body_start = 0usize;
    // Re-iterate with byte tracking so we can slice the body after the closing ---.
    let header_and_rest = &text[4..]; // skip the leading "---\n" (or "---\r\n")
    let after_first = header_and_rest
        .strip_prefix('\n')
        .or_else(|| header_and_rest.strip_prefix("\r\n"))
        .unwrap_or(header_and_rest);
    let _ = (&mut lines, &mut body_start); // (kept for clarity; body sliced below)

    let mut collecting_script = false;
    let mut script_lines: Vec<String> = Vec::new();
    let mut consumed = 0usize;
    for raw in after_first.split_inclusive('\n') {
        consumed += raw.len();
        let line = raw.trim_end_matches(['\n', '\r']);
        if collecting_script {
            // A literal block continues while lines are indented (or blank).
            if line.is_empty() || line.starts_with(' ') || line.starts_with('\t') {
                script_lines.push(line.trim_start().to_string());
                continue;
            }
            collecting_script = false;
            script_block = Some(script_lines.join("\n").trim().to_string());
        }
        if line.trim() == "---" {
            closed = true;
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim().to_lowercase();
            let val = v.trim();
            if key == "script" && (val == "|" || val.is_empty()) {
                collecting_script = true;
                script_lines.clear();
            } else {
                fields.insert(key, unquote(val).to_string());
            }
        }
    }
    if collecting_script && script_block.is_none() {
        script_block = Some(script_lines.join("\n").trim().to_string());
    }
    if !closed {
        return Err("front-matter not closed with `---`".to_string());
    }
    let prompt = after_first[consumed..].trim().to_string();

    let get = |k: &str| fields.get(k).cloned().filter(|s| !s.is_empty());
    let name = get("name").ok_or("front-matter missing `name`")?;
    let schedule = get("schedule").ok_or("front-matter missing `schedule`")?;
    // Canonical `profile:`; `agent_profile:` accepted forever as a legacy alias.
    let agent_profile = get("profile")
        .or_else(|| get("agent_profile"))
        .unwrap_or_else(|| "default".to_string());
    let provider = get("provider").unwrap_or_else(|| "claude_code".to_string());
    let script = get("script").or(script_block).filter(|s| !s.is_empty());
    // Canonical `workspace_root:`; `workspace:` accepted forever as a legacy alias.
    let workspace_root = get("workspace_root").or_else(|| get("workspace"));
    // Explicit `uncategorized` is the spelled-out absent default.
    let task_mode = get("task_mode").filter(|m| m != "uncategorized");
    let task_id = get("task_id");
    if prompt.is_empty() {
        return Err("schedule has no prompt body (after the closing `---`)".to_string());
    }
    if next_run_unix(&schedule).is_none() {
        return Err(format!("invalid cron schedule: {schedule:?}"));
    }
    if let Some(mode) = task_mode.as_deref() {
        if mode != "fixed" && mode != "per_run" {
            return Err(format!(
                "invalid task_mode {mode:?} (expected uncategorized | fixed | per_run)"
            ));
        }
        if workspace_root.is_none() {
            return Err("task_mode requires a `workspace_root:` target".to_string());
        }
        if mode == "fixed" && task_id.is_none() {
            return Err("task_mode `fixed` requires `task_id:`".to_string());
        }
    }
    Ok(ScheduleDef {
        name,
        schedule,
        agent_profile,
        provider,
        script,
        prompt,
        workspace_root,
        task_mode,
        task_id,
    })
}

fn unquote(s: &str) -> &str {
    let s = s.trim();
    s.strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .or_else(|| s.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')))
        .unwrap_or(s)
}

/// The next fire time (unix seconds) for a 5-field POSIX cron string, or None if
/// the spec is invalid or can never fire. saffron treats Sun=0, so `1-5` = Mon–Fri.
pub fn next_run_unix(cron: &str) -> Option<u64> {
    let parsed: saffron::Cron = cron.trim().parse().ok()?;
    if !parsed.any() {
        return None; // a spec that can never match (e.g. Feb 31)
    }
    let next = parsed.next_after(chrono::Utc::now())?;
    Some(next.timestamp().max(0) as u64)
}

/// Substitute `[[var]]` placeholders from a FIXED allowlist only (never arbitrary
/// env — prevents prompt/command injection). Unknown placeholders are left as-is.
pub fn substitute_vars(prompt: &str, vars: &HashMap<&str, String>) -> String {
    let mut out = prompt.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("[[{k}]]"), v);
    }
    out
}

/// Render a schedule back to its markdown form (for writing UI-created schedules
/// to `~/.taime/schedules/<name>.md`). Emits only the canonical key spellings
/// (`profile:`, `workspace_root:`), never the legacy aliases.
pub fn to_markdown(def: &ScheduleDef) -> String {
    let mut s = String::from("---\n");
    s.push_str(&format!("name: {}\n", def.name));
    s.push_str(&format!("schedule: \"{}\"\n", def.schedule));
    s.push_str(&format!("profile: {}\n", def.agent_profile));
    s.push_str(&format!("provider: {}\n", def.provider));
    if let Some(script) = &def.script {
        s.push_str(&format!("script: {script}\n"));
    }
    if let Some(root) = &def.workspace_root {
        s.push_str(&format!("workspace_root: {root}\n"));
    }
    if let Some(mode) = &def.task_mode {
        s.push_str(&format!("task_mode: {mode}\n"));
    }
    if let Some(tid) = &def.task_id {
        s.push_str(&format!("task_id: {tid}\n"));
    }
    s.push_str("---\n");
    s.push_str(def.prompt.trim());
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_schedule_with_canonical_keys() {
        let md = "---\nname: nightly-review\nschedule: \"0 2 * * *\"\nprofile: security-reviewer\nprovider: claude_code\nworkspace_root: /projects/app\n---\nReview yesterday's changes for anything risky.";
        let def = parse_schedule(md).unwrap();
        assert_eq!(def.name, "nightly-review");
        assert_eq!(def.schedule, "0 2 * * *");
        assert_eq!(def.agent_profile, "security-reviewer");
        assert_eq!(def.provider, "claude_code");
        assert_eq!(def.workspace_root.as_deref(), Some("/projects/app"));
        assert!(def.script.is_none());
        assert!(def.prompt.contains("Review yesterday's"));
    }

    #[test]
    fn legacy_key_aliases_still_parse() {
        // `agent_profile:` and `workspace:` are accepted forever as aliases.
        let md = "---\nname: nightly-review\nschedule: \"0 2 * * *\"\nagent_profile: security-reviewer\nworkspace: /projects/app\n---\nbody";
        let def = parse_schedule(md).unwrap();
        assert_eq!(def.agent_profile, "security-reviewer");
        assert_eq!(def.workspace_root.as_deref(), Some("/projects/app"));

        // Canonical spelling wins when both are present.
        let both = "---\nname: x\nschedule: \"0 2 * * *\"\nprofile: new\nagent_profile: old\nworkspace_root: /new\nworkspace: /old\n---\nbody";
        let def = parse_schedule(both).unwrap();
        assert_eq!(def.agent_profile, "new");
        assert_eq!(def.workspace_root.as_deref(), Some("/new"));
    }

    #[test]
    fn explicit_uncategorized_task_mode_equals_absent() {
        // `task_mode: uncategorized` is the spelled-out default: parses to None
        // and requires no workspace.
        let md = "---\nname: x\nschedule: \"0 2 * * *\"\ntask_mode: uncategorized\n---\nbody";
        let def = parse_schedule(md).unwrap();
        assert_eq!(def.task_mode, None);
    }

    #[test]
    fn defaults_profile_and_provider_and_requires_body() {
        let md = "---\nname: x\nschedule: \"*/5 * * * *\"\n---\ndo a thing";
        let def = parse_schedule(md).unwrap();
        assert_eq!(def.agent_profile, "default");
        assert_eq!(def.provider, "claude_code");

        let no_body = "---\nname: x\nschedule: \"*/5 * * * *\"\n---\n";
        assert!(parse_schedule(no_body).is_err());
    }

    #[test]
    fn rejects_bad_cron_and_missing_frontmatter() {
        let bad_cron = "---\nname: x\nschedule: \"not a cron\"\n---\nbody";
        assert!(parse_schedule(bad_cron).is_err());
        assert!(parse_schedule("no frontmatter here").is_err());
    }

    #[test]
    fn weekday_cron_yields_a_future_time() {
        // 9am Mon–Fri — POSIX 1-5 (saffron Sun=0).
        assert!(next_run_unix("0 9 * * 1-5").is_some());
        assert!(next_run_unix("garbage").is_none());
    }

    #[test]
    fn substitutes_only_known_vars() {
        let mut vars = HashMap::new();
        vars.insert("flow_name", "nightly".to_string());
        let out = substitute_vars("run [[flow_name]] but keep [[unknown]]", &vars);
        assert_eq!(out, "run nightly but keep [[unknown]]");
    }

    #[test]
    fn schedule_name_and_legacy_flow_name_both_substitute() {
        // The daemon's allowlist carries both spellings: [[schedule_name]] is
        // canonical, [[flow_name]] substitutes forever as the legacy alias.
        let mut vars = HashMap::new();
        vars.insert("schedule_name", "nightly".to_string());
        vars.insert("flow_name", "nightly".to_string());
        let out = substitute_vars("[[schedule_name]] == [[flow_name]]", &vars);
        assert_eq!(out, "nightly == nightly");
    }

    #[test]
    fn markdown_roundtrips() {
        let def = ScheduleDef {
            name: "daily".into(),
            schedule: "0 9 * * 1-5".into(),
            agent_profile: "developer".into(),
            provider: "claude_code".into(),
            script: None,
            prompt: "summarize commits".into(),
            workspace_root: None,
            task_mode: None,
            task_id: None,
        };
        let reparsed = parse_schedule(&to_markdown(&def)).unwrap();
        assert_eq!(reparsed, def);
    }

    #[test]
    fn task_targeting_roundtrips_and_validates() {
        // Full task-targeted schedule survives a markdown roundtrip.
        let def = ScheduleDef {
            name: "triage".into(),
            schedule: "0 8 * * *".into(),
            agent_profile: "bug-fixer".into(),
            provider: "claude_code".into(),
            script: None,
            prompt: "triage new issues".into(),
            workspace_root: Some("/projects/app".into()),
            task_mode: Some("fixed".into()),
            task_id: Some("task-deadbeef".into()),
        };
        assert_eq!(parse_schedule(&to_markdown(&def)).unwrap(), def);

        // task_mode without a workspace is rejected.
        let bad = "---\nname: x\nschedule: \"0 8 * * *\"\ntask_mode: per_run\n---\nbody";
        assert!(parse_schedule(bad).unwrap_err().contains("workspace"));
        // fixed without task_id is rejected.
        let bad =
            "---\nname: x\nschedule: \"0 8 * * *\"\nworkspace: /p\ntask_mode: fixed\n---\nbody";
        assert!(parse_schedule(bad).unwrap_err().contains("task_id"));
        // unknown mode is rejected.
        let bad =
            "---\nname: x\nschedule: \"0 8 * * *\"\nworkspace: /p\ntask_mode: weekly\n---\nbody";
        assert!(parse_schedule(bad).unwrap_err().contains("task_mode"));
    }
}
