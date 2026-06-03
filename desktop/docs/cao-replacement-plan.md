# Taime — Replacing CAO/tmux with an all-Rust orchestrator

**Status:** proposed plan of record (revised after review). Companion to
[`terminal-architecture-plan.md`](./terminal-architecture-plan.md) (the session
daemon, built) and [`terminal-daemon-implementation-notes.md`](./terminal-daemon-implementation-notes.md).

**Revision note:** an earlier draft sequenced "move all CLIs to daemon I/O first,
keep CAO for sessions/worktrees/status." Review showed that's unsafe: CAO's
session/status APIs are **tmux-shaped** and its provider launch recipes inject
the **MCP/tools/profile** config that makes agents useful — so removing tmux or
moving a provider before that config exists breaks status/session calls and
disables orchestration tools. The sequence below is reordered accordingly
(provider+MCP config → session registry/API shim → worktrees+persistence →
status → message bus → attribution/route migration → delete), and adds the
persistence + API-compatibility layer the first draft omitted.

## North star

One long-lived **Rust** process — `taime-session-daemon`, grown into a full
**agent orchestrator** — owns everything CAO/tmux owns: PTYs, authoritative
terminal state, session/worktree lifecycle, agent status, **inter-agent
communication**, and **attribution**. The Tauri app is a thin client; the Python
CAO backend, its SQLite, and tmux are deleted. No orchestration layer the daemon
can't own; no state trapped in tmux, SQLite-behind-HTTP, or a webview.

The daemon already proves the hard part: it owns PTYs out-of-process, **survives
app crashes**, holds an authoritative `wezterm-term` grid, emits attribution
turns, detects **idle/quiet windows**, and (recently) supports **connect-only
enumeration + boot adoption** (`daemon_list` never spawns; `adoptDaemonSession` +
the reconcile boot-scan re-adopt survivors). Idle detection is exactly the
primitive CAO's message watchdog needs, and boot-adoption is the foundation for
the "auto-adopt after crash" the orchestrator relies on — so this is an extension
of what exists, not a greenfield subsystem.

## What CAO/tmux owns today (the inventory we must absorb)

Grounded in `backend/cao/src/cli_agent_orchestrator` + the `api.ts` contract:

1. **Terminal transport** — tmux panes + a WebSocket PTY stream
   (`/terminals/{id}/ws`). The daemon already replaces this for Claude.
2. **Sessions & terminals (tmux-shaped)** — terminal creation makes tmux
   sessions/windows + `pipe-pane` logs (`terminal_service.py` ~:203); the UI
   polls `/terminals/{id}` (`api.ts` ~:282). Status/session calls **assume real
   tmux panes**.
3. **Provider launch recipes (tools-bearing)** — per-CLI setup does far more than
   `binary + args`: agent **profiles**, **allowed-tools**, **skill prompts**,
   **MCP env/config injection**, startup prompts, model flags
   (`terminal_service.py` ~:308 + `providers/*`). This is what makes an agent
   capable; the current daemon launch is **Claude-only and injects none of it**
   (`commands.rs` `claude_args`).
4. **Worktrees** — a git worktree per agent + contention (`worktree_service`,
   `provisionWorktree`).
5. **Status** — per-terminal `PROCESSING / WAITING_USER_ANSWER / IDLE /
   COMPLETED / ERROR`, inferred from provider/tmux output
   (`terminal_service.py` ~:419, `providers/*.get_status`).
6. **Inter-agent communication (the orchestrator core)** — an **MCP server**
   (`mcp_server/server.py`) exposing `send_message`, `handoff`, `assign`
   (spawn a constrained worker), `load_skill`…; a **SQLite inbox**
   (`models/inbox.py`: `sender_id → receiver_id`, `pending|delivered|failed`); a
   **watchdog** that tails each pane and, when the receiver is **IDLE**, delivers
   by typing into its stdin (`inbox_service` → `terminal_service.send_input` →
   tmux `send_keys`).
