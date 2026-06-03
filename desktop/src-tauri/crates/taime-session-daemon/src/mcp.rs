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
//! What remains (the per-agent transport) is wiring THIS dispatcher to each agent:
//! a tiny stdio shim bridging the CLI's MCP client to the daemon (or loopback
//! HTTP+SSE + per-agent token), injected via the Phase-1 per-provider MCP config.
//! That transport + the live MCP handshake against a real CLI is the remaining
//! Phase-5 integration — hence this module is unit-tested here but not yet
//! mounted on a socket.
#![allow(dead_code)]

use serde_json::{json, Value};

use crate::manager::Manager;

/// MCP protocol revision we advertise at `initialize`.
const MCP_PROTOCOL: &str = "2024-11-05";

/// The orchestration tool schemas (JSON Schema per the MCP spec) returned by
/// `tools/list`. Phase-5 MVP exposes `list_agents` + `send_message`; the rest
/// (`broadcast`/`request`/`reply`/`handoff`/`assign`/`share`) land with the
/// transport.
pub fn tool_definitions() -> Value {
    json!([
        {
            "name": "list_agents",
            "description": "List the live agent sessions: id, provider, status, cwd.",
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
                    json!({
                        "id": s.attribution_key.unwrap_or(s.id),
                        "provider": s.provider,
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
                Ok(mid) => tool_ok(id, json!({ "ok": true, "message_id": mid })),
                Err(e) => tool_err(id, &format!("send_message failed: {e}")),
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
    fn unknown_method_is_jsonrpc_error() {
        let mgr = manager();
        let resp = handle(&mgr, "term-a", &json!({"jsonrpc":"2.0","id":6,"method":"nope"}));
        assert_eq!(resp["error"]["code"], -32601);
    }
}
