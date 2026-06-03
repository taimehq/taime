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

## Remaining Phase-6 work (parity-gated / live-validated)

- **fs-watch into the daemon.** Today it's app-side + per-*visible*-frame
  (`useFsWatch.ts`), so attribution dies when the UI closes. Extract
  `fs_watch.rs`'s watcher core into a shared module the daemon mounts per session
  (recording fs-events to `taime_activity_events`), keeping Tauri a UI subscriber.
  This is additive (the app watcher can stay) but integration-heavy; the worktree
  paths it watches are the daemon-provisioned ones.
- **diff_service per-hunk authorship.** Port `get_terminal_diff` / `get_file_diffs`
  / `get_hunked_diff` / `apply_selection` (the selective merge/revert) +
  `file_attribution` (per-hunk author via snapshot content-match) to `git`/`git2`.
  This is **diff-parity-gated** — it must match CAO's output on real repos before
  the frontend trusts it. `Store::worktree_path` (added here) gives the daemon the
  path to diff.
- **Route-layer flip.** Move the diff/hunks/attribution/graph/checkpoint `api.ts`
  calls to the daemon commands. The data *shapes* don't change (components keyed
  on them don't), but the fetch layer does — and this is also where the **Phase-3
  worktree provisioning switch** (`daemonProvisionWorktree`) flips, since diff and
  worktree must move together (CAO can't diff a daemon-provisioned worktree).
