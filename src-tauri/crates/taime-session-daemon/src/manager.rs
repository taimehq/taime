//! Owns the live sessions. Orphan GC reaps only **dead** sessions — it never
//! auto-reaps a live agent (closing the app overnight must not kill a running
//! agent). The daemon shuts itself down only when it has no sessions AND no
//! connected client for an idle grace period; a live session or a connected app
//! keeps it alive.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use taime_protocol::{
    AgentProfile, AgentSpawnSpec, McpServerConfig, SessionSummary, SpawnSpec, StoreHealth,
    WorktreeInfo,
};

use crate::providers::Registry;
use crate::schedules::{self, ScheduleDef};
use crate::session::Session;
use crate::store::{ScheduleRow, SessionRow, Store};

pub struct Manager {
    sessions: Mutex<HashMap<String, Session>>,
    /// Random per-daemon-generation nonce woven into every pty session id so the
    /// monotonic `session_counter` (which restarts at 0 each boot) can never mint
    /// an id that collides with a prior generation's durable `daemon_sessions` row.
    /// Idle-shutdown makes daemon restarts routine; without this the first spawn
    /// after every restart was `pty-0`, resurrecting the dead gen-1 agent's row.
    boot_nonce: String,
    session_counter: AtomicU64,
    conn_counter: AtomicU64,
    active_conns: AtomicU64,
    last_activity: Mutex<Instant>,
    /// Provider adapter registry (Phase 1): builds the launch command + MCP
    /// injection for high-level `SpawnAgent` requests.
    registry: Registry,
    /// Durable orchestration store (Phase 3). `None` if the DB couldn't open —
    /// persistence is best-effort and never blocks spawning. `Arc` so each session
    /// can hold a handle for its fs-watch attribution writes.
    store: Option<Arc<Store>>,
    /// How the store open went (Ok / recovered-from-corruption / unavailable),
    /// reported in every `HelloOk` so degraded persistence is visible in the UI
    /// instead of silently dropping the attribution substrate.
    store_health: StoreHealth,
    /// Per-agent MCP token → attribution key (Phase 5 transport). Issued at spawn
    /// for orchestration-enabled agents and injected into their MCP shim's env;
    /// the daemon resolves the authenticated caller from it. Cleaned on exit.
    tokens: Mutex<HashMap<String, String>>,
    /// attribution key → profile/role name (Phase 5). Set at spawn so `broadcast`
    /// can filter by role and `list_agents` can report it.
    roles: Mutex<HashMap<String, String>>,
    /// child attribution key → its assignment node (parent + depth). Powers the
    /// `assign` fan/depth limits and the result fan-in on worker exit.
    assignments: Mutex<HashMap<String, AssignNode>>,
    /// Weak handle to our own `Arc`, set once at startup, so methods that spawn a
    /// background driver thread (the Workflow engine) can hand it an owned `Arc`.
    weak_self: std::sync::OnceLock<std::sync::Weak<Manager>>,
    /// Worktree rows already GC'd this daemon run (checkout removed) — skipped on
    /// subsequent sweeps so a sweep never re-shells `git` for settled rows.
    worktrees_gced: Mutex<std::collections::HashSet<String>>,
    /// Live workflow-engine threads. Within a run the engine is sequential (one
    /// worker at a time), but RUNS would otherwise be unbounded — each is a
    /// thread + a stream of spawned agents outside the assign fan/depth guards.
    active_workflow_runs: AtomicU64,
    /// Single-flight guard for the maintenance tick (review M6): `gc_tick` now
    /// runs on a `spawn_blocking` thread, so if one tick wedges (e.g. a blocking
    /// PTY write to a stalled child) the 250 ms loop must NOT pile up more blocked
    /// jobs — a tick that finds this already set skips itself.
    gc_running: AtomicBool,
    /// Single-flight guard for the cron check (review H3): two overlapping
    /// `check_schedules` runs must not both fire the same due schedule.
    schedules_checking: AtomicBool,
}

// LOCK DISCIPLINE: never hold two Manager mutexes at once. The canonical
// pattern is snapshot-under-one-lock, release, then act (see `broadcast`,
// `gc_tick`). `sessions` is the hottest lock — in particular, never do SQLite
// writes or shell out while holding it.

/// One node in the assignment tree: who spawned this worker, and how deep it sits
/// (a top-level orchestrator is depth 0; its workers depth 1; …).
struct AssignNode {
    parent: String,
    depth: u32,
}

/// Max assignment chain depth (a worker at this depth may not `assign` further)
/// and max direct children per parent — runaway-fanout backstops for `assign`.
const MAX_ASSIGN_DEPTH: u32 = 4;
const MAX_ASSIGN_FAN: usize = 8;

/// Grace between a `kill()`'s group SIGTERM and the gc reaper's escalation to a
/// group SIGKILL (review H1). Generous — a well-behaved CLI exits on SIGTERM in
/// well under this; the escalation is the backstop for one that ignores it.
const KILL_GRACE: Duration = Duration::from_secs(2);

/// Example workflow seeded on first run: implement → test → (PASS: review, FAIL:
/// loop back to implement). Demonstrates a conditional branch + a loop.
const EXAMPLE_FEATURE_REVIEW: &str = r#"{
  "name": "feature-with-review",
  "entry": "implement",
  "max_iterations": 12,
  "nodes": [
    { "id": "implement", "profile": "feature-builder",
      "prompt": "Implement the feature described by the user or orchestrator in this workspace, matching the existing patterns. Add or update tests." },
    { "id": "test", "profile": "default",
      "prompt": "Run the project's test suite. Start your shared result with PASS if everything passes, or FAIL (listing what failed) otherwise." },
    { "id": "review", "profile": "security-reviewer",
      "prompt": "Review the implemented changes for correctness and security; summarize findings and severity." }
  ],
  "edges": [
    { "from": "implement", "to": "test",      "when": "always" },
    { "from": "test",      "to": "review",    "when": "keyword:PASS" },
    { "from": "test",      "to": "implement", "when": "keyword:FAIL" }
  ]
}
"#;

/// Example workflow seeded on first run: fix → verify, looping back to fix until
/// the tests pass.
const EXAMPLE_FIX_VERIFY: &str = r#"{
  "name": "fix-and-verify",
  "entry": "fix",
  "max_iterations": 10,
  "nodes": [
    { "id": "fix", "profile": "bug-fixer",
      "prompt": "Reproduce and fix the bug described by the user or orchestrator. Add a regression test." },
    { "id": "verify", "profile": "default",
      "prompt": "Run the tests, including the new regression test. Start your shared result with PASS if they all pass, or FAIL with details otherwise." }
  ],
  "edges": [
    { "from": "fix",    "to": "verify", "when": "always" },
    { "from": "verify", "to": "fix",    "when": "keyword:FAIL" }
  ]
}
"#;

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Provider id → human title for provenance (`Co-authored-by`). Mirrors the
/// frontend `PROVIDER_TITLE`; unknown providers fall back to the raw id so the
/// trailer is never empty (the daemon is the single writer of the commit).
fn provider_title(provider: &str) -> String {
    match provider {
        "claude_code" => "Claude Code".to_string(),
        "codex" => "Codex CLI".to_string(),
        "gemini_cli" => "Gemini CLI".to_string(),
        "grok_cli" => "Grok Build CLI".to_string(),
        "" => "Agent".to_string(),
        other => other.to_string(),
    }
}

/// The first 8 chars of an Agent ID — the short display form in commit subjects.
fn short_id(agent_id: &str) -> String {
    agent_id.chars().take(8).collect()
}

/// Attach the provenance record as a git note on the merge commit under
/// `refs/notes/taime` — the in-repo, pushable twin of the trailer (`git push
/// origin refs/notes/taime` travels it to collaborators). Best-effort: a note
/// failure never undoes the commit (the trailer already carries the core).
fn write_git_note(repo: &str, commit: &str, json: &str) -> bool {
    std::process::Command::new("git")
        .current_dir(repo)
        .args(["notes", "--ref=taime", "add", "-f", "-m", json, commit])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Best-effort `git push` of the target's current branch after a provenance
/// commit (the opt-in `push` flag). Returns `(pushed, error)`: a missing remote /
/// no upstream / rejected push is `(false, Some(reason))` and NEVER undoes the
/// commit — provenance is already durable in the commit and its note.
fn git_push(repo: &str) -> (bool, Option<String>) {
    match std::process::Command::new("git").current_dir(repo).args(["push"]).output() {
        Ok(o) if o.status.success() => (true, None),
        Ok(o) => (false, Some(String::from_utf8_lossy(&o.stderr).trim().to_string())),
        Err(e) => (false, Some(e.to_string())),
    }
}

/// A recorded merge → the JSON shape the app reads (merge history + attribution
/// export). Field names align with the commit trailer / git note.
fn merge_record_json(m: &crate::store::MergeRecord) -> serde_json::Value {
    serde_json::json!({
        "id": m.id,
        "commit": m.commit_sha,
        "target": m.target_symbol,
        "target_repo": m.target_repo,
        "base_sha": m.base_sha,
        "archive_ref": m.archive_ref,
        "digest": m.digest,
        "scope": m.scope,
        "hunks_selected": m.hunks_selected,
        "hunks_total": m.hunks_total,
        "reviewed": m.reviewed,
        "pushed": m.pushed,
        "pr_url": m.pr_url,
        "files": m.files,
        "merged_at": m.created_at_unix,
    })
}

/// Read a `u64` tunable from the environment, falling back to `default`. Used for
/// the worktree retention caps (grace / keep-recent / max-idle), so they're
/// configurable without a config surface.
fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key).ok().and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(default)
}

/// Pure victim-selection for the retention sweep (extracted so the cap/grace
/// logic is unit-testable without git/fs/env). `physical` is the non-reclaimed
/// ISOLATED worktrees newest-provisioned first (the recency rank the count cap
/// protects). A worktree is reclaimed when it is dead (not in `live`), isolated,
/// past the `grace`, AND either ranks beyond the `keep` most-recent checkouts OR
/// is older than `max_idle` (0 ⇒ no age cap). Live agents and within-grace rows
/// are never victims — and since archive-then-reclaim is non-lossy, an evicted
/// agent stays fully reviewable from its archive.
fn select_reclaim_victims<'a>(
    physical: &'a [crate::store::WorktreeRow],
    live: &std::collections::HashSet<String>,
    now: u64,
    grace: u64,
    keep: usize,
    max_idle: u64,
) -> Vec<&'a crate::store::WorktreeRow> {
    physical
        .iter()
        .enumerate()
        .filter_map(|(rank, w)| {
            if live.contains(&w.terminal_id) {
                return None;
            }
            if w.mode.as_deref() != Some("isolated") {
                return None;
            }
            let age = now.saturating_sub(w.created_at.unwrap_or(0));
            if age < grace {
                return None;
            }
            let over_count = rank >= keep;
            let over_age = max_idle > 0 && age >= max_idle;
            (over_count || over_age).then_some(w)
        })
        .collect()
}

/// Where an agent's review/diff surfaces are rendered from (see
/// [`Manager::review_source`]).
enum ReviewSource {
    /// The agent's live worktree checkout is on disk; diff it against `base`.
    Live { cwd: String, base: Option<String> },
    /// The checkout was reclaimed; render from the cached patch + archive ref.
    Archive { repo_root: String, patch: crate::store::ReviewPatch },
    /// The checkout was reclaimed and had no changes — there is no diff.
    Empty,
}

/// Clears an `AtomicBool` single-flight latch on scope exit (even on early return
/// or panic). Used to guard the gc tick + cron check against re-entry.
struct FlagGuard<'a>(&'a AtomicBool);
impl Drop for FlagGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The last path component (a workspace's display name), falling back to the
/// whole string for a rootless/relative path.
fn basename(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .map(String::from)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| path.to_string())
}

/// Hard cap on a schedule gate's wall-clock runtime (review H4). A hanging gate
/// (waits on stdin, network, a wedged subprocess) must not leak a thread + child
/// every ~30 s cron tick; a timeout is treated as gate-fail (skip the fire).
const GATE_TIMEOUT: Duration = Duration::from_secs(30);

/// Run a schedule's optional shell gate: `sh -c <script>` → exit 0 means proceed.
/// Runs inside the schedule check's `spawn_blocking`. `stdin` is `/dev/null` so a
/// gate that reads stdin returns EOF instead of blocking forever, and the run is
/// bounded by [`GATE_TIMEOUT`] — a timeout kills the gate and fails closed
/// (review H4). stdout/stderr are discarded (no captured-pipe deadlock).
fn run_script_gate(script: &str) -> bool {
    run_script_gate_with_timeout(script, GATE_TIMEOUT)
}

/// Inner gate runner with an injectable timeout (so tests can exercise the
/// timeout path without waiting [`GATE_TIMEOUT`]).
fn run_script_gate_with_timeout(script: &str, timeout: Duration) -> bool {
    use std::process::{Command, Stdio};
    let mut child = match Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    eprintln!(
                        "[taime-daemon] schedule gate exceeded {}s — treating as fail",
                        timeout.as_secs()
                    );
                    return false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return false,
        }
    }
}

/// The FIXED, injection-safe `[[var]]` allowlist for a schedule prompt.
/// `[[schedule_name]]` is canonical; `[[flow_name]]` substitutes forever as the
/// legacy alias (both resolve to the schedule name).
fn schedule_vars(name: &str) -> HashMap<&'static str, String> {
    let now = chrono::Local::now();
    let mut m: HashMap<&'static str, String> = HashMap::new();
    m.insert("date", now.format("%Y-%m-%d").to_string());
    m.insert("time", now.format("%H:%M").to_string());
    m.insert("schedule_name", name.to_string());
    m.insert("flow_name", name.to_string());
    m
}

/// A 128-bit random hex id (per-agent MCP tokens + activity-event ids).
fn gen_id() -> String {
    format!("{:016x}{:016x}", rand::random::<u64>(), rand::random::<u64>())
}

/// Mint a 64-bit random hex id, re-rolling while `taken` reports a collision.
/// The Agent ID is the durable identity pivot — worktrees, turns, fs events,
/// edges, and the `refs/taime/archive/<id>` keep-around ref are all keyed by it,
/// and a collision silently merges two agents' attribution (and now corrupts an
/// archive ref). 64 random bits (vs the old 32) make a birthday collision
/// astronomically unlikely even before the check; the mint-time guard closes it
/// entirely. Bounded retry; falls back to a full 128-bit [`gen_id`] in the
/// (unreachable) event every roll is taken rather than spinning or panicking.
fn mint_unique_id(taken: impl Fn(&str) -> bool) -> String {
    for _ in 0..64 {
        let id = format!("{:016x}", rand::random::<u64>());
        if !taken(&id) {
            return id;
        }
    }
    gen_id()
}

/// Whether a provider binary is resolvable (PATH + the known install locations
/// the adapters check). Best-effort; uses the daemon's inherited env.
fn binary_installed(binary: &str) -> bool {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    // Known per-CLI install locations the adapters resolve explicitly.
    if let Some(h) = &home {
        let known: &[&str] = match binary {
            "claude" => &[".local/bin/claude"],
            "grok" => &[".grok/bin/grok"],
            _ => &[],
        };
        for rel in known {
            if h.join(rel).exists() {
                return true;
            }
        }
    }
    if binary.contains('/') {
        return std::path::Path::new(binary).exists();
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            if dir.join(binary).exists() {
                return true;
            }
        }
    }
    false
}

/// The delimited stdin payload for an inbox delivery — a clear visual frame so
/// the user can tell orchestrator injection from agent output (the plan's
/// delivery format). Pure + testable.
/// Frame an inbox message for injection. Uses bare line-feeds (NO carriage
/// returns), so nothing in the payload reads as Enter: a long multi-line message
/// is detected as a bracketed paste by the CLI, and `deliver_pending` submits it
/// with a SEPARATE, delayed `\r`. (A trailing `\r\n` in the same burst becomes
/// paste content, leaving the prompt sitting in the input box unsent.)
fn format_delivery(sender_id: &str, body: &str) -> String {
    format!("\n--- MESSAGE FROM {sender_id} ---\n{body}\n--- END MESSAGE ---\n")
}

impl Default for Manager {
    fn default() -> Self {
        Self::new()
    }
}

impl Manager {
    pub fn new() -> Self {
        let (store, store_health) = Store::open_or_recover();
        match &store_health {
            StoreHealth::Ok => {}
            StoreHealth::Recovered { moved_to } => eprintln!(
                "[taime-daemon] store recovered: corrupt taime.sqlite moved aside to \
                 {moved_to}; starting on a fresh DB (prior history is in the moved file)"
            ),
            StoreHealth::Unavailable { error } => eprintln!(
                "[taime-daemon] persistence disabled (store open failed): {error}"
            ),
        }
        let store = store.map(Arc::new);
        Manager {
            sessions: Mutex::new(HashMap::new()),
            boot_nonce: format!("{:016x}", rand::random::<u64>()),
            session_counter: AtomicU64::new(0),
            conn_counter: AtomicU64::new(0),
            active_conns: AtomicU64::new(0),
            last_activity: Mutex::new(Instant::now()),
            registry: Registry::load(),
            store,
            store_health,
            tokens: Mutex::new(HashMap::new()),
            roles: Mutex::new(HashMap::new()),
            assignments: Mutex::new(HashMap::new()),
            weak_self: std::sync::OnceLock::new(),
            worktrees_gced: Mutex::new(std::collections::HashSet::new()),
            active_workflow_runs: AtomicU64::new(0),
            gc_running: AtomicBool::new(false),
            schedules_checking: AtomicBool::new(false),
        }
    }

    pub fn next_conn_id(&self) -> u64 {
        self.conn_counter.fetch_add(1, Ordering::SeqCst)
    }

