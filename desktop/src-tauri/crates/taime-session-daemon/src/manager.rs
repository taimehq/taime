//! Owns the live sessions. Orphan GC reaps only **dead** sessions — it never
//! auto-reaps a live agent (closing the app overnight must not kill a running
//! agent). The daemon shuts itself down only when it has no sessions AND no
//! connected client for an idle grace period; a live session or a connected app
//! keeps it alive.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use taime_protocol::{
    AgentProfile, AgentSpawnSpec, McpServerConfig, SessionSummary, SpawnSpec, WorktreeInfo,
};

use crate::providers::Registry;
use crate::session::Session;
use crate::store::{SessionRow, Store};

pub struct Manager {
    sessions: Mutex<HashMap<String, Session>>,
    session_counter: AtomicU64,
    conn_counter: AtomicU64,
    active_conns: AtomicU64,
    last_activity: Mutex<Instant>,
    /// Provider adapter registry (Phase 1): builds the launch command + MCP
    /// injection for high-level `SpawnAgent` requests.
    registry: Registry,
    /// Durable orchestration store (Phase 3). `None` if the DB couldn't open —
    /// persistence is best-effort and never blocks spawning.
    store: Option<Store>,
    /// Per-agent MCP token → attribution key (Phase 5 transport). Issued at spawn
    /// for orchestration-enabled agents and injected into their MCP shim's env;
    /// the daemon resolves the authenticated caller from it. Cleaned on exit.
    tokens: Mutex<HashMap<String, String>>,
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A 128-bit random hex id (per-agent MCP tokens + activity-event ids).
fn gen_id() -> String {
    format!("{:016x}{:016x}", rand::random::<u64>(), rand::random::<u64>())
}

/// The delimited stdin payload for an inbox delivery — a clear visual frame so
/// the user can tell orchestrator injection from agent output (the plan's
/// delivery format). Pure + testable.
fn format_delivery(sender_id: &str, body: &str) -> String {
    format!("\r\n--- MESSAGE FROM {sender_id} ---\r\n{body}\r\n--- END MESSAGE ---\r\n")
}

impl Default for Manager {
    fn default() -> Self {
        Self::new()
    }
}

impl Manager {
    pub fn new() -> Self {
        let store = match Store::open() {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("[taime-daemon] persistence disabled (store open failed): {e}");
                None
            }
        };
        Manager {
            sessions: Mutex::new(HashMap::new()),
            session_counter: AtomicU64::new(0),
            conn_counter: AtomicU64::new(0),
            active_conns: AtomicU64::new(0),
            last_activity: Mutex::new(Instant::now()),
            registry: Registry::load(),
            store,
            tokens: Mutex::new(HashMap::new()),
        }
    }

    pub fn next_conn_id(&self) -> u64 {
        self.conn_counter.fetch_add(1, Ordering::SeqCst)
    }

    pub fn conn_opened(&self) {
        self.active_conns.fetch_add(1, Ordering::SeqCst);
        self.touch();
    }

    pub fn conn_closed(&self) {
        self.active_conns.fetch_sub(1, Ordering::SeqCst);
        self.touch();
    }

    /// Mark recent client activity (resets the idle-shutdown timer).
    pub fn touch(&self) {
        *self.last_activity.lock().unwrap() = Instant::now();
    }

    pub fn spawn(&self, spec: SpawnSpec) -> Result<String, String> {
        let id = format!("pty-{:x}", self.session_counter.fetch_add(1, Ordering::SeqCst));
        let session = Session::spawn(id.clone(), &spec)?;
        self.sessions.lock().unwrap().insert(id.clone(), session);
        self.touch();
        Ok(id)
    }