7. **Persistence** — SQLite tables for terminals, inbox, memory, flows,
   worktrees, turns, activity (`clients/database.py` ~:28). This is real state,
   not just tmux + MCP.
8. **Attribution / activity** — turns, activity graph (nodes, per-turn bursts,
   **edges** `{kind, source, target}`, file **contention**), diffs, per-hunk
   authorship (`activity_service`, `diff_service`; Taime-added REST endpoints in
   `api.ts` ~:328). Note: fs-watching today is **app-side** — `useFsWatch.ts`
   ~:27 mounts a watcher per *visible* frame and forwards to CAO best-effort, so
   **attribution stops when the UI is closed**.

## Target architecture (all-Rust)

```
┌──────────────────────────  taime-session-daemon (Rust, detached, persistent) ───────────────────────────┐
│  PTY + wezterm-term/session   │ session+worktree+provider/profile registry │ status inference (grid/OSC) │
│  fs-watch + attribution turns │ persistence store (SQLite/postcard in run/) │ message bus + inbox+deliver │
│         ▲ control/data socket (app)                              ▲ MCP endpoint (agents)                  │
└─────────┼────────────────────────────────────────────────────── │ ───────────────────────────────────-─┘
          │ Tauri commands  (terminal/session/diff/graph/status…)  │
     ┌────┴────┐                                  ┌────────────────┴──────────────────┐
     │Tauri app│ thin client (xterm, diff, graph) │ agents (claude/codex/gemini/grok) │
     └─────────┘                                  │  MCP transport (per provider):    │
                                                   │  stdio shim  OR  HTTP+SSE + token │
                                                   └───────────────────────────────────┘
```

Two interfaces onto one process: the **control/data socket** the app uses, and an
**MCP endpoint** the *agents* use to talk to each other and the orchestrator. The
MCP transport is **per-provider** (some CLIs take an MCP config file + flag,
others an env var, others a provider config) — see Phase 1 `mcp_injection_strategy`.

## Revised staged plan (each phase leaves a working app; CAO shrinks, never a big-bang)

> Optionality is permanent during migration: the daemon path stays **fallback-capable**
> (`daemonAvailable()` → daemon, else CAO) and dev/unbundled builds keep working,
> until Phase 7 deletes CAO.

### Phase 1 — Provider adapter + profile/MCP-compatible daemon spawn (all CLIs)
*Must be first: moving a provider to the daemon without its launch config would
disable the very tools that make it an agent. This phase is larger than "a TOML
edit" — CAO providers carry real behavior (config-file mutation + cleanup,
permission detection, `paste_enter_count`, response extraction, MCP differences,
status heuristics). The shape of the Rust abstraction is the decision that keeps
the migration clean vs. a pile of per-provider conditionals, so it is defined
here before any Phase-1 code.*

**The provider abstraction: a Rust `Provider` adapter trait + TOML-backed
defaults.** TOML holds the *data* (binary, base args, env, model→flag map, prompt
templates, MCP strategy); the trait holds the *behavior* that can't be data. A
default impl serves data-only providers from TOML; providers with quirks override
methods — no `match provider { … }` conditionals scattered through the daemon.

```rust
/// One adapter per CLI. `DaemonSessionSpec` generalizes today's Claude-only
/// SpawnSpec to any binary+args+env.
trait Provider {
    fn id(&self) -> &str;
    /// Build the spawn command from a profile + run options (model, permission
    /// mode, cwd, startup/seed prompt).
    fn command(&self, profile: &Profile, opts: &LaunchOpts) -> DaemonSessionSpec;
    /// Inject the daemon's MCP endpoint per `mcp_injection_strategy`; returns
    /// any temp files/edits to undo on exit.
    fn inject_mcp(&self, spec: &mut DaemonSessionSpec, mcp: &McpEndpoint) -> Cleanup;
    /// Enters to send after a paste (CAO `paste_enter_count`).
    fn paste_enter_count(&self) -> u8 { 1 }
    /// Status from the live grid/stream — state machine, OSC/exit-code first,
    /// regex fallback (Phase 4).
    fn status(&self, view: &GridView) -> Status;
    /// Detect an approval/permission prompt (the WAITING_USER_ANSWER heuristic).
    fn approval_prompt(&self, view: &GridView) -> Option<ApprovalPrompt>;
    /// Extract a structured response/result for handoff/attribution, if exposed.
    fn extract_response(&self, turn: &TurnText) -> Option<String> { None }
}
```

