//! Owns the live sessions. Orphan GC reaps only **dead** sessions — it never
//! auto-reaps a live agent (closing the app overnight must not kill a running
//! agent). The daemon shuts itself down only when it has no sessions AND no
//! connected client for an idle grace period; a live session or a connected app
//! keeps it alive.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use taime_protocol::{AgentSpawnSpec, SessionSummary, SpawnSpec};

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
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
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
    pub fn spawn_agent(&self, spec: AgentSpawnSpec) -> Result<String, String> {
        let prepared = self.registry.build(&spec)?;
        // The adapter (status inference) travels with the session.
        let adapter = self.registry.adapter(&spec.provider);
        let id = format!("pty-{:x}", self.session_counter.fetch_add(1, Ordering::SeqCst));
        let program = prepared.spec.prog.clone();
        let session = Session::spawn_prepared(id.clone(), prepared, adapter)?;
        self.sessions.lock().unwrap().insert(id.clone(), session);
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

    pub fn get(&self, id: &str) -> Option<Session> {
        self.sessions.lock().unwrap().get(id).cloned()
    }

    pub fn list(&self) -> Vec<SessionSummary> {
        self.sessions.lock().unwrap().values().map(|s| s.summary()).collect()
    }

    pub fn kill(&self, id: &str) {
        let session = self.sessions.lock().unwrap().remove(id);
        if let Some(s) = session {
            s.kill();
            // Removed from the live map, so gc_tick won't see it die — record the
            // exit here.
            if let Some(store) = &self.store {
                let _ = store.set_session_status(id, "exited");
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
        // Reap only DEAD sessions; record their exit in the durable store.
        {
            let mut map = self.sessions.lock().unwrap();
            let dead: Vec<String> = map
                .iter()
                .filter(|(_, s)| !s.is_alive())
                .map(|(k, _)| k.clone())
                .collect();
            for k in dead {
                map.remove(&k);
                if let Some(store) = &self.store {
                    let _ = store.set_session_status(&k, "exited");
                }
            }
        }
        // Quiet-window attribution boundaries for live sessions.
        let live: Vec<Session> = self.sessions.lock().unwrap().values().cloned().collect();
        for s in &live {
            s.quiet_check(quiet_threshold);
        }
        // Shutdown decision: nothing left to serve and the app isn't connected.
        let empty = self.sessions.lock().unwrap().is_empty();
        let no_clients = self.active_conns.load(Ordering::SeqCst) == 0;
        let idle = self.last_activity.lock().unwrap().elapsed() > idle_grace;
        empty && no_clients && idle
    }
}
