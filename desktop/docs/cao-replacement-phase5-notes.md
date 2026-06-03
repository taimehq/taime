# CAO replacement — Phase 5 implementation notes (MVP loop)

**Status:** the **MVP loop** (persisted inbox + idle-gated stdin delivery +
message bus) is built. The agent-facing **MCP server** (so agents themselves call
`send_message`/`handoff`/`assign`) is the larger remaining piece — see below.
Companion to [`cao-replacement-plan.md`](./cao-replacement-plan.md).

This implements step 1 of the plan's "Build order within Phase 5": *persisted
mailbox + send_message + idle-gated stdin delivery (MVP loop)*.

## What landed

- **Persisted inbox (`store.rs`):** `enqueue_message` (monotonic id, status
  `pending`), `receivers_with_pending` (delivery work-list),
  `pending_for(receiver, limit)` (FIFO oldest-first), `set_message_status`
  (`delivered`/`failed`). Persisted-by-default in the Phase-3 app-data SQLite, so
  undelivered messages **survive a daemon restart**; the `pending → delivered`
  gate makes delivery idempotent (no double-deliver across a restart).
- **Idle-gated delivery engine (`Manager::deliver_pending`, run on the 250 ms gc
  tick):** for each receiver with a pending message that maps to a live,
  **ready** session (`Session::is_ready_for_delivery` = IDLE or COMPLETED via the
  Phase-4 provider status), inject the oldest message into its PTY stdin with a
  clear visual delimiter (`format_delivery`) and mark it delivered. **The safety
  property:** never mid-turn — the daemon already watches every byte, so this is
  CAO's watchdog made native (no `pipe-pane` log tailing). Sessions are addressed
  by `attribution_key` (the inbox routes to it).
- **Message bus (control protocol, `PROTOCOL_VERSION` → 4):**
  `ClientMsg::SendMessage { sender, receiver, message }` → `ServerMsg::
  MessageQueued { id }`. `Manager::enqueue_message`, the conn dispatch, the app
  `DaemonClient::send_message` + `daemon_send_message` command, and the
  `daemonSendMessage` frontend wrapper. This is the ops/app entry; the
  agent-facing MCP `send_message` tool will feed the same `Manager::
  enqueue_message` in-process.
- **Tests:** inbox FIFO + status-gate; the delimited delivery payload; the
  protocol round-trip. 52 daemon + 9 protocol + 2 integration green; zero
  warnings; typecheck clean.

## MCP tool dispatcher (`src/mcp.rs`) — built (in-process; transport remaining)

The **in-process JSON-RPC dispatcher** that turns the orchestration primitives
into MCP tools: handles `initialize` / `tools/list` / `tools/call` for
`list_agents` + `send_message`, resolving against the live registry + inbox.
Crucially, `send_message`'s `from` is **stamped from the authenticated `caller`**
(the calling session's attribution key), never a client field — closing CAO's
`sender_id` spoof. Tool-level failures are MCP `isError` content; unknown methods
are JSON-RPC errors. Unit-tested (initialize/tools-list/from-stamping/self-send
rejection/unknown-method).

## Per-agent transport + the full tool surface — built

- **Stdio shim** (`taime-session-daemon --mcp-stdio`): the daemon binary doubles
  as the per-agent MCP shim. It bridges the CLI's MCP client (newline-delimited
  JSON-RPC on stdin/stdout, per the MCP stdio transport) to the daemon's
  dispatcher over the control socket, authenticated by `$TAIME_MCP_TOKEN`. No
  separate binary, no network surface.
- **Per-agent token auth** (`McpRequest`/`McpResponse`, protocol → v5): the
  daemon issues a 128-bit token at spawn, injects it into the agent's MCP-server
  env (via the **Phase-1 per-provider injection** — `inject_orchestration`),
  maps token → attribution key, and resolves the **authenticated caller** from it
  (cleaned on exit). `conn.rs` runs `handle_mcp` in `spawn_blocking` (assign
  shells out to git).
- **Tools:** `list_agents`, `send_message`, `broadcast`, `handoff`, and **`assign`**
  (`Manager::assign_worker` — provision a worktree off the parent, spawn a
  default-profile worker with the **same provider but no orchestration tools** so
  it can't re-`assign`, seed the task via the inbox, record the parent→child
  `assign` edge). 8 dispatcher unit tests.

## Remaining Phase-5 work (needs a live agent to validate)

- **Turn it on for a supervisor:** `inject_orchestration` defaults **off** so the
  proven plain-agent launch is untouched. Launching a supervisor through the
  daemon with it `true` + the supervisor's resolved profile needs the deferred
  Phase-1 *profile passing* (the app routes non-default profiles to CAO today).
  Once on, validate the **MCP handshake against a real CLI** (the one piece that
  genuinely needs a live agent).
- `request`/`reply` (correlation id over the inbox) and a constrained per-worker
  tool allow-list (richer than "no tools") + `assign` depth/fan limits.
- **`assign`** (worker spawn): `Manager::spawn_agent` + `provision_worktree`
  already exist; `assign` adds a constrained MCP allow-list (so a worker can't
  infinitely re-`assign`), parent→child linkage, and result fan-in via `reply`.
- **`interaction_id`** on `TurnInfo` (provable inter-agent attribution chains)
  and the activity-graph edges — overlaps Phase 6.
