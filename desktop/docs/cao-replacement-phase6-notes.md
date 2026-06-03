# CAO replacement — Phase 6 implementation notes

**Status:** the **daemon-side activity graph** is built. The diff/per-hunk port,
the fs-watch move, and the frontend route flip are the parity-gated /
integration-heavy / live-validated remainder (below). Companion to
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

## What "daemon-side attribution" means here (honest scope)

The plan's Phase-6 bullet said *"move fs-watching into the daemon."* That is **not**
what was delivered, and the language overstated it. What's actually true:

- **The attribution OUTPUTS work with the UI closed**, because the daemon computes
  them on demand from **git + the stored worktrees/edges**: `terminal_diff`,
  `file_diffs`, `hunked_diff`, `contention` (git across a session's worktrees), and
  the activity `graph` (assign/handoff/message edges persisted by Phase 5). None of
  these need a running UI or live fs-events — git is the source of truth at query
  time, and the worktree rows + edges are durable.
- **The live fs-event SUBSTRATE did NOT move.** `fs_watch.rs` + `useFsWatch` +
  `watch_terminal`/`unwatch_terminal` are still app-side and mount per *visible*
  frame, driving the real-time **dirty badge** only. `postFsEvents` is a no-op (the
  daemon doesn't ingest a live event stream), and daemon turns still emit
  `fs_dirty_paths: []`. So the per-file change **timeline** for a *headless* agent
  isn't captured — only its final git diff is.
- **Rich per-hunk authorship is not delivered:** `attribution` returns an empty
  team/files map (the UI falls back to an ungrouped file list), because it needs
  per-turn snapshots persisted to `taime_agent_turns`, which the daemon doesn't
  write yet.

## Genuine remaining work (additive)

- Extract the `fs_watch.rs` watcher core into a daemon module mounted per session
  (recording fs-events to `taime_activity_events`), so the live change timeline +
  dirty state survive a closed UI — Tauri becomes a pure subscriber.
- Persist daemon turn boundaries (`taime_agent_turns`) so `attribution` can return
  real per-hunk authorship (the diff/snapshot machinery already exists).
