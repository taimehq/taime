# CAO replacement — Phase 3 implementation notes

**Status:** persistence half built; worktree-provisioning half remaining.
Companion to [`cao-replacement-plan.md`](./cao-replacement-plan.md).

Phase 3 = **daemon-owned worktrees + a durable persistence store**. This commit
lands the **persistence store** — the layer the plan's first draft omitted and
the foundation the Phase-5 inbox and Phase-6 attribution write into.

## Persistence store (`taime-session-daemon/src/store.rs`) — built

- **SQLite via `rusqlite` (bundled)** at the **app-data dir**
  (`dirs::data_dir()/taime/taime.sqlite`, e.g. `~/Library/Application
  Support/taime/` on macOS — the CAO-equivalent durable home). `bundled` compiles
  SQLite from source so the daemon is self-contained. WAL mode for durable
  concurrent reads.
- **Location split** (the plan's invariant): durable state in app-data; the
  runtime dir (`$TMPDIR/taime/`) stays transient (socket/lock/token only).
- **Schema mirrors CAO's** `clients/database.py` verbatim (`terminals`, `inbox`,
  `memory_metadata` + its 3 indexes, `flows`, `taime_worktrees`,
  `taime_agent_turns`, `taime_activity_events`) so the Phase-7 `--import-cao` is
  a row copy, plus daemon-native `daemon_sessions` (PTY lifecycle) and
  `taime_meta` (the idempotent-import marker). All `IF NOT EXISTS` → `migrate()`
  is restart-safe.
- **Wiring:** `Manager` holds `Option<Store>` (best-effort: a DB-open failure
  disables persistence, never blocks a spawn). `spawn_agent` records a
  `daemon_sessions` row (provider + attribution key + cwd + program); `kill` and
  the `gc_tick` reap path flip it to `exited`.
- **Tests:** schema creates all CAO + daemon tables; record → list → mark-exited;
  idempotent migrate + upsert. 47 daemon unit + 2 integration green, zero
  warnings.

The other tables exist (schema ready) but are populated by their owning phases:
`inbox` → Phase 5; `taime_agent_turns`/`taime_activity_events` → Phase 6;
`taime_worktrees` → the worktree half below.

## Remaining Phase-3 work: worktrees into the daemon

Today the app still provisions worktrees via CAO REST (`api.provisionWorktree`)
even for daemon agents (`store.ts launchAgentDaemon`). Moving provisioning
daemon-side (`git worktree` / `git2`, persisting `taime_worktrees`, returning the
attribution key) is required for Phase-5 headless `assign` (workers must get a
worktree without the app) and is **gated on a diff-parity check vs CAO** before
the CAO path is removed. Sketch:
- a daemon `worktree` module: `git worktree add -b <branch> <path> <base>`, shared
  fallback for non-git roots, persist the row;
- fold provisioning into the daemon spawn (the daemon mints the attribution key)
  or a dedicated `ProvisionWorktree` control message;
- keep the CAO path as the fallback until parity is proven.
