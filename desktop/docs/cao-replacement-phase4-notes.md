# CAO replacement — Phase 4 implementation notes

**Status:** built. Companion to [`cao-replacement-plan.md`](./cao-replacement-plan.md).

Phase 4 = **daemon-driven status** (done out of plan order — independent of Phase 3
and a near-pure wiring of the per-provider status heuristics already written in
Phase 1). Before this, the daemon's `SessionSummary` carried only `alive`/
`attached` and the app *skipped* daemon frames in `refreshStatuses`, so daemon
agents had no StatusBadge.

## What landed

- **Protocol (`PROTOCOL_VERSION` → 2):** `SessionSummary` gains `provider:
  Option<String>` and `status: Option<AgentStatus>`. `AgentStatus` is
  `#[serde(rename_all = "SCREAMING_SNAKE_CASE")]` so the JSON forwarded to the
  frontend is exactly the CAO vocabulary the existing `StatusBadge` keys on
  (`IDLE`/`PROCESSING`/`WAITING_USER_ANSWER`/`COMPLETED`/`ERROR`). Postcard
  encodes enum variants by index, so the rename is wire-neutral on the
  daemon↔app socket; the bump is for the positional struct-layout change (a stale
  v1 daemon is rejected at handshake → app falls back, never misparses).
- **Daemon:** the provider adapter now travels with the session
  (`Session::spawn_prepared(…, adapter)` → stored in `SessionInner`).
  `Session::summary()` infers status from the live grid:
  `emulator::snapshot_visible_text` → `GridView` → `Provider::status` (the
  Phase-1 heuristics, ported from CAO's `providers/*.get_status`). A dead session
  reports no live status (the app's `alive=false` drives that case).
- **Frontend:** `useRustPtyReconcile` mirrors each daemon session's
  `status` into the shared `terminalStatuses` map keyed by the attribution id —
  so a daemon agent's badge is **daemon-driven**, with no CAO `/terminals/{id}`
  poll (the Phase-2 routing adapter's intent). Boot adoption uses the
  daemon-reported `provider` (falling back to program inference for an older
  daemon). `useTurnCheckpoints` now excludes daemon frames so the newly-present
  daemon `terminalStatuses` entries don't double-attribute via CAO checkpoints
  (daemon turns already arrive as turn events → `recordTurn`).

## Parity + tests
- Per-provider status-transition unit tests (claude/codex/gemini/grok) +
  `agent_status_json_matches_cao_status_vocab` (locks the StatusBadge contract).
- 44 daemon unit + 2 integration + protocol round-trips green; app + daemon zero
  warnings; `pnpm typecheck` clean.

## Deferred (vs the plan's aspiration)
- **OSC-133/exit-code-first state machine.** Phase 4 ports CAO's text-scraping
  heuristics for *parity*; the plan preferred driving transitions from robust
  signals first (OSC-133 `;D` exit codes — already parsed in `attribution.rs`
  as `command_exit` — and quiet windows) with regex only as a last-resort
  WAITING_USER_ANSWER fallback. The substrate exists; layering it on top of the
  heuristics is a refinement.
- **Push event.** Status is polled via `daemon_list` every 4s (matching CAO's
  polling cadence). A `StatusChanged` push (computed on grid change) would cut the
  worst-case "needs you" latency; deferred.