    /// Spawn a high-level agent request: the registry builds the provider command
    /// + MCP injection, then we spawn it like any other session (the Phase-1
    /// all-CLI path). The session carries the MCP cleanup + provider id.
    pub fn spawn_agent(&self, mut spec: AgentSpawnSpec) -> Result<String, String> {
        // Phase-5 transport: an orchestration-enabled agent gets the daemon's own
        // MCP endpoint injected (the stdio shim = this binary `--mcp-stdio`),
        // authenticated by a per-agent token. Plain agents skip it (CAO parity).
        let mut issued_token: Option<String> = None;
        if spec.inject_orchestration {
            if let Some(bin) = std::env::current_exe().ok().and_then(|p| p.to_str().map(String::from)) {
                let token = gen_id();
                spec.profile.mcp_servers.push(McpServerConfig {
                    name: "taime".to_string(),
                    command: bin,
                    args: vec!["--mcp-stdio".to_string()],
                    env: vec![("TAIME_MCP_TOKEN".to_string(), token.clone())],
                });
                issued_token = Some(token);
            }
        }
        let prepared = self.registry.build(&spec)?;
        // The adapter (status inference) travels with the session.
        let adapter = self.registry.adapter(&spec.provider);
        let id = format!("pty-{:x}", self.session_counter.fetch_add(1, Ordering::SeqCst));
        let program = prepared.spec.prog.clone();
        let session = Session::spawn_prepared(id.clone(), prepared, adapter)?;
        self.sessions.lock().unwrap().insert(id.clone(), session);
        if let (Some(token), Some(key)) = (issued_token, &spec.attribution_key) {
            self.tokens.lock().unwrap().insert(token, key.clone());
        }
        // Durable record (best-effort): the agent existed, with its provider +
        // attribution key + cwd, for history / Phase-6 attribution.
        if let Some(store) = &self.store {
            let row = SessionRow {
                pty_session_id: id.clone(),
                provider: Some(spec.provider.clone()),
                attribution_key: spec.attribution_key.clone(),
                cwd: spec.cwd.clone(),
                program,
                created_at_unix: now_unix(),
                status: "running".to_string(),
            };
            if let Err(e) = store.record_session(&row) {
                eprintln!("[taime-daemon] persist session {id} failed: {e}");
            }
        }
        self.touch();
        Ok(id)
    }

    /// Provision (or resolve) an isolated git worktree for a new agent (Phase 3).
    /// The daemon mints the attribution key, runs `git worktree`, and persists the
    /// `taime_worktrees` row. Shells out to git — call from a blocking context.
    pub fn provision_worktree(
        &self,
        project_root: String,
        provider: String,
        isolate: bool,
    ) -> WorktreeInfo {
        let terminal_key = format!("{:08x}", rand::random::<u32>());
        let info = crate::worktree::provision(&project_root, &provider, isolate, &terminal_key);
        if let Some(store) = &self.store {
            if let Err(e) = store.upsert_worktree(&info, &provider, now_unix()) {
                eprintln!("[taime-daemon] persist worktree {terminal_key} failed: {e}");
            }
        }
        self.touch();
        info
    }

    pub fn get(&self, id: &str) -> Option<Session> {
        self.sessions.lock().unwrap().get(id).cloned()
    }

    /// The live session addressed by `attribution_key` (the inbox routes to it).
    fn session_by_attribution(&self, key: &str) -> Option<Session> {
        self.sessions
            .lock()
            .unwrap()
            .values()
            .find(|s| s.attribution_key().as_deref() == Some(key))
            .cloned()
    }

