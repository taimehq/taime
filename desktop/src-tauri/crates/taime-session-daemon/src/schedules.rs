//! **Schedules** — cron-triggered, unattended Agent runs (the CAO "Flow" feature
//! under the finalized Taime lexicon). A schedule is a markdown file with a YAML
//! front-matter header in `~/.taime/schedules/*.md`:
//!
//! ```text
//! ---
//! name: nightly-review
//! schedule: "0 2 * * *"        # 5-field POSIX cron (Sun=0)
//! agent_profile: security-reviewer
//! provider: claude_code
//! script: ./health-check.sh    # optional gate; non-zero exit = skip this run
//! ---
//! Review yesterday's changes and open issues for anything risky.
//! ```
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
    pub agent_profile: String,
    pub provider: String,
    pub script: Option<String>,
    pub prompt: String,
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
    let agent_profile = get("agent_profile").unwrap_or_else(|| "default".to_string());
    let provider = get("provider").unwrap_or_else(|| "claude_code".to_string());
    let script = get("script").or(script_block).filter(|s| !s.is_empty());
    if prompt.is_empty() {
        return Err("schedule has no prompt body (after the closing `---`)".to_string());
    }
    if next_run_unix(&schedule).is_none() {
        return Err(format!("invalid cron schedule: {schedule:?}"));
    }
    Ok(ScheduleDef { name, schedule, agent_profile, provider, script, prompt })
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
/// to `~/.taime/schedules/<name>.md`).
pub fn to_markdown(def: &ScheduleDef) -> String {
    let mut s = String::from("---\n");
    s.push_str(&format!("name: {}\n", def.name));
    s.push_str(&format!("schedule: \"{}\"\n", def.schedule));
    s.push_str(&format!("agent_profile: {}\n", def.agent_profile));
    s.push_str(&format!("provider: {}\n", def.provider));
    if let Some(script) = &def.script {
        s.push_str(&format!("script: {script}\n"));
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
    fn parses_a_full_schedule() {
        let md = "---\nname: nightly-review\nschedule: \"0 2 * * *\"\nagent_profile: security-reviewer\nprovider: claude_code\n---\nReview yesterday's changes for anything risky.";
        let def = parse_schedule(md).unwrap();
        assert_eq!(def.name, "nightly-review");
        assert_eq!(def.schedule, "0 2 * * *");
        assert_eq!(def.agent_profile, "security-reviewer");
        assert_eq!(def.provider, "claude_code");
        assert!(def.script.is_none());
        assert!(def.prompt.contains("Review yesterday's"));
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
    fn markdown_roundtrips() {
        let def = ScheduleDef {
            name: "daily".into(),
            schedule: "0 9 * * 1-5".into(),
            agent_profile: "developer".into(),
            provider: "claude_code".into(),
            script: None,
            prompt: "summarize commits".into(),
        };
        let reparsed = parse_schedule(&to_markdown(&def)).unwrap();
        assert_eq!(reparsed, def);
    }
}
