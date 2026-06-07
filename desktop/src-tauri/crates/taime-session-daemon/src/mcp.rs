//! Daemon-hosted MCP tool dispatcher (CAO-replacement Phase 5).
//!
//! Coding CLIs already speak MCP, so orchestration is exposed as MCP **tools**.
//! This module is the **in-process JSON-RPC dispatcher** that turns the daemon's
//! orchestration primitives (the live registry, idle state, the inbox) into tool
//! calls, resolving against one source of truth. The agent's identity (`caller`)
//! is the calling session's attribution key — the daemon knows which connection
//! a request came in on — so `send_message`'s `from` is **stamped from the
//! authenticated session, never a client field** (closing CAO's `sender_id`
//! spoof).
//!
//! The transport is wired: the `--mcp-stdio` shim (see `main::run_mcp_shim`)
//! bridges each CLI's MCP client to the daemon's control socket, the per-agent
//! token is injected via the Phase-1 per-provider MCP config, and
//! `Manager::handle_mcp` resolves the caller from the token and dispatches here.
//! The remaining validation is a live MCP handshake against a real CLI.

use serde_json::{json, Value};

use crate::manager::Manager;

/// MCP protocol revision we advertise at `initialize`.
const MCP_PROTOCOL: &str = "2024-11-05";

/// The orchestration tool schemas (JSON Schema per the MCP spec) returned by
/// `tools/list`: the full Phase-5 surface — `list_agents`, `send_message`,
/// `broadcast` (role-filterable), `request`/`reply` (correlated), `handoff`,
/// `assign` (role + tools + fan/depth limits), and the `share`/`get` blackboard.
pub fn tool_definitions() -> Value {
    json!([
        {
            "name": "list_agents",
            "description": "List the live agent sessions: id, provider, role, status, cwd.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "send_message",
            "description": "Send a message to another agent's inbox; delivered when it next goes idle. Your sender id is stamped by the daemon.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to": { "type": "string", "description": "Receiver agent id (attribution key)." },
                    "body": { "type": "string", "description": "Message body." }
                },
                "required": ["to", "body"],
                "additionalProperties": false
            }
        },
        {
            "name": "broadcast",
            "description": "Send a message to every other live agent's inbox, optionally only those with a given role (profile name).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "body": { "type": "string", "description": "Message body." },
                    "role": { "type": "string", "description": "Optional role filter: only agents launched under this profile name receive it." }
                },
                "required": ["body"],
                "additionalProperties": false
            }
        },
        {
            "name": "request",
            "description": "Send a message that expects a reply. Returns an interaction_id; the receiver answers with `reply`. Use to ask another agent a question and correlate the answer.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to": { "type": "string", "description": "Receiver agent id." },
                    "body": { "type": "string", "description": "The question / request body." }
                },
                "required": ["to", "body"],
                "additionalProperties": false
            }
        },
        {
            "name": "reply",
            "description": "Answer a `request` you received, by its interaction_id. Delivered back to the original requester.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "interaction_id": { "type": "string", "description": "The interaction id from the request you received." },
                    "body": { "type": "string", "description": "Your answer." }
                },
                "required": ["interaction_id", "body"],
                "additionalProperties": false
            }
        },
        {
            "name": "handoff",
            "description": "Hand off to another agent with a context summary (transfers the active role; records a handoff edge).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to": { "type": "string", "description": "Receiver agent id." },
                    "summary": { "type": "string", "description": "Context summary for the receiver." }
                },
                "required": ["to", "summary"],
                "additionalProperties": false
            }
        },
        {
            "name": "assign",
            "description": "Spawn a worker sub-agent in its own worktree under an optional role (profile name) with an optional tool allow-list, seed it with a task, and get back its id. The worker reports its result to you via send_message; the daemon also notifies you when it exits. Subject to fan-out/depth limits.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "message": { "type": "string", "description": "The task for the worker." },
                    "role": { "type": "string", "description": "Optional profile name to launch the worker under (default: default)." },
                    "tools": { "type": "array", "items": { "type": "string" }, "description": "Optional allowed-tools override for the worker." },
                    "working_directory": { "type": "string", "description": "Optional project root to fork the worker's worktree from." }
                },
                "required": ["message"],
                "additionalProperties": false
            }
        },
        {
            "name": "share",
            "description": "Post a value to the shared blackboard under a key (last writer wins). Other agents read it with `get`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": { "type": "string", "description": "Blackboard key." },
                    "value": { "type": "string", "description": "Value to store." }
                },
                "required": ["key", "value"],
                "additionalProperties": false
            }
        },
        {
            "name": "get",
            "description": "Read a value from the shared blackboard by key. Returns the value, its author, and when it was last updated (or null if absent).",
            "inputSchema": {
                "type": "object",
                "properties": { "key": { "type": "string", "description": "Blackboard key." } },
                "required": ["key"],
                "additionalProperties": false
            }
        },
        {
            "name": "create_workflow",
            "description": "Define a reusable Workflow — a graph of agent steps with conditional branches and loops. Pass a JSON definition: {name, entry, max_iterations?, nodes:[{id, role, prompt, output_key?}], edges:[{from, to, when}]} where `role` is a profile name and `when` is \"always\" | \"keyword:WORD\" | \"/regex/\" (first matching edge wins; no match = terminal). Each node's worker posts its result with the `share` tool to the node's output_key; edges route on that. Returns the workflow name; run it with run_workflow.",
            "inputSchema": {
                "type": "object",
                "properties": { "definition": { "type": "string", "description": "The workflow JSON." } },
                "required": ["definition"],
                "additionalProperties": false
            }
        },
        {
            "name": "run_workflow",
            "description": "Start a run of a named Workflow in the background. Each step spawns a worker in its own worktree; the engine routes between steps on their results, with branches + bounded loops. Returns the run id immediately; you'll be messaged when it finishes.",
            "inputSchema": {
                "type": "object",
                "properties": { "name": { "type": "string", "description": "Workflow name." } },
                "required": ["name"],
                "additionalProperties": false
            }
        }
    ])
}