    /// Enqueue an inbox message for `receiver_id` (Phase 5 message bus). Persisted
    /// `pending`; the delivery engine injects it when the receiver next goes
    /// idle. Returns the monotonic message id.
    pub fn enqueue_message(
        &self,
        sender_id: String,
        receiver_id: String,
        message: String,
    ) -> Result<i64, String> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| "persistence disabled; cannot enqueue".to_string())?;
        store
            .enqueue_message(&sender_id, &receiver_id, &message, now_unix())
            .map_err(|e| format!("enqueue: {e}"))
    }

    /// Idle-gated delivery (the safety property): for each receiver with a pending
    /// message that maps to a live, **ready** (idle/completed) session, inject the
    /// oldest message into its stdin with a visual delimiter and mark it
    /// `delivered` (idempotent). Never interrupts a mid-turn agent. Best-effort.
    pub fn deliver_pending(&self) {
        let Some(store) = &self.store else { return };
        let receivers = match store.receivers_with_pending() {
            Ok(r) => r,
            Err(_) => return,
        };
        for receiver in receivers {
            let Some(session) = self.session_by_attribution(&receiver) else { continue };
            if !session.is_ready_for_delivery() {
                continue;
            }
            let msg = match store.pending_for(&receiver, 1) {
                Ok(mut m) => m.pop(),
                Err(_) => None,
            };
            let Some(msg) = msg else { continue };
            let payload = format_delivery(&msg.sender_id, &msg.message);
            match session.input(payload.as_bytes()) {
                Ok(_) => {
                    let _ = store.set_message_status(msg.id, "delivered");
                }
                Err(_) => {
                    let _ = store.set_message_status(msg.id, "failed");
                }
            }
        }
    }

    // ---- Phase 5 MCP transport + orchestration tools ----

    /// Resolve a per-agent MCP token to the authenticated caller's attribution
    /// key. `None` = unknown/expired token (request is rejected).
    fn resolve_token(&self, token: &str) -> Option<String> {
        self.tokens.lock().unwrap().get(token).cloned()
    }

    /// Dispatch one MCP JSON-RPC request from an agent's stdio shim: authenticate
    /// via `token`, then run the in-process tool layer with the resolved caller.
    /// Returns the JSON-RPC response (empty string for a notification). Shells out
    /// (assign → git worktree), so call from a blocking context.
    pub fn handle_mcp(&self, token: &str, json: &str) -> String {
        let caller = match self.resolve_token(token) {
            Some(c) => c,
            None => {
                return r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32001,"message":"unauthorized"}}"#
                    .to_string()
            }
        };
        let req: serde_json::Value = match serde_json::from_str(json) {
            Ok(v) => v,
            Err(_) => {
                return r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"parse error"}}"#
                    .to_string()
            }
        };
        let resp = crate::mcp::handle(self, &caller, &req);
        if resp.is_null() {
            String::new()
        } else {
            resp.to_string()
        }
    }

    /// The activity graph as JSON (Phase 6): agents (from the recorded sessions)
    /// + inter-agent edges (the send_message/handoff/assign events). Read from the
    /// durable store, so it reflects the full history even with the UI closed.
    pub fn activity_graph_json(&self) -> String {
        let Some(store) = &self.store else {
            return r#"{"agents":[],"edges":[]}"#.to_string();
        };
        let agents: Vec<serde_json::Value> = store
            .graph_agents()
            .unwrap_or_default()
            .into_iter()
            .map(|(id, provider, status)| serde_json::json!({ "id": id, "provider": provider, "status": status }))
            .collect();
        let edges: Vec<serde_json::Value> = store
            .activity_edges()
            .unwrap_or_default()
            .into_iter()
            .map(|(kind, source, target)| serde_json::json!({ "kind": kind, "source": source, "target": target }))
            .collect();
        serde_json::json!({ "agents": agents, "edges": edges }).to_string()
    }

    /// Broadcast a message to every live agent except the sender. Returns the
    /// count enqueued.
    pub fn broadcast(&self, sender: &str, body: &str) -> usize {
        let receivers: Vec<String> = self
            .sessions
            .lock()
            .unwrap()
            .values()
            .filter_map(|s| s.attribution_key())
            .filter(|k| k != sender)
            .collect();
        let mut n = 0;
        for r in receivers {
            if self.enqueue_message(sender.to_string(), r, body.to_string()).is_ok() {
                n += 1;
            }
        }
        n
    }

    /// Hand off to another agent: enqueue the summary + record a handoff edge.
    pub fn handoff(&self, from: &str, to: &str, summary: &str) -> Result<i64, String> {
        let id = self.enqueue_message(from.to_string(), to.to_string(), summary.to_string())?;
        if let Some(store) = &self.store {
            let _ = store.record_activity_edge(&gen_id(), "handoff", from, to, now_unix());
        }
        Ok(id)
    }

    /// Spawn a worker sub-agent (`assign`): provision a worktree off the parent's
    /// project, launch a default-profile worker (same provider, **no**
    /// orchestration tools so it can't re-assign), seed the task via the inbox
    /// (delivered when the worker is idle), and record the parent→child edge.
    /// Returns the worker's attribution key.
    pub fn assign_worker(
        &self,
        parent_key: &str,
        prompt: &str,
        working_directory: Option<String>,
    ) -> Result<String, String> {
        let parent = self.session_by_attribution(parent_key);
        let provider = parent
            .as_ref()
            .and_then(|s| s.provider())
            .unwrap_or_else(|| "claude_code".to_string());
        let project_root = working_directory
            .clone()
            .or_else(|| parent.as_ref().map(|s| s.cwd()))
            .filter(|c| !c.is_empty());

        let (cwd, child_key) = match project_root {
            Some(root) => {
                let info = self.provision_worktree(root, provider.clone(), true);
                (Some(info.worktree_path), info.terminal_key)
            }
            None => (working_directory, gen_id()[..8].to_string()),
        };

        let spec = AgentSpawnSpec {
            provider,
            profile: AgentProfile { name: "default".to_string(), ..Default::default() },
            cwd,
            rows: 24,
            cols: 80,
            attribution_key: Some(child_key.clone()),
            seed_prompt: None,
            env: vec![],
            inject_orchestration: false,
        };
        self.spawn_agent(spec)?;
        // Seed the task: delivered to the worker when it first goes idle.
        let _ = self.enqueue_message(parent_key.to_string(), child_key.clone(), prompt.to_string());
        if let Some(store) = &self.store {
            let _ = store.record_activity_edge(&gen_id(), "assign", parent_key, &child_key, now_unix());
        }
        Ok(child_key)
    }

    pub fn list(&self) -> Vec<SessionSummary> {
        self.sessions.lock().unwrap().values().map(|s| s.summary()).collect()
    }

    pub fn kill(&self, id: &str) {
        let session = self.sessions.lock().unwrap().remove(id);
        if let Some(s) = session {
            let akey = s.attribution_key();
            s.kill();
            // Removed from the live map, so gc_tick won't see it die — record the
            // exit + drop its MCP token here.
            if let Some(store) = &self.store {
                let _ = store.set_session_status(id, "exited");
            }
            if let Some(akey) = akey {
                self.tokens.lock().unwrap().retain(|_, v| v != &akey);
            }
        }
        self.touch();
    }

    pub fn kill_all(&self) {
        let sessions: Vec<Session> = self.sessions.lock().unwrap().drain().map(|(_, s)| s).collect();
        for s in sessions {
            s.kill();
        }
    }

    /// Periodic maintenance. Reaps dead sessions (never a live agent), runs the
    /// quiet-window attribution check, and returns `true` when the daemon should
    /// shut down (no sessions + no connected client + idle past `idle_grace`).
    pub fn gc_tick(&self, quiet_threshold: Duration, idle_grace: Duration) -> bool {
        // Reap only DEAD sessions; record their exit + drop their MCP tokens.
        {
            let mut map = self.sessions.lock().unwrap();
            let dead: Vec<(String, Option<String>)> = map
                .iter()
                .filter(|(_, s)| !s.is_alive())
                .map(|(k, s)| (k.clone(), s.attribution_key()))
                .collect();
            let mut dead_akeys: Vec<String> = Vec::new();
            for (k, akey) in dead {
                map.remove(&k);
                if let Some(store) = &self.store {
                    let _ = store.set_session_status(&k, "exited");
                }
                if let Some(a) = akey {
                    dead_akeys.push(a);
                }
            }
            drop(map);
            if !dead_akeys.is_empty() {
                self.tokens.lock().unwrap().retain(|_, v| !dead_akeys.contains(v));
            }
        }
        // Quiet-window attribution boundaries for live sessions.
        let live: Vec<Session> = self.sessions.lock().unwrap().values().cloned().collect();
        for s in &live {
            s.quiet_check(quiet_threshold);
        }
        // Idle-gated inbox delivery (Phase 5): deliver pending messages to any
        // receiver that's now idle. Runs on the same 250ms tick as quiet-window.
        self.deliver_pending();
        // Shutdown decision: nothing left to serve and the app isn't connected.
        let empty = self.sessions.lock().unwrap().is_empty();
        let no_clients = self.active_conns.load(Ordering::SeqCst) == 0;
        let idle = self.last_activity.lock().unwrap().elapsed() > idle_grace;
        empty && no_clients && idle
    }
}

