# CAO replacement — Phase 3 implementation notes

**Status:** built (persistence store + daemon worktree provisioning). The
*interactive-launch frontend switch* to daemon provisioning is intentionally
deferred to Phase 6 (see below). Companion to
[`cao-replacement-plan.md`](./cao-replacement-plan.md).

Phase 3 = **daemon-owned worktrees + a durable persistence store** — the
foundation the Phase-5 inbox + headless `assign` and Phase-6 attribution build
on.

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

## Worktrees into the daemon (`taime-session-daemon/src/worktree.rs`) — built

Rust port of CAO's `worktree_service.ensure_worktree`:
- `git worktree add -b taime/<provider>-<key> <path> <base_sha>` under the
  app-data dir (`…/taime/worktrees/<slug>/<key>`); attaches an existing branch on
  relaunch; idempotent reuse of an existing checkout; **shared-mode fallback**
  (the project dir, `error` set) for a non-git root / empty repo / git failure —
  never fatal.
- `link_gitignored_deps` (best-effort): copy `.env*`, symlink heavy gitignored
  dirs (`node_modules`/`.venv`/`target`/…) so a fresh worktree is usable.
- Protocol (`PROTOCOL_VERSION` → 3): `WorktreeInfo` + `ClientMsg::
  ProvisionWorktree` / `ServerMsg::Worktree`. `Manager::provision_worktree` mints
  the attribution key, provisions, and persists the `taime_worktrees` row;
  `conn.rs` runs it in `spawn_blocking` (git shells out). App: `daemon_provision_
  worktree` command + `DaemonClient::provision_worktree`. Frontend:
  `daemonProvisionWorktree` (pty.ts).
- Tests: shared on `isolate=false` / non-git; a real temp git repo provisions an
  isolated worktree (branch, base_sha, `.git`, tracked files, `.env` copied) and
  is idempotent on relaunch.

### Why the interactive launch still uses CAO provisioning (deferred to Phase 6)

`store.ts launchAgentDaemon` still calls `api.provisionWorktree` (CAO). Switching
it to `daemonProvisionWorktree` now would **break diff/graph for daemon agents**:
CAO-backed `getTerminalDiff`/`getGraph` look the worktree up in CAO's DB, and a
daemon-provisioned worktree isn't there. So the frontend switch lands in **Phase
6**, together with the diff/graph move daemon-side — keeping every phase a working
app. The daemon provisioning is already used **internally** by Phase-5 headless
`assign` (workers get a worktree without the app), which is the case that
*required* it to be daemon-owned.
