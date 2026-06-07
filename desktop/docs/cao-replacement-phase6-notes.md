# CAO replacement — Phase 6 implementation notes

**Status:** complete. The daemon-side activity graph, the diff/per-hunk port, the
**fs-watch move into the daemon**, turn persistence, and the frontend route flip
are all done (see "fs-watch move — DONE" below). Companion to
[`cao-replacement-plan.md`](./cao-replacement-plan.md).

## Daemon activity graph (built)

`ClientMsg::GetGraph` → `ServerMsg::Graph { json }` (protocol → v6), backed by the
durable Phase-3 store so it's **complete even with the UI closed**:
- `Store::graph_agents` (agents from `daemon_sessions`) + `Store::activity_edges`
  (the `send_message`/`handoff`/`assign` edges from `taime_activity_events`, which
  Phase 5 records). `Manager::activity_graph_json` assembles
  `{agents:[{id,provider,status}], edges:[{kind,source,target}]}`.
- App `daemon_activity_graph` command (connect-only — never spawns a daemon just
  to read) + `daemonActivityGraph()` frontend wrapper. Wiring `ActivityGraph.tsx`
  to it lands with the route flip below.
- Tested: a seeded session + an `assign` edge produce the expected agents+edges.

This is the flagship "who delegated to whom" view, now backed by daemon-owned,
durable inter-agent edges instead of CAO plugin events.

## Diff port + route flip — done

- **`diff.rs`** — the `diff_service` port: `terminal_diff` (combined working-tree
  diff + untracked), `file_diffs` (both-sides reconstruction for Monaco),
  `hunked_diff` + `parse_unified` (per-file/per-hunk for selective review),
  `apply_selection` (reassemble chosen hunks + `git apply [--reverse]` for
  selective merge/revert), and `workspace_info`. Diffs against the worktree's
  `base_sha` (fork point). Tested on real temp repos incl. a **selective hunk
  merge into a separate worktree**.
- **Generic query RPC** (`ClientMsg::Query`/`QueryResult`, protocol → v7) +
  `Manager::query` dispatch + `daemon_query` command — one message subsumes
  diff/file_diffs/hunked_diff/apply/contention/worktree/attribution/workspace/
  sessions, returning JSON in the frontend's shape.
- **Route flip:** `api.ts` is rewritten daemon-only (no HTTP); the **Phase-3
  worktree provisioning** flips to `daemonProvisionWorktree` (diff + worktree move
  together). Done as part of the Phase-7 cut-over.

## fs-watch move — DONE

The plan's Phase-6 bullet *"move fs-watching into the daemon"* is now delivered.
Current reality:

- **The daemon owns the one filesystem watcher** (`fswatch.rs`). Each live agent
  gets a per-session debounced `notify` watcher on its worktree, started at spawn
  (`session.rs::start_fs_watch`, RAII — drops with the session). It ports the old
  app-side noise filtering (deny-dirs scanned **relative to the watched root**,
  `CACHEDIR.TAG`, `.gitignore`, lockfile/swap suppression).
- **The app-side watcher is gone.** `src/fs_watch.rs`, `src/fswatch.ts`,
  `src/hooks/useFsWatch.ts`, the `watch_terminal`/`unwatch_terminal`/`clear_dirty`
  commands, `FsWatchState`, `postFsEvents`, and the `notify`/`ignore` app deps were
  all removed. `ShellGrid` no longer mounts a per-frame watcher.
- **Live push, not poll.** On each change the session records the events to
  `taime_activity_events` (kind=`fs`), accumulates the per-turn + badge dirty sets,
  and pushes `ServerMsg::FsDirty { paths }` to the attached client (the app mirrors
  it into the badge via `markDaemonFsDirty`). Review-clear routes to the daemon
  (`clear_dirty` query → `Session::clear_fs_dirty`).
- **Turns carry their files.** Each closed turn's `fs_dirty_paths` is filled from
  the watcher's per-turn set and persisted to `taime_agent_turns`
  (`session.rs::persist_turn`); the bridge forwards `fsDirtyPaths` to the app's live
  per-frame turn history.
- **Per-file authorship is real.** `attribution` builds the workspace team and, for
  each touched file, its contributors + last author from the durable fs-activity
  log; `graph` agents carry `branch`/`mode`/`turns`; `contention` is computed across
  the workspace's live agent worktrees (worktree `session_name` is no longer set).

## Known approximations (documented, not blocking)

- Turn `started_at`/`ended_at` are stamped at close time (no separate start
  wall-clock), and `attribution` contributors carry `turn_index: 0` — the per-file →
  specific-turn mapping isn't stored, so authorship is "which agent(s) touched this
  file," not "in which turn."
- Live `FsDirty` is pushed only to the **currently attached** client (one per
  session); `attach` replays the full accumulated dirty set immediately so a
  (re)attaching badge is correct without waiting for the next change. A frame that
  is mounted but not the attached client still updates only on (re)attach.
