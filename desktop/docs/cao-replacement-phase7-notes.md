# CAO replacement — Phase 7 implementation notes (state migration)

**Status:** the **state-migration** half is built (`--import-cao`). The actual
**deletion** of the Python CAO backend + tmux is the live-validation-gated step
(see below). Companion to [`cao-replacement-plan.md`](./cao-replacement-plan.md).

## `--import-cao` (built)

`taime-session-daemon --import-cao <cao.sqlite>` does the plan's one-time,
idempotent import of a CAO SQLite db into the daemon's app-data store:
- a `taime_meta.migrated_from_cao` marker gates it → re-runs no-op;
- per table (`terminals`, `inbox`, `memory_metadata`, `flows`, `taime_worktrees`,
  `taime_agent_turns`, `taime_activity_events`): `ATTACH` the CAO db, then
  `INSERT OR IGNORE INTO <t> (cols) SELECT cols FROM cao.<t>` with **explicit
  column lists** (robust against SQLAlchemy-vs-our-DDL column-order drift); a
  table missing from the CAO db is skipped, not fatal.
- The daemon schema (Phase 3) deliberately mirrors CAO's, which is what makes the
  import a row copy.
- Tested: a CAO-shaped db imports (terminals + worktrees rows land) and the
  re-run is a no-op.

File-based skills/agent profiles (`agent_store/*`, `skills/*`) copy into the Rust
provider/profile config — that part rides on the deferred Phase-1 profile store.

## The deletion (done)

CAO + tmux are removed and the app is **daemon-only** (builds, tests, typechecks,
and production-builds clean):

- **Python `cli_agent_orchestrator`** — `backend/` deleted (it was the user's
  gitignored CAO fork, committed to their remote `Custos/cli-agent-orchestrator`,
  so recoverable).
- **`backend.rs` managed sidecar** + `config.rs` (CAO config) + the
  `get_api_url`/`get_backend_routing`/`get_backend_status` commands + the
  `reqwest` dep — deleted. `main.rs` no longer spawns or supervises a sidecar.
- **The CAO REST client** (`api.ts`) — rewritten to route entirely through the
  daemon via `daemon_query` (Phase-6 diff/hunks/attribution/contention/worktree/
  workspace) + the dedicated daemon commands. Every method signature + return
  shape is unchanged, so components are untouched.
- **The CAO-WS terminal** (`TerminalView.tsx`) + the CAO launch fallback +
  `terminalWsUrl` — deleted; terminals render only via the daemon
  (`TerminalViewRustPty`). The backend status pill is static ("healthy" = daemon).
- **tmux** — the app never spawned tmux directly (CAO did); with CAO gone there is
  no tmux in the product.

The one-time `--import-cao` (above) migrates any existing CAO SQLite into the
daemon's app-data store before the switch.

## Degradations (documented follow-ups)

The tmux-shaped **session grouping** (CAO sessions containing terminals) is gone —
agents are standalone, surfaced via the daemon registry / detached panel.
**Non-default agent profiles** and the **rich per-hunk authorship** (turn-snapshot
attribution) await the daemon profile store + turn persistence; the launcher uses
the default profile and the diff falls back to an ungrouped file list. These are
additive follow-ups, not regressions to the core review/attribution flow (diff,
hunks, selective merge, graph, status, worktrees all work daemon-side).