- Replace `daemon_spawn_claude`/`claude_args` with the registry of `Provider`
  adapters (claude/codex/gemini/grok to start), defaults from
  `~/.taime/providers.toml`. A simple provider = TOML; a quirky one = a thin
  trait override. `mcp_injection_strategy ∈ {env_var, config_file+flag,
  provider_config}` (see the table in Phase 5).
- Port enough of CAO's recipes (profiles, allowed-tools, skill prompts, MCP
  config) to **preserve tool parity** — even though inter-agent tools don't exist
  daemon-side until Phase 5, the *injection plumbing* must be ready so a
  daemon-launched agent isn't crippled.
- **Exit:** any CLI launches via the daemon through its adapter with the same
  capabilities CAO gave it; MCP injection plumbing exists (wired to CAO's MCP for
  now, swapped to the daemon's in Phase 5).

### Phase 2 — Daemon-owned session registry + API-compatibility shim
*Unblocks removing tmux from I/O without breaking the tmux-shaped session/status
APIs.*
- Give the daemon a **session/terminal registry** the app can query the way it
  queries CAO today. Move the app's terminal/session **route layer** (`api.ts`)
  to **Tauri commands → daemon** (preferred) — or a daemon-hosted HTTP shim / a
  local proxy if a drop-in REST surface is easier short-term.
- **Compatibility shim:** route each terminal/session call by whether the
  terminal is daemon-owned; CAO still backs not-yet-migrated terminals. This is
  the explicit "daemon-owned terminal registry adapter" the review called for.
- **Exit:** for daemon sessions, tmux is out of the I/O *and* status/session path;
  the app no longer needs CAO to list/inspect daemon terminals.

### Phase 3 — Daemon-owned worktrees + persistence
*Worktrees must be daemon-owned (not "app or daemon"): `assign()` (Phase 5) spawns
workers headlessly and sessions outlive the app, so app-owned provisioning would
break background orchestration + crash survival.*
- Move `provisionWorktree`/contention into the **daemon** via `git worktree`
  (shell-out) or `git2`, preserving the `terminalId`/`attribution_key` contract.
  Gate behind a **diff-parity check** vs CAO before trusting it.
- **Persistence store** (the layer the first draft omitted), with a clear
  location split:
  - **Durable orchestration state → app data dir** (`dirs::data_dir()/taime/`,
    e.g. `~/Library/Application Support/dev.taime.app/` on macOS — the
    CAO-equivalent home), as **SQLite via `rusqlite`** (decision: one store, not
    "postcard files"; mirroring CAO's schema makes the Phase-7 import trivial).
    Holds sessions, worktrees, turns, activity, and (Phase 5) the inbox.
  - **Runtime dir** (`$TMPDIR/taime/`) stays **transient only**: the socket,
    lock, pid, attach token, and ephemeral snapshots — never durable state (a
    temp dir can be GC'd, and inbox/worktrees/turns must outlive it).
- **Exit:** launch + worktrees + their state don't touch CAO, and survive a
  daemon restart from the app-data store.

### Phase 4 — Daemon status inference (parity-tested)
- Add a **`status` field to `SessionSummary`** + a **push event** (today it has
  only `alive`/`attached`, and `refreshStatuses` *skips* daemon frames — this
  closes that gap).
- Prefer a **state-machine-per-provider** over text-scraping: drive transitions
  from robust signals first — **OSC sequences, exit codes**, the daemon's
  quiet-window/turn substrate (quiet ⇒ IDLE/COMPLETED, sustained output ⇒
  PROCESSING, OSC-133 nonzero ⇒ ERROR) — and keep **per-provider approval-prompt
  regex sets as a last-resort fallback** for WAITING_USER_ANSWER.
- Validate against **CAO provider fixtures as parity tests** (the `providers/test_*`
  status cases) so we match today's badges.
- **Exit:** StatusBadge is daemon-driven; CAO's status path is unused.

### Phase 5 — Inter-agent communication (MCP + message bus)
The flagship; full design in its own section below. Build the message bus +
**persisted-by-default** inbox + idle-gated delivery + `send_message`/`handoff`/
`assign` as an **MCP server hosted by the daemon**, with the per-provider MCP
injection from Phase 1.
- **Exit:** agents discover each other, exchange messages, hand off, and spawn
  workers entirely through the daemon — CAO's MCP server + inbox + watchdog are
  unused.

### Phase 6 — Attribution + activity/diff into the daemon (and the route layer)
- **Move fs-watching into the daemon.** Today it's app-side + per-*visible*-frame
  (`useFsWatch.ts`), so attribution dies when the UI closes — unacceptable for an
  orchestrator whose agents run headless. Note `fs_watch.rs` is currently
  **app/Tauri-state code**, not daemon code: **extract its watcher core into a
  shared crate (or daemon module)** that the daemon mounts per session, and keep
  Tauri purely as a **UI event subscriber**. (Alternative the plan explicitly
  rejects: "attribution only while UI open.")
- Build the activity graph from Phase-5 interactions (edges
  `{kind: message|handoff|assign, source, target, ts}`) + fs-watch contention;
  port `diff_service`/per-hunk authorship to `git`/`git2` behind the diff-parity
  gate.
- **Route-layer migration (explicit):** "no UI rework" applies only to the data
  *shapes* — the diff/hunks/attribution/activity/graph/checkpoint calls
  (`api.ts` ~:328, CAO REST) **must move** to Tauri commands / a daemon API /
  proxy. Components keyed on the same shapes don't change; the fetch layer does.
  Note several of these (`getTerminalDiff`, `getHunks`, `getAttribution`,
  `getGraph`, checkpoints) are **Taime-added on top of CAO**, not native CAO — so
  they need first-class daemon equivalents (the daemon already has the turn/grid
  substrate to compute them), not just a passthrough proxy.
- **Exit:** graph/diff/contention/attribution read from the daemon and keep
  working with the UI closed.

### Phase 7 — Remove CAO + tmux (with state migration)
- **State migration (recommended path):** because the daemon store (Phase 3)
  deliberately mirrors CAO's schema, do a **one-time import** rather than a clean
  break. Skeleton:
  ```
  taime-session-daemon --import-cao <cao_home>/cao.sqlite   # run-once, idempotent
    → open CAO sqlite read-only; for each table (worktrees, turns, activity,
      inbox, memory, profiles) map rows → daemon app-data SQLite; set a
      `migrated_from_cao` marker so re-runs no-op.
  ```
  Skills/agent profiles that are files (`agent_store/*`, `skills/*`) copy into the
  Rust provider/profile config (Phase 1/3). Document a clean-break fallback for
  users who'd rather start fresh.
- Remove the Python `cli_agent_orchestrator`, tmux, the `backend.rs` managed
  sidecar, and the CAO REST client.
- **Exit:** no Python, no tmux, no managed sidecar.

**Sequencing rationale (revised):** provider+MCP config (1) and the session
registry/API shim (2) are prerequisites to taking tmux off the I/O+status path
without breakage. Worktrees+persistence (3) make orchestration state daemon-owned
so headless `assign` and crash survival work. Status (4) feeds delivery gating in
(5). Attribution+route migration (6) is the last thing CAO backs. (7) is pure
deletion once nothing depends on CAO.

---

## Inter-agent communication — detailed design (Phase 5)

Goal: agents communicate **fully** — directed messages, broadcast,
request/response, control handoff, worker delegation, shared context — with the
daemon as the trusted router that knows who's idle, so it never interrupts a
working agent.

### Transport to the agents: an MCP server hosted by the daemon
Coding CLIs already speak **MCP**, so the agent side needs no bespoke client — we
expose orchestration as MCP **tools**. The daemon hosts the MCP endpoint; the
transport is **per-provider** (from Phase 1's `mcp_injection_strategy`):
- **stdio shim per agent** — a tiny stdio↔daemon bridge launched alongside the
  CLI (works for every MCP client; no network surface); or
- **one HTTP+SSE endpoint** bound to loopback with a **per-agent token** issued at
  spawn and injected via the provider's MCP config.
The MCP server lives **in the daemon process** so tool calls resolve against the
live registry, idle state, and inbox with **one source of truth** — and with at
most a single local hop: HTTP+SSE is direct (in-process handler, no hop); the
stdio shim is one tiny local IPC bridge to the daemon (the honest tradeoff —
zero network surface, one hop). (CAO splits an `ops_mcp_server` from the
per-agent MCP server; decide whether app-ops tools and agent tools share one
endpoint or two.)

What each `mcp_injection_strategy` actually does at spawn:

| strategy | injected at spawn | cleanup on exit |
|---|---|---|
| `env_var` | an env var on the child (e.g. `TAIME_MCP=<url-or-stdio>` + per-agent token) | none |
| `config_file+flag` | write a temp MCP json (server + token) and pass the CLI's flag (e.g. `--mcp-config <path>`) | delete the temp file |
| `provider_config` | merge an `mcpServers` entry into the CLI's own config file | restore the prior config |

### Tool surface (what an agent can do)
- **`list_agents()`** → live sessions `{id, role, provider, status, cwd}`.
- **`send_message(to, body, kind?)`** → enqueue in `to`'s inbox (`kind ∈
  {info, request, result}`). The daemon stamps `from` from the **authenticated
  session**, never a client field (closes the `sender_id`-spoofing CAO allows).
- **`broadcast(body, role?)`** → fan-out to all / a role.
- **`request(to, body, timeout)` / `reply(request_id, body)`** → request/response
  over the inbox, matched by a correlation id.
- **`handoff(to, summary)`** → transfer the "active" role + a context summary;
  records a handoff edge.
- **`assign(role, prompt, tools?)`** → spawn a **worker sub-agent**: daemon
  creates a child session (Phase-1 spawn + Phase-3 worktree), seeds `prompt`,
  **constrains its MCP allow-list** (so a worker can't infinitely re-`assign` —
  CAO's `_resolve_child_allowed_tools`), links parent→child; results flow back via
  `reply`. True fan-out/fan-in.
- **`share(key, value)` / `get(key)`** → a per-session **blackboard** for results
  too large/persistent for one message.

### Inbox + idle-gated delivery
- **Persisted-by-default mailbox.** In-memory for speed, but **checkpoint on every
  enqueue/status-change to the Phase-3 app-data SQLite** (not the transient
  runtime dir) so undelivered handoffs/requests **survive a daemon restart**.
  Monotonic message id + per-receiver FIFO; the status gate (`pending→delivered`)
  makes delivery **idempotent**, surviving the duplicate idle-wakeup race across a
  restart too.
- **Delivery rule (the safety property):** deliver a pending message **only when
  the receiver is IDLE** (Phase-4 status / quiet-window) — never mid-turn. This is
  CAO's watchdog, native: the daemon already watches every byte, so it's "deliver
  on next idle" with no `pipe-pane` log tailing.
- **Delivery mechanism:** inject into the receiver's PTY stdin (the daemon owns
  the writer) with a **clear visual delimiter** so the user can tell orchestrator
  injection from agent output, e.g.
  `\r\n--- MESSAGE FROM <id> ---\r\n<body>\r\n--- END MESSAGE ---\r\n`, then mark
  `delivered`. (For MCP-aware agents, surfacing as a `read_inbox` tool result is a
  cleaner long-term option; stdin injection matches today's behavior and works
  for every CLI.)

### Attribution linkage (provable chains)
Extend `TurnInfo` with an optional **`interaction_id`**: when a delivered message
triggers a burst of output, stamp that turn with the message's id. This links
"agent B produced these edits" → "because agent A sent this message," a provable
inter-agent attribution chain. Every message/handoff/assign is also an **edge**
`{kind, source, target, ts}` — the shape `api.ts` already models — so the activity
graph renders inter-agent flow with no component rework (Phase 6).

### Security & safety
- MCP endpoint is **same-user** + **per-agent token** (reuse the socket posture:
  peer-uid + rotating token; HTTP+SSE binds loopback + token). An agent acts only
  as itself (daemon-stamped `from`).
- **Per-agent tool allow-lists** (workers get a reduced set); `assign` depth/fan
  limits prevent runaway spawning.
- Idle-gated delivery guarantees orchestration **never corrupts an in-progress
  turn** — the core correctness property.

### Build order within Phase 5
1. Persisted mailbox + `send_message` + idle-gated stdin delivery (MVP loop).
2. MCP server scaffold + `list_agents` + spawn-time MCP injection (per provider).
3. `request`/`reply` + `broadcast`.
4. `handoff` (+ active-agent UI pointer).
5. `assign` (worker spawn + constrained tools + parent/child edges + result fan-in).
6. `share`/blackboard + `interaction_id` stamping.

---

## What stays, what goes

- **Goes:** tmux; the Python `cli_agent_orchestrator` (REST API, MCP server, inbox
  DB, watchdog, providers/services); the CAO SQLite; the `backend.rs` managed
  sidecar; the CAO WS terminal transport + REST client in `api.ts`.
- **Stays / grows:** the Rust daemon (now orchestrator + persistence +
  fs-watch + MCP); the Tauri app (thin client: xterm, diff/graph); the
  `taime-protocol` crate. Protocol evolution: keep the app↔daemon control socket;
  add orchestration messages (status push, session/registry, worktree, activity)
  there; the **agent side is MCP**, kept separate from the app control protocol.
  `SessionSummary` will grow several fields over the phases (Phase 4 `status`,
  Phase 5/6 interaction metadata) — expected, version the protocol accordingly.
- **Open (deferred decision):** CAO's `skills/` + `plugins/` system
  (`cao-session-management`, `cao-supervisor-protocols`, …). Whether these are
  re-expressed as daemon-side skills / MCP tools, ported as provider profile
  content, or dropped is left open until Phase 5/1 make the agent-tool + provider
  surfaces concrete.

## Top risks

1. **Provider/MCP injection per CLI (Phase 1/5).** Each CLI configures MCP +
   tools + profiles differently; needs a per-provider `mcp_injection_strategy` +
   an acceptance test per CLI. Highest-churn integration surface.
2. **Status fidelity (Phase 4).** "Waiting for approval" is genuinely
   provider-coupled; use the state-machine (OSC/exit-first, regex fallback) and
   validate against CAO provider fixtures.
3. **Session/status API reshaping (Phase 2).** Removing tmux breaks the
   tmux-shaped APIs unless the registry adapter + route migration land first;
   keep the compat shim until every endpoint is migrated.
4. **Worktree/diff parity (Phase 3/6).** Rust contention + per-hunk authorship
   must match CAO; gate behind a diff-parity check before deleting the Python.
5. **Inbox crash-survival (Phase 5).** Persist-by-default + idempotent status gate
   so a restart never drops or double-delivers a handoff/request.
6. **Attribution-while-headless (Phase 6).** fs-watch must move daemon-side or
   attribution silently stops when the UI closes.
7. **State migration (Phase 7).** Don't delete CAO until existing SQLite state is
   migrated or a clean break is documented + scripted.
8. **Big-bang temptation.** Every phase must leave a working app with CAO
   shrinking — never a half-cut-over break.
