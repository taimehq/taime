//! The Workflow execution engine. Drives a workflow run to completion on a
//! dedicated background thread, communicating ONLY via the store + the shared
//! blackboard (it cannot stream a worker's PTY): each node spawns a worker, the
//! worker posts its result to the blackboard via the `share` tool, and the engine
//! reads that result and routes to the next node. Supports conditional branches
//! and bounded loops (a per-node cap + a global `max_iterations` guard).

use std::sync::Arc;
use std::time::Duration;

use crate::manager::Manager;
use crate::store::Store;
use crate::workflow::{self, WorkflowDefinition};

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn gen_id() -> String {
    format!("{:016x}{:016x}", rand::random::<u64>(), rand::random::<u64>())
}

/// A node may run at most this many times within a run (loop backstop), on top of
/// the workflow's own `max_iterations`.
const PER_NODE_CAP: u32 = 10;
/// Poll budget per node: 600 × 2s = 20 min before a node is declared timed-out.
const WAIT_TICKS: u32 = 600;

/// Run a workflow to completion (call on a background thread). The run row must
/// already exist (`create_run`); this fills in node runs + finishes the run.
pub fn run(
    manager: Arc<Manager>,
    def: WorkflowDefinition,
    run_id: String,
    project_root: Option<String>,
    provider: String,
    notify: Option<String>,
) {
    let Some(store) = manager.store_arc() else { return };
    let outcome = drive(&manager, &store, &def, &run_id, project_root.as_deref(), &provider);
    let (status, err) = match &outcome {
        Ok(()) => ("completed", None),
        Err(e) => ("failed", Some(e.as_str())),
    };
    let _ = store.finish_run(&run_id, status, now(), err);
    if let Some(to) = notify {
        let msg = match &outcome {
            Ok(()) => format!("Workflow '{}' completed.", def.name),
            Err(e) => format!("Workflow '{}' failed: {e}", def.name),
        };
        let _ = manager.enqueue_message("workflow".to_string(), to, msg);
    }
}

fn drive(
    manager: &Arc<Manager>,
    store: &Arc<Store>,
    def: &WorkflowDefinition,
    run_id: &str,
    project_root: Option<&str>,
    provider: &str,
) -> Result<(), String> {
    let mut current = def.entry.clone();
    let mut global = 0u32;
    loop {
        global += 1;
        if global > def.max_iterations {
            return Err(format!("max_iterations ({}) exceeded", def.max_iterations));
        }
        let node =
            def.node(&current).cloned().ok_or_else(|| format!("node '{current}' not found"))?;
        let iteration = store.node_iteration_count(run_id, &node.id) + 1;
        if iteration > PER_NODE_CAP {
            return Err(format!("node '{}' looped more than {PER_NODE_CAP} times", node.id));
        }

        let node_run_id = gen_id();
        let output_key = format!("{run_id}::{}", node.output_key());
        let node_provider = node.provider.clone().unwrap_or_else(|| provider.to_string());
        let started = now();
        let prompt = format!(
            "{}\n\n[Taime workflow step] When you have completed this step, call the `share` \
             MCP tool exactly once to post your result:\n  share(key=\"{output_key}\", \
             value=\"<a concise result; if this step is a check or decision, START the value \
             with PASS or FAIL>\")\nThat hand-off is required for the workflow to continue.",
            node.prompt
        );

        let (session_id, agent_key) =
            match manager.spawn_workflow_node(project_root, &node_provider, &node.role, &prompt) {
                Ok(v) => v,
                Err(e) => {
                    let _ = store.insert_node_run(&node_run_id, run_id, &node.id, None, iteration, started);
                    let _ = store.finish_node_run(&node_run_id, "failed", None, now());
                    return Err(format!("node '{}' could not spawn: {e}", node.id));
                }
            };
        let _ =
            store.insert_node_run(&node_run_id, run_id, &node.id, Some(&agent_key), iteration, started);

        let output = wait_for_output(manager, store, &output_key, &agent_key, run_id, started);
        manager.kill(&session_id); // free the worker once it's reported (or timed out)

        let output = match output {
            Some(o) => o,
            None => {
                let _ = store.finish_node_run(&node_run_id, "failed", None, now());
                return Err(format!("node '{}' did not report a result (timed out)", node.id));
            }
        };
        let _ = store.finish_node_run(&node_run_id, "completed", Some(&output), now());

        // Route: first matching outgoing edge wins; no match = terminal → done.
        let next = def
            .edges_from(&node.id)
            .find(|e| workflow::edge_matches(&e.when, &output))
            .map(|e| e.to.clone());
        match next {
            Some(n) => current = n,
            None => return Ok(()),
        }
    }
}

/// Poll the blackboard for the node's `share`d output (authored by THIS node's
/// worker, written after it started). Returns None on timeout or if the run was
/// cancelled out from under us.
fn wait_for_output(
    manager: &Arc<Manager>,
    store: &Arc<Store>,
    output_key: &str,
    author: &str,
    run_id: &str,
    since: u64,
) -> Option<String> {
    for _ in 0..WAIT_TICKS {
        if let Ok(Some(r)) = store.get_run(run_id) {
            if r.status != "running" {
                return None;
            }
        }
        if let Ok(Some((value, key_author, updated))) = manager.blackboard_get(output_key) {
            if updated >= since && key_author.as_deref() == Some(author) {
                return Some(value);
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    None
}