/// Handle one MCP JSON-RPC request from the agent identified by `caller`. Returns
/// the JSON-RPC response, or `Value::Null` for a notification (no reply).
pub fn handle(manager: &Manager, caller: &str, req: &Value) -> Value {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    match method {
        "initialize" => result(
            id,
            json!({
                "protocolVersion": MCP_PROTOCOL,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "taime-orchestrator", "version": env!("CARGO_PKG_VERSION") }
            }),
        ),
        "tools/list" => result(id, json!({ "tools": tool_definitions() })),
        "tools/call" => handle_tool_call(manager, caller, id, req.get("params")),
        // Notifications carry no id and expect no response.
        "notifications/initialized" | "initialized" => Value::Null,
        _ => error(id, -32601, "method not found"),
    }
}

fn handle_tool_call(manager: &Manager, caller: &str, id: Value, params: Option<&Value>) -> Value {
    let params = match params {
        Some(p) => p,
        None => return error(id, -32602, "missing params"),
    };
    let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    match name {
        "list_agents" => {
            let agents: Vec<Value> = manager
                .list()
                .into_iter()
                .map(|s| {
                    let key = s.attribution_key.clone().unwrap_or_else(|| s.id.clone());
                    json!({
                        "id": key,
                        "provider": s.provider,
                        "role": manager.role_of(&key),
                        "status": s.status.map(status_str),
                        "cwd": s.cwd,
                    })
                })
                .collect();
            tool_ok(id, json!({ "agents": agents }))
        }
        "send_message" => {
            let to = args.get("to").and_then(|v| v.as_str()).unwrap_or("");
            let body = args.get("body").and_then(|v| v.as_str()).unwrap_or("");
            if to.is_empty() {
                return tool_err(id, "send_message: 'to' is required");
            }
            if to == caller {
                return tool_err(id, "send_message: cannot message yourself");
            }
            // `from` is the authenticated caller — never a client field.
            match manager.enqueue_message(caller.to_string(), to.to_string(), body.to_string()) {
                Ok(mid) => {
                    // Record the message as a graph edge (the activity substrate).
                    manager.record_edge("message", caller, to);
                    tool_ok(id, json!({ "ok": true, "message_id": mid }))
                }
                Err(e) => tool_err(id, &format!("send_message failed: {e}")),
            }
        }
        "broadcast" => {
            let body = args.get("body").and_then(|v| v.as_str()).unwrap_or("");
            if body.is_empty() {
                return tool_err(id, "broadcast: 'body' is required");
            }
            let role = args.get("role").and_then(|v| v.as_str()).filter(|r| !r.is_empty());
            let n = manager.broadcast(caller, body, role);
            tool_ok(id, json!({ "ok": true, "recipients": n }))
        }
        "request" => {
            let to = args.get("to").and_then(|v| v.as_str()).unwrap_or("");
            let body = args.get("body").and_then(|v| v.as_str()).unwrap_or("");
            if to.is_empty() || to == caller {
                return tool_err(id, "request: a distinct 'to' is required");
            }
            if body.is_empty() {
                return tool_err(id, "request: 'body' is required");
            }
            match manager.request(caller, to, body) {
                Ok(interaction_id) => tool_ok(id, json!({ "ok": true, "interaction_id": interaction_id })),
                Err(e) => tool_err(id, &format!("request failed: {e}")),
            }
        }
        "reply" => {
            let interaction_id = args.get("interaction_id").and_then(|v| v.as_str()).unwrap_or("");
            let body = args.get("body").and_then(|v| v.as_str()).unwrap_or("");
            if interaction_id.is_empty() {
                return tool_err(id, "reply: 'interaction_id' is required");
            }
            match manager.reply(caller, interaction_id, body) {
                Ok(mid) => tool_ok(id, json!({ "ok": true, "message_id": mid })),
                Err(e) => tool_err(id, &format!("reply failed: {e}")),
            }
        }
        "handoff" => {
            let to = args.get("to").and_then(|v| v.as_str()).unwrap_or("");
            let summary = args.get("summary").and_then(|v| v.as_str()).unwrap_or("");
            if to.is_empty() || to == caller {
                return tool_err(id, "handoff: a distinct 'to' is required");
            }
            match manager.handoff(caller, to, summary) {
                Ok(mid) => tool_ok(id, json!({ "ok": true, "message_id": mid })),
                Err(e) => tool_err(id, &format!("handoff failed: {e}")),
            }
        }
        "assign" => {
            let message = args.get("message").and_then(|v| v.as_str()).unwrap_or("");
            if message.is_empty() {
                return tool_err(id, "assign: 'message' is required");
            }
            let wd = args
                .get("working_directory")
                .and_then(|v| v.as_str())
                .map(String::from);
            let role = args.get("role").and_then(|v| v.as_str()).filter(|r| !r.is_empty());
            let tools: Option<Vec<String>> = args.get("tools").and_then(|v| v.as_array()).map(|a| {
                a.iter().filter_map(|t| t.as_str().map(String::from)).collect()
            });
            match manager.assign_worker(caller, message, wd, role, tools) {
                Ok(worker) => tool_ok(id, json!({ "ok": true, "worker_id": worker })),
                Err(e) => tool_err(id, &format!("assign failed: {e}")),
            }
        }
        "share" => {
            let key = args.get("key").and_then(|v| v.as_str()).unwrap_or("");
            let value = args.get("value").and_then(|v| v.as_str()).unwrap_or("");
            if key.is_empty() {
                return tool_err(id, "share: 'key' is required");
            }
            match manager.blackboard_set(key, value, caller) {
                Ok(()) => tool_ok(id, json!({ "ok": true, "key": key })),
                Err(e) => tool_err(id, &format!("share failed: {e}")),
            }
        }
        "get" => {
            let key = args.get("key").and_then(|v| v.as_str()).unwrap_or("");
            if key.is_empty() {
                return tool_err(id, "get: 'key' is required");
            }
            match manager.blackboard_get(key) {
                Ok(Some((value, author, updated_at))) => tool_ok(
                    id,
                    json!({ "found": true, "key": key, "value": value, "author": author, "updated_at": updated_at }),
                ),
                Ok(None) => tool_ok(id, json!({ "found": false, "key": key, "value": Value::Null })),
                Err(e) => tool_err(id, &format!("get failed: {e}")),
            }
        }
        "create_workflow" => {
            let def = args.get("definition").and_then(|v| v.as_str()).unwrap_or("");
            match manager.create_workflow(def, "generated") {
                Ok(name) => tool_ok(id, json!({ "ok": true, "name": name })),
                Err(e) => tool_err(id, &format!("create_workflow failed: {e}")),
            }
        }
        "run_workflow" => {
            // Workflow node workers hold orchestration tools (for `share`), but
            // must not start runs: a node recursing into its own workflow would
            // mint agents unboundedly, outside the assign fan/depth guards.
            if manager.is_workflow_worker(caller) {
                return tool_err(id, "run_workflow is not available to workflow node workers");
            }
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            // An orchestrator-started run inherits the orchestrator's task, so
            // workflow node agents land in the same Task as the rest of the team.
            let task = manager.task_of_agent(caller);
            match manager.run_workflow(name, None, Some(caller.to_string()), task) {
                Ok(run_id) => tool_ok(id, json!({ "ok": true, "run_id": run_id })),
                Err(e) => tool_err(id, &format!("run_workflow failed: {e}")),
            }
        }
        other => error(id, -32602, &format!("unknown tool '{other}'")),
    }
}

