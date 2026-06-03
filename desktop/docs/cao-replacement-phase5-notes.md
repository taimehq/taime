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

## Remaining Phase-5 work (the flagship's larger half)

- **Daemon-hosted MCP server** so agents call orchestration tools themselves:
  expose `list_agents`/`send_message`/`broadcast`/`request`/`reply`/`handoff`/
  `assign`/`share` as MCP tools over a per-provider transport (stdio shim or
  loopback HTTP+SSE + per-agent token), resolving against the live registry +
  idle state + inbox in-process. The daemon **stamps `from` from the
  authenticated session** (closes CAO's `sender_id` spoof). Wire the Phase-1
  per-provider MCP injection to *this* endpoint instead of CAO's.
- **`assign`** (worker spawn): `Manager::spawn_agent` + `provision_worktree`
  already exist; `assign` adds a constrained MCP allow-list (so a worker can't
  infinitely re-`assign`), parent→child linkage, and result fan-in via `reply`.
- **`interaction_id`** on `TurnInfo` (provable inter-agent attribution chains)
  and the activity-graph edges — overlaps Phase 6.
