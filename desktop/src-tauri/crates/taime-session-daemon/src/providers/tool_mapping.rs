//! CAO-vocabulary → provider-native tool mapping, ported from CAO's
//! `utils/tool_mapping.py`.
//!
//! CAO profiles name tools in a universal vocabulary (`execute_bash`, `fs_read`,
//! `fs_write`, `fs_list`, `fs_*`, plus `@server` MCP refs); a provider enforces
//! the allow-list by BLOCKING every native tool it doesn't cover (Claude: one
//! `--disallowedTools <tool>` arg pair per blocked tool). Note `fs_*` is a
//! **literal mapping key**, not a glob — lookups are exact, so an unknown name
//! (e.g. `fs_reed`) silently contributes nothing, exactly as in CAO.

use std::collections::BTreeSet;

/// CAO's `TOOL_MAPPING["claude_code"]` verbatim: CAO tool name → Claude-native
/// tool names.
const CLAUDE_CODE_MAPPING: &[(&str, &[&str])] = &[
    ("execute_bash", &["Bash"]),
    ("fs_read", &["Read"]),
    ("fs_write", &["Edit", "Write"]),
    ("fs_list", &["Glob", "Grep"]),
    ("fs_*", &["Read", "Edit", "Write", "Glob", "Grep"]),
];

/// The mapping table for a provider id. Only `claude_code` is wired so far; CAO
/// also carried `copilot_cli`/`gemini_cli` tables — port them when those
/// providers enforce restrictions natively.
fn mapping_for(provider: &str) -> Option<&'static [(&'static str, &'static [&'static str])]> {
    match provider {
        "claude_code" => Some(CLAUDE_CODE_MAPPING),
        _ => None,
    }
}

/// Provider-native tool names to BLOCK given the profile's allowed CAO tools —
/// CAO's `get_disallowed_tools` (`tool_mapping.py:118-147`): `"*"` anywhere ⇒
/// unrestricted (empty); `@server` MCP refs are skipped; every other entry is an
/// exact-key lookup into the mapping; unknown providers get no restrictions
/// (no table ⇒ nothing to block). Returns `sorted(all_native − allowed_native)`
/// so the derived args are deterministic.
///
/// One deliberate deviation: an EMPTY allow-list also returns no restrictions.
/// CAO's raw function blocks everything for `[]`, but every CAO call site guards
/// with `if self._allowed_tools and "*" not in …` — that guard is folded in here
/// so the [`AgentProfile`](taime_protocol::AgentProfile) contract (empty ⇒
/// unrestricted) can't be violated by future provider wiring that skips the
/// `tools_restricted` check.
pub fn get_disallowed_tools(provider: &str, allowed: &[String]) -> Vec<String> {
    if allowed.is_empty() || allowed.iter().any(|t| t == "*") {
        return Vec::new();
    }
    let Some(mapping) = mapping_for(provider) else {
        return Vec::new();
    };

    let mut allowed_native: BTreeSet<&str> = BTreeSet::new();
    for tool in allowed {
        if tool.starts_with('@') {
            continue; // MCP server references don't map to native tools
        }
        if let Some((_, natives)) = mapping.iter().find(|(name, _)| *name == tool.as_str()) {
            allowed_native.extend(natives.iter().copied());
        }
    }

    // CAO's ALL_NATIVE_TOOLS: the union of every mapping value. BTreeSet keeps
    // the difference sorted, matching Python's `sorted(all - allowed)`.
    let all_native: BTreeSet<&str> =
        mapping.iter().flat_map(|(_, natives)| natives.iter().copied()).collect();
    all_native.difference(&allowed_native).map(|t| t.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn wildcard_or_unknown_provider_means_no_restrictions() {
        assert!(get_disallowed_tools("claude_code", &allowed(&["*"])).is_empty());
        assert!(get_disallowed_tools("claude_code", &allowed(&["fs_read", "*"])).is_empty());
        assert!(get_disallowed_tools("kimi_cli", &allowed(&["fs_read"])).is_empty());
    }

    #[test]
    fn empty_allow_list_means_unrestricted() {
        // The AgentProfile contract (folded in from CAO's call-site guard):
        // `[]` is "unrestricted", not "block everything".
        assert!(get_disallowed_tools("claude_code", &[]).is_empty());
    }

    #[test]
    fn disallowed_is_the_sorted_complement_of_allowed() {
        assert_eq!(
            get_disallowed_tools("claude_code", &allowed(&["fs_read"])),
            vec!["Bash", "Edit", "Glob", "Grep", "Write"]
        );
        assert_eq!(
            get_disallowed_tools("claude_code", &allowed(&["execute_bash", "fs_write"])),
            vec!["Glob", "Grep", "Read"]
        );
    }

    #[test]
    fn fs_star_is_a_literal_key_not_a_glob() {
        // `fs_*` hits its own mapping entry (all fs tools) → only Bash blocked.
        assert_eq!(get_disallowed_tools("claude_code", &allowed(&["fs_*"])), vec!["Bash"]);
        // An unknown name and an MCP ref contribute nothing → everything blocked.
        assert_eq!(
            get_disallowed_tools("claude_code", &allowed(&["fs_reed", "@cao-mcp-server"])),
            vec!["Bash", "Edit", "Glob", "Grep", "Read", "Write"]
        );
    }
}