    /// Store health for the `HelloOk` handshake (set once at boot — the store is
    /// opened exactly once per daemon run).
    pub fn store_health(&self) -> StoreHealth {
        self.store_health.clone()
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

    /// Mint the next pty session id for THIS daemon generation: `pty-<nonce>-<n>`.
    /// The boot nonce makes it globally unique across restarts (the counter alone
    /// restarts at 0), so a new agent never inherits a dead agent's durable row.
    fn next_pty_id(&self) -> String {
        format!("pty-{}-{:x}", self.boot_nonce, self.session_counter.fetch_add(1, Ordering::SeqCst))
    }

    pub fn spawn(&self, spec: SpawnSpec) -> Result<String, String> {
        let id = self.next_pty_id();
        // The low-level Spawn path is the legacy Claude-only entry; it isn't a
        // worktree-mode agent, so it never runs in a shared tree.
        let session = Session::spawn(id.clone(), &spec, self.store.clone(), false)?;
        self.sessions.lock().unwrap().insert(id.clone(), session);
        self.touch();
        Ok(id)
    }

    /// Spawn a high-level agent request: the registry builds the provider command
    /// and MCP injection, then we spawn it like any other session. The session
    /// carries the MCP cleanup and provider id.
    pub fn spawn_agent(&self, mut spec: AgentSpawnSpec) -> Result<String, String> {
        // Resolve the named profile from the store (`~/.taime/agents/*.toml` +
        // the built-ins): fill any field the app left unset and flip on
        // orchestration for a supervisor role. App-provided values win.
        if let Some(def) = crate::profiles::ProfileStore::load().resolve(&spec.profile.name) {
            crate::profiles::apply_to(def, &mut spec.profile, &mut spec.inject_orchestration);
        }
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
        let id = self.next_pty_id();
        let program = prepared.spec.prog.clone();
        // Persist the provider-cleanup ledger (review H2) NOW — `registry.build`
        // already wrote the injected config (gemini settings.json / grok
        // config.toml / policy files), so a crash before or during this spawn must
        // still be reconcilable on boot. The session drops this row when it
        // finalizes (session.rs `finalize_exit`); whatever survives a daemon
        // crash is replayed by `reconcile_cleanups_on_boot`.
        if !prepared.cleanup.actions.is_empty() {
            if let (Ok(json), Some(store)) = (serde_json::to_string(&prepared.cleanup), &self.store) {
                let _ = store.record_cleanup(&id, &json, now_unix());
            }
        }
        // Is this agent's worktree SHARED (the user's real tree) rather than its
        // own isolated checkout? The session needs to know so its fs-watcher gates
        // attribution on the agent being mid-turn + de-dups co-watchers (review
        // items 4/11). The Agent ID is the worktree row's key, provisioned before
        // this spawn; absent/non-shared ⇒ isolated (record unconditionally).
        let shared = spec
            .agent_id
            .as_deref()
            .and_then(|aid| self.store.as_ref().and_then(|s| s.worktree_row(aid).ok().flatten()))
            .is_some_and(|w| w.mode.as_deref() == Some("shared"));
        let session =
            Session::spawn_prepared(id.clone(), prepared, adapter, self.store.clone(), shared)?;
        self.sessions.lock().unwrap().insert(id.clone(), session);
        if let (Some(token), Some(key)) = (issued_token, &spec.agent_id) {
            self.tokens.lock().unwrap().insert(token, key.clone());
        }
        // Remember this agent's role so `broadcast` can filter by it and
        // `list_agents` can report it.
        if let Some(key) = &spec.agent_id {
            self.roles.lock().unwrap().insert(key.clone(), spec.profile.name.clone());
        }
        // Durable record (best-effort): the agent existed, with its provider +
        // Agent ID + cwd, for history / Phase-6 attribution.
        if let Some(store) = &self.store {
            let row = SessionRow {
                pty_session_id: id.clone(),
                provider: Some(spec.provider.clone()),
                attribution_key: spec.agent_id.clone(),
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
    /// The daemon mints the Agent ID, runs `git worktree`, and persists the
    /// `taime_worktrees` row. Shells out to git — call from a blocking context.
    /// Mint a fresh durable Agent ID, re-rolling on any collision with the
    /// durable collision domain: an existing `taime_worktrees` row (rows are never
    /// pruned — the "durable Agent-ID anchors" — so they are the true domain) OR an
    /// existing `refs/taime/archive/<id>` in the target repo (a reclaimed agent's
    /// work lives ONLY in that ref, so colliding onto it would corrupt attribution).
    /// The archive-ref check is scoped to the resolved repo and only meaningful for
    /// isolated agents in a git repo (shared mode has no archive ref).
    fn mint_agent_id(&self, project_root: &str, isolate: bool) -> String {
        let repo_root = if isolate { crate::worktree::repo_root_of(project_root) } else { None };
        mint_unique_id(|id| {
            let row_taken = self
                .store
                .as_ref()
                .and_then(|s| s.worktree_row(id).ok().flatten())
                .is_some();
            row_taken
                || repo_root.as_deref().is_some_and(|repo| crate::worktree::archive_ref_exists(repo, id))
        })
    }

    pub fn provision_worktree(
        &self,
        project_root: String,
        provider: String,
        isolate: bool,
        task_id: Option<String>,
    ) -> WorktreeInfo {
        let agent_id = self.mint_agent_id(&project_root, isolate);
        let info = crate::worktree::provision(&project_root, &provider, isolate, &agent_id);
        if let Some(store) = &self.store {
            if let Err(e) = store.upsert_worktree(&info, &provider, now_unix()) {
                eprintln!("[taime-daemon] persist worktree {agent_id} failed: {e}");
            }
            // Task membership lands on the worktree row at provision — the
            // durable anchor of the Task partition (NULL ⇒ Uncategorized).
            if task_id.is_some() {
                if let Err(e) = store.set_worktree_task(&info.agent_id, task_id.as_deref()) {
                    eprintln!("[taime-daemon] set worktree task failed: {e}");
                }
            }
        }
        self.touch();
        info
    }

    /// How many of a workspace's agents hold ARCHIVED, unmerged work — a durable
    /// `refs/taime/archive/<id>` snapshot (the reclaimed agent's only copy of its
    /// changes). This is exactly what a HARD `delete_workspace` would destroy, so
    /// the UI shows the count and demands a typed confirm before destroying it.
    pub fn workspace_archived_count(&self, workspace_root: &str) -> usize {
        let Some(store) = &self.store else { return 0 };
        store
            .worktrees_in_workspace(workspace_root)
            .unwrap_or_default()
            .iter()
            .filter(|w| w.archive_ref.is_some())
            .count()
    }

    /// Tear down a workspace (the "delete workspace" flow): kill every live agent
    /// provisioned from `workspace_root`, reclaim their isolated checkouts, drop
    /// their worktree rows + review acks, and delete the workspace's tasks.
    /// Agents/tasks in OTHER workspaces are untouched.
    ///
    /// `destroy_archives` gates the only IRREVERSIBLE part — the durable
    /// `refs/taime/archive/*` snapshots that hold reclaimed agents' unmerged work:
    ///  - `false` (SOFT, the default): non-lossy. A still-on-disk checkout is
    ///    archived BEFORE it is reclaimed, and NO archive ref is dropped — so every
    ///    agent's work survives in git and nothing is silently destroyed. Only the
    ///    disposable checkouts + Taime's rows/acks/tasks go.
    ///  - `true` (HARD, opt-in + typed confirm): also `drop_archive_ref` on each
    ///    archived agent, permanently discarding that unmerged work (git GCs it).
    ///
    /// Shared-mode rows point at the user's real project dir — never fs-delete
    /// those here (the optional folder delete is a separate, guarded step). Returns
    /// `(agents_removed, agents_killed, tasks_deleted)`. Shells out to git — call
    /// from a blocking context.
    pub fn delete_workspace(
        &self,
        workspace_root: &str,
        destroy_archives: bool,
    ) -> (usize, usize, usize) {
        let Some(store) = &self.store else { return (0, 0, 0) };
        let agents = store.worktrees_in_workspace(workspace_root).unwrap_or_default();
        let agent_ids: Vec<String> = agents.iter().map(|w| w.terminal_id.clone()).collect();
        // Kill live sessions for this workspace. Collect pty ids UNDER the lock,
        // then kill — never hold the sessions lock across kill() (lock discipline).
        let pty_ids: Vec<String> = {
            let map = self.sessions.lock().unwrap();
            map.iter()
                .filter(|(_, s)| {
                    s.attribution_key().map(|k| agent_ids.contains(&k)).unwrap_or(false)
                })
                .map(|(id, _)| id.clone())
                .collect()
        };
        let killed = pty_ids.len();
        for id in &pty_ids {
            self.kill(id);
        }
        for wt in &agents {
            if wt.mode.as_deref() == Some("isolated") {
                if destroy_archives && wt.archive_ref.is_some() {
                    // HARD destroy — but ONLY the archived work the user was shown a
                    // count of and typed-confirmed (`archive_ref.is_some()` is
                    // exactly `workspace_archived_count`). Drop the durable ref (the
                    // reclaimed agent's only copy) and remove any checkout.
                    crate::worktree::remove_force(
                        &wt.worktree_path,
                        wt.repo_root.as_deref(),
                        wt.branch.as_deref(),
                    );
                    if let Some(repo) = wt.repo_root.as_deref() {
                        if let Some(ar) = wt.archive_ref.as_deref() {
                            crate::worktree::drop_archive_ref(repo, ar);
                        }
                    }
                } else if wt.reclaimed_at.is_none() {
                    // PRESERVE (the default, AND every agent outside the confirmed
                    // destroy set even on a hard delete): snapshot any still-on-disk
                    // checkout into its archive ref (non-lossy) before reclaiming,
                    // and NEVER drop a ref. So a hard delete destroys exactly the
                    // counted archived work and nothing uncounted. (An archive
                    // failure keeps the checkout; nothing is lost.)
                    self.archive_and_reclaim(wt);
                }
                // else (reclaimed, archive ref preserved): nothing to do.
            }
            let _ = store.delete_review_patch(&wt.terminal_id);
            let _ = store.delete_worktree_row(&wt.terminal_id);
            let _ = store.clear_reviewed(&wt.terminal_id);
        }
        let tasks = store.delete_tasks_for_workspace(workspace_root).unwrap_or(0);
        self.touch();
        (agents.len(), killed, tasks)
    }

    pub fn get(&self, id: &str) -> Option<Session> {
        self.sessions.lock().unwrap().get(id).cloned()
    }

    /// The live session addressed by an Agent ID (the inbox routes to it).
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
            // Close any still-open turn BEFORE injecting (self-guarded no-op
            // when the quiet window already closed it): the receiver flips
            // idle the moment output stops, ~700ms before the quiet close, so
            // an undelimited injection would coalesce the prior turn's work
            // with the delivered prompt's response — misattributing
            // files_touched on the flagship Attribution surface. Mirrors the
            // app's keystroke-submit checkpoint.
            session.checkpoint();
            match session.input(payload.as_bytes()) {
                Ok(_) => {
                    let _ = store.set_message_status(msg.id, "delivered");
                    // Submit on a SEPARATE, delayed write. A long multi-line
                    // message is detected as a bracketed paste, so a newline in the
                    // same burst becomes paste content rather than Enter — the
                    // prompt would sit in the input box unsent. A short delay
                    // breaks the burst so this `\r` reads as a real submit (mirrors
                    // the app's keystroke-delivery path).
                    let submit = session.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(250));
                        let _ = submit.input(b"\r");
                    });
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

    /// The activity graph as JSON: agents (from the recorded sessions) plus the
    /// inter-agent edges (message/request/reply/handoff/assign events) and
    /// contention. Read from the durable store, so it reflects the full history
    /// even with the UI closed.
    /// The activity graph (agents + inter-agent edges + fs contention). When
    /// `workspace_root` is `Some`, every surface is scoped to that workspace's
    /// team — agents not in it, edges touching an out-of-scope agent, and
    /// contention rows that drop below two in-scope writers are all filtered out
    /// (so the Team drawer reflects the active workspace, not every agent the
    /// daemon has run). `None` is the legacy, daemon-wide view.
    pub fn activity_graph_json(&self, workspace_root: Option<&str>) -> String {
        let Some(store) = &self.store else {
            return r#"{"agents":[],"edges":[],"contention":[]}"#.to_string();
        };
        let roster = match workspace_root {
            Some(ws) => store.graph_agents_in_workspace(ws).unwrap_or_default(),
            None => store.graph_agents().unwrap_or_default(),
        };
        // The in-scope id set: edges/contention reference agents by the same
        // COALESCE(attribution_key, pty_session_id) key the roster is keyed on.
        let in_scope: std::collections::HashSet<String> =
            roster.iter().map(|(id, _, _)| id.clone()).collect();
        let scoped = workspace_root.is_some();
        let keep = |id: &str| !scoped || in_scope.contains(id);
        let agents: Vec<serde_json::Value> = roster
            .into_iter()
            .map(|(id, provider, status)| {
                let (branch, mode, member_of) =
                    store.worktree_attrs(&id).ok().flatten().unwrap_or((None, None, None));
                let turns: Vec<serde_json::Value> = store
                    .agent_turns(&id, 100)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(tid, idx, started, ended, files_json)| {
                        let files: Vec<String> = serde_json::from_str(&files_json).unwrap_or_default();
                        serde_json::json!({
                            "id": tid,
                            "turn_index": idx,
                            "started_at": started,
                            "ended_at": ended,
                            "files_touched": files,
                            "start_snapshot": serde_json::Value::Null,
                            "end_snapshot": serde_json::Value::Null,
                        })
                    })
                    .collect();
                serde_json::json!({
                    "agent_id": id,
                    "provider": provider,
                    "status": status,
                    "branch": branch,
                    "mode": mode,
                    "member_of": member_of,
                    "task_id": store.task_of_worktree(&id),
                    "turns": turns,
                })
            })
            .collect();
        let edges: Vec<serde_json::Value> = store
            .activity_edges()
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, source, target)| keep(source) && keep(target))
            .map(|(kind, source, target)| serde_json::json!({ "kind": kind, "source": source, "target": target }))
            .collect();
        let contention: Vec<serde_json::Value> = store
            .fs_contention(200)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(path, terminals)| {
                // Drop out-of-scope writers; a path is only contended if ≥2
                // in-scope agents still touch it.
                let terminals: Vec<String> = terminals.into_iter().filter(|t| keep(t)).collect();
                (terminals.len() > 1)
                    .then(|| serde_json::json!({ "path": path, "terminals": terminals }))
            })
            .collect();
        serde_json::json!({ "agents": agents, "edges": edges, "contention": contention }).to_string()
    }

    /// Resolve an agent's diff context `(cwd, base)`: an isolated worktree diffs
    /// against its `base_sha` (fork point → shows all the agent's changes); a
    /// shared/live agent against HEAD (`None`).
    fn diff_context(&self, agent_id: &str) -> (String, Option<String>) {
        if let Some(store) = &self.store {
            if let Ok(Some((path, base_sha, mode))) = store.worktree(agent_id) {
                let base = if mode == "isolated" { base_sha } else { None };
                return (path, base);
            }
        }
        let cwd = self.session_by_attribution(agent_id).map(|s| s.cwd()).unwrap_or_default();
        (cwd, None)
    }

    /// Where to render an agent's review surfaces from: its live worktree
    /// checkout, or — once the checkout has been reclaimed — its archive (the
    /// cached patch + `refs/taime/archive/*` ref). The diff surfaces dispatch on
    /// this so a reclaimed agent stays fully reviewable and mergeable.
    fn review_source(&self, agent_id: &str) -> ReviewSource {
        if let Some(store) = &self.store {
            if let Ok(Some(w)) = store.worktree_row(agent_id) {
                if w.reclaimed_at.is_some() {
                    // Reclaimed: render from the archive. A clean agent has no
                    // cached patch (nothing was kept) ⇒ an empty diff.
                    return match store.get_review_patch(agent_id) {
                        Ok(Some(patch)) => ReviewSource::Archive {
                            repo_root: w.repo_root.unwrap_or_default(),
                            patch,
                        },
                        _ => ReviewSource::Empty,
                    };
                }
                let base = if w.mode.as_deref() == Some("isolated") { w.base_sha } else { None };
                return ReviewSource::Live { cwd: w.worktree_path, base };
            }
        }
        // No worktree row: a shared / low-level agent — diff its live session cwd.
        let cwd = self.session_by_attribution(agent_id).map(|s| s.cwd()).unwrap_or_default();
        ReviewSource::Live { cwd, base: None }
    }

    /// Whether the agent has a standing review ack — the merge gate's read.
    /// `None` store ⇒ `false`: the gate fails closed without persistence.
    fn has_review_ack(&self, agent_id: &str) -> bool {
        self.store.as_ref().and_then(|s| s.is_reviewed(agent_id).ok()).unwrap_or(false)
    }

    /// Resolve the review surfaces' symbolic merge/revert target to a real
    /// directory. The UI only ever sends `"self"` (revert), `"main"` (merge to
    /// the mainline checkout), or a sibling Agent ID (merge to its worktree) —
    /// anything else is rejected rather than treated as a literal path, so a
    /// caller can never aim `git apply` at an arbitrary directory.
    ///
    /// `"main"` resolves to the agent's recorded `repo_root` (diff paths are
    /// toplevel-relative) falling back to `project_root`; an Agent ID resolves
    /// to that agent's worktree only when both agents share a workspace.
    fn resolve_apply_target(&self, agent_id: &str, cwd: &str, target: &str) -> Result<String, String> {
        if target == "self" {
            // The dir the diff was computed from — the agent's own worktree.
            return Ok(cwd.to_string());
        }
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| format!("persistence unavailable: cannot resolve target '{target}'"))?;
        let src = store.worktree_row(agent_id).ok().flatten();
        if target == "main" {
            let src = src.ok_or("no worktree recorded for this agent: cannot resolve 'main'")?;
            return src
                .repo_root
                .or(src.project_root)
                .ok_or_else(|| "agent has no recorded workspace root".to_string());
        }
        let Some(dst) = store.worktree_row(target).ok().flatten() else {
            return Err(format!("unknown merge target '{target}'"));
        };
        let same_workspace = match (src.and_then(|s| s.project_root), &dst.project_root) {
            (Some(a), Some(b)) => a == *b,
            _ => false,
        };
        if !same_workspace {
            return Err(format!("merge target '{target}' is not in this agent's workspace"));
        }
        // A shared-mode sibling's "worktree" IS the user's real project dir
        // (worktree.rs's fallback) — merging "into agent-b" must never silently
        // write the mainline under another agent's name.
        if dst.mode.as_deref() != Some("isolated") {
            return Err(format!(
                "merge target '{target}' shares the project directory — merge to 'main' instead"
            ));
        }
        Ok(dst.worktree_path)
    }

    /// Assemble the provenance commit message (subject + body + trailer block) for
    /// a merge of `agent_id`'s selected hunks into `target_symbol`. The always-present
    /// core is `Co-authored-by` plus `Taime-Agent-Id` (== `refs/taime/archive/<id>`);
    /// `Taime-Reviewed` records whether the user opted into review (the
    /// autonomy-primary model: review is opt-in, attribution is always-on).
    #[allow(clippy::too_many_arguments)]
    fn build_merge_message(
        &self,
        agent_id: &str,
        target_symbol: &str,
        base: Option<&str>,
        reviewed: bool,
        digest: Option<&str>,
        selected: usize,
        total: usize,
        archive_ref: Option<&str>,
    ) -> String {
        let (provider, task_id) = self
            .store
            .as_ref()
            .and_then(|s| s.worktree_row(agent_id).ok().flatten())
            .map(|w| (w.provider.unwrap_or_default(), w.task_id))
            .unwrap_or_default();
        let turns = self
            .store
            .as_ref()
            .and_then(|s| s.count_agent_turns(agent_id).ok())
            .unwrap_or(0);
        let title = provider_title(&provider);
        let short = short_id(agent_id);
        let scope = if total > 0 && selected >= total { "full" } else { "partial" };
        let mode = if reviewed { "reviewed" } else { "autonomous" };
        let target_disp = if target_symbol == "main" {
            "main".to_string()
        } else {
            format!("agent {}", short_id(target_symbol))
        };
        let mut m = String::new();
        m.push_str(&format!("taime: merge {title} agent {short} → {target_disp}\n\n"));
        m.push_str(&format!("{selected}/{total} hunk(s) merged from agent {short}. {mode}.\n\n"));
        m.push_str(&format!("Co-authored-by: {title} <agent-{agent_id}@taime.local>\n"));
        m.push_str(&format!("Taime-Agent-Id: {agent_id}\n"));
        if let Some(ar) = archive_ref {
            m.push_str(&format!("Taime-Archive-Ref: {ar}\n"));
        }
        if let Some(b) = base.filter(|b| !b.is_empty()) {
            m.push_str(&format!("Taime-Base-Sha: {b}\n"));
        }
        if !provider.is_empty() {
            m.push_str(&format!("Taime-Provider: {provider}\n"));
        }
        if let Some(t) = task_id.as_deref().filter(|t| !t.is_empty()) {
            m.push_str(&format!("Taime-Task-Id: {t}\n"));
        }
        m.push_str(&format!("Taime-Reviewed: {reviewed}\n"));
        // The content fingerprint of the merged change set — proof the merger held
        // what they merged (the integrity floor), present whether or not the user
        // opted into review. `Taime-Reviewed` above records the opt-in itself.
        if let Some(d) = digest {
            m.push_str(&format!("Taime-Patch-Digest: {d}\n"));
        }
        m.push_str(&format!("Taime-Merge-Scope: {scope}\n"));
        m.push_str(&format!("Taime-Hunks: {selected}/{total}\n"));
        if turns > 0 {
            m.push_str(&format!("Taime-Turns: {turns}\n"));
        }
        m
    }

    /// After a `commit_selection*` returns, record the provenance of a committed
    /// merge: write the `refs/notes/taime` git note (the pushable in-repo record)
    /// and append a `taime_merges` ledger row (the queryable backing for the badge
    /// and export). Both best-effort — the commit and trailer already stand, so a
    /// store-less or note-failing daemon still produced a fully attributed commit.
    /// A non-committed `res` (refused / conflicted / stale) passes straight
    /// through. Returns the result augmented with `note_written`.
    #[allow(clippy::too_many_arguments)]
    fn finalize_merge(
        &self,
        res: serde_json::Value,
        agent_id: &str,
        target_symbol: &str,
        target_repo: &str,
        base: Option<&str>,
        archive_ref: Option<&str>,
        digest: Option<&str>,
        reviewed: bool,
        selected: usize,
        total: usize,
        push: bool,
    ) -> String {
        let mut res = res;
        if res["committed"].as_bool() != Some(true) {
            return res.to_string();
        }
        let commit = res["commit"].as_str().unwrap_or("").to_string();
        let files: Vec<String> = res["files"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let scope = if total > 0 && selected >= total { "full" } else { "partial" };
        // Opt-in push of the target branch — best-effort, recorded, never fatal.
        let (pushed, push_error) = if push { git_push(target_repo) } else { (false, None) };
        let (provider, task_id) = self
            .store
            .as_ref()
            .and_then(|s| s.worktree_row(agent_id).ok().flatten())
            .map(|w| (w.provider.unwrap_or_default(), w.task_id))
            .unwrap_or_default();
        let turns = self
            .store
            .as_ref()
            .and_then(|s| s.count_agent_turns(agent_id).ok())
            .unwrap_or(0);
        let now = now_unix();

        // The git note: the full machine-readable provenance record, field-aligned
        // with the trailer + the attribution export.
        let note = serde_json::json!({
            "schema": "taime.provenance/v1",
            "agent_id": agent_id,
            "provider": provider,
            "base_sha": base,
            "archive_ref": archive_ref,
            "task_id": task_id,
            "commit": commit,
            "target": target_symbol,
            "reviewed": reviewed,
            "digest": digest,
            "scope": scope,
            "hunks_selected": selected,
            "hunks_total": total,
            "turns": turns,
            "files": files,
            "pushed": pushed,
            "merged_at": now,
        });
        let note_written =
            write_git_note(target_repo, &commit, &serde_json::to_string(&note).unwrap_or_default());

        if let Some(store) = &self.store {
            let _ = store.put_merge(&crate::store::MergeRecord {
                id: None,
                agent_id: agent_id.to_string(),
                target_repo: target_repo.to_string(),
                target_symbol: target_symbol.to_string(),
                commit_sha: commit.clone(),
                base_sha: base.map(String::from),
                archive_ref: archive_ref.map(String::from),
                digest: digest.map(String::from),
                scope: scope.to_string(),
                hunks_selected: selected as i64,
                hunks_total: total as i64,
                reviewed,
                pushed,
                pr_url: None,
                files,
                created_at_unix: now,
            });
        }
        res["note_written"] = serde_json::json!(note_written);
        res["pushed"] = serde_json::json!(pushed);
        if let Some(e) = push_error {
            res["push_error"] = serde_json::json!(e);
        }
        res.to_string()
    }

    /// The attribution export (gap #2): a portable JSON artifact for `agent_id` —
    /// identity + current change set + recorded merges + turn count. The git
    /// trailer + `refs/notes/taime` note are the in-repo twin; this is the
    /// download / external-tooling form.
    fn export_attribution_json(&self, agent_id: &str) -> String {
        let row = self.store.as_ref().and_then(|s| s.worktree_row(agent_id).ok().flatten());
        let provider = row.as_ref().and_then(|w| w.provider.clone()).unwrap_or_default();
        let base_sha = row.as_ref().and_then(|w| w.base_sha.clone());
        let archive_ref = row.as_ref().and_then(|w| w.archive_ref.clone());
        let task_id = row.as_ref().and_then(|w| w.task_id.clone());
        let turns = self
            .store
            .as_ref()
            .and_then(|s| s.count_agent_turns(agent_id).ok())
            .unwrap_or(0);
        let merges =
            self.store.as_ref().and_then(|s| s.merges_for_agent(agent_id).ok()).unwrap_or_default();
        // The agent's current (pre-merge) change set, from wherever it renders.
        let current_files: Vec<String> = match self.review_source(agent_id) {
            ReviewSource::Live { cwd, base } => {
                let d = crate::diff::terminal_diff(agent_id, &cwd, base.as_deref());
                d["files"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default()
            }
            ReviewSource::Archive { patch, .. } => {
                let d = crate::diff::terminal_diff_archived(agent_id, &patch.diff_blob);
                d["files"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default()
            }
            ReviewSource::Empty => Vec::new(),
        };
        serde_json::json!({
            "schema": "taime.attribution/v1",
            "agent_id": agent_id,
            "provider": provider,
            "base_sha": base_sha,
            "archive_ref": archive_ref,
            "task_id": task_id,
            "turns": turns,
            "current_files": current_files,
            "merges": merges.iter().map(merge_record_json).collect::<Vec<_>>(),
            "exported_at": now_unix(),
        })
        .to_string()
    }

    /// Generic query RPC (Phase 6 route-layer migration): returns a JSON string in
    /// the frontend's shape for `kind`. Shells out to git (diffs) — call from a
    /// blocking context.
    pub fn query(&self, kind: &str, args_json: &str) -> String {
        let a: serde_json::Value =
            serde_json::from_str(args_json).unwrap_or_else(|_| serde_json::json!({}));
        let tk = a.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        match kind {
            "terminal_diff" => match self.review_source(tk) {
                ReviewSource::Live { cwd, base } => {
                    crate::diff::terminal_diff(tk, &cwd, base.as_deref()).to_string()
                }
                ReviewSource::Archive { patch, .. } => {
                    crate::diff::terminal_diff_archived(tk, &patch.diff_blob).to_string()
                }
                ReviewSource::Empty => serde_json::json!({
                    "agent_id": tk, "working_directory": null, "is_git": true,
                    "diff": "", "files_changed": 0, "files": [], "error": null
                })
                .to_string(),
            },
            "file_diffs" => match self.review_source(tk) {
                ReviewSource::Live { cwd, base } => {
                    crate::diff::file_diffs(tk, &cwd, base.as_deref()).to_string()
                }
                ReviewSource::Archive { repo_root, patch } => {
                    crate::diff::file_diffs_archived(tk, &repo_root, &patch.base_sha, &patch.archive_ref)
                        .to_string()
                }
                ReviewSource::Empty => serde_json::json!({ "agent_id": tk, "files": [] }).to_string(),
            },
            "hunked_diff" => match self.review_source(tk) {
                ReviewSource::Live { cwd, base } => {
                    crate::diff::hunked_diff(tk, &cwd, base.as_deref()).to_string()
                }
                ReviewSource::Archive { patch, .. } => {
                    crate::diff::hunked_diff_archived(tk, &patch.base_sha, &patch.diff_blob).to_string()
                }
                ReviewSource::Empty => serde_json::json!({
                    "agent_id": tk, "base": null, "digest": null, "files": []
                })
                .to_string(),
            },
            "apply_selection" => {
                let src = self.review_source(tk);
                let symbol = a.get("target_dir").and_then(|v| v.as_str()).unwrap_or("self");
                let mode = a.get("mode").and_then(|v| v.as_str()).unwrap_or("merge");
                let sel = a.get("selections").cloned().unwrap_or_else(|| serde_json::json!({}));
                let digest = a.get("expected_digest").and_then(|v| v.as_str());
                let err_json = |e: String| {
                    serde_json::json!({
                        "applied": false, "target_dir": symbol, "files": [], "conflicts": [], "error": e
                    })
                    .to_string()
                };
                // The merge gate, enforced where the merge happens — not by UI
                // placement. Agents work autonomously and never auto-merge; this
                // governs only the explicit, user-initiated merge of an agent's
                // work into another tree. Such a merge requires (1) a standing
                // review ack for the agent — the merge surface records it as part
                // of merging, and any new dirty path invalidates it — and (2) the
                // digest of the reviewed hunked_diff, which apply_selection checks
                // against the patch it assembles NOW: the content binding that
                // refuses merging what nobody saw. Together these are an integrity
                // + staleness guard on the merge action, not a gate on autonomous
                // work. A revert discards the agent's own work back to base — the
                // safe direction — so it skips the ack, but ONLY toward "self":
                // aimed anywhere else it is cross-tree destruction, not a revert.
                // An empty selection map is refused outright — "everything,
                // implicitly" is never an explicit selection (the review surfaces
                // always send explicit selections).
                let gate: Result<(), String> = if sel.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                    Err("nothing selected — fetch hunked_diff and select hunks".to_string())
                } else if mode == "revert" && symbol != "self" {
                    Err(format!("revert only applies to the agent's own worktree, not '{symbol}'"))
                } else if mode != "revert" && self.store.is_none() {
                    Err("merge refused: daemon persistence is unavailable, so the review ack that binds a merge can't be recorded".to_string())
                } else if mode != "revert" && !self.has_review_ack(tk) {
                    Err(format!(
                        "merge refused: no standing review ack for '{tk}' — merge through the review surface (it records the ack) or mark it reviewed first"
                    ))
                } else if mode != "revert" && digest.is_none() {
                    Err("merge refused: missing expected_digest (the hunked_diff fingerprint of the reviewed changes)".to_string())
                } else {
                    Ok(())
                };
                match gate {
                    Err(e) => err_json(e),
                    Ok(()) => match src {
                        ReviewSource::Live { cwd, base } => {
                            match self.resolve_apply_target(tk, &cwd, symbol) {
                                Ok(target) => crate::diff::apply_selection(
                                    &cwd, base.as_deref(), &target, mode, &sel, digest,
                                )
                                .to_string(),
                                Err(e) => err_json(e),
                            }
                        }
                        // A reclaimed agent has no checkout to revert into; its work
                        // lives in the archive and can only be merged forward.
                        ReviewSource::Archive { repo_root, patch } => {
                            if mode == "revert" {
                                err_json(
                                    "revert is unavailable: this agent's worktree was reclaimed — its work lives in the archive. Merge it forward instead.".to_string(),
                                )
                            } else {
                                // cwd is unused for 'main'/sibling targets (resolved
                                // from the worktree row, not the gone checkout).
                                match self.resolve_apply_target(tk, "", symbol) {
                                    Ok(target) => crate::diff::apply_selection_archived(
                                        &repo_root,
                                        &patch.archive_ref,
                                        &patch.diff_blob,
                                        &target,
                                        mode,
                                        &sel,
                                        digest,
                                    )
                                    .to_string(),
                                    Err(e) => err_json(e),
                                }
                            }
                        }
                        ReviewSource::Empty => {
                            err_json("nothing to apply: this agent made no changes".to_string())
                        }
                    },
                }
            }
            // Merge an agent's selected hunks into a target AND record them as one
            // provenance commit (gap #1: the merge no longer dead-ends at `git
            // apply`). Gate (approved policy): `expected_digest` is REQUIRED — the
            // integrity floor, proof the merger held the fingerprint of what they
            // merge; a standing review ack is OPTIONAL (autonomy is primary) and
            // its presence is RECORDED in the trailer as reviewed|autonomous.
            // Always forward (main / sibling), never "self".
            "commit_merge" => {
                let src = self.review_source(tk);
                let symbol = a.get("target_dir").and_then(|v| v.as_str()).unwrap_or("main");
                let sel = a.get("selections").cloned().unwrap_or_else(|| serde_json::json!({}));
                let digest = a.get("expected_digest").and_then(|v| v.as_str());
                let push = a.get("push").and_then(|v| v.as_bool()).unwrap_or(false);
                let err_json = |e: String| {
                    serde_json::json!({
                        "committed": false, "applied": false, "target_dir": symbol,
                        "files": [], "conflicts": [], "error": e
                    })
                    .to_string()
                };
                let reviewed = self.has_review_ack(tk);
                let gate: Result<(), String> = if sel.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                    Err("nothing selected — fetch hunked_diff and select hunks".to_string())
                } else if symbol == "self" {
                    Err("commit_merge applies an agent's work forward (main or a sibling), not to itself".to_string())
                } else if digest.is_none() {
                    Err("merge refused: missing expected_digest (the hunked_diff fingerprint of the reviewed changes)".to_string())
                } else {
                    Ok(())
                };
                match gate {
                    Err(e) => err_json(e),
                    Ok(()) => match src {
                        ReviewSource::Empty => {
                            err_json("nothing to merge: this agent made no changes".to_string())
                        }
                        ReviewSource::Live { cwd, base } => {
                            match self.resolve_apply_target(tk, &cwd, symbol) {
                                Err(e) => err_json(e),
                                Ok(target) => {
                                    let (s, t) =
                                        crate::diff::selection_counts_live(&cwd, base.as_deref(), &sel);
                                    // A live agent has no resolvable archive ref yet
                                    // (it materializes at reclaim, under the SAME id —
                                    // `Taime-Agent-Id`). Omit the ref line rather than
                                    // emit one that doesn't dereference, and never
                                    // snapshot a live checkout here: that would set
                                    // `archive_ref` on a live row, which the
                                    // delete-workspace HARD path treats as reclaimed,
                                    // destructible work.
                                    let msg = self.build_merge_message(
                                        tk, symbol, base.as_deref(), reviewed, digest, s, t, None,
                                    );
                                    let res = crate::diff::commit_selection(
                                        &cwd, base.as_deref(), &target, &sel, digest, &msg,
                                    );
                                    self.finalize_merge(
                                        res, tk, symbol, &target, base.as_deref(), None,
                                        digest, reviewed, s, t, push,
                                    )
                                }
                            }
                        }
                        ReviewSource::Archive { repo_root, patch } => {
                            match self.resolve_apply_target(tk, "", symbol) {
                                Err(e) => err_json(e),
                                Ok(target) => {
                                    let (s, t) =
                                        crate::diff::selection_counts_archived(&patch.diff_blob, &sel);
                                    let msg = self.build_merge_message(
                                        tk,
                                        symbol,
                                        Some(&patch.base_sha),
                                        reviewed,
                                        digest,
                                        s,
                                        t,
                                        Some(&patch.archive_ref),
                                    );
                                    let res = crate::diff::commit_selection_archived(
                                        &repo_root,
                                        &patch.archive_ref,
                                        &patch.diff_blob,
                                        &target,
                                        &sel,
                                        digest,
                                        &msg,
                                    );
                                    self.finalize_merge(
                                        res, tk, symbol, &target, Some(&patch.base_sha),
                                        Some(&patch.archive_ref), digest, reviewed, s, t, push,
                                    )
                                }
                            }
                        }
                    },
                }
            }
            // Attribution export (gap #2): a portable JSON artifact for an agent —
            // identity + current change set + recorded merges + turns. The git
            // commit trailer + `refs/notes/taime` note are the in-repo twin.
            "export_attribution" => self.export_attribution_json(tk),
            // The agent's recorded provenance merges (the merged-✓ badge / history).
            "merge_history" => {
                let merges =
                    self.store.as_ref().and_then(|s| s.merges_for_agent(tk).ok()).unwrap_or_default();
                serde_json::json!(merges.iter().map(merge_record_json).collect::<Vec<_>>()).to_string()
            }
            "contention" => self.contention_json(a.get("session").and_then(|v| v.as_str()).unwrap_or("")),
            "workspace_info" => {
                crate::diff::workspace_info(a.get("path").and_then(|v| v.as_str()).unwrap_or("")).to_string()
            }
            "worktree" => self.worktree_json(tk),
            "clear_dirty" => {
                // The user reviewed the diff: reset the agent's fs dirty set so the
                // next FsDirty push starts fresh (Phase 6).
                if let Some(s) = self.session_by_attribution(tk) {
                    s.clear_fs_dirty();
                }
                // A reset dirty set begins a fresh review cycle — drop any standing
                // review ack so subsequent changes re-flag the agent as needs-review
                // (durable review state; the in-memory flag used to just linger).
                if let Some(store) = &self.store {
                    let _ = store.clear_reviewed(tk);
                }
                "true".to_string()
            }
            // Durable review acks (the merge gate's ack). The app records an
            // ack when the user marks an agent reviewed, and hydrates `reviewed`
            // on boot so the ack survives a restart.
            "mark_reviewed" => {
                // Honest result: an ack that wasn't durably recorded must not
                // report success — the merge gate reads the store, so a client
                // proceeding on a phantom "true" would hit a refusal it can't
                // explain (or worse, believe a review was recorded).
                let ok = self
                    .store
                    .as_ref()
                    .map(|store| store.mark_reviewed(tk, now_unix()).is_ok())
                    .unwrap_or(false);
                if ok { "true" } else { "false" }.to_string()
            }
            // Drop a standing ack without touching the watcher's dirty
            // accumulation (unlike clear_dirty): the agent changed more files
            // after the ack, so the app re-flags it as needs-review.
            "clear_reviewed" => {
                if let Some(store) = &self.store {
                    let _ = store.clear_reviewed(tk);
                }
                "true".to_string()
            }
            "reviewed" => {
                let ids =
                    self.store.as_ref().and_then(|s| s.reviewed_agents().ok()).unwrap_or_default();
                serde_json::json!(ids).to_string()
            }
            // Worktree retention (archive-then-reclaim): the cleanup panel's read
            // (reclaimable, not-live agents) + the explicit "clean up now" action.
            // Reclaim archives first (non-lossy) — the agent stays fully reviewable
            // and mergeable from the archive afterwards.
            "reclaimable" => self.reclaimable_json(),
            "reclaim_agent" => {
                let ok = self.reclaim_agent(tk);
                serde_json::json!({ "ok": ok, "agent_id": tk }).to_string()
            }
            // Workspace teardown: stop the workspace's agents + delete its tasks
            // (the "delete workspace" flow). Folder deletion is a separate,
            // typed-confirmation step in the app (delete_directory command).
            "workspace_archived_count" => {
                let root = a.get("workspace_root").and_then(|v| v.as_str()).unwrap_or("");
                serde_json::json!({ "count": self.workspace_archived_count(root) }).to_string()
            }
            "workspace_delete" => {
                let root = a.get("workspace_root").and_then(|v| v.as_str()).unwrap_or("");
                if root.is_empty() {
                    serde_json::json!({ "error": "workspace_root required" }).to_string()
                } else {
                    // Destroying the durable archive refs is opt-in only (the UI
                    // demands a typed confirm first); default is the soft, non-lossy
                    // teardown that preserves them.
                    let destroy_archives =
                        a.get("destroy_archives").and_then(|v| v.as_bool()).unwrap_or(false);
                    let (agents, killed, tasks) = self.delete_workspace(root, destroy_archives);
                    serde_json::json!({ "ok": true, "agents": agents, "killed": killed, "tasks": tasks })
                        .to_string()
                }
            }
            "attribution" => self.attribution_json(tk),
            "agents" => self.agents_json(),
            "providers" => Self::providers_json(),
            "profiles" => crate::profiles::ProfileStore::load().infos_json(),
            "schedules" => self.schedules_json(),
            "schedule_add" => {
                let res = self.create_schedule(
                    a.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                    a.get("schedule").and_then(|v| v.as_str()).unwrap_or(""),
                    a.get("agent_profile").and_then(|v| v.as_str()).unwrap_or("default"),
                    a.get("provider").and_then(|v| v.as_str()).unwrap_or("claude_code"),
                    a.get("prompt").and_then(|v| v.as_str()).unwrap_or(""),
                    a.get("script").and_then(|v| v.as_str()).map(String::from),
                    a.get("workspace_root").and_then(|v| v.as_str()).map(String::from),
                    a.get("task_mode").and_then(|v| v.as_str()).map(String::from),
                    a.get("task_id").and_then(|v| v.as_str()).map(String::from),
                );
                match res {
                    Ok(()) => r#"{"ok":true}"#.to_string(),
                    Err(e) => serde_json::json!({ "error": e }).to_string(),
                }
            }
            "schedule_run" => {
                match self.run_schedule(a.get("name").and_then(|v| v.as_str()).unwrap_or("")) {
                    Ok(()) => r#"{"ok":true}"#.to_string(),
                    Err(e) => serde_json::json!({ "error": e }).to_string(),
                }
            }
            "schedule_toggle" => {
                let _ = self.set_schedule_enabled(
                    a.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                    a.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
                );
                r#"{"ok":true}"#.to_string()
            }
            "schedule_delete" => {
                let _ = self.delete_schedule(a.get("name").and_then(|v| v.as_str()).unwrap_or(""));
                r#"{"ok":true}"#.to_string()
            }
            "workflows" => self.workflows_json(),
            "workflow_create" => {
                // App-authored workflow (source "user"): same validate+persist path
                // as the MCP create_workflow tool. Validation failures are a
                // structured ok:false (never a wire error) so the dialog can show
                // the message inline.
                let def = a.get("definition").and_then(|v| v.as_str()).unwrap_or("");
                match self.create_workflow(def, "user") {
                    Ok(name) => serde_json::json!({ "ok": true, "name": name }).to_string(),
                    Err(e) => serde_json::json!({ "ok": false, "error": e }).to_string(),
                }
            }
            "workflow_run" => {
                let name = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let root = a.get("project_root").and_then(|v| v.as_str()).map(String::from);
                let task = a.get("task_id").and_then(|v| v.as_str()).map(String::from);
                match self.run_workflow(name, root, None, task) {
                    Ok(run_id) => serde_json::json!({ "run_id": run_id }).to_string(),
                    Err(e) => serde_json::json!({ "error": e }).to_string(),
                }
            }
            "workflow_run_status" => {
                self.workflow_run_status_json(a.get("run_id").and_then(|v| v.as_str()).unwrap_or(""))
            }
            "workflow_cancel" => {
                let run_id = a.get("run_id").and_then(|v| v.as_str()).unwrap_or("");
                match self.cancel_workflow_run(run_id) {
                    Ok(cancelled) => serde_json::json!({ "ok": true, "cancelled": cancelled }).to_string(),
                    Err(e) => serde_json::json!({ "error": e }).to_string(),
                }
            }
            "workflow_delete" => {
                let _ = self.delete_workflow(a.get("name").and_then(|v| v.as_str()).unwrap_or(""));
                r#"{"ok":true}"#.to_string()
            }
            // ---- Tasks (v9): workspace-scoped intent grouping. Membership is
            // ---- the nullable task_id on durable rows; rollups are derived. ----
            "tasks" => self.tasks_json(
                a.get("workspace_root").and_then(|v| v.as_str()).unwrap_or(""),
                a.get("include_archived").and_then(|v| v.as_bool()).unwrap_or(false),
            ),
            "task_create" => {
                let root = a.get("workspace_root").and_then(|v| v.as_str()).unwrap_or("");
                let title = a.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let description = a.get("description").and_then(|v| v.as_str()).unwrap_or("");
                match self.create_task(root, title, description) {
                    Ok(json) => json,
                    Err(e) => serde_json::json!({ "error": e }).to_string(),
                }
            }
            "task_update" => {
                let id = a.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let res = self.update_task(
                    id,
                    a.get("title").and_then(|v| v.as_str()),
                    a.get("description").and_then(|v| v.as_str()),
                    a.get("status").and_then(|v| v.as_str()),
                );
                match res {
                    Ok(()) => r#"{"ok":true}"#.to_string(),
                    Err(e) => serde_json::json!({ "error": e }).to_string(),
                }
            }
            "task_delete" => {
                // Delete demotes members to Uncategorized (store-side transaction);
                // never kills runtimes or touches attribution.
                let id = a.get("id").and_then(|v| v.as_str()).unwrap_or("");
                match self.store.as_ref().map(|s| s.delete_task(id)) {
                    Some(Ok(())) => r#"{"ok":true}"#.to_string(),
                    Some(Err(e)) => serde_json::json!({ "error": e.to_string() }).to_string(),
                    None => r#"{"error":"persistence disabled"}"#.to_string(),
                }
            }
            "task_assign" => {
                let agent = a.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
                let task = a.get("task_id").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
                match self.assign_task(agent, task) {
                    Ok(()) => r#"{"ok":true}"#.to_string(),
                    Err(e) => serde_json::json!({ "error": e }).to_string(),
                }
            }
            "task_detail" => self.task_detail_json(a.get("id").and_then(|v| v.as_str()).unwrap_or("")),
            "graph" => {
                // Empty/absent workspace_root ⇒ daemon-wide (legacy / agent-roster
                // lookups); a real root scopes the graph to that workspace's team.
                let ws = a
                    .get("workspace_root")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty());
                self.activity_graph_json(ws)
            }
            other => serde_json::json!({ "error": format!("unknown query {other}") }).to_string(),
        }
    }

    /// Live agents whose workspace matches `session`, each with its resolved root.
    /// An **exact `project_root`** match wins when any agent's root equals
    /// `session` (so a unique full-root query never merges two same-basename
    /// workspaces); otherwise fall back to a basename match (the app's display-name
    /// query). Worktree `session_name` is no longer populated, so membership is
    /// derived from the live sessions, not the (always-NULL) column.
    fn workspace_members(&self, session: &str) -> Vec<(SessionSummary, String)> {
        let entries: Vec<(SessionSummary, String)> = self
            .list()
            .into_iter()
            .map(|s| {
                let root = self.session_root_of(&s);
                (s, root)
            })
            .collect();
        let exact = entries.iter().any(|(_, root)| root == session);
        entries
            .into_iter()
            .filter(|(_, root)| if exact { root == session } else { basename(root) == session })
            .collect()
    }

    /// Files changed in ≥2 of a workspace's agent worktrees (contention).
    fn contention_json(&self, session: &str) -> String {
        let members: Vec<String> = self
            .workspace_members(session)
            .into_iter()
            .filter_map(|(s, _)| s.agent_id)
            .collect();
        let mut map: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for tid in &members {
            let (cwd, base) = self.diff_context(tid);
            let v = crate::diff::terminal_diff(tid, &cwd, base.as_deref());
            if let Some(files) = v.get("files").and_then(|f| f.as_array()) {
                for f in files.iter().filter_map(|f| f.as_str()) {
                    map.entry(f.to_string()).or_default().insert(tid.clone());
                }
            }
        }
        let contention: Vec<serde_json::Value> = map
            .into_iter()
            .filter(|(_, t)| t.len() > 1)
            .map(|(path, terms)| serde_json::json!({ "path": path, "terminals": terms.into_iter().collect::<Vec<_>>() }))
            .collect();
        serde_json::json!(contention).to_string()
    }

    /// The 4 supported providers with an accurate `installed` flag (binary
    /// resolvable in the daemon's env), so the launcher doesn't offer a CLI that
    /// will fail at spawn.
    fn providers_json() -> String {
        let providers = [
            ("claude_code", "claude"),
            ("codex", "codex"),
            ("gemini_cli", "gemini"),
            ("grok_cli", "grok"),
        ];
        let list: Vec<serde_json::Value> = providers
            .iter()
            .map(|(name, bin)| {
                serde_json::json!({ "name": name, "binary": bin, "installed": binary_installed(bin) })
            })
            .collect();
        serde_json::json!(list).to_string()
    }

    fn worktree_json(&self, tk: &str) -> String {
        if let Some(store) = &self.store {
            if let Ok(Some(w)) = store.worktree_row(tk) {
                // Full `WorktreeInfo` shape (the app's `getWorktree` reads `mode`,
                // `branch`, `project_root`, …; a reduced shape silently dropped the
                // branch chip and repo root in the diff view).
                return serde_json::json!({
                    "agent_id": w.terminal_id,
                    "project_root": w.project_root,
                    "repo_root": w.repo_root,
                    "worktree_path": w.worktree_path,
                    "branch": w.branch,
                    "base_sha": w.base_sha,
                    "mode": w.mode,
                    "provider": w.provider,
                    "member_of": w.member_of,
                    "task_id": w.task_id,
                    // Archive-then-reclaim state: when `reclaimed` is true the
                    // physical checkout is gone and the diff renders from the
                    // archive — the UI shows a "reviewing from archive" hint.
                    "archived_at": w.archived_at,
                    "reclaimed_at": w.reclaimed_at,
                    "reclaimed": w.reclaimed_at.is_some(),
                })
                .to_string();
            }
        }
        "null".to_string()
    }

    // ---- Tasks (v9): grouping/lifecycle/rollups — never raw attribution ----

    /// The Task lifecycle. `in_review` may be *suggested* by derived state, but
    /// the user confirms transitions (the manager only validates the enum).
    const TASK_STATUSES: [&'static str; 4] = ["open", "in_review", "done", "archived"];

    fn task_json(t: &crate::store::TaskRow, agent_count: i64) -> serde_json::Value {
        serde_json::json!({
            "id": t.id,
            "workspace_root": t.workspace_root,
            "title": t.title,
            "description": t.description,
            "status": t.status,
            "created_at": t.created_at,
            "updated_at": t.updated_at,
            "archived_at": t.archived_at,
            "agent_count": agent_count,
        })
    }

    /// Create a task (status `open`); returns its JSON.
    pub fn create_task(
        &self,
        workspace_root: &str,
        title: &str,
        description: &str,
    ) -> Result<String, String> {
        let store = self.store.as_ref().ok_or("persistence disabled")?;
        let (root, title) = (workspace_root.trim(), title.trim());
        if root.is_empty() {
            return Err("workspace_root is required".into());
        }
        if title.is_empty() {
            return Err("a task title is required".into());
        }
        let id = format!("task-{}", &gen_id()[..8]);
        store.create_task(&id, root, title, description, now_unix()).map_err(|e| e.to_string())?;
        let task = store
            .get_task(&id)
            .map_err(|e| e.to_string())?
            .ok_or("task not found after create")?;
        Ok(Self::task_json(&task, 0).to_string())
    }

    /// Update title/description/status. Status is validated against the
    /// lifecycle enum; `archived` stamps `archived_at` (store-side).
    pub fn update_task(
        &self,
        id: &str,
        title: Option<&str>,
        description: Option<&str>,
        status: Option<&str>,
    ) -> Result<(), String> {
        let store = self.store.as_ref().ok_or("persistence disabled")?;
        if let Some(s) = status {
            if !Self::TASK_STATUSES.contains(&s) {
                return Err(format!(
                    "invalid status '{s}' (expected open | in_review | done | archived)"
                ));
            }
        }
        if store.update_task(id, title, description, status, now_unix()).map_err(|e| e.to_string())? {
            Ok(())
        } else {
            Err(format!("unknown task '{id}'"))
        }
    }

    /// An agent's current task membership (`None` ⇒ Uncategorized).
    pub fn task_of_agent(&self, agent_id: &str) -> Option<String> {
        self.store.as_ref().and_then(|s| s.task_of_worktree(agent_id))
    }

    /// Assign (or unassign with `None`) an agent to a task. The membership rule:
    /// at most one task per agent; reassignment just rewrites the pointer. Tasks
    /// are workspace-scoped, so the agent's worktree must come from the task's
    /// workspace — a cross-workspace pointer would corrupt the partition.
    pub fn assign_task(&self, agent_id: &str, task_id: Option<&str>) -> Result<(), String> {
        let store = self.store.as_ref().ok_or("persistence disabled")?;
        if let Some(tid) = task_id {
            let task = store
                .get_task(tid)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("unknown task '{tid}'"))?;
            let agent_root = store.worktree_project_root(agent_id).ok().flatten();
            if agent_root.as_deref() != Some(task.workspace_root.as_str()) {
                return Err("task belongs to a different workspace".into());
            }
        }
        if store.set_worktree_task(agent_id, task_id).map_err(|e| e.to_string())? {
            Ok(())
        } else {
            Err(format!("unknown agent '{agent_id}'"))
        }
    }

    /// Tasks of a workspace with member-agent counts (the sidebar list).
    fn tasks_json(&self, workspace_root: &str, include_archived: bool) -> String {
        let Some(store) = &self.store else { return "[]".to_string() };
        let counts: std::collections::HashMap<String, i64> =
            store.task_agent_counts(workspace_root).unwrap_or_default().into_iter().collect();
        let list: Vec<serde_json::Value> = store
            .list_tasks(workspace_root, include_archived)
            .unwrap_or_default()
            .iter()
            .map(|t| Self::task_json(t, counts.get(&t.id).copied().unwrap_or(0)))
            .collect();
        serde_json::json!(list).to_string()
    }

    /// One task with its member agents (worktree row + live status + fs-dirty
    /// rollup) and attached workflow runs — the Task Review surface. Rollups are
    /// derived by join here, never stored.
    fn task_detail_json(&self, id: &str) -> String {
        let Some(store) = &self.store else { return "null".to_string() };
        let Some(task) = store.get_task(id).ok().flatten() else { return "null".to_string() };
        let agents: Vec<serde_json::Value> = store
            .agents_for_task(id)
            .unwrap_or_default()
            .iter()
            .map(|w| {
                // Role/profile from the in-memory roles map (separate lock scope,
                // released before we touch the sessions lock below).
                let role = self.roles.lock().unwrap().get(&w.terminal_id).cloned();
                let live = self.session_by_attribution(&w.terminal_id);
                let (status, alive, dirty) = live
                    .map(|s| {
                        let sum = s.summary();
                        (sum.status, sum.alive, s.fs_dirty_paths())
                    })
                    .unwrap_or((None, false, Vec::new()));
                serde_json::json!({
                    "agent_id": w.terminal_id,
                    "provider": w.provider,
                    "branch": w.branch,
                    "mode": w.mode,
                    "worktree_path": w.worktree_path,
                    "status": status,
                    "alive": alive,
                    "dirty_count": dirty.len(),
                    "dirty_paths": dirty,
                    "role": role,
                })
            })
            .collect();
        let runs: Vec<serde_json::Value> = store
            .runs_for_task(id)
            .unwrap_or_default()
            .iter()
            .map(|r| self.run_summary_json(r))
            .collect();
        serde_json::json!({
            "task": Self::task_json(&task, agents.len() as i64),
            "agents": agents,
            "runs": runs,
        })
        .to_string()
    }

    /// Per-file authorship for the diff under review (`tk`): the workspace's team
    /// and, for each touched file, its contributors + last author — derived from
    /// the durable fs-activity log (Phase 6). In the default worktree-per-agent
    /// mode the team is just the reviewed agent; in shared mode it's everyone in
    /// the workspace, so a file edited by two agents shows both.
    fn attribution_json(&self, tk: &str) -> String {
        let Some(store) = &self.store else { return r#"{"team":[],"files":{}}"#.to_string() };
        let sums: Vec<SessionSummary> = self.list();
        // Team = live agents sharing the reviewed agent's workspace; fall back
        // to the reviewed agent alone (e.g. it already exited).
        let target_root = sums
            .iter()
            .find(|s| s.agent_id.as_deref() == Some(tk))
            .map(|s| self.session_root_of(s));
        let team_members: Vec<SessionSummary> = match &target_root {
            Some(root) => sums.into_iter().filter(|s| self.session_root_of(s) == *root).collect(),
            None => sums.into_iter().filter(|s| s.agent_id.as_deref() == Some(tk)).collect(),
        };
        let provider_of = |key: &str| -> Option<String> {
            team_members
                .iter()
                .find(|s| s.agent_id.as_deref() == Some(key))
                .and_then(|s| s.provider.clone())
        };

        let mut team = Vec::new();
        let mut keys: Vec<String> = Vec::new();
        for s in &team_members {
            let key = s.agent_id.clone().unwrap_or_else(|| s.id.clone());
            let (_, mode, member_of) =
                store.worktree_attrs(&key).ok().flatten().unwrap_or((None, None, None));
            team.push(serde_json::json!({
                "agent_id": key, "provider": s.provider, "mode": mode, "member_of": member_of,
            }));
            keys.push(key);
        }
        if keys.is_empty() {
            keys.push(tk.to_string());
            team.push(serde_json::json!({
                "agent_id": tk, "provider": serde_json::Value::Null,
                "mode": serde_json::Value::Null, "member_of": serde_json::Value::Null,
            }));
        }

        // Files: union of the team's fs touches; contributors + last (max ts).
        struct Acc {
            contributors: BTreeSet<String>,
            last: String,
            last_ts: u64,
        }
        let mut files: BTreeMap<String, Acc> = BTreeMap::new();
        for key in &keys {
            for (path, ts) in store.fs_path_touches(key).unwrap_or_default() {
                let e = files.entry(path).or_insert_with(|| Acc {
                    contributors: BTreeSet::new(),
                    last: key.clone(),
                    last_ts: 0,
                });
                e.contributors.insert(key.clone());
                if ts >= e.last_ts {
                    e.last_ts = ts;
                    e.last = key.clone();
                }
            }
        }
        let contributor = |key: &str| -> serde_json::Value {
            serde_json::json!({
                "agent_id": key,
                "provider": provider_of(key),
                "turn_index": 0,
                "ended_at": serde_json::Value::Null,
            })
        };
        let files_json: serde_json::Map<String, serde_json::Value> = files
            .into_iter()
            .map(|(path, acc)| {
                let contributors: Vec<serde_json::Value> =
                    acc.contributors.iter().map(|k| contributor(k)).collect();
                (
                    path,
                    serde_json::json!({ "last": contributor(&acc.last), "contributors": contributors }),
                )
            })
            .collect();

        serde_json::json!({ "team": team, "files": files_json }).to_string()
    }

    /// The workspace a live agent belongs to: its worktree's `project_root` when
    /// provisioned, else its cwd. Live data is the source of truth, so the agent
    /// list reflects what's actually running.
    fn session_root_of(&self, sum: &SessionSummary) -> String {
        if let (Some(store), Some(key)) = (&self.store, sum.agent_id.as_ref()) {
            if let Ok(Some(root)) = store.worktree_project_root(key) {
                return root;
            }
        }
        sum.cwd.clone()
    }

    /// Live agents as flat rows (the `agents` query). Identity is the Agent ID
    /// (pty session id fallback for a pre-provision low-level spawn); the app
    /// groups by `workspace_root`. Replaces the retired tmux-shaped `sessions`/
    /// `session_detail` surface.
    fn agents_json(&self) -> String {
        let out: Vec<serde_json::Value> = self
            .list()
            .into_iter()
            .map(|s| {
                let workspace_root = self.session_root_of(&s);
                serde_json::json!({
                    "agent_id": s.agent_id.clone().unwrap_or_else(|| s.id.clone()),
                    "workspace_root": workspace_root,
                    "provider": s.provider,
                    "status": s.status,
                    "alive": s.alive,
                    "task_id": s.task_id,
                    "role": s.role,
                })
            })
            .collect();
        serde_json::json!(out).to_string()
    }

    /// Broadcast a message to every live agent except the sender — optionally
    /// only those whose role (profile name) matches `role`. Returns the count
    /// enqueued.
    pub fn broadcast(&self, sender: &str, body: &str, role: Option<&str>) -> usize {
        // Two sequential lock scopes, never nested (see the lock-discipline note
        // on `Manager`) — holding `roles` across `sessions` was a latent
        // inversion waiting for any future sessions→roles path to deadlock it.
        let candidates: Vec<String> = self
            .sessions
            .lock()
            .unwrap()
            .values()
            .filter_map(|s| s.attribution_key())
            .filter(|k| k != sender)
            .collect();
        let receivers: Vec<String> = {
            let roles = self.roles.lock().unwrap();
            candidates
                .into_iter()
                .filter(|k| match role {
                    Some(want) => roles.get(k).map(|r| r == want).unwrap_or(false),
                    None => true,
                })
                .collect()
        };
        let mut n = 0;
        for r in receivers {
            if self.enqueue_message(sender.to_string(), r.clone(), body.to_string()).is_ok() {
                self.record_edge("message", sender, &r);
                n += 1;
            }
        }
        n
    }

    /// Send a message that expects a reply: stamp a fresh interaction id, record
    /// the open correlation, and deliver the body to `to` annotated with the id
    /// and how to answer. Returns the interaction id.
    pub fn request(&self, from: &str, to: &str, body: &str) -> Result<String, String> {
        let interaction_id = gen_id()[..16].to_string();
        if let Some(store) = &self.store {
            store
                .interaction_open(&interaction_id, from, to, body, now_unix())
                .map_err(|e| format!("request: {e}"))?;
        }
        let annotated = format!(
            "[interaction {interaction_id}] {body}\n(reply with the `reply` tool: \
             interaction_id=\"{interaction_id}\")"
        );
        self.enqueue_message(from.to_string(), to.to_string(), annotated)?;
        self.record_edge("request", from, to);
        Ok(interaction_id)
    }

    /// Answer an open interaction: validate the caller is the addressed responder,
    /// close the correlation, and deliver the reply back to the requester (stamped
    /// with the interaction id).
    pub fn reply(&self, from: &str, interaction_id: &str, body: &str) -> Result<i64, String> {
        let store = self.store.as_ref().ok_or_else(|| "persistence disabled".to_string())?;
        let (requester, responder, status) = store
            .interaction_parties(interaction_id)
            .map_err(|e| format!("reply: {e}"))?
            .ok_or_else(|| format!("reply: unknown interaction '{interaction_id}'"))?;
        if from != responder {
            return Err("reply: you are not the addressed responder".to_string());
        }
        if status != "pending" {
            return Err("reply: interaction already answered".to_string());
        }
        store.interaction_answer(interaction_id, body).map_err(|e| format!("reply: {e}"))?;
        let annotated = format!("[interaction {interaction_id} reply] {body}");
        let mid = self.enqueue_message(from.to_string(), requester.clone(), annotated)?;
        self.record_edge("reply", from, &requester);
        Ok(mid)
    }

    /// Record an inter-agent edge in the durable activity log (graph substrate):
    /// `message`/`request`/`reply`/`handoff`/`assign`. Best-effort.
    pub fn record_edge(&self, kind: &str, from: &str, to: &str) {
        if let Some(store) = &self.store {
            let _ = store.record_activity_edge(&gen_id(), kind, from, to, now_unix());
        }
    }

    /// The role (profile name) an agent was spawned under, if known.
    pub fn role_of(&self, key: &str) -> Option<String> {
        self.roles.lock().unwrap().get(key).cloned()
    }

    /// Post to the shared blackboard (last writer wins). `author` is the caller.
    pub fn blackboard_set(&self, key: &str, value: &str, author: &str) -> Result<(), String> {
        let store = self.store.as_ref().ok_or_else(|| "persistence disabled".to_string())?;
        store.blackboard_set(key, value, author, now_unix()).map_err(|e| format!("share: {e}"))
    }

    /// Read the shared blackboard: `(value, author, updated_at)` or None.
    pub fn blackboard_get(
        &self,
        key: &str,
    ) -> Result<Option<(String, Option<String>, u64)>, String> {
        let store = self.store.as_ref().ok_or_else(|| "persistence disabled".to_string())?;
        store.blackboard_get(key).map_err(|e| format!("get: {e}"))
    }

    // ---- Schedules (cron-triggered unattended agent runs) ----

    /// `~/.taime/schedules/` — where schedule `.md` files live.
    fn schedules_dir() -> std::path::PathBuf {
        dirs::home_dir().unwrap_or_default().join(".taime").join("schedules")
    }

    /// At startup: ingest any `~/.taime/schedules/*.md` files, then recompute
    /// `next_run` for every enabled schedule (forward; a down daemon skips missed
    /// runs rather than backfilling).
    pub fn load_schedules_on_start(&self) {
        let Some(store) = &self.store else { return };
        if let Ok(entries) = std::fs::read_dir(Self::schedules_dir()) {
            for e in entries.flatten() {
                let path = e.path();
                if path.extension().and_then(|x| x.to_str()) != Some("md") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else { continue };
                match schedules::parse_schedule(&text) {
                    Ok(def) => {
                        let _ = store.upsert_schedule(&Self::row_from_def(
                            &def,
                            &path.to_string_lossy(),
                            schedules::next_run_unix(&def.schedule),
                        ));
                    }
                    Err(err) => eprintln!("[taime-daemon] bad schedule {path:?}: {err}"),
                }
            }
        }
        for row in store.list_schedules().unwrap_or_default() {
            if row.enabled {
                let _ = store.set_schedule_next(&row.name, schedules::next_run_unix(&row.schedule));
            }
        }
    }

    fn row_from_def(def: &ScheduleDef, file_path: &str, next_run: Option<u64>) -> ScheduleRow {
        ScheduleRow {
            name: def.name.clone(),
            file_path: file_path.to_string(),
            schedule: def.schedule.clone(),
            agent_profile: def.agent_profile.clone(),
            provider: def.provider.clone(),
            script: def.script.clone(),
            prompt: Some(def.prompt.clone()),
            last_run: None,
            next_run,
            enabled: true,
            workspace_root: def.workspace_root.clone(),
            task_mode: def.task_mode.clone(),
            task_id: def.task_id.clone(),
            shared: def.shared,
        }
    }

    /// Create or replace a schedule from the UI: validate the cron, write its `.md`
    /// to `~/.taime/schedules/`, and store the row.
    #[allow(clippy::too_many_arguments)]
    pub fn create_schedule(
        &self,
        name: &str,
        schedule: &str,
        agent_profile: &str,
        provider: &str,
        prompt: &str,
        script: Option<String>,
        workspace_root: Option<String>,
        task_mode: Option<String>,
        task_id: Option<String>,
    ) -> Result<(), String> {
        let store = self.store.as_ref().ok_or("persistence disabled")?;
        let name = name.trim();
        if name.is_empty() {
            return Err("schedule name is required".into());
        }
        let next = schedules::next_run_unix(schedule)
            .ok_or_else(|| format!("invalid cron schedule: {schedule:?}"))?;
        if prompt.trim().is_empty() {
            return Err("a prompt is required".into());
        }
        // Task targeting is workspace-scoped: any task behavior needs a workspace,
        // and "fixed" needs an existing task in THAT workspace. Schedules must
        // never implicitly create unlimited tasks — per_run is explicit opt-in.
        let workspace_root = workspace_root.filter(|s| !s.trim().is_empty());
        let task_mode = task_mode.filter(|s| !s.trim().is_empty());
        let task_id = task_id.filter(|s| !s.trim().is_empty());
        match task_mode.as_deref() {
            None => {}
            Some("per_run") => {
                if workspace_root.is_none() {
                    return Err("task per run requires a workspace".into());
                }
            }
            Some("fixed") => {
                let root =
                    workspace_root.as_deref().ok_or("a task target requires a workspace")?;
                let tid = task_id.as_deref().ok_or("task_mode 'fixed' requires task_id")?;
                let task = store
                    .get_task(tid)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("unknown task '{tid}'"))?;
                if task.workspace_root != root {
                    return Err("task belongs to a different workspace".into());
                }
            }
            Some(other) => return Err(format!("invalid task_mode '{other}'")),
        }
        let def = ScheduleDef {
            name: name.to_string(),
            schedule: schedule.to_string(),
            agent_profile: agent_profile.to_string(),
            provider: provider.to_string(),
            script: script.filter(|s| !s.trim().is_empty()),
            prompt: prompt.to_string(),
            workspace_root,
            task_mode,
            task_id,
            // UI-created schedules default to isolated (reviewable) fires; shared
            // is opt-in via the `.md` front-matter only (no RPC surface yet).
            shared: false,
        };
        let dir = Self::schedules_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("create schedules dir: {e}"))?;
        let safe: String = name
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
            .collect();
        let path = dir.join(format!("{safe}.md"));
        std::fs::write(&path, schedules::to_markdown(&def))
            .map_err(|e| format!("write schedule: {e}"))?;
        store
            .upsert_schedule(&Self::row_from_def(&def, &path.to_string_lossy(), Some(next)))
            .map_err(|e| format!("store schedule: {e}"))
    }

    /// The `(attribution key, spawn cwd)` for a headless fire. With a
    /// `workspace_root` it provisions a worktree and runs in that worktree's path —
    /// for an ISOLATED fire the private checkout (NOT `root`, the user's real
    /// tree); for a shared fire `worktree_path == root`. Without a workspace it
    /// runs in the daemon's cwd under a synthetic `sched-` key. Extracted so the
    /// cwd selection is unit-testable without a live spawn (the isolated-vs-shared
    /// cwd is the difference between reviewable work and silently mutating the
    /// user's tree).
    fn headless_target(
        &self,
        workspace_root: Option<String>,
        provider: &str,
        isolate: bool,
        task_id: Option<String>,
    ) -> (String, Option<String>) {
        match workspace_root.filter(|r| !r.trim().is_empty()) {
            Some(root) => {
                let info = self.provision_worktree(root, provider.to_string(), isolate, task_id);
                (info.agent_id, Some(info.worktree_path))
            }
            None => (format!("sched-{}", &gen_id()[..8]), None),
        }
    }

    /// Spawn a headless agent under `profile`/`provider` and deliver `prompt` when
    /// it next goes idle (the shared Schedule/Workflow fire path). With a
    /// `workspace_root` the agent is provisioned a worktree there (ISOLATED by
    /// default — its diff surfaces in Review like any other agent and nothing
    /// lands in the user's tree unattended; `isolate=false` is the schedule's
    /// `shared:` opt-in) so attribution AND Task membership land on the durable
    /// anchor; without one it runs in the daemon's cwd under a synthetic `sched-`
    /// key.
    pub fn fire_headless(
        &self,
        profile: &str,
        provider: &str,
        workspace_root: Option<String>,
        prompt: &str,
        task_id: Option<String>,
        isolate: bool,
    ) -> Result<String, String> {
        let (key, cwd) = self.headless_target(workspace_root, provider, isolate, task_id);
        let spec = AgentSpawnSpec {
            provider: provider.to_string(),
            profile: AgentProfile { name: profile.to_string(), ..Default::default() },
            cwd,
            rows: 24,
            cols: 80,
            agent_id: Some(key.clone()),
            seed_prompt: None,
            env: vec![],
            inject_orchestration: false,
        };
        let id = self.spawn_agent(spec)?;
        // Seed the prompt: delivered when the agent first goes idle (deliver_pending).
        let _ = self.enqueue_message("schedule".to_string(), key, prompt.to_string());
        Ok(id)
    }

    /// Resolve a schedule's task target at fire time. `fixed` requires the task
    /// to still exist, in THIS schedule's workspace, and not be archived —
    /// anything else degrades the fire to Uncategorized, loudly (the `.md`
    /// ingest path has no create-time validation, and a target can be archived/
    /// deleted/edited after creation). `per_run` creates a fresh task titled
    /// from the schedule + fire date (explicit opt-in, never default). Returns
    /// `(task_id, created_this_fire)` so a failed fire can roll back a per-run
    /// task instead of leaking one per cron tick.
    fn schedule_task_target(&self, row: &ScheduleRow) -> (Option<String>, bool) {
        let Some(root) = row.workspace_root.as_deref().filter(|r| !r.trim().is_empty()) else {
            return (None, false);
        };
        let Some(store) = self.store.as_ref() else { return (None, false) };
        match row.task_mode.as_deref() {
            Some("fixed") => {
                let task = row.task_id.clone().filter(|tid| {
                    store
                        .get_task(tid)
                        .ok()
                        .flatten()
                        .is_some_and(|t| t.workspace_root == root && t.status != "archived")
                });
                if task.is_none() {
                    eprintln!(
                        "[taime-daemon] schedule '{}': fixed task target missing/archived/\
                         foreign-workspace — firing Uncategorized",
                        row.name
                    );
                }
                (task, false)
            }
            Some("per_run") => {
                let id = format!("task-{}", &gen_id()[..8]);
                let when = chrono::DateTime::from_timestamp(now_unix() as i64, 0)
                    .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default();
                let title = format!("{} — {}", row.name, when);
                match store.create_task(&id, root, &title, "", now_unix()) {
                    Ok(()) => (Some(id), true),
                    Err(e) => {
                        eprintln!("[taime-daemon] per-run task create failed: {e}");
                        (None, false)
                    }
                }
            }
            _ => (None, false),
        }
    }

    /// Fire one schedule: CLAIM it (advance last/next FIRST), then run its optional
    /// shell gate (non-zero exit = skip) and spawn the agent with the
    /// var-substituted prompt.
    fn fire_schedule(&self, row: &ScheduleRow) {
        let now = now_unix();
        let next = schedules::next_run_unix(&row.schedule);
        // CLAIM up front (review H3): advance next_run BEFORE the gate + spawn so
        // an overlapping ~30s tick — or a slow gate — can't see this row as still
        // due and double-fire it. A failed/slow fire waits for the next cron
        // instant instead of re-firing every tick (the daemon already does not
        // backfill instants missed while it was down, so skipping one is in band).
        if let Some(store) = &self.store {
            let _ = store.set_schedule_run(&row.name, now, next);
        }
        if let Some(script) = row.script.as_deref().filter(|s| !s.trim().is_empty()) {
            if !run_script_gate(script) {
                return;
            }
        }
        let prompt = row.prompt.clone().unwrap_or_default();
        let prompt = schedules::substitute_vars(&prompt, &schedule_vars(&row.name));
        let (task, task_created) = self.schedule_task_target(row);
        if let Err(e) = self.fire_headless(
            &row.agent_profile,
            &row.provider,
            row.workspace_root.clone(),
            &prompt,
            task.clone(),
            !row.shared, // isolated unless the schedule opts into a shared fire
        ) {
            eprintln!("[taime-daemon] schedule '{}' fire failed: {e}", row.name);
            // Roll back a task minted for THIS fire — a persistently failing
            // schedule must not accumulate one orphan task per cron tick.
            if task_created {
                if let (Some(store), Some(tid)) = (&self.store, task.as_deref()) {
                    let _ = store.delete_task(tid);
                }
            }
        }
    }

    /// The cron tick (called every ~30s off the hot loop): fire all due schedules.
    /// Single-flight (review H3): if a previous check is still running (e.g. a slow
    /// gate), skip this one so two overlapping ticks can't fire the same row.
    pub fn check_schedules(&self) {
        if self
            .schedules_checking
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        let _guard = FlagGuard(&self.schedules_checking);
        let Some(store) = &self.store else { return };
        for row in store.due_schedules(now_unix()).unwrap_or_default() {
            self.fire_schedule(&row);
        }
    }

    /// Manually fire a schedule now (test run) — bypasses the gate, like `cao flow run`.
    pub fn run_schedule(&self, name: &str) -> Result<(), String> {
        let store = self.store.as_ref().ok_or("persistence disabled")?;
        let row = store
            .get_schedule(name)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("unknown schedule '{name}'"))?;
        let prompt =
            schedules::substitute_vars(&row.prompt.clone().unwrap_or_default(), &schedule_vars(name));
        let (task, task_created) = self.schedule_task_target(&row);
        if let Err(e) = self.fire_headless(
            &row.agent_profile,
            &row.provider,
            row.workspace_root.clone(),
            &prompt,
            task.clone(),
            !row.shared, // isolated unless the schedule opts into a shared fire
        ) {
            // Roll back a per-run task minted for this (failed) manual fire.
            if task_created {
                if let Some(tid) = task.as_deref() {
                    let _ = store.delete_task(tid);
                }
            }
            return Err(e);
        }
        let _ = store.set_schedule_run(name, now_unix(), schedules::next_run_unix(&row.schedule));
        Ok(())
    }

    pub fn set_schedule_enabled(&self, name: &str, enabled: bool) -> Result<(), String> {
        let store = self.store.as_ref().ok_or("persistence disabled")?;
        store.set_schedule_enabled(name, enabled).map_err(|e| e.to_string())?;
        if enabled {
            if let Ok(Some(row)) = store.get_schedule(name) {
                let _ = store.set_schedule_next(name, schedules::next_run_unix(&row.schedule));
            }
        }
        Ok(())
    }

    pub fn delete_schedule(&self, name: &str) -> Result<(), String> {
        let store = self.store.as_ref().ok_or("persistence disabled")?;
        if let Ok(Some(row)) = store.get_schedule(name) {
            let _ = std::fs::remove_file(&row.file_path);
        }
        store.delete_schedule(name).map_err(|e| e.to_string())
    }

    /// All schedules as JSON (the `schedules` query — the app's list surface).
    fn schedules_json(&self) -> String {
        let Some(store) = &self.store else { return "[]".to_string() };
        let list: Vec<serde_json::Value> = store
            .list_schedules()
            .unwrap_or_default()
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.name,
                    "schedule": r.schedule,
                    "agent_profile": r.agent_profile,
                    "provider": r.provider,
                    "enabled": r.enabled,
                    "last_run": r.last_run,
                    "next_run": r.next_run,
                    "workspace_root": r.workspace_root,
                    "task_mode": r.task_mode,
                    "task_id": r.task_id,
                    // The stored prompt body (the .md body for file schedules,
                    // inline for app-created ones) — the detail surface shows it.
                    "prompt": r.prompt,
                })
            })
            .collect();
        serde_json::json!(list).to_string()
    }

    // ---- Workflows (the loopable agent step-graph) ----

    /// Record our own `Arc` (called once at startup) so the workflow engine can run
    /// on a background thread with an owned handle.
    pub fn init_self(self: &Arc<Self>) {
        let _ = self.weak_self.set(Arc::downgrade(self));
    }

    /// Upgrade the stored weak self to an owned `Arc` (None before `init_self`).
    pub fn arc(&self) -> Option<Arc<Manager>> {
        self.weak_self.get().and_then(|w| w.upgrade())
    }

    /// A clone of the durable store handle (the engine holds its own).
    pub fn store_arc(&self) -> Option<Arc<Store>> {
        self.store.clone()
    }

    fn workflows_dir() -> std::path::PathBuf {
        dirs::home_dir().unwrap_or_default().join(".taime").join("workflows")
    }

    /// On first run (no `~/.taime/workflows` dir yet) drop a couple of example
    /// workflows so the feature is discoverable and shows off branches + loops.
    /// Never overwrites once the dir exists (user edits are safe).
    pub fn seed_example_workflows(&self) {
        let dir = Self::workflows_dir();
        if dir.exists() {
            return;
        }
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        for (file, json) in [
            ("feature-with-review.json", EXAMPLE_FEATURE_REVIEW),
            ("fix-and-verify.json", EXAMPLE_FIX_VERIFY),
        ] {
            let _ = std::fs::write(dir.join(file), json);
        }
    }

    /// Ingest `~/.taime/workflows/*.json` at startup (file-authored workflows).
    pub fn load_workflow_files(&self) {
        let Some(store) = &self.store else { return };
        if let Ok(entries) = std::fs::read_dir(Self::workflows_dir()) {
            for e in entries.flatten() {
                let path = e.path();
                if path.extension().and_then(|x| x.to_str()) != Some("json") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else { continue };
                match crate::workflow::parse_workflow(&text) {
                    Ok(def) => {
                        let _ = store.upsert_workflow(
                            &def.name,
                            Some(&path.to_string_lossy()),
                            &text,
                            "file",
                            now_unix(),
                        );
                    }
                    Err(err) => eprintln!("[taime-daemon] bad workflow {path:?}: {err}"),
                }
            }
        }
    }

    /// Create/replace a workflow from JSON (validates, writes `.json`, stores).
    /// `source` is "generated" (orchestrator) or "file". Returns the name.
    pub fn create_workflow(&self, json: &str, source: &str) -> Result<String, String> {
        let store = self.store.as_ref().ok_or("persistence disabled")?;
        let def = crate::workflow::parse_workflow(json)?;
        let dir = Self::workflows_dir();
        let _ = std::fs::create_dir_all(&dir);
        let safe: String = def
            .name
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
            .collect();
        let path = dir.join(format!("{safe}.json"));
        let pretty = serde_json::to_string_pretty(&def).unwrap_or_else(|_| json.to_string());
        let _ = std::fs::write(&path, &pretty);
        store
            .upsert_workflow(&def.name, Some(&path.to_string_lossy()), &pretty, source, now_unix())
            .map_err(|e| e.to_string())?;
        Ok(def.name)
    }

    pub fn delete_workflow(&self, name: &str) -> Result<(), String> {
        let store = self.store.as_ref().ok_or("persistence disabled")?;
        if let Ok(Some((_, _, Some(path)))) = store.get_workflow(name) {
            let _ = std::fs::remove_file(&path);
        }
        store.delete_workflow(name).map_err(|e| e.to_string())
    }

    /// Cancel an in-flight workflow run (review M8): flip its status to
    /// `cancelled`. The engine's `wait_for_output` bail observes it within a poll
    /// tick (≤2s), ends the run, and kills the in-flight node worker — so a wedged
    /// run (a node whose worker never `share`s) no longer burns up to 20 min/node
    /// and a concurrency slot uncancellably.
    pub fn cancel_workflow_run(&self, run_id: &str) -> Result<bool, String> {
        let store = self.store.as_ref().ok_or("persistence disabled")?;
        store.cancel_run(run_id, now_unix()).map_err(|e| e.to_string())
    }

    /// Spawn one workflow-node worker: a worktree off `project_root` (if any), the
    /// agent under `profile`/`provider` WITH orchestration tools (so it can `share`
    /// its result), seeded with `prompt`. Returns `(session_id, agent_id)`. Not
    /// subject to the `assign` fan/depth limits — the workflow's own iteration
    /// guards bound it.
    pub fn spawn_workflow_node(
        &self,
        project_root: Option<&str>,
        provider: &str,
        profile: &str,
        prompt: &str,
        task_id: Option<&str>,
    ) -> Result<(String, String), String> {
        let (cwd, attr_key) = match project_root.filter(|r| !r.is_empty()) {
            Some(root) => {
                // Node agents inherit the run's task (spawn path 3 of 4).
                let info = self.provision_worktree(
                    root.to_string(),
                    provider.to_string(),
                    true,
                    task_id.map(str::to_string),
                );
                (Some(info.worktree_path), info.agent_id)
            }
            None => (None, format!("wf-{}", &gen_id()[..8])),
        };
        let spec = AgentSpawnSpec {
            provider: provider.to_string(),
            profile: AgentProfile { name: profile.to_string(), ..Default::default() },
            cwd,
            rows: 24,
            cols: 80,
            agent_id: Some(attr_key.clone()),
            seed_prompt: None,
            env: vec![],
            inject_orchestration: true,
        };
        let session_id = self.spawn_agent(spec)?;
        let _ = self.enqueue_message("workflow".to_string(), attr_key.clone(), prompt.to_string());
        Ok((session_id, attr_key))
    }

    /// Start a workflow run on a background thread; returns the run id immediately.
    /// `caller` (an orchestrator's key, if any) is notified on completion and used
    /// to resolve the run's provider + project root when not given explicitly.
    pub fn run_workflow(
        &self,
        name: &str,
        project_root: Option<String>,
        caller: Option<String>,
        task_id: Option<String>,
    ) -> Result<String, String> {
        let store = self.store.as_ref().ok_or("persistence disabled")?;
        let (def_json, _src, _fp) = store
            .get_workflow(name)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("unknown workflow '{name}'"))?;
        let def = crate::workflow::parse_workflow(&def_json)?;
        let arc = self.arc().ok_or("daemon not fully initialized")?;
        let caller_session = caller.as_deref().and_then(|c| self.session_by_attribution(c));
        let provider = caller_session
            .as_ref()
            .and_then(|s| s.provider())
            .unwrap_or_else(|| "claude_code".to_string());
        let project_root = project_root.filter(|r| !r.is_empty()).or_else(|| {
            caller_session.as_ref().map(|s| self.session_root_of(&s.summary()))
        });
        // A task can only group work inside its own workspace; validate so a
        // stale/foreign task id degrades to Uncategorized instead of mislabeling.
        let task_id = task_id.filter(|tid| {
            store.get_task(tid).ok().flatten().is_some_and(|t| match project_root.as_deref() {
                Some(root) => t.workspace_root == root,
                None => true,
            })
        });
        // Cap concurrent runs: each is an engine thread spawning agents outside
        // the assign fan/depth guards, so without a ceiling a loop (or a worker
        // recursing into run_workflow before the MCP gate existed) could mint
        // unbounded live agents. Slot is taken before the thread spawns and
        // released when the engine returns (any exit path).
        const MAX_CONCURRENT_WORKFLOW_RUNS: u64 = 8;
        if self.active_workflow_runs.fetch_add(1, Ordering::SeqCst) >= MAX_CONCURRENT_WORKFLOW_RUNS
        {
            self.active_workflow_runs.fetch_sub(1, Ordering::SeqCst);
            return Err(format!(
                "too many concurrent workflow runs (max {MAX_CONCURRENT_WORKFLOW_RUNS})"
            ));
        }
        let run_id = format!("wfrun-{}", &gen_id()[..12]);
        if let Err(e) = store.create_run(&run_id, name, now_unix(), task_id.as_deref()) {
            self.active_workflow_runs.fetch_sub(1, Ordering::SeqCst);
            return Err(e.to_string());
        }
        let (def2, run2, caller2, task2) = (def, run_id.clone(), caller, task_id);
        let arc2 = arc.clone();
        std::thread::spawn(move || {
            // Drop guard so the slot releases on ANY exit — including a panic
            // unwinding out of the engine (a poisoned store mutex is the live
            // candidate). A leaked slot would consume the cap until daemon
            // restart; eight leaks would reject workflows forever.
            struct Slot(Arc<Manager>);
            impl Drop for Slot {
                fn drop(&mut self) {
                    self.0.active_workflow_runs.fetch_sub(1, Ordering::SeqCst);
                }
            }
            let _slot = Slot(arc2);
            crate::workflow_engine::run(arc, def2, run2, project_root, provider, caller2, task2);
        });
        Ok(run_id)
    }

    /// Whether `key` is the agent of a currently-running workflow node. Node
    /// workers get orchestration tools (they must `share` results), which would
    /// otherwise include `run_workflow` — letting a node recurse into its own
    /// workflow, unbounded. The MCP layer gates on this.
    pub fn is_workflow_worker(&self, key: &str) -> bool {
        self.store.as_ref().map(|s| s.is_active_node_agent(key)).unwrap_or(false)
    }

    /// All workflows as JSON for the app's panel: each with its graph + last run.
    fn workflows_json(&self) -> String {
        let Some(store) = &self.store else { return "[]".to_string() };
        let list: Vec<serde_json::Value> = store
            .list_workflows()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(name, source, def_json)| {
                let def: crate::workflow::WorkflowDefinition =
                    serde_json::from_str(&def_json).ok()?;
                let last_run = store
                    .latest_run(&name)
                    .ok()
                    .flatten()
                    .map(|r| self.run_summary_json(&r));
                Some(serde_json::json!({
                    "name": name,
                    "source": source,
                    "entry": def.entry,
                    "nodes": serde_json::to_value(&def.nodes).unwrap_or_default(),
                    "edges": serde_json::to_value(&def.edges).unwrap_or_default(),
                    "last_run": last_run,
                }))
            })
            .collect();
        serde_json::json!(list).to_string()
    }

    fn run_summary_json(&self, r: &crate::store::WorkflowRunRow) -> serde_json::Value {
        let states: serde_json::Map<String, serde_json::Value> = self
            .store
            .as_ref()
            .and_then(|s| s.node_states(&r.id).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|n| {
                (
                    n.node_id,
                    serde_json::json!({
                        "status": n.status, "iteration": n.iteration, "agent_id": n.agent_key,
                    }),
                )
            })
            .collect();
        serde_json::json!({
            "id": r.id,
            "workflow_name": r.workflow_name,
            "status": r.status,
            "started_at": r.started_at,
            "ended_at": r.ended_at,
            "error": r.error,
            "task_id": r.task_id,
            "node_states": states,
        })
    }

    fn workflow_run_status_json(&self, run_id: &str) -> String {
        match self.store.as_ref().and_then(|s| s.get_run(run_id).ok().flatten()) {
            Some(r) => self.run_summary_json(&r).to_string(),
            None => "null".to_string(),
        }
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
    /// project, launch the worker under `role` (a profile name; default
    /// `"default"`) with an optional `tools` allow-list override, seed the task via
    /// the inbox (delivered when the worker is idle), and record the parent→child
    /// edge + assignment node. Enforces depth/fan limits so a buggy orchestrator
    /// can't fork-bomb the host. Returns the worker's attribution key.
    pub fn assign_worker(
        &self,
        parent_key: &str,
        prompt: &str,
        working_directory: Option<String>,
        role: Option<&str>,
        tools: Option<Vec<String>>,
    ) -> Result<String, String> {
        // Depth/fan guards (runaway-fanout backstop). Check BOTH limits AND claim
        // the slot under a SINGLE lock so two concurrent assigns from the same
        // parent can't both pass the fan check and both spawn. The slot is held by
        // a temporary reservation key (the real child key isn't known until we've
        // provisioned its worktree); on success we swap it for the child key, on
        // failure we release it — so a rejected/failed assign never leaks fan.
        let reservation = gen_id();
        let parent_depth = {
            let mut a = self.assignments.lock().unwrap();
            let parent_depth = a.get(parent_key).map(|n| n.depth).unwrap_or(0);
            if parent_depth >= MAX_ASSIGN_DEPTH {
                return Err(format!(
                    "assign: max assignment depth {MAX_ASSIGN_DEPTH} reached (chain too deep)"
                ));
            }
            let fanned = a.values().filter(|n| n.parent == parent_key).count();
            if fanned >= MAX_ASSIGN_FAN {
                return Err(format!("assign: max fan-out {MAX_ASSIGN_FAN} reached for this agent"));
            }
            a.insert(
                reservation.clone(),
                AssignNode { parent: parent_key.to_string(), depth: parent_depth + 1 },
            );
            parent_depth
        };
        // From here, release the reservation on any early return.
        let release = || {
            self.assignments.lock().unwrap().remove(&reservation);
        };

        let parent = self.session_by_attribution(parent_key);
        let provider = parent
            .as_ref()
            .and_then(|s| s.provider())
            .unwrap_or_else(|| "claude_code".to_string());
        // Prefer the parent's durable workspace root (its worktree row) over its
        // session cwd: an ISOLATED parent's cwd is its worktree path, which would
        // root the child — and the workspace grouping — under the wrong "project".
        let project_root = working_directory
            .clone()
            .or_else(|| {
                self.store
                    .as_ref()
                    .and_then(|s| s.worktree_project_root(parent_key).ok().flatten())
            })
            .or_else(|| parent.as_ref().map(|s| s.cwd()))
            .filter(|c| !c.is_empty());

        let (cwd, child_key) = match project_root {
            Some(root) => {
                // Orchestrator-assigned workers inherit the parent's task
                // (spawn path 2 of 4) — the team stays inside one Task. Tasks
                // are workspace-scoped, so inherit ONLY when the task's
                // workspace matches the provision root: an explicit
                // cross-workspace working_directory must not smuggle the
                // pointer along (the same invariant assign_task enforces).
                let parent_task = self
                    .store
                    .as_ref()
                    .and_then(|s| s.task_of_worktree(parent_key))
                    .filter(|tid| {
                        self.store
                            .as_ref()
                            .and_then(|s| s.get_task(tid).ok().flatten())
                            .is_some_and(|t| t.workspace_root == root)
                    });
                let info = self.provision_worktree(root, provider.clone(), true, parent_task);
                (Some(info.worktree_path), info.agent_id)
            }
            None => (working_directory, gen_id()[..8].to_string()),
        };

        let spec = AgentSpawnSpec {
            provider,
            profile: AgentProfile {
                // The named role resolves against the profile store at spawn; an
                // explicit `tools` list overrides the profile's allow-list.
                name: role.unwrap_or("default").to_string(),
                allowed_tools: tools.unwrap_or_default(),
                ..Default::default()
            },
            cwd,
            rows: 24,
            cols: 80,
            agent_id: Some(child_key.clone()),
            seed_prompt: None,
            env: vec![],
            // Workers GET the Taime team tools so they can report back to the
            // orchestrator — `share` their result to the blackboard and/or
            // `send_message` the parent. Without this they had no Taime channel
            // and fell back to Claude's native team layer (which can't see Taime
            // agents), so findings never reached the orchestrator. Runaway
            // sub-delegation is bounded by MAX_ASSIGN_DEPTH / MAX_ASSIGN_FAN + the
            // recursion gate, so this is safe.
            inject_orchestration: true,
        };
        if let Err(e) = self.spawn_agent(spec) {
            release();
            return Err(e);
        }
        // Swap the reservation for the real child key (still counts as one slot —
        // the parent's fan never dips between reservation and this swap).
        {
            let mut a = self.assignments.lock().unwrap();
            a.remove(&reservation);
            a.insert(
                child_key.clone(),
                AssignNode { parent: parent_key.to_string(), depth: parent_depth + 1 },
            );
        }
        // Seed the task + a fan-in instruction: deliver results back to the parent.
        let seeded = format!(
            "{prompt}\n\n(When done, report your result to the orchestrator with: \
             send_message to=\"{parent_key}\" body=\"…\".)"
        );
        let _ = self.enqueue_message(parent_key.to_string(), child_key.clone(), seeded);
        if let Some(store) = &self.store {
            let _ = store.record_activity_edge(&gen_id(), "assign", parent_key, &child_key, now_unix());
        }
        Ok(child_key)
    }

    pub fn list(&self) -> Vec<SessionSummary> {
        let mut sums: Vec<SessionSummary> =
            self.sessions.lock().unwrap().values().map(|s| s.summary()).collect();
        // Task membership lives on the worktree row (the durable anchor), not in
        // the live session — fill at list time so reassignment shows next tick.
        if let Some(store) = &self.store {
            for s in &mut sums {
                if let Some(key) = &s.agent_id {
                    s.task_id = store.task_of_worktree(key);
                }
            }
        }
        // Role/profile lives in the in-memory roles map (set at spawn) — fill it so
        // the UI can show each agent's role (incl. assigned workers it adopted).
        {
            let roles = self.roles.lock().unwrap();
            for s in &mut sums {
                if let Some(key) = &s.agent_id {
                    s.role = roles.get(key).cloned();
                }
            }
        }
        sums
    }

    /// Kill one session. SIGTERMs the agent's whole process group (review H1) and
    /// leaves it in the live map: the gc reaper escalates to SIGKILL after a
    /// grace if it ignores SIGTERM, then reaps it (running provider cleanup +
    /// recording the exit + dropping its MCP token + the parent fan-in) on a
    /// later tick — the single reap path, instead of a second one here that would
    /// miss a SIGTERM-ignoring CLI.
    pub fn kill(&self, id: &str) {
        let session = self.sessions.lock().unwrap().get(id).cloned();
        if let Some(s) = session {
            s.kill();
        }
        self.touch();
    }

    /// Shutdown teardown (review H1/H2): terminate every agent's process GROUP,
    /// give one shared grace, SIGKILL stragglers, then reap + run provider cleanup
    /// SYNCHRONOUSLY — the signal handler `process::exit`s next, so the reader
    /// threads and the gc reaper will never get to it. Without this, injected
    /// gemini/grok MCP config (dead `taime` server entries, policy files) leaks
    /// into the user's real `~/.gemini`/`~/.grok` on every shutdown.
    pub fn kill_all(&self) {
        let sessions: Vec<Session> = self.sessions.lock().unwrap().drain().map(|(_, s)| s).collect();
        if sessions.is_empty() {
            return;
        }
        // 1. SIGTERM every group.
        for s in &sessions {
            s.shutdown_terminate();
        }
        // 2. One shared grace for graceful exit.
        std::thread::sleep(Duration::from_millis(400));
        // 3. SIGKILL any straggler group, then reap + run cleanup inline.
        for s in &sessions {
            s.shutdown_finalize();
        }
    }

    /// Begin the maintenance tick iff no tick is already running (review M6):
    /// returns true if the caller now owns the single-flight latch. The owner must
    /// call [`Manager::end_gc`] when done (unless it `process::exit`s).
    pub fn try_begin_gc(&self) -> bool {
        self.gc_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Release the maintenance-tick single-flight latch.
    pub fn end_gc(&self) {
        self.gc_running.store(false, Ordering::SeqCst);
    }

    /// Archive-then-reclaim a dead agent's isolated worktree: snapshot its full
    /// state into `refs/taime/archive/<id>`, cache the rendered review patch, then
    /// remove the physical checkout. NON-LOSSY — every durable write lands BEFORE
    /// the checkout is deleted, and a clean worktree (nothing to keep) skips
    /// straight to removal. The `taime_worktrees` row survives as the Agent-ID
    /// attribution anchor; its diff renders from the archive thereafter. Returns
    /// `true` when the checkout was reclaimed. A snapshot failure keeps the
    /// checkout (never reclaim what we couldn't preserve). Caller must have gated
    /// on isolated mode + not-live.
    fn archive_and_reclaim(&self, w: &crate::store::WorktreeRow) -> bool {
        let Some(store) = &self.store else { return false };
        let Some(repo) = w.repo_root.as_deref() else { return false };
        let base = w.base_sha.as_deref().unwrap_or("");
        let now = now_unix();
        // Checkout already gone (a legacy row from before archive-then-reclaim, or
        // a prior external removal): nothing to archive — it IS reclaimed, so
        // record that and stop surfacing it. Without this, such rows would forever
        // re-fail the snapshot ("checkout missing") and linger in the cleanup list.
        if !std::path::Path::new(&w.worktree_path).exists() {
            let _ = store.mark_reclaimed(&w.terminal_id, now);
            self.worktrees_gced.lock().unwrap().insert(w.terminal_id.clone());
            return true;
        }
        let outcome =
            crate::worktree::archive_agent(&w.terminal_id, &w.worktree_path, repo, base);
        match outcome {
            crate::worktree::ArchiveOutcome::Archived(res) => {
                // Durable record BEFORE deletion (crash-safe / idempotent).
                let _ = store.put_review_patch(
                    &crate::store::ReviewPatch {
                        agent_id: w.terminal_id.clone(),
                        base_sha: res.base_sha.clone(),
                        archive_ref: res.archive_ref.clone(),
                        digest: res.digest.clone(),
                        diff_blob: res.diff_blob.clone(),
                        files_changed: res.files_changed as i64,
                    },
                    now,
                );
                let _ = store.mark_archived(&w.terminal_id, Some(&res.archive_ref), now);
                crate::worktree::reclaim_checkout(&w.worktree_path, repo, w.branch.as_deref());
                let _ = store.mark_reclaimed(&w.terminal_id, now);
                self.worktrees_gced.lock().unwrap().insert(w.terminal_id.clone());
                true
            }
            crate::worktree::ArchiveOutcome::NoChanges => {
                // Clean worktree: nothing to preserve, just reclaim.
                let _ = store.mark_archived(&w.terminal_id, None, now);
                crate::worktree::reclaim_checkout(&w.worktree_path, repo, w.branch.as_deref());
                let _ = store.mark_reclaimed(&w.terminal_id, now);
                self.worktrees_gced.lock().unwrap().insert(w.terminal_id.clone());
                true
            }
            crate::worktree::ArchiveOutcome::Failed(e) => {
                eprintln!(
                    "[taime-daemon] archive {} failed, keeping checkout: {e}",
                    w.terminal_id
                );
                false
            }
        }
    }

    /// Explicitly reclaim one agent's checkout NOW (the cleanup UI's "clean up
    /// now" / dismiss). Archives first (non-lossy), bypasses the retention caps,
    /// but still refuses a LIVE agent. Returns `true` if reclaimed.
    pub fn reclaim_agent(&self, agent_id: &str) -> bool {
        let Some(store) = &self.store else { return false };
        let live = {
            let map = self.sessions.lock().unwrap();
            map.values().any(|s| s.attribution_key().as_deref() == Some(agent_id))
        };
        if live {
            return false;
        }
        let Ok(Some(w)) = store.worktree_row(agent_id) else { return false };
        if w.mode.as_deref() != Some("isolated") || w.reclaimed_at.is_some() {
            return false;
        }
        let did = self.archive_and_reclaim(&w);
        self.touch();
        did
    }

    /// The reclaimable surface for the cleanup UI: every dead (not-live) isolated
    /// agent whose checkout is still on disk, with how much it would free. Returns
    /// `(agent_id, files_changed_estimate, reclaimed_already)` is overkill — we
    /// expose JSON the panel consumes directly.
    pub fn reclaimable_json(&self) -> String {
        let Some(store) = &self.store else { return "[]".to_string() };
        let live: std::collections::HashSet<String> = self
            .sessions
            .lock()
            .unwrap()
            .values()
            .filter_map(|s| s.attribution_key())
            .collect();
        let physical = store.physical_isolated_worktrees().unwrap_or_default();
        let items: Vec<serde_json::Value> = physical
            .iter()
            .filter(|w| !live.contains(&w.terminal_id))
            .map(|w| {
                serde_json::json!({
                    "agent_id": w.terminal_id,
                    "project_root": w.project_root,
                    "worktree_path": w.worktree_path,
                    "created_at": w.created_at,
                    "task_id": w.task_id,
                })
            })
            .collect();
        serde_json::json!(items).to_string()
    }

    /// Retention sweep (startup + every ~10min, off the hot loop via
    /// spawn_blocking — shells out to git). The worktree is a disposable sandbox;
    /// attribution is the archive. A dead agent's checkout is archived into
    /// `refs/taime/archive/*` and then reclaimed — NON-LOSSY (the snapshot lands
    /// first). Reclaim is driven by caps: keep the most-recent `KEEP_RECENT`
    /// checkouts for fast live review, reclaim the rest, and age-evict any past
    /// `MAX_IDLE`. Live agents, shared-mode rows, and worktrees within the grace
    /// are never touched; the `taime_worktrees` ROW always survives.
    pub fn sweep_worktrees(&self) {
        let Some(store) = &self.store else { return };
        // Snapshot live keys under the sessions lock, then release (lock
        // discipline: never shell out under it).
        let live: std::collections::HashSet<String> = self
            .sessions
            .lock()
            .unwrap()
            .values()
            .filter_map(|s| s.attribution_key())
            .collect();
        let now = now_unix();
        let grace = env_u64("TAIME_WORKTREE_RECLAIM_GRACE_SECS", 600);
        let keep = env_u64("TAIME_WORKTREE_KEEP", 25) as usize;
        let max_idle = env_u64("TAIME_WORKTREE_MAX_IDLE_DAYS", 14).saturating_mul(86_400);
        // All physical (non-reclaimed) isolated checkouts, newest-provisioned
        // first — the recency rank that the count cap protects.
        let physical = store.physical_isolated_worktrees().unwrap_or_default();
        let victims = select_reclaim_victims(&physical, &live, now, grace, keep, max_idle);
        let mut reclaimed = 0usize;
        for w in victims {
            if self.worktrees_gced.lock().unwrap().contains(&w.terminal_id) {
                continue;
            }
            if self.archive_and_reclaim(w) {
                reclaimed += 1;
            }
        }
        if reclaimed > 0 {
            eprintln!(
                "[taime-daemon] worktree retention: archived + reclaimed {reclaimed} checkout(s)"
            );
        }
    }

    /// One-time collapse migration (archive-then-reclaim rollout): archive AND
    /// reclaim EVERY existing dead isolated worktree, regardless of the retention
    /// caps, so the historical pile of per-agent checkouts becomes
    /// `refs/taime/archive/*` refs and `git branch` settles to main-only. Guarded
    /// by a `taime_meta` marker so it runs exactly once. Live agents are skipped
    /// (they'll be reclaimed by the normal sweep after they exit).
    pub fn collapse_worktrees_once(&self) {
        let Some(store) = &self.store else { return };
        if store.meta_get("worktree_collapse_v12").ok().flatten().is_some() {
            return;
        }
        let live: std::collections::HashSet<String> = self
            .sessions
            .lock()
            .unwrap()
            .values()
            .filter_map(|s| s.attribution_key())
            .collect();
        let physical = store.physical_isolated_worktrees().unwrap_or_default();
        let mut reclaimed = 0usize;
        for w in &physical {
            if live.contains(&w.terminal_id) {
                continue;
            }
            if w.mode.as_deref() != Some("isolated") {
                continue;
            }
            if self.archive_and_reclaim(w) {
                reclaimed += 1;
            }
        }
        let _ = store.meta_set("worktree_collapse_v12", &now_unix().to_string());
        if reclaimed > 0 {
            eprintln!(
                "[taime-daemon] one-time worktree collapse: archived + reclaimed {reclaimed} \
                 pre-existing checkout(s) into refs/taime/archive/*"
            );
        }
    }

    /// Replay + clear any provider-cleanup ledger rows left by a PRIOR daemon that
    /// died (SIGKILL/crash/OOM) before its own teardown ran (review H2 — the
    /// load-bearing half, since an abrupt death bypasses both the reader EOF path
    /// and the signal handler's synchronous cleanup). Idempotent: the actions are
    /// removals. Runs once at startup, before serving. A freshly-spawned session
    /// won't have a row yet, so only orphans are touched.
    pub fn reconcile_cleanups_on_boot(&self) {
        let Some(store) = &self.store else { return };
        let rows = match store.all_cleanups() {
            Ok(r) => r,
            Err(_) => return,
        };
        if rows.is_empty() {
            return;
        }
        let mut actions = 0usize;
        for (session_id, json) in rows {
            if let Ok(cleanup) = serde_json::from_str::<crate::providers::Cleanup>(&json) {
                cleanup.run();
                actions += cleanup.actions.len();
            }
            let _ = store.delete_cleanup(&session_id);
        }
        if actions > 0 {
            eprintln!(
                "[taime-daemon] boot reconcile: replayed {actions} orphaned provider-cleanup action(s)"
            );
        }
    }

    /// Retention pruning for the history tables (startup + daily; see
    /// `Store::prune_history` for the windows).
    pub fn prune_history(&self) {
        if let Some(store) = &self.store {
            match store.prune_history(now_unix()) {
                Ok(n) if n > 0 => eprintln!("[taime-daemon] pruned {n} aged history row(s)"),
                Ok(_) => {}
                Err(e) => eprintln!("[taime-daemon] history prune failed: {e}"),
            }
        }
    }

    /// Periodic maintenance. Reaps dead sessions (never a live agent), runs the
    /// quiet-window attribution check, and returns `true` when the daemon should
    /// shut down (no sessions + no connected client + idle past `idle_grace`).
    pub fn gc_tick(&self, quiet_threshold: Duration, idle_grace: Duration) -> bool {
        // Reap only DEAD sessions; record their exit + drop their MCP tokens.
        // Removal happens under the sessions lock; the SQLite writes happen
        // AFTER it drops — a contended DB write must never block list/attach/
        // spawn behind this 250ms tick.
        {
            let dead: Vec<(String, Option<String>)> = {
                let mut map = self.sessions.lock().unwrap();
                let dead: Vec<(String, Option<String>)> = map
                    .iter()
                    .filter(|(_, s)| !s.is_alive())
                    .map(|(k, s)| (k.clone(), s.attribution_key()))
                    .collect();
                for (k, _) in &dead {
                    map.remove(k);
                }
                dead
            };
            let mut dead_akeys: Vec<String> = Vec::new();
            for (k, akey) in dead {
                if let Some(store) = &self.store {
                    let _ = store.set_session_status(&k, "exited");
                }
                if let Some(a) = akey {
                    dead_akeys.push(a);
                }
            }
            if !dead_akeys.is_empty() {
                self.tokens.lock().unwrap().retain(|_, v| !dead_akeys.contains(v));
                for akey in &dead_akeys {
                    self.roles.lock().unwrap().remove(akey);
                    // Result fan-in: notify the parent (once) that its worker
                    // finished, then drop the assignment node.
                    let node = self.assignments.lock().unwrap().remove(akey);
                    if let Some(node) = node {
                        let _ = self.enqueue_message(
                            akey.clone(),
                            node.parent,
                            format!("Worker {akey} has finished (process exited)."),
                        );
                    }
                }
            }
        }
        // Live-session maintenance. The reaper (review H1/M15) runs FIRST: reap a
        // session whose child exited while its reader was parked on backpressure
        // (or before the reader observed EOF), and escalate an unanswered kill()
        // to a group SIGKILL after the grace. A session reaped here is removed
        // from the map on the next tick's dead-sweep above.
        let live: Vec<Session> = self.sessions.lock().unwrap().values().cloned().collect();
        for s in &live {
            if s.reap_if_exited() {
                continue;
            }
            s.escalate_kill_if_due(KILL_GRACE);
            s.quiet_check(quiet_threshold);
            s.push_status_if_changed();
        }
        // Idle-gated inbox delivery (Phase 5): deliver pending messages to any
        // receiver that's now idle. Runs on the same 250ms tick as quiet-window.
        self.deliver_pending();
        // Shutdown decision: nothing left to serve and the app isn't connected.
        let empty = self.sessions.lock().unwrap().is_empty();
        let no_clients = self.active_conns.load(Ordering::SeqCst) == 0;
        let idle = self.last_activity.lock().unwrap().elapsed() > idle_grace;
        // Stay alive while any schedule is enabled, so cron fires unattended even
        // with the app closed (the daemon IS the scheduler).
        let has_schedules = self.store.as_ref().map(|s| s.has_enabled_schedules()).unwrap_or(false);
        empty && no_clients && idle && !has_schedules
    }
}

#[cfg(test)]
impl Manager {
    /// Construct a manager with an injected store (so tests don't touch the real
    /// app-data DB). Empty session map + the built-in provider registry.
    pub fn for_test(store: Option<Store>) -> Self {
        Manager {
            sessions: Mutex::new(HashMap::new()),
            boot_nonce: format!("{:016x}", rand::random::<u64>()),
            session_counter: AtomicU64::new(0),
            conn_counter: AtomicU64::new(0),
            active_conns: AtomicU64::new(0),
            last_activity: Mutex::new(Instant::now()),
            registry: Registry::load(),
            store: store.map(Arc::new),
            store_health: StoreHealth::Ok,
            tokens: Mutex::new(HashMap::new()),
            roles: Mutex::new(HashMap::new()),
            assignments: Mutex::new(HashMap::new()),
            weak_self: std::sync::OnceLock::new(),
            worktrees_gced: Mutex::new(std::collections::HashSet::new()),
            active_workflow_runs: AtomicU64::new(0),
            gc_running: AtomicBool::new(false),
            schedules_checking: AtomicBool::new(false),
        }
    }

    /// The durable store (test inspection).
    pub fn store(&self) -> Option<&Store> {
        self.store.as_deref()
    }

    /// Seed an assignment node (so the fan/depth guards can be tested without
    /// actually spawning worker processes).
    pub fn seed_assignment(&self, child: &str, parent: &str, depth: u32) {
        self.assignments
            .lock()
            .unwrap()
            .insert(child.to_string(), AssignNode { parent: parent.to_string(), depth });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review H4 regression: the schedule gate must honor exit status AND fail
    /// closed (kill the child) on timeout, without waiting the full sleep.
    #[test]
    fn gate_honors_exit_status() {
        assert!(run_script_gate_with_timeout("exit 0", Duration::from_secs(5)));
        assert!(!run_script_gate_with_timeout("exit 1", Duration::from_secs(5)));
    }

    #[test]
    fn gate_times_out_and_fails_closed() {
        let start = Instant::now();
        let passed = run_script_gate_with_timeout("sleep 30", Duration::from_millis(300));
        assert!(!passed, "a hanging gate must fail closed");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "must abandon at the timeout, not wait the full sleep"
        );
    }

    #[test]
    fn delivery_payload_is_clearly_delimited() {
        let out = format_delivery("term-a", "please review the diff");
        // Bare line-feeds only — the submit is a SEPARATE delayed `\r` so the
        // payload itself never reads as Enter (long messages paste, don't submit).
        assert_eq!(
            out,
            "\n--- MESSAGE FROM term-a ---\nplease review the diff\n--- END MESSAGE ---\n"
        );
        assert!(!out.contains('\r'), "no carriage returns in the framed payload");
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
        let v: serde_json::Value = serde_json::from_str(&mgr.activity_graph_json(None)).unwrap();
        assert_eq!(v["agents"].as_array().unwrap().len(), 1);
        assert_eq!(v["agents"][0]["agent_id"], "a");
        assert_eq!(v["agents"][0]["provider"], "claude_code");
        assert_eq!(v["edges"].as_array().unwrap().len(), 1);
        assert_eq!(v["edges"][0]["kind"], "assign");
        assert_eq!(v["edges"][0]["source"], "a");
        assert_eq!(v["edges"][0]["target"], "b");
    }

    fn mem_manager() -> Manager {
        let store = crate::store::Store::open_at(std::path::Path::new(":memory:")).unwrap();
        Manager::for_test(Some(store))
    }

    #[test]
    fn delete_workspace_tears_down_only_the_target_workspace() {
        let mgr = mem_manager();
        let store = mgr.store().unwrap();
        let mk = |id: &str, root: &str| WorktreeInfo {
            agent_id: id.into(),
            project_root: root.into(),
            repo_root: None,
            worktree_path: root.into(),
            branch: None,
            base_sha: None,
            mode: "shared".into(), // shared → no git/fs removal in the test
            error: None,
        };
        store.upsert_worktree(&mk("a", "/ws"), "claude_code", 1).unwrap();
        store.upsert_worktree(&mk("b", "/ws"), "claude_code", 2).unwrap();
        store.upsert_worktree(&mk("c", "/other"), "claude_code", 3).unwrap();
        store.create_task("t1", "/ws", "One", "", 1).unwrap();
        store.create_task("t2", "/other", "Other", "", 2).unwrap();

        // No live sessions in for_test → killed = 0; 2 agents + 1 task removed.
        // destroy_archives=false (soft) — the default safe teardown.
        let (agents, killed, tasks) = mgr.delete_workspace("/ws", false);
        assert_eq!((agents, killed, tasks), (2, 0, 1));

        // The target workspace is gone; the other workspace is untouched.
        assert_eq!(store.worktrees_in_workspace("/ws").unwrap().len(), 0);
        assert!(store.worktree_row("c").unwrap().is_some(), "/other agent survives");
        assert_eq!(store.list_tasks("/other", true).unwrap().len(), 1, "/other tasks survive");
    }

    #[test]
    fn soft_delete_preserves_archive_refs_hard_delete_destroys_them() {
        // The default (soft) delete must NOT destroy a reclaimed agent's only copy
        // of its work — its refs/taime/archive/<id> snapshot. Only an explicit hard
        // delete (the typed-confirm path) may.
        let mgr = mem_manager();
        let proj = project_repo();
        let root = proj.to_string_lossy().into_owned();

        // Agent A: real changes → reclaim → archived (now holds an archive ref).
        let wt = seed_isolated_agent(&mgr, &proj, "agent-a");
        std::fs::write(wt.join("work.rs"), "fn agent() {}\n").unwrap();
        assert!(mgr.reclaim_agent("agent-a"), "reclaim archives the work");
        assert!(crate::worktree::archive_ref_exists(&root, "agent-a"), "agent-a archived");
        assert_eq!(mgr.workspace_archived_count(&root), 1, "one archived-but-unmerged agent");

        // SOFT delete: the rows go, but the archive ref SURVIVES — no unmerged work
        // is silently destroyed.
        mgr.delete_workspace(&root, false);
        assert_eq!(mgr.store().unwrap().worktrees_in_workspace(&root).unwrap().len(), 0, "rows gone");
        assert!(
            crate::worktree::archive_ref_exists(&root, "agent-a"),
            "SOFT delete preserves the durable archive ref"
        );

        // A second archived agent, then HARD delete destroys ITS ref (typed-confirm
        // path). agent-a's earlier-preserved ref is independent and stays.
        let wt2 = seed_isolated_agent(&mgr, &proj, "agent-b");
        std::fs::write(wt2.join("more.rs"), "fn b() {}\n").unwrap();
        assert!(mgr.reclaim_agent("agent-b"));
        assert!(crate::worktree::archive_ref_exists(&root, "agent-b"));
        assert_eq!(mgr.workspace_archived_count(&root), 1, "only agent-b's row remains");

        mgr.delete_workspace(&root, true); // destroy_archives = true
        assert!(
            !crate::worktree::archive_ref_exists(&root, "agent-b"),
            "HARD delete drops the archive ref"
        );
        assert!(
            crate::worktree::archive_ref_exists(&root, "agent-a"),
            "the soft-preserved ref is untouched by a later hard delete of another agent"
        );

        let _ = std::fs::remove_dir_all(&proj);
    }

    #[test]
    fn isolated_fire_runs_in_its_worktree_not_the_users_tree() {
        // Regression for the schedule-isolation fix: an isolated headless fire must
        // run in its PRIVATE worktree checkout, never the user's real project root
        // — else it mutates the user's tree unattended and its work never surfaces
        // in Review (the isolated checkout would sit empty). headless_target is the
        // exact cwd-selection fire_headless uses.
        let mgr = mem_manager();
        let proj = project_repo();
        let root = proj.to_string_lossy().into_owned();
        let store = mgr.store().unwrap();

        // Isolated (the default): cwd is a private checkout, distinct from root.
        let (key, cwd) = mgr.headless_target(Some(root.clone()), "claude_code", true, None);
        let cwd = cwd.expect("a workspace fire has a cwd");
        assert_ne!(cwd, root, "isolated fire must NOT run in the user's real tree");
        let row = store.worktree_row(&key).unwrap().unwrap();
        assert_eq!(row.mode.as_deref(), Some("isolated"));
        assert_eq!(row.worktree_path, cwd, "fire runs in the provisioned worktree");
        crate::worktree::remove_force(&cwd, Some(&root), row.branch.as_deref());

        // Shared (the `shared:` opt-in): worktree_path == root, so cwd IS the root.
        let (skey, scwd) = mgr.headless_target(Some(root.clone()), "claude_code", false, None);
        assert_eq!(scwd.as_deref(), Some(root.as_str()), "shared fire runs in the workspace itself");
        assert_eq!(store.worktree_row(&skey).unwrap().unwrap().mode.as_deref(), Some("shared"));

        let _ = std::fs::remove_dir_all(&proj);
    }

    #[test]
    fn mint_unique_id_is_64_bit_and_rerolls_on_collision() {
        // 64-bit width: 16 lowercase hex chars that parse back as a u64 (vs the
        // old 32-bit / 8-char Agent ID whose birthday collisions merged agents).
        let id = mint_unique_id(|_| false);
        assert_eq!(id.len(), 16, "64-bit id is 16 hex chars");
        assert!(u64::from_str_radix(&id, 16).is_ok(), "id is hex: {id}");

        // Re-rolls past every reported collision: reject the first 3 rolls, accept
        // the 4th — the mint must consult `taken` exactly 4 times and return then.
        let calls = std::cell::Cell::new(0u32);
        let id = mint_unique_id(|_| {
            let n = calls.get();
            calls.set(n + 1);
            n < 3
        });
        assert_eq!(calls.get(), 4, "rejected 3 rolls, accepted the 4th");
        assert_eq!(id.len(), 16);

        // Exhaustion fallback: if every roll is taken, widen to a 128-bit id rather
        // than spin forever or panic.
        let id = mint_unique_id(|_| true);
        assert_eq!(id.len(), 32, "fell back to 128-bit gen_id()");
    }

    #[test]
    fn mint_agent_id_never_reuses_a_live_worktree_row() {
        let mgr = mem_manager();
        let store = mgr.store().unwrap();
        let mut seen = std::collections::HashSet::new();
        // A non-git project root → the archive-ref check is skipped and the durable
        // worktree-row check is the active guard. Every mint must dodge every row
        // we persist, so ids stay distinct (the old code minted blind).
        for _ in 0..200 {
            let id = mgr.mint_agent_id("/no/such/repo", false);
            assert_eq!(id.len(), 16, "64-bit agent id");
            assert!(store.worktree_row(&id).unwrap().is_none(), "mint never lands on a live row");
            assert!(seen.insert(id.clone()), "ids distinct across mints: {id}");
            store
                .upsert_worktree(
                    &WorktreeInfo {
                        agent_id: id.clone(),
                        project_root: "/no/such/repo".into(),
                        repo_root: None,
                        worktree_path: "/no/such/repo".into(),
                        branch: None,
                        base_sha: None,
                        mode: "shared".into(),
                        error: None,
                    },
                    "claude_code",
                    1,
                )
                .unwrap();
        }
    }

    #[test]
    fn pty_ids_are_unique_within_and_across_daemon_generations() {
        // Two managers model two daemon generations (each session_counter restarts
        // at 0). Within a generation ids are monotonic-unique; across generations
        // the boot nonce guarantees no overlap — so a gen-2 spawn can never reuse a
        // gen-1 pty id and resurrect its durable daemon_sessions row.
        let gen1 = Manager::for_test(None);
        let gen2 = Manager::for_test(None);
        let a = gen1.next_pty_id();
        let b = gen1.next_pty_id();
        let c = gen2.next_pty_id(); // gen-2's counter is also 0 here
        assert!(a.starts_with("pty-"), "id shape: {a}");
        assert_ne!(a, b, "unique within a generation");
        assert_ne!(a, c, "no cross-generation collision despite both counters at 0");
    }

    #[test]
    fn assign_depth_limit_blocks_a_too_deep_chain() {
        let mgr = mem_manager();
        // A parent already at the max depth cannot spawn another level.
        mgr.seed_assignment("deep", "root", MAX_ASSIGN_DEPTH);
        let err = mgr.assign_worker("deep", "go", None, None, None).unwrap_err();
        assert!(err.contains("depth"), "expected a depth error, got: {err}");
    }

    #[test]
    fn assign_fan_limit_blocks_too_many_children() {
        let mgr = mem_manager();
        // A parent that already has the max number of children cannot spawn more.
        for i in 0..MAX_ASSIGN_FAN {
            mgr.seed_assignment(&format!("child-{i}"), "busy", 1);
        }
        let err = mgr.assign_worker("busy", "go", None, None, None).unwrap_err();
        assert!(err.contains("fan-out"), "expected a fan-out error, got: {err}");
    }

    #[test]
    fn request_reply_round_trips_and_rejects_wrong_responder() {
        let mgr = mem_manager();
        let iid = mgr.request("term-a", "term-b", "what's the status?").unwrap();
        // The request lands in the responder's inbox, annotated with the id.
        let to_b = mgr.store().unwrap().pending_for("term-b", 10).unwrap();
        assert_eq!(to_b.len(), 1);
        assert!(to_b[0].message.contains(&iid));

        // A non-addressed agent cannot answer.
        assert!(mgr.reply("term-c", &iid, "nope").is_err());

        // The addressed responder can; the reply lands in the requester's inbox.
        mgr.reply("term-b", &iid, "all green").unwrap();
        let to_a = mgr.store().unwrap().pending_for("term-a", 10).unwrap();
        assert_eq!(to_a.len(), 1);
        assert!(to_a[0].message.contains("all green"));
        assert_eq!(to_a[0].sender_id, "term-b");

        // The interaction is closed: a second reply is rejected.
        assert!(mgr.reply("term-b", &iid, "again").is_err());
    }

    #[test]
    fn blackboard_share_and_get_round_trip() {
        let mgr = mem_manager();
        assert!(mgr.blackboard_get("plan").unwrap().is_none());
        mgr.blackboard_set("plan", "ship it", "term-a").unwrap();
        let (value, author, _ts) = mgr.blackboard_get("plan").unwrap().unwrap();
        assert_eq!(value, "ship it");
        assert_eq!(author.as_deref(), Some("term-a"));
        // Last writer wins.
        mgr.blackboard_set("plan", "hold", "term-b").unwrap();
        let (value, author, _ts) = mgr.blackboard_get("plan").unwrap().unwrap();
        assert_eq!(value, "hold");
        assert_eq!(author.as_deref(), Some("term-b"));
    }

    #[test]
    fn graph_includes_persisted_turns_with_files() {
        let mgr = mem_manager();
        let store = mgr.store().unwrap();
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
        store.record_turn("t1", "a", 0, 10, 20, &["src/x.rs".into(), "src/y.rs".into()]).unwrap();

        let v: serde_json::Value = serde_json::from_str(&mgr.activity_graph_json(None)).unwrap();
        let agent = &v["agents"][0];
        assert_eq!(agent["agent_id"], "a");
        assert_eq!(agent["turns"].as_array().unwrap().len(), 1);
        assert_eq!(agent["turns"][0]["files_touched"][0], "src/x.rs");
        assert_eq!(agent["turns"][0]["turn_index"], 0);
    }

    #[test]
    fn graph_contention_comes_from_shared_fs_events() {
        let mgr = mem_manager();
        let store = mgr.store().unwrap();
        store.record_fs_event("e1", "a", "shared.rs", "modify", 1).unwrap();
        store.record_fs_event("e2", "b", "shared.rs", "modify", 2).unwrap();
        store.record_fs_event("e3", "a", "solo.rs", "modify", 3).unwrap();

        let v: serde_json::Value = serde_json::from_str(&mgr.activity_graph_json(None)).unwrap();
        let cont = v["contention"].as_array().unwrap();
        assert_eq!(cont.len(), 1, "only the file touched by 2 agents is contended");
        assert_eq!(cont[0]["path"], "shared.rs");
        let terms: Vec<&str> =
            cont[0]["terminals"].as_array().unwrap().iter().map(|t| t.as_str().unwrap()).collect();
        assert!(terms.contains(&"a") && terms.contains(&"b"));
    }

    #[test]
    fn graph_scopes_agents_edges_and_contention_to_workspace() {
        let mgr = mem_manager();
        let store = mgr.store().unwrap();
        // a, b live in /ws/alpha; c lives in /ws/beta (different workspace). No
        // worktree rows ⇒ scoping falls back to the session cwd.
        for (pty, key, cwd) in [
            ("pty-a", "a", "/ws/alpha"),
            ("pty-b", "b", "/ws/alpha"),
            ("pty-c", "c", "/ws/beta"),
        ] {
            store
                .record_session(&SessionRow {
                    pty_session_id: pty.into(),
                    provider: Some("claude_code".into()),
                    attribution_key: Some(key.into()),
                    cwd: Some(cwd.into()),
                    program: "claude".into(),
                    created_at_unix: 1,
                    status: "running".into(),
                })
                .unwrap();
        }
        // All three touch shared.rs; an in-scope edge a→b and a cross-scope a→c.
        store.record_fs_event("e1", "a", "shared.rs", "modify", 1).unwrap();
        store.record_fs_event("e2", "b", "shared.rs", "modify", 2).unwrap();
        store.record_fs_event("e3", "c", "shared.rs", "modify", 3).unwrap();
        mgr.request("a", "b", "in scope?").unwrap();
        mgr.request("a", "c", "cross scope?").unwrap();

        let v: serde_json::Value =
            serde_json::from_str(&mgr.activity_graph_json(Some("/ws/alpha"))).unwrap();

        let ids: Vec<&str> =
            v["agents"].as_array().unwrap().iter().map(|a| a["agent_id"].as_str().unwrap()).collect();
        assert_eq!(ids.len(), 2, "only /ws/alpha's agents");
        assert!(ids.contains(&"a") && ids.contains(&"b") && !ids.contains(&"c"));

        // a→c crosses out of scope and is dropped; a→b survives.
        let edges = v["edges"].as_array().unwrap();
        assert!(
            edges.iter().all(|e| e["target"] != "c" && e["source"] != "c"),
            "edges touching the out-of-scope agent are filtered"
        );
        assert!(edges.iter().any(|e| e["source"] == "a" && e["target"] == "b"));

        // shared.rs stays contended on a+b (still ≥2 in-scope writers); c's write
        // is filtered out rather than inflating the workspace's contention.
        let cont = v["contention"].as_array().unwrap();
        assert_eq!(cont.len(), 1);
        assert_eq!(cont[0]["path"], "shared.rs");
        let terms: Vec<&str> =
            cont[0]["terminals"].as_array().unwrap().iter().map(|t| t.as_str().unwrap()).collect();
        assert!(terms.contains(&"a") && terms.contains(&"b") && !terms.contains(&"c"));
    }

    #[test]
    fn agents_query_lists_live_agents_with_workspace_root() {
        let mgr = mem_manager();
        let dir = std::env::temp_dir().join(format!("taime-sess-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let spec = taime_protocol::SpawnSpec {
            prog: "sleep".into(),
            args: vec!["10".into()],
            cwd: Some(dir.to_string_lossy().into_owned()),
            env: vec![],
            rows: 24,
            cols: 80,
            agent_id: Some("a".into()),
        };
        mgr.spawn(spec).unwrap();

        let root = dir.to_string_lossy().into_owned();

        // One flat row per live agent: identity is the Agent ID, the workspace
        // grouping key travels as `workspace_root` (no tmux-shaped fields).
        let agents: serde_json::Value = serde_json::from_str(&mgr.query("agents", "{}")).unwrap();
        assert_eq!(agents.as_array().unwrap().len(), 1);
        assert_eq!(agents[0]["agent_id"], "a");
        assert_eq!(agents[0]["workspace_root"], root);
        assert_eq!(agents[0]["alive"], true);
        assert!(agents[0].get("tmux_session").is_none());
        assert!(agents[0].get("tmux_window").is_none());

        mgr.kill_all();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn workspace_members_prefers_exact_root_over_basename() {
        let mgr = mem_manager();
        let base = std::env::temp_dir().join(format!("taime-dup-{}", std::process::id()));
        let a = base.join("alpha").join("proj");
        let b = base.join("beta").join("proj");
        for d in [&a, &b] {
            let _ = std::fs::create_dir_all(d);
        }
        let spawn = |cwd: &std::path::Path, key: &str| {
            mgr.spawn(taime_protocol::SpawnSpec {
                prog: "sleep".into(),
                args: vec!["10".into()],
                cwd: Some(cwd.to_string_lossy().into_owned()),
                env: vec![],
                rows: 24,
                cols: 80,
                agent_id: Some(key.into()),
            })
            .unwrap();
        };
        spawn(&a, "ka");
        spawn(&b, "kb");

        // An exact full-root lookup must return ONLY that workspace's agent — two
        // workspaces sharing the basename "proj" must not merge (this membership
        // resolution backs the contention surface).
        let ids: Vec<String> = mgr
            .workspace_members(&a.to_string_lossy())
            .into_iter()
            .filter_map(|(s, _)| s.agent_id)
            .collect();
        assert_eq!(ids, vec!["ka"], "exact root must not merge same-basename workspaces");

        mgr.kill_all();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn request_and_reply_record_graph_edges() {
        let mgr = mem_manager();
        let iid = mgr.request("a", "b", "status?").unwrap();
        mgr.reply("b", &iid, "all green").unwrap();
        let v: serde_json::Value = serde_json::from_str(&mgr.activity_graph_json(None)).unwrap();
        let kinds: Vec<&str> =
            v["edges"].as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap()).collect();
        assert!(kinds.contains(&"request"), "request edge missing: {kinds:?}");
        assert!(kinds.contains(&"reply"), "reply edge missing: {kinds:?}");
    }

    #[test]
    fn worktree_query_returns_full_shape_with_branch() {
        let mgr = mem_manager();
        let info = WorktreeInfo {
            agent_id: "a".into(),
            project_root: "/proj".into(),
            repo_root: Some("/proj".into()),
            worktree_path: "/wt/a".into(),
            branch: Some("taime/a".into()),
            base_sha: Some("abc".into()),
            mode: "isolated".into(),
            error: None,
        };
        mgr.store().unwrap().upsert_worktree(&info, "claude_code", 1).unwrap();

        let v: serde_json::Value =
            serde_json::from_str(&mgr.query("worktree", r#"{"agent_id":"a"}"#)).unwrap();
        // The app's getWorktree reads agent_id/branch/mode/project_root.
        assert_eq!(v["agent_id"], "a");
        assert_eq!(v["branch"], "taime/a");
        assert_eq!(v["mode"], "isolated");
        assert_eq!(v["project_root"], "/proj");
    }

    #[test]
    fn schedules_query_includes_the_prompt_body() {
        let mgr = mem_manager();
        mgr.store()
            .unwrap()
            .upsert_schedule(&ScheduleRow {
                name: "daily-review".into(),
                file_path: "/tmp/daily-review.md".into(),
                schedule: "0 9 * * *".into(),
                agent_profile: "default".into(),
                provider: "claude_code".into(),
                script: None,
                prompt: Some("Review yesterday's commits".into()),
                last_run: None,
                next_run: Some(1),
                enabled: true,
                workspace_root: Some("/proj".into()),
                task_mode: None,
                task_id: None,
                shared: false,
            })
            .unwrap();

        let v: serde_json::Value = serde_json::from_str(&mgr.query("schedules", "{}")).unwrap();
        // The SchedulesScreen reads name/schedule/enabled and the prompt body.
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(v[0]["name"], "daily-review");
        assert_eq!(v[0]["schedule"], "0 9 * * *");
        assert_eq!(v[0]["enabled"], true);
        assert_eq!(v[0]["workspace_root"], "/proj");
        assert_eq!(v[0]["prompt"], "Review yesterday's commits");
    }

    #[test]
    fn attribution_maps_files_to_their_authors() {
        let mgr = mem_manager();
        let store = mgr.store().unwrap();
        store.record_fs_event("e1", "a", "src/x.rs", "create", 5).unwrap();
        store.record_fs_event("e2", "a", "src/y.rs", "modify", 6).unwrap();

        let v: serde_json::Value = serde_json::from_str(&mgr.attribution_json("a")).unwrap();
        // No live session in for_test → team falls back to the reviewed agent.
        assert_eq!(v["team"][0]["agent_id"], "a");
        assert_eq!(v["files"]["src/x.rs"]["last"]["agent_id"], "a");
        assert_eq!(v["files"]["src/y.rs"]["contributors"][0]["agent_id"], "a");
    }

    #[test]
    fn seeded_example_workflows_parse_and_validate() {
        crate::workflow::parse_workflow(EXAMPLE_FEATURE_REVIEW).expect("feature-with-review valid");
        crate::workflow::parse_workflow(EXAMPLE_FIX_VERIFY).expect("fix-and-verify valid");
    }

    #[test]
    fn workflow_create_query_persists_and_lists_immediately() {
        let mgr = mem_manager();
        // Unique name: create_workflow also writes ~/.taime/workflows/<name>.json,
        // so this must not collide with (or clobber) a real workflow.
        let name = format!("taime-test-wf-create-{}", std::process::id());
        let def = serde_json::json!({
            "name": name,
            "entry": "build",
            "nodes": [
                { "id": "build", "profile": "feature-builder", "prompt": "Implement X." },
                { "id": "check", "prompt": "Run tests; reply PASS or FAIL." }
            ],
            "edges": [
                { "from": "build", "to": "check", "when": "always" },
                { "from": "check", "to": "build", "when": "keyword:FAIL" }
            ]
        });
        let args = serde_json::json!({ "definition": def.to_string() }).to_string();

        let v: serde_json::Value =
            serde_json::from_str(&mgr.query("workflow_create", &args)).unwrap();
        assert_eq!(v["ok"], true, "create failed: {v}");
        assert_eq!(v["name"], name.as_str());

        // The new workflow shows in the same `workflows` query the panel reads,
        // immediately and source-tagged "user" (DB row, not just the file).
        let list: serde_json::Value = serde_json::from_str(&mgr.query("workflows", "{}")).unwrap();
        let entry = list
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["name"] == name.as_str())
            .expect("created workflow listed by the workflows query");
        assert_eq!(entry["source"], "user");
        assert_eq!(entry["entry"], "build");
        assert_eq!(entry["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(entry["edges"].as_array().unwrap().len(), 2);

        // Cleanup: removes both the store row and the ~/.taime/workflows file.
        mgr.delete_workflow(&name).unwrap();
        assert!(!Manager::workflows_dir().join(format!("{name}.json")).exists());
    }

    #[test]
    fn workflow_create_query_rejects_invalid_definitions_without_creating() {
        let mgr = mem_manager();
        let create = |def: &str| -> serde_json::Value {
            let args = serde_json::json!({ "definition": def }).to_string();
            serde_json::from_str(&mgr.query("workflow_create", &args)).unwrap()
        };

        // A dangling edge target is a structured ok:false with the validation
        // message — never a wire error.
        let bad_target = r#"{"name":"taime-test-bad-target","entry":"a",
            "nodes":[{"id":"a","prompt":"p"}],
            "edges":[{"from":"a","to":"missing","when":"always"}]}"#;
        let v = create(bad_target);
        assert_eq!(v["ok"], false);
        assert!(
            v["error"].as_str().unwrap().contains("edge to unknown node 'missing'"),
            "unexpected error: {v}"
        );

        // So is a bad `when` condition.
        let bad_when = r#"{"name":"taime-test-bad-when","entry":"a",
            "nodes":[{"id":"a","prompt":"p"}],
            "edges":[{"from":"a","to":"a","when":"nope"}]}"#;
        let v = create(bad_when);
        assert_eq!(v["ok"], false);
        assert!(
            v["error"].as_str().unwrap().contains("unknown edge condition"),
            "unexpected error: {v}"
        );

        // Nothing was created in either store: no DB rows...
        let list: serde_json::Value = serde_json::from_str(&mgr.query("workflows", "{}")).unwrap();
        assert!(list.as_array().unwrap().is_empty(), "rejected workflows must not persist");
        // ...and no ~/.taime/workflows files (validation precedes the write).
        for f in ["taime-test-bad-target.json", "taime-test-bad-when.json"] {
            assert!(!Manager::workflows_dir().join(f).exists());
        }
    }

    #[test]
    fn basename_takes_last_path_component() {
        assert_eq!(basename("/Users/me/projects/taime"), "taime");
        assert_eq!(basename("/Users/me/projects/taime/"), "taime");
        assert_eq!(basename("taime"), "taime");
        assert_eq!(basename(""), "");
    }

    #[test]
    fn agents_surface_is_real_shaped_when_empty() {
        // No live agents → an empty list (NOT the old hardcoded stub path; this
        // exercises the real query dispatch). The retired surfaces stay retired.
        let mgr = Manager::for_test(None);
        let agents: serde_json::Value = serde_json::from_str(&mgr.agents_json()).unwrap();
        assert!(agents.as_array().unwrap().is_empty());

        // The generic Query dispatch routes to the same real implementation.
        let via_query: serde_json::Value =
            serde_json::from_str(&mgr.query("agents", "{}")).unwrap();
        assert!(via_query.as_array().unwrap().is_empty());

        // The retired tmux-shaped queries are gone, not aliased.
        for retired in ["sessions", "session_detail"] {
            let v: serde_json::Value = serde_json::from_str(&mgr.query(retired, "{}")).unwrap();
            assert!(v.get("error").is_some(), "{retired} must be an unknown query");
        }
    }

    // ---- Review pipeline end-to-end: the `apply_selection` query arm driven
    // ---- with the UI's ACTUAL arguments (symbolic "main"/"self"/agent-id
    // ---- targets — which used to reach `git -C` as literal directories).

    use std::path::{Path, PathBuf};

    fn git_in(cwd: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").current_dir(cwd).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A real project repo with one commit of `a.txt` — the "main" checkout.
    fn project_repo() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("taime-mgr-e2e-{:08x}", rand::random::<u32>()));
        std::fs::create_dir_all(&dir).unwrap();
        git_in(&dir, &["init", "-q"]);
        git_in(&dir, &["config", "user.email", "t@t"]);
        git_in(&dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        git_in(&dir, &["add", "-A"]);
        git_in(&dir, &["commit", "-qm", "init"]);
        dir
    }

    /// Provision a REAL isolated `git worktree` for `agent_id` at the project's
    /// HEAD and record its durable row — what the symbolic-target resolver and
    /// `diff_context` read.
    fn seed_isolated_agent(mgr: &Manager, proj: &Path, agent_id: &str) -> PathBuf {
        let base = git_in(proj, &["rev-parse", "HEAD"]);
        let wt = std::env::temp_dir().join(format!("taime-mgr-wt-{agent_id}-{:08x}", rand::random::<u32>()));
        let branch = format!("taime/test-{agent_id}");
        git_in(proj, &["worktree", "add", "-q", "-b", &branch, wt.to_str().unwrap(), &base]);
        mgr.store()
            .unwrap()
            .upsert_worktree(
                &WorktreeInfo {
                    agent_id: agent_id.into(),
                    project_root: proj.to_string_lossy().into_owned(),
                    repo_root: Some(proj.to_string_lossy().into_owned()),
                    worktree_path: wt.to_string_lossy().into_owned(),
                    branch: Some(branch),
                    base_sha: Some(base),
                    mode: "isolated".into(),
                    error: None,
                },
                "claude_code",
                1,
            )
            .unwrap();
        wt
    }

    fn apply_args(agent: &str, target: &str, mode: &str, selections: serde_json::Value) -> String {
        // Exactly the payload api.ts `applySelection` sends.
        serde_json::json!({
            "agent_id": agent, "target_dir": target, "mode": mode, "selections": selections
        })
        .to_string()
    }

    fn apply_args_with_digest(
        agent: &str,
        target: &str,
        mode: &str,
        selections: serde_json::Value,
        digest: &str,
    ) -> String {
        serde_json::json!({
            "agent_id": agent, "target_dir": target, "mode": mode,
            "selections": selections, "expected_digest": digest
        })
        .to_string()
    }

    /// Record the review ack the merge gate requires — the same `mark_reviewed`
    /// query the review surfaces issue when the user merges/marks reviewed.
    fn ack_review(mgr: &Manager, agent: &str) {
        let args = serde_json::json!({ "agent_id": agent }).to_string();
        assert_eq!(mgr.query("mark_reviewed", &args), "true");
    }

    /// Fetch the served diff's digest the way the UI does (hunked_diff) — the
    /// reviewed-content fingerprint apply_selection requires for merges.
    fn fetch_digest(mgr: &Manager, agent: &str) -> String {
        let h: serde_json::Value = serde_json::from_str(
            &mgr.query("hunked_diff", &serde_json::json!({ "agent_id": agent }).to_string()),
        )
        .unwrap();
        h["digest"].as_str().expect("hunked_diff serves a digest").to_string()
    }

    /// The full UI merge flow: review (hunked_diff → digest), ack, apply.
    fn ui_merge(mgr: &Manager, agent: &str, target: &str, selections: serde_json::Value) -> serde_json::Value {
        let digest = fetch_digest(mgr, agent);
        ack_review(mgr, agent);
        serde_json::from_str(
            &mgr.query("apply_selection", &apply_args_with_digest(agent, target, "merge", selections, &digest)),
        )
        .unwrap()
    }

    #[test]
    fn apply_selection_resolves_main_symbol_to_the_project_checkout() {
        let mgr = mem_manager();
        let proj = project_repo();
        let wt = seed_isolated_agent(&mgr, &proj, "agent-a");
        std::fs::write(wt.join("a.txt"), "one\nTWO\nthree\n").unwrap();

        let v = ui_merge(&mgr, "agent-a", "main", serde_json::json!({ "a.txt": [0] }));
        assert_eq!(v["applied"], true, "merge to 'main' failed: {v}");
        assert!(
            std::fs::read_to_string(proj.join("a.txt")).unwrap().contains("TWO"),
            "the hunk must land in the project checkout"
        );
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[test]
    fn apply_selection_resolves_self_symbol_for_revert() {
        let mgr = mem_manager();
        let proj = project_repo();
        let wt = seed_isolated_agent(&mgr, &proj, "agent-a");
        std::fs::write(wt.join("a.txt"), "one\nTWO\nthree\n").unwrap();

        // Deliberately NO review ack: revert (discarding the agent's own work
        // back to base) is the safe direction and stays ungated — toward self.
        let v: serde_json::Value = serde_json::from_str(
            &mgr.query("apply_selection", &apply_args("agent-a", "self", "revert", serde_json::json!({ "a.txt": null }))),
        )
        .unwrap();
        assert_eq!(v["applied"], true, "revert from 'self' failed: {v}");
        assert_eq!(
            std::fs::read_to_string(wt.join("a.txt")).unwrap(),
            "one\ntwo\nthree\n",
            "the agent's worktree must be back at base"
        );
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[test]
    fn revert_refuses_any_target_but_self() {
        let mgr = mem_manager();
        let proj = project_repo();
        let wt_a = seed_isolated_agent(&mgr, &proj, "agent-a");
        let _wt_b = seed_isolated_agent(&mgr, &proj, "agent-b");
        std::fs::write(wt_a.join("a.txt"), "one\nTWO\nthree\n").unwrap();

        // Reverts skip the review ack BECAUSE they only discard the agent's
        // own work; aimed at "main" or a sibling they'd be ungated cross-tree
        // destruction (reverse-applying merged work from the mainline).
        for target in ["main", "agent-b"] {
            let v: serde_json::Value = serde_json::from_str(
                &mgr.query("apply_selection", &apply_args("agent-a", target, "revert", serde_json::json!({ "a.txt": null }))),
            )
            .unwrap();
            assert_eq!(v["applied"], false, "revert to '{target}' must be refused: {v}");
            assert!(v["error"].as_str().unwrap().contains("own worktree"), "{v}");
        }
        assert_eq!(
            std::fs::read_to_string(proj.join("a.txt")).unwrap(),
            "one\ntwo\nthree\n",
            "the project checkout is untouched"
        );
        let _ = std::fs::remove_dir_all(&wt_a);
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[test]
    fn apply_selection_merges_into_a_same_workspace_sibling() {
        let mgr = mem_manager();
        let proj = project_repo();
        let wt_a = seed_isolated_agent(&mgr, &proj, "agent-a");
        let wt_b = seed_isolated_agent(&mgr, &proj, "agent-b");
        std::fs::write(wt_a.join("a.txt"), "one\nTWO\nthree\n").unwrap();

        let v = ui_merge(&mgr, "agent-a", "agent-b", serde_json::json!({ "a.txt": null }));
        assert_eq!(v["applied"], true, "merge to sibling failed: {v}");
        assert!(std::fs::read_to_string(wt_b.join("a.txt")).unwrap().contains("TWO"));
        let _ = std::fs::remove_dir_all(&wt_a);
        let _ = std::fs::remove_dir_all(&wt_b);
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[test]
    fn shared_mode_sibling_is_not_a_merge_target() {
        let mgr = mem_manager();
        let proj = project_repo();
        let wt = seed_isolated_agent(&mgr, &proj, "agent-a");
        // agent-b's isolation failed → worktree.rs's shared fallback records
        // the USER'S project dir as its "worktree". Merging "into agent-b"
        // must refuse, not silently write the mainline under another name.
        mgr.store()
            .unwrap()
            .upsert_worktree(
                &WorktreeInfo {
                    agent_id: "agent-b".into(),
                    project_root: proj.to_string_lossy().into_owned(),
                    repo_root: None,
                    worktree_path: proj.to_string_lossy().into_owned(),
                    branch: None,
                    base_sha: None,
                    mode: "shared".into(),
                    error: None,
                },
                "claude_code",
                2,
            )
            .unwrap();
        std::fs::write(wt.join("a.txt"), "one\nTWO\nthree\n").unwrap();

        let v = ui_merge(&mgr, "agent-a", "agent-b", serde_json::json!({ "a.txt": null }));
        assert_eq!(v["applied"], false, "shared sibling must be refused: {v}");
        assert!(v["error"].as_str().unwrap().contains("shares the project directory"), "{v}");
        assert_eq!(
            std::fs::read_to_string(proj.join("a.txt")).unwrap(),
            "one\ntwo\nthree\n",
            "the user's checkout is untouched"
        );
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[test]
    fn apply_selection_rejects_non_symbolic_and_cross_workspace_targets() {
        let mgr = mem_manager();
        let proj = project_repo();
        let wt = seed_isolated_agent(&mgr, &proj, "agent-a");
        // A sibling row in a DIFFERENT workspace — never a valid target.
        mgr.store()
            .unwrap()
            .upsert_worktree(
                &WorktreeInfo {
                    agent_id: "agent-z".into(),
                    project_root: "/elsewhere".into(),
                    repo_root: None,
                    worktree_path: "/elsewhere".into(),
                    branch: None,
                    base_sha: None,
                    mode: "shared".into(),
                    error: None,
                },
                "claude_code",
                2,
            )
            .unwrap();
        std::fs::write(wt.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        // Acked + digested, so target rejection (not the review gate) is what's
        // exercised.
        let digest = fetch_digest(&mgr, "agent-a");
        ack_review(&mgr, "agent-a");

        // A literal directory must NOT be treated as a path (the old behavior).
        for bad in ["no-such-agent", "/tmp", "agent-z"] {
            let v: serde_json::Value = serde_json::from_str(&mgr.query(
                "apply_selection",
                &apply_args_with_digest("agent-a", bad, "merge", serde_json::json!({ "a.txt": null }), &digest),
            ))
            .unwrap();
            assert_eq!(v["applied"], false, "target '{bad}' must be rejected");
            assert!(v["error"].as_str().unwrap_or("").contains(bad), "error names the target: {v}");
        }
        assert_eq!(
            std::fs::read_to_string(proj.join("a.txt")).unwrap(),
            "one\ntwo\nthree\n",
            "nothing may be applied anywhere on a rejected target"
        );
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[test]
    fn merge_is_refused_without_a_standing_review_ack() {
        let mgr = mem_manager();
        let proj = project_repo();
        let wt = seed_isolated_agent(&mgr, &proj, "agent-a");
        std::fs::write(wt.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        let digest = fetch_digest(&mgr, "agent-a");
        let merge = || -> serde_json::Value {
            serde_json::from_str(&mgr.query(
                "apply_selection",
                &apply_args_with_digest("agent-a", "main", "merge", serde_json::json!({ "a.txt": null }), &digest),
            ))
            .unwrap()
        };

        // No ack → the daemon refuses, and nothing touches the project checkout.
        let v = merge();
        assert_eq!(v["applied"], false, "unacked merge must be refused: {v}");
        assert!(v["error"].as_str().unwrap().contains("review"), "refusal names the gate: {v}");
        assert_eq!(std::fs::read_to_string(proj.join("a.txt")).unwrap(), "one\ntwo\nthree\n");

        // Acked → the same merge lands.
        ack_review(&mgr, "agent-a");
        let v = merge();
        assert_eq!(v["applied"], true, "acked merge must apply: {v}");
        assert!(std::fs::read_to_string(proj.join("a.txt")).unwrap().contains("TWO"));
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[test]
    fn merge_requires_the_reviewed_diff_digest() {
        let mgr = mem_manager();
        let proj = project_repo();
        let wt = seed_isolated_agent(&mgr, &proj, "agent-a");
        std::fs::write(wt.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        ack_review(&mgr, "agent-a");

        // Acked but WITHOUT the hunked_diff digest → refused (nothing binds the
        // ack to the content being merged).
        let v: serde_json::Value = serde_json::from_str(&mgr.query(
            "apply_selection",
            &apply_args("agent-a", "main", "merge", serde_json::json!({ "a.txt": null })),
        ))
        .unwrap();
        assert_eq!(v["applied"], false, "digest-less merge must be refused: {v}");
        assert!(v["error"].as_str().unwrap().contains("expected_digest"), "{v}");

        // With a STALE digest (worktree moved after review) → stale refusal.
        let digest = fetch_digest(&mgr, "agent-a");
        std::fs::write(wt.join("a.txt"), "one\nTWO\nTHREE\n").unwrap();
        let v: serde_json::Value = serde_json::from_str(&mgr.query(
            "apply_selection",
            &apply_args_with_digest("agent-a", "main", "merge", serde_json::json!({ "a.txt": null }), &digest),
        ))
        .unwrap();
        assert_eq!(v["applied"], false, "stale digest must refuse: {v}");
        assert_eq!(v["stale"], true, "{v}");
        assert_eq!(
            std::fs::read_to_string(proj.join("a.txt")).unwrap(),
            "one\ntwo\nthree\n",
            "nothing lands from an unreviewed worktree state"
        );
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[test]
    fn empty_selections_are_refused_never_merge_all() {
        let mgr = mem_manager();
        let proj = project_repo();
        let wt = seed_isolated_agent(&mgr, &proj, "agent-a");
        std::fs::write(wt.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        let digest = fetch_digest(&mgr, "agent-a");
        ack_review(&mgr, "agent-a");

        // "{}" used to mean "everything" — one RPC could merge (or worse,
        // revert) an entire tree nobody enumerated. Now it's a refusal, for
        // both modes.
        for (mode, target) in [("merge", "main"), ("revert", "self")] {
            let v: serde_json::Value = serde_json::from_str(&mgr.query(
                "apply_selection",
                &apply_args_with_digest("agent-a", target, mode, serde_json::json!({}), &digest),
            ))
            .unwrap();
            assert_eq!(v["applied"], false, "{mode} with empty selections must refuse: {v}");
            assert!(v["error"].as_str().unwrap().contains("nothing selected"), "{v}");
        }
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[test]
    fn clear_dirty_drops_the_ack_and_re_arms_the_merge_gate() {
        let mgr = mem_manager();
        let proj = project_repo();
        let wt = seed_isolated_agent(&mgr, &proj, "agent-a");
        std::fs::write(wt.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        let digest = fetch_digest(&mgr, "agent-a");
        ack_review(&mgr, "agent-a");
        assert!(mgr.store().unwrap().is_reviewed("agent-a").unwrap());

        // A fresh review cycle (clear_dirty) drops the standing ack…
        let _ = mgr.query("clear_dirty", &serde_json::json!({ "agent_id": "agent-a" }).to_string());
        assert!(!mgr.store().unwrap().is_reviewed("agent-a").unwrap());

        // …so the next merge is refused until the diff is reviewed again.
        let v: serde_json::Value = serde_json::from_str(&mgr.query(
            "apply_selection",
            &apply_args_with_digest("agent-a", "main", "merge", serde_json::json!({ "a.txt": null }), &digest),
        ))
        .unwrap();
        assert_eq!(v["applied"], false, "merge after a cleared ack must be refused: {v}");
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[test]
    fn mark_reviewed_reports_failure_without_persistence() {
        // With no store an ack can never be recorded; answering "true" anyway
        // would send the client into an inexplicable merge refusal.
        let mgr = Manager::for_test(None);
        let args = serde_json::json!({ "agent_id": "agent-a" }).to_string();
        assert_eq!(mgr.query("mark_reviewed", &args), "false");

        // And the merge refusal itself names the real cause.
        let v: serde_json::Value = serde_json::from_str(&mgr.query(
            "apply_selection",
            &apply_args("agent-a", "main", "merge", serde_json::json!({ "a.txt": null })),
        ))
        .unwrap();
        assert_eq!(v["applied"], false);
        assert!(v["error"].as_str().unwrap().contains("persistence"), "{v}");
    }

    #[test]
    fn untracked_agent_files_flow_through_review_end_to_end() {
        let mgr = mem_manager();
        let proj = project_repo();
        let wt = seed_isolated_agent(&mgr, &proj, "agent-a");
        // The most common agent output: a brand-new file, never committed.
        std::fs::write(wt.join("generated.rs"), "fn agent_made_this() {}\n").unwrap();

        // It must be hunked (selectable), not just listed.
        let h: serde_json::Value = serde_json::from_str(
            &mgr.query("hunked_diff", &serde_json::json!({ "agent_id": "agent-a" }).to_string()),
        )
        .unwrap();
        let file = h["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["path"] == "generated.rs")
            .expect("untracked file appears on the hunked surface");
        assert!(!file["hunks"].as_array().unwrap().is_empty(), "untracked file has hunks");

        // And the UI's selection of that hunk must merge it into 'main'.
        let v = ui_merge(&mgr, "agent-a", "main", serde_json::json!({ "generated.rs": [0] }));
        assert_eq!(v["applied"], true, "untracked merge failed: {v}");
        assert_eq!(
            std::fs::read_to_string(proj.join("generated.rs")).unwrap(),
            "fn agent_made_this() {}\n",
            "the agent-created file must land in the project checkout"
        );
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[test]
    fn reclaimed_agent_stays_reviewable_and_mergeable_from_archive() {
        let mgr = mem_manager();
        let proj = project_repo();
        let wt = seed_isolated_agent(&mgr, &proj, "agent-a");
        // A modification + a brand-new file — the agent's full change set.
        std::fs::write(wt.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        std::fs::write(wt.join("generated.rs"), "fn agent_made_this() {}\n").unwrap();

        // The live hunked surface, for a before/after comparison.
        let live: serde_json::Value = serde_json::from_str(
            &mgr.query("hunked_diff", &serde_json::json!({ "agent_id": "agent-a" }).to_string()),
        )
        .unwrap();
        let live_paths: Vec<String> = live["files"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|f| f["path"].as_str().map(String::from))
            .collect();
        assert!(live_paths.iter().any(|p| p == "a.txt"));
        assert!(live_paths.iter().any(|p| p == "generated.rs"));

        // Reclaim: archive the worktree, then remove the physical checkout.
        assert!(mgr.reclaim_agent("agent-a"), "reclaim should succeed for a dead agent");
        assert!(!wt.exists(), "physical checkout reclaimed");
        let wjson: serde_json::Value = serde_json::from_str(
            &mgr.query("worktree", &serde_json::json!({ "agent_id": "agent-a" }).to_string()),
        )
        .unwrap();
        assert_eq!(wjson["reclaimed"], true, "row marks the agent reclaimed");

        // STILL reviewable — now rendered from the archive, with the same files.
        let arch: serde_json::Value = serde_json::from_str(
            &mgr.query("hunked_diff", &serde_json::json!({ "agent_id": "agent-a" }).to_string()),
        )
        .unwrap();
        let arch_paths: Vec<String> = arch["files"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|f| f["path"].as_str().map(String::from))
            .collect();
        assert!(arch_paths.iter().any(|p| p == "a.txt"), "archived diff still shows the edit");
        assert!(
            arch_paths.iter().any(|p| p == "generated.rs"),
            "archived diff still shows the new file"
        );

        // file_diffs (side-by-side) reconstructs both sides from the refs.
        let fd: serde_json::Value = serde_json::from_str(
            &mgr.query("file_diffs", &serde_json::json!({ "agent_id": "agent-a" }).to_string()),
        )
        .unwrap();
        let a_entry = fd["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["path"] == "a.txt")
            .expect("a.txt present in archived file_diffs");
        assert!(a_entry["original"].as_str().unwrap().contains("two"), "original side from base");
        assert!(a_entry["modified"].as_str().unwrap().contains("TWO"), "modified side from archive");

        // STILL mergeable — the selected hunks merge into 'main' from the archive,
        // and the agent-created file lands in the project checkout.
        let v = ui_merge(
            &mgr,
            "agent-a",
            "main",
            serde_json::json!({ "a.txt": [0], "generated.rs": [0] }),
        );
        assert_eq!(v["applied"], true, "merge-from-archive failed: {v}");
        assert!(std::fs::read_to_string(proj.join("a.txt")).unwrap().contains("TWO"));
        assert_eq!(
            std::fs::read_to_string(proj.join("generated.rs")).unwrap(),
            "fn agent_made_this() {}\n",
        );

        // Revert is unavailable post-reclaim (no checkout to revert into).
        ack_review(&mgr, "agent-a");
        let rev: serde_json::Value = serde_json::from_str(&mgr.query(
            "apply_selection",
            &serde_json::json!({
                "agent_id": "agent-a", "target_dir": "self", "mode": "revert",
                "selections": { "a.txt": [0] }
            })
            .to_string(),
        ))
        .unwrap();
        assert_eq!(rev["applied"], false, "revert of a reclaimed agent is refused");

        let _ = std::fs::remove_dir_all(&proj);
    }

    // ---- commit_merge: the provenance-commit path (gap #1). The merge no longer
    // ---- dead-ends at `git apply` — it records ONE commit with the trailer.

    fn commit_merge_args(
        agent: &str,
        target: &str,
        selections: serde_json::Value,
        digest: Option<&str>,
    ) -> String {
        let mut o = serde_json::json!({
            "agent_id": agent, "target_dir": target, "selections": selections
        });
        if let Some(d) = digest {
            o["expected_digest"] = serde_json::json!(d);
        }
        o.to_string()
    }

    fn commit_merge_args_push(
        agent: &str,
        target: &str,
        selections: serde_json::Value,
        digest: &str,
        push: bool,
    ) -> String {
        serde_json::json!({
            "agent_id": agent, "target_dir": target, "selections": selections,
            "expected_digest": digest, "push": push
        })
        .to_string()
    }

    #[test]
    fn commit_merge_records_an_autonomous_provenance_commit() {
        let mgr = mem_manager();
        let proj = project_repo();
        let wt = seed_isolated_agent(&mgr, &proj, "agent-a");
        std::fs::write(wt.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        let head_before = git_in(&proj, &["rev-parse", "HEAD"]);
        let digest = fetch_digest(&mgr, "agent-a");

        // NO ack — autonomy is primary; the merge proceeds and records reviewed:false.
        let v: serde_json::Value = serde_json::from_str(&mgr.query(
            "commit_merge",
            &commit_merge_args("agent-a", "main", serde_json::json!({ "a.txt": [0] }), Some(&digest)),
        ))
        .unwrap();
        assert_eq!(v["committed"], true, "autonomous merge should commit: {v}");
        assert!(v["commit"].as_str().is_some_and(|s| !s.is_empty()), "{v}");
        assert!(std::fs::read_to_string(proj.join("a.txt")).unwrap().contains("TWO"));

        let head_after = git_in(&proj, &["rev-parse", "HEAD"]);
        assert_ne!(head_before, head_after, "a provenance commit was recorded");
        let body = git_in(&proj, &["log", "-1", "--format=%B"]);
        assert!(body.contains("Co-authored-by: Claude Code <agent-agent-a@taime.local>"), "{body}");
        assert!(body.contains("Taime-Agent-Id: agent-a"), "{body}");
        assert!(body.contains("Taime-Reviewed: false"), "autonomous merge recorded: {body}");
        assert!(body.contains("Taime-Patch-Digest:"), "{body}");
        assert!(body.contains("Taime-Base-Sha:"), "{body}");
        // A LIVE agent is NOT snapshotted at merge: the trailer omits the archive
        // ref (it materializes at reclaim, under the same id), and no archive ref
        // is set on the live row — which delete-workspace HARD would otherwise
        // treat as reclaimed, destructible work.
        assert!(!body.contains("Taime-Archive-Ref"), "no archive ref for a live agent: {body}");
        assert!(
            !crate::worktree::archive_ref_exists(&proj.to_string_lossy(), "agent-a"),
            "a live merge must not create an archive ref"
        );
        let wrow = mgr.store().unwrap().worktree_row("agent-a").unwrap().unwrap();
        assert!(wrow.archive_ref.is_none(), "live agent's row has no archive_ref after merge");
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[test]
    fn commit_merge_records_reviewed_true_when_acked() {
        let mgr = mem_manager();
        let proj = project_repo();
        let wt = seed_isolated_agent(&mgr, &proj, "agent-a");
        std::fs::write(wt.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        let digest = fetch_digest(&mgr, "agent-a");
        ack_review(&mgr, "agent-a"); // the user opted into review

        let v: serde_json::Value = serde_json::from_str(&mgr.query(
            "commit_merge",
            &commit_merge_args("agent-a", "main", serde_json::json!({ "a.txt": [0] }), Some(&digest)),
        ))
        .unwrap();
        assert_eq!(v["committed"], true, "{v}");
        let body = git_in(&proj, &["log", "-1", "--format=%B"]);
        assert!(body.contains("Taime-Reviewed: true"), "reviewed merge recorded: {body}");
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[test]
    fn commit_merge_requires_a_digest_and_refuses_self() {
        let mgr = mem_manager();
        let proj = project_repo();
        let wt = seed_isolated_agent(&mgr, &proj, "agent-a");
        std::fs::write(wt.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        let head_before = git_in(&proj, &["rev-parse", "HEAD"]);

        // No digest → refused (the integrity floor is never relaxed).
        let v: serde_json::Value = serde_json::from_str(&mgr.query(
            "commit_merge",
            &commit_merge_args("agent-a", "main", serde_json::json!({ "a.txt": [0] }), None),
        ))
        .unwrap();
        assert_eq!(v["committed"], false, "{v}");
        assert!(v["error"].as_str().unwrap().contains("expected_digest"), "{v}");

        // Self target → refused (a merge is always forward).
        let digest = fetch_digest(&mgr, "agent-a");
        let v: serde_json::Value = serde_json::from_str(&mgr.query(
            "commit_merge",
            &commit_merge_args("agent-a", "self", serde_json::json!({ "a.txt": [0] }), Some(&digest)),
        ))
        .unwrap();
        assert_eq!(v["committed"], false, "{v}");
        assert!(v["error"].as_str().unwrap().contains("not to itself"), "{v}");

        assert_eq!(head_before, git_in(&proj, &["rev-parse", "HEAD"]), "no commit on refusal");
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[test]
    fn commit_merge_from_a_reclaimed_archive() {
        let mgr = mem_manager();
        let proj = project_repo();
        let wt = seed_isolated_agent(&mgr, &proj, "agent-a");
        std::fs::write(wt.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        assert!(mgr.reclaim_agent("agent-a"), "reclaim should succeed");
        assert!(!wt.exists(), "checkout reclaimed");

        let digest = fetch_digest(&mgr, "agent-a"); // served from the archive now
        let v: serde_json::Value = serde_json::from_str(&mgr.query(
            "commit_merge",
            &commit_merge_args("agent-a", "main", serde_json::json!({ "a.txt": [0] }), Some(&digest)),
        ))
        .unwrap();
        assert_eq!(v["committed"], true, "merge-from-archive should commit: {v}");
        assert!(std::fs::read_to_string(proj.join("a.txt")).unwrap().contains("TWO"));
        let body = git_in(&proj, &["log", "-1", "--format=%B"]);
        assert!(body.contains("Taime-Archive-Ref: refs/taime/archive/agent-a"), "{body}");
        assert!(body.contains("Taime-Base-Sha:"), "{body}");
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[test]
    fn commit_merge_records_ledger_note_and_export() {
        let mgr = mem_manager();
        let proj = project_repo();
        let wt = seed_isolated_agent(&mgr, &proj, "agent-a");
        std::fs::write(wt.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        let digest = fetch_digest(&mgr, "agent-a");

        let v: serde_json::Value = serde_json::from_str(&mgr.query(
            "commit_merge",
            &commit_merge_args("agent-a", "main", serde_json::json!({ "a.txt": [0] }), Some(&digest)),
        ))
        .unwrap();
        assert_eq!(v["committed"], true, "{v}");
        assert_eq!(v["note_written"], true, "git note written: {v}");
        let commit = v["commit"].as_str().unwrap();

        // The git note (refs/notes/taime) carries the machine-readable record.
        let note = git_in(&proj, &["notes", "--ref=taime", "show", commit]);
        let note_json: serde_json::Value = serde_json::from_str(&note).expect("note is JSON");
        assert_eq!(note_json["schema"], "taime.provenance/v1");
        assert_eq!(note_json["agent_id"], "agent-a");
        assert_eq!(note_json["reviewed"], false);
        assert_eq!(note_json["files"][0], "a.txt");

        // merge_history surfaces the ledger row.
        let hist: serde_json::Value = serde_json::from_str(
            &mgr.query("merge_history", &serde_json::json!({ "agent_id": "agent-a" }).to_string()),
        )
        .unwrap();
        let rows = hist.as_array().unwrap();
        assert_eq!(rows.len(), 1, "one recorded merge: {hist}");
        assert_eq!(rows[0]["target"], "main");
        assert_eq!(rows[0]["scope"], "full");

        // export_attribution returns the portable agent artifact.
        let exp: serde_json::Value = serde_json::from_str(
            &mgr.query("export_attribution", &serde_json::json!({ "agent_id": "agent-a" }).to_string()),
        )
        .unwrap();
        assert_eq!(exp["schema"], "taime.attribution/v1");
        assert_eq!(exp["agent_id"], "agent-a");
        assert_eq!(exp["provider"], "claude_code");
        assert_eq!(exp["merges"].as_array().unwrap().len(), 1);
        assert_eq!(exp["merges"][0]["commit"], commit);
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[test]
    fn commit_merge_pushes_to_an_upstream_when_requested() {
        let mgr = mem_manager();
        let proj = project_repo();
        // A bare remote, with the project's branch tracking it.
        let remote = std::env::temp_dir().join(format!("taime-remote-{:08x}", rand::random::<u32>()));
        std::fs::create_dir_all(&remote).unwrap();
        git_in(&remote, &["init", "-q", "--bare"]);
        git_in(&proj, &["remote", "add", "origin", remote.to_str().unwrap()]);
        git_in(&proj, &["push", "-q", "-u", "origin", "HEAD"]);

        let wt = seed_isolated_agent(&mgr, &proj, "agent-a");
        std::fs::write(wt.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        let digest = fetch_digest(&mgr, "agent-a");
        let v: serde_json::Value = serde_json::from_str(&mgr.query(
            "commit_merge",
            &commit_merge_args_push("agent-a", "main", serde_json::json!({ "a.txt": [0] }), &digest, true),
        ))
        .unwrap();
        assert_eq!(v["committed"], true, "{v}");
        assert_eq!(v["pushed"], true, "push to a tracked upstream: {v}");
        // The bare remote now has the provenance commit.
        let local_head = git_in(&proj, &["rev-parse", "HEAD"]);
        let remote_head = git_in(&remote, &["rev-parse", "HEAD"]);
        assert_eq!(local_head, remote_head, "remote received the commit");
        assert_eq!(v["push_error"], serde_json::Value::Null, "no push error: {v}");
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&proj);
        let _ = std::fs::remove_dir_all(&remote);
    }

    #[test]
    fn commit_merge_push_failure_never_undoes_the_commit() {
        let mgr = mem_manager();
        let proj = project_repo(); // NO remote configured
        let wt = seed_isolated_agent(&mgr, &proj, "agent-a");
        std::fs::write(wt.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        let digest = fetch_digest(&mgr, "agent-a");
        let v: serde_json::Value = serde_json::from_str(&mgr.query(
            "commit_merge",
            &commit_merge_args_push("agent-a", "main", serde_json::json!({ "a.txt": [0] }), &digest, true),
        ))
        .unwrap();
        // The commit stands; only the push reports a structured failure.
        assert_eq!(v["committed"], true, "commit must stand without a remote: {v}");
        assert_eq!(v["pushed"], false, "{v}");
        assert!(v["push_error"].as_str().is_some(), "names the push failure: {v}");
        assert!(std::fs::read_to_string(proj.join("a.txt")).unwrap().contains("TWO"));
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[test]
    fn retention_caps_pick_oldest_eligible_victims() {
        fn wt(id: &str, created: u64) -> crate::store::WorktreeRow {
            crate::store::WorktreeRow {
                terminal_id: id.into(),
                project_root: Some("/p".into()),
                repo_root: Some("/p".into()),
                worktree_path: format!("/wt/{id}"),
                branch: None,
                base_sha: Some("base".into()),
                mode: Some("isolated".into()),
                provider: Some("claude_code".into()),
                member_of: None,
                task_id: None,
                created_at: Some(created),
                archived_at: None,
                reclaimed_at: None,
                archive_ref: None,
            }
        }
        let ids = |v: Vec<&crate::store::WorktreeRow>| {
            let mut s: Vec<String> = v.iter().map(|w| w.terminal_id.clone()).collect();
            s.sort();
            s
        };
        // Newest-provisioned first (the rank the count cap protects).
        let physical = vec![wt("new", 900), wt("mid", 500), wt("old", 100)];
        let now = 1000;
        let none = std::collections::HashSet::new();

        // Count cap: keep the 1 newest, reclaim the rest (no age cap, no grace).
        assert_eq!(
            ids(select_reclaim_victims(&physical, &none, now, 0, 1, 0)),
            vec!["mid".to_string(), "old".to_string()]
        );
        // Age cap alone (keep all by count): only the one older than max_idle.
        // ages: new=100, mid=500, old=900 ⇒ max_idle=600 evicts only `old`.
        assert_eq!(
            ids(select_reclaim_victims(&physical, &none, now, 0, 99, 600)),
            vec!["old".to_string()]
        );
        // Grace protects the youngest even when the count cap would evict it.
        // keep=0 ⇒ all over-count; grace=200 spares `new` (age 100 < 200).
        assert_eq!(
            ids(select_reclaim_victims(&physical, &none, now, 200, 0, 0)),
            vec!["mid".to_string(), "old".to_string()]
        );
        // A live agent is never a victim (here `mid`).
        let live: std::collections::HashSet<String> = ["mid".to_string()].into_iter().collect();
        assert_eq!(
            ids(select_reclaim_victims(&physical, &live, now, 0, 0, 0)),
            vec!["new".to_string(), "old".to_string()]
        );
        // A shared-mode row is never a victim, even at rank beyond the cap.
        let mut mixed = physical.clone();
        mixed.push(crate::store::WorktreeRow { mode: Some("shared".into()), ..wt("shared", 1) });
        let victims = ids(select_reclaim_victims(&mixed, &none, now, 0, 0, 0));
        assert!(!victims.contains(&"shared".to_string()), "shared row spared: {victims:?}");
    }
}
