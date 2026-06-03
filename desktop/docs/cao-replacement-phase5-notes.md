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

## Remaining Phase-5 work (needs a live agent to validate)

- **The per-agent transport** that mounts the dispatcher: a tiny **stdio shim**
  bridging each CLI's MCP client to the daemon (or loopback HTTP+SSE + per-agent
  token), injected via the Phase-1 per-provider MCP config (swap the daemon's
  endpoint in for CAO's). Validating the MCP handshake against a real CLI is why
  this isn't mounted yet.
- **More tools:** `broadcast`/`request`/`reply` (correlation id over the inbox),
  `handoff` (transfer + edge).
- **`assign`** (worker spawn): `Manager::spawn_agent` + `provision_worktree`
  already exist; `assign` adds a constrained MCP allow-list (so a worker can't
  infinitely re-`assign`), parent→child linkage, and result fan-in via `reply`.
- **`interaction_id`** on `TurnInfo` (provable inter-agent attribution chains)
  and the activity-graph edges — overlaps Phase 6.