fn status_str(s: taime_protocol::AgentStatus) -> &'static str {
    use taime_protocol::AgentStatus::*;
    match s {
        Idle => "IDLE",
        Processing => "PROCESSING",
        WaitingUserAnswer => "WAITING_USER_ANSWER",
        Completed => "COMPLETED",
        Error => "ERROR",
    }
}

// --- JSON-RPC envelope helpers ---

fn result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// An MCP tool result (content blocks). Tool-level failures are reported as
/// `isError` content, not a JSON-RPC error (per the MCP spec).
fn tool_ok(id: Value, structured: Value) -> Value {
    result(
        id,
        json!({
            "content": [ { "type": "text", "text": structured.to_string() } ],
            "isError": false
        }),
    )
}

fn tool_err(id: Value, message: &str) -> Value {
    result(
        id,
        json!({
            "content": [ { "type": "text", "text": message } ],
            "isError": true
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn manager() -> Manager {
        let store = Store::open_at(std::path::Path::new(":memory:")).unwrap();
        Manager::for_test(Some(store))
    }

    #[test]
    fn initialize_advertises_server() {
        let mgr = manager();
        let resp = handle(&mgr, "term-a", &json!({"jsonrpc":"2.0","id":1,"method":"initialize"}));
        assert_eq!(resp["result"]["serverInfo"]["name"], "taime-orchestrator");
        assert_eq!(resp["result"]["protocolVersion"], MCP_PROTOCOL);
    }

    #[test]
    fn tools_list_has_the_mvp_tools() {
        let mgr = manager();
        let resp = handle(&mgr, "term-a", &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
        let names: Vec<&str> = resp["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"list_agents"));
        assert!(names.contains(&"send_message"));
    }

    #[test]
    fn send_message_stamps_from_caller_not_a_client_field() {
        let mgr = manager();
        let req = json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": "send_message", "arguments": { "to": "term-b", "body": "review please" } }
        });
        let resp = handle(&mgr, "term-a", &req);
        assert_eq!(resp["result"]["isError"], false);

        // The inbox row's sender is the authenticated caller, not anything the
        // client could have supplied.
        let pending = mgr.store().unwrap().pending_for("term-b", 10).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].sender_id, "term-a");
        assert_eq!(pending[0].message, "review please");
    }

    #[test]
    fn send_message_rejects_self_and_missing_to() {
        let mgr = manager();
        let self_msg = json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": { "name": "send_message", "arguments": { "to": "term-a", "body": "x" } }
        });
        assert_eq!(handle(&mgr, "term-a", &self_msg)["result"]["isError"], true);

        let no_to = json!({
            "jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": { "name": "send_message", "arguments": { "body": "x" } }
        });
        assert_eq!(handle(&mgr, "term-a", &no_to)["result"]["isError"], true);
    }

    #[test]
    fn broadcast_with_no_agents_is_zero_recipients() {
        let mgr = manager();
        let req = json!({
            "jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": { "name": "broadcast", "arguments": { "body": "standup in 5" } }
        });
        let resp = handle(&mgr, "term-a", &req);
        assert_eq!(resp["result"]["isError"], false);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("\"recipients\":0"));
    }

    #[test]
    fn handoff_enqueues_and_is_distinct() {
        let mgr = manager();
        // self-handoff rejected.
        let self_ho = json!({
            "jsonrpc": "2.0", "id": 8, "method": "tools/call",
            "params": { "name": "handoff", "arguments": { "to": "term-a", "summary": "x" } }
        });
        assert_eq!(handle(&mgr, "term-a", &self_ho)["result"]["isError"], true);

        let ho = json!({
            "jsonrpc": "2.0", "id": 9, "method": "tools/call",
            "params": { "name": "handoff", "arguments": { "to": "term-b", "summary": "take the diff from here" } }
        });
        assert_eq!(handle(&mgr, "term-a", &ho)["result"]["isError"], false);
        let pending = mgr.store().unwrap().pending_for("term-b", 10).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].sender_id, "term-a");
    }

    #[test]
    fn tools_list_has_the_full_surface() {
        let mgr = manager();
        let resp = handle(&mgr, "term-a", &json!({"jsonrpc":"2.0","id":10,"method":"tools/list"}));
        let names: Vec<&str> = resp["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        for t in [
            "list_agents",
            "send_message",
            "broadcast",
            "request",
            "reply",
            "handoff",
            "assign",
            "share",
            "get",
        ] {
            assert!(names.contains(&t), "missing tool {t}");
        }
    }

    fn call(mgr: &Manager, caller: &str, id: i64, name: &str, args: Value) -> Value {
        handle(
            mgr,
            caller,
            &json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call",
                     "params": { "name": name, "arguments": args } }),
        )
    }

    /// Pull the `text` content block of a tool result and parse it as JSON.
    fn tool_json(resp: &Value) -> Value {
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn request_then_reply_correlates_over_mcp() {
        let mgr = manager();
        let req = call(&mgr, "term-a", 20, "request", json!({ "to": "term-b", "body": "status?" }));
        assert_eq!(req["result"]["isError"], false);
        let iid = tool_json(&req)["interaction_id"].as_str().unwrap().to_string();
        assert!(!iid.is_empty());

        // Wrong responder is rejected.
        let bad = call(&mgr, "term-c", 21, "reply", json!({ "interaction_id": iid, "body": "x" }));
        assert_eq!(bad["result"]["isError"], true);

        // Addressed responder succeeds; the reply reaches the requester's inbox.
        let ok = call(&mgr, "term-b", 22, "reply", json!({ "interaction_id": iid, "body": "all green" }));
        assert_eq!(ok["result"]["isError"], false);
        let to_a = mgr.store().unwrap().pending_for("term-a", 10).unwrap();
        assert!(to_a.iter().any(|m| m.message.contains("all green")));
    }

    #[test]
    fn share_then_get_round_trips_over_mcp() {
        let mgr = manager();
        let s = call(&mgr, "term-a", 30, "share", json!({ "key": "plan", "value": "ship it" }));
        assert_eq!(s["result"]["isError"], false);

        let g = call(&mgr, "term-b", 31, "get", json!({ "key": "plan" }));
        assert_eq!(g["result"]["isError"], false);
        let got = tool_json(&g);
        assert_eq!(got["found"], true);
        assert_eq!(got["value"], "ship it");
        assert_eq!(got["author"], "term-a");

        // A missing key is found:false, not an error.
        let miss = call(&mgr, "term-b", 32, "get", json!({ "key": "nope" }));
        assert_eq!(tool_json(&miss)["found"], false);
    }

    #[test]
    fn broadcast_accepts_a_role_filter() {
        let mgr = manager();
        // With no live agents either form is 0 recipients (the role param plumbs
        // through without error).
        let resp = call(&mgr, "term-a", 40, "broadcast", json!({ "body": "hi", "role": "reviewer" }));
        assert_eq!(resp["result"]["isError"], false);
        assert!(resp["result"]["content"][0]["text"].as_str().unwrap().contains("\"recipients\":0"));
    }

    #[test]
    fn unknown_method_is_jsonrpc_error() {
        let mgr = manager();
        let resp = handle(&mgr, "term-a", &json!({"jsonrpc":"2.0","id":6,"method":"nope"}));
        assert_eq!(resp["error"]["code"], -32601);
    }
}
