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

## Remaining Phase-6 follow-ups (additive, not regressions)

- **fs-watch into the daemon** for attribution while the UI is closed: the app
  watcher (`useFsWatch`) still runs (UI subscriber); moving the watcher core into
  the daemon to record fs-events durably is the additive next step (the daemon
  already provisions the worktrees it would watch). `postFsEvents` is now a no-op.
- **Per-hunk authorship** awaits turn persistence (the daemon's turn boundaries
  recorded to `taime_agent_turns`); `attribution` currently returns an empty
  team/files map and the UI falls back to an ungrouped file list.
