//! **Workflows** — the loopable agent step-graph (the finalized Taime lexicon for
//! a defined, executable orchestration with routes, branches, and cycles/loops).
//! Distinct from a Schedule (a cron trigger): a Workflow is a graph an Orchestrator
//! can author + run, or a user can drop as JSON in `~/.taime/workflows/*.json`.
//!
//! ```json
//! {
//!   "name": "build-feature",
//!   "entry": "implement",
//!   "max_iterations": 20,
//!   "nodes": [
//!     { "id": "implement", "role": "feature-builder", "prompt": "Implement X." },
//!     { "id": "test", "role": "default", "prompt": "Run the tests; reply PASS or FAIL." },
//!     { "id": "review", "role": "security-reviewer", "prompt": "Review the change." }
//!   ],
//!   "edges": [
//!     { "from": "implement", "to": "test", "when": "always" },
//!     { "from": "test", "to": "review", "when": "keyword:PASS" },
//!     { "from": "test", "to": "implement", "when": "keyword:FAIL" }
//!   ]
//! }
//! ```

use serde::{Deserialize, Serialize};

fn default_max_iter() -> u32 {
    20
}
fn default_role() -> String {
    "default".to_string()
}
fn default_when() -> String {
    "always".to_string()
}

/// One node: an agent step. `output_key` is the blackboard key the node writes its
/// result to (defaults to the node id); edges route on that output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowNode {
    pub id: String,
    #[serde(default = "default_role")]
    pub role: String,
    pub prompt: String,
    #[serde(default)]
    pub output_key: Option<String>,
    /// Optional provider override (else inherits the run's provider).
    #[serde(default)]
    pub provider: Option<String>,
}

impl WorkflowNode {
    pub fn output_key(&self) -> String {
        self.output_key.clone().unwrap_or_else(|| self.id.clone())
    }
}

/// A directed edge. `when` is "always" | "keyword:WORD" | "/regex/". Edges are
/// evaluated in definition order; the first match wins; a node with no matching
/// outgoing edge is terminal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowEdge {
    pub from: String,
    pub to: String,
    #[serde(default = "default_when")]
    pub when: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowDefinition {
    pub name: String,
    pub entry: String,
    #[serde(default = "default_max_iter")]
    pub max_iterations: u32,
    pub nodes: Vec<WorkflowNode>,
    pub edges: Vec<WorkflowEdge>,
}

impl WorkflowDefinition {
    pub fn node(&self, id: &str) -> Option<&WorkflowNode> {
        self.nodes.iter().find(|n| n.id == id)
    }
    /// Outgoing edges from `id`, in definition order.
    pub fn edges_from<'a>(&'a self, id: &str) -> impl Iterator<Item = &'a WorkflowEdge> + 'a {
        let id = id.to_string();
        self.edges.iter().filter(move |e| e.from == id)
    }
}

/// Parse + validate a workflow JSON definition.
pub fn parse_workflow(json: &str) -> Result<WorkflowDefinition, String> {
    let def: WorkflowDefinition =
        serde_json::from_str(json).map_err(|e| format!("invalid workflow JSON: {e}"))?;
    validate(&def)?;
    Ok(def)
}

fn validate(def: &WorkflowDefinition) -> Result<(), String> {
    if def.name.trim().is_empty() {
        return Err("workflow needs a name".to_string());
    }
    if def.nodes.is_empty() {
        return Err("workflow needs at least one node".to_string());
    }
    let mut seen = std::collections::HashSet::new();
    for n in &def.nodes {
        if n.id.trim().is_empty() {
            return Err("a node has an empty id".to_string());
        }
        if !seen.insert(&n.id) {
            return Err(format!("duplicate node id '{}'", n.id));
        }
        if n.prompt.trim().is_empty() {
            return Err(format!("node '{}' has no prompt", n.id));
        }
    }
    if def.node(&def.entry).is_none() {
        return Err(format!("entry node '{}' not found", def.entry));
    }
    for e in &def.edges {
        if def.node(&e.from).is_none() {
            return Err(format!("edge from unknown node '{}'", e.from));
        }
        if def.node(&e.to).is_none() {
            return Err(format!("edge to unknown node '{}'", e.to));
        }
        validate_when(&e.when)?;
    }
    Ok(())
}

fn validate_when(when: &str) -> Result<(), String> {
    let w = when.trim();
    if w == "always" || w.is_empty() {
        return Ok(());
    }
    if let Some(word) = w.strip_prefix("keyword:") {
        if word.trim().is_empty() {
            return Err("keyword: condition is empty".to_string());
        }
        return Ok(());
    }
    if let Some(re) = w.strip_prefix('/').and_then(|x| x.strip_suffix('/')) {
        regex::Regex::new(re).map_err(|e| format!("bad regex in edge condition: {e}"))?;
        return Ok(());
    }
    Err(format!("unknown edge condition {when:?} (use always | keyword:WORD | /regex/)"))
}

/// Whether an edge's `when` matches a node's `output`.
pub fn edge_matches(when: &str, output: &str) -> bool {
    let w = when.trim();
    if w == "always" || w.is_empty() {
        return true;
    }
    if let Some(word) = w.strip_prefix("keyword:") {
        return output.to_lowercase().contains(word.trim().to_lowercase().as_str());
    }
    if let Some(re) = w.strip_prefix('/').and_then(|x| x.strip_suffix('/')) {
        return regex::Regex::new(re).map(|r| r.is_match(output)).unwrap_or(false);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "name": "build-feature", "entry": "implement", "max_iterations": 10,
        "nodes": [
            {"id":"implement","role":"feature-builder","prompt":"do it"},
            {"id":"test","prompt":"reply PASS or FAIL"}
        ],
        "edges": [
            {"from":"implement","to":"test","when":"always"},
            {"from":"test","to":"implement","when":"keyword:FAIL"}
        ]
    }"#;

    #[test]
    fn parses_and_validates() {
        let def = parse_workflow(SAMPLE).unwrap();
        assert_eq!(def.entry, "implement");
        assert_eq!(def.nodes.len(), 2);
        assert_eq!(def.node("test").unwrap().role, "default"); // defaulted
        assert_eq!(def.node("test").unwrap().output_key(), "test");
    }

    #[test]
    fn rejects_dangling_and_bad_conditions() {
        let dangling = r#"{"name":"x","entry":"a","nodes":[{"id":"a","prompt":"p"}],"edges":[{"from":"a","to":"missing"}]}"#;
        assert!(parse_workflow(dangling).is_err());
        let bad_entry = r#"{"name":"x","entry":"z","nodes":[{"id":"a","prompt":"p"}],"edges":[]}"#;
        assert!(parse_workflow(bad_entry).is_err());
        let bad_when = r#"{"name":"x","entry":"a","nodes":[{"id":"a","prompt":"p"}],"edges":[{"from":"a","to":"a","when":"nope"}]}"#;
        assert!(parse_workflow(bad_when).is_err());
    }

    #[test]
    fn edge_conditions_match() {
        assert!(edge_matches("always", "anything"));
        assert!(edge_matches("keyword:PASS", "the tests PASS now"));
        assert!(edge_matches("keyword:pass", "ALL PASS")); // case-insensitive
        assert!(!edge_matches("keyword:FAIL", "all green"));
        assert!(edge_matches("/error|fail/", "build error"));
        assert!(!edge_matches("/error/", "ok"));
    }
}
