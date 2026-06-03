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

## The deletion (live-validation-gated — NOT done)

Deleting the Python `cli_agent_orchestrator` + tmux + the `backend.rs` managed
sidecar + the CAO REST client is the last step, and it is deliberately **not**
done here: it would break the working app unless the daemon paths are first
proven **equivalent** to CAO under real use. The gates:
- **Phase 6 must land first** — diff/hunks/attribution/graph + fs-watch move
  daemon-side, and the frontend route layer + the Phase-3 worktree switch flip to
  the daemon. Until then the app still depends on CAO for those.
- **Diff-parity** (Phase 6) and the **real-CLI MCP handshake** (Phase 5
  `inject_orchestration` on) must be validated against actual provider binaries.
- Only then: run `--import-cao` once, flip the remaining routes, and remove the
  Python + tmux + sidecar.

Everything up to that flip is in place: the daemon launches all four CLIs, owns
worktrees + persistence + status + the inbox + the MCP server, and can import
CAO's state. The remaining work is the diff/graph/fs-watch move (Phase 6) and the
final, validated cut-over.