#[cfg(test)]
impl Manager {
    /// Construct a manager with an injected store (so tests don't touch the real
    /// app-data DB). Empty session map + the built-in provider registry.
    pub fn for_test(store: Option<Store>) -> Self {
        Manager {
            sessions: Mutex::new(HashMap::new()),
            session_counter: AtomicU64::new(0),
            conn_counter: AtomicU64::new(0),
            active_conns: AtomicU64::new(0),
            last_activity: Mutex::new(Instant::now()),
            registry: Registry::load(),
            store,
            tokens: Mutex::new(HashMap::new()),
        }
    }

    /// The durable store (test inspection).
    pub fn store(&self) -> Option<&Store> {
        self.store.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivery_payload_is_clearly_delimited() {
        let out = format_delivery("term-a", "please review the diff");
        assert_eq!(
            out,
            "\r\n--- MESSAGE FROM term-a ---\r\nplease review the diff\r\n--- END MESSAGE ---\r\n"
        );
        // The frame is visually distinguishable from agent output.
        assert!(out.contains("--- MESSAGE FROM term-a ---"));
        assert!(out.contains("--- END MESSAGE ---"));
    }

    #[test]
    fn activity_graph_includes_agents_and_edges() {
        let store = crate::store::Store::open_at(std::path::Path::new(":memory:")).unwrap();
        store
            .record_session(&SessionRow {
                pty_session_id: "pty-0".into(),
                provider: Some("claude_code".into()),
                attribution_key: Some("a".into()),
                cwd: None,
                program: "claude".into(),
                created_at_unix: 1,
                status: "running".into(),
            })
            .unwrap();
        store.record_activity_edge("e1", "assign", "a", "b", 2).unwrap();

        let mgr = Manager::for_test(Some(store));
        let v: serde_json::Value = serde_json::from_str(&mgr.activity_graph_json()).unwrap();
        assert_eq!(v["agents"].as_array().unwrap().len(), 1);
        assert_eq!(v["agents"][0]["id"], "a");
        assert_eq!(v["agents"][0]["provider"], "claude_code");
        assert_eq!(v["edges"].as_array().unwrap().len(), 1);
        assert_eq!(v["edges"][0]["kind"], "assign");
        assert_eq!(v["edges"][0]["source"], "a");
        assert_eq!(v["edges"][0]["target"], "b");
    }
}
