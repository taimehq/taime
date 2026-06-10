import { invoke } from "@tauri-apps/api/core";
import {
  daemonQuery,
  daemonQueryStrict,
  daemonProvisionWorktree,
  type DaemonActivityGraph,
} from "./pty";
import { inTauri } from "./backend";

/**
 * The data layer. After the CAO/tmux → all-Rust migration (Phase 6/7), there is
 * NO HTTP backend: every call routes to the `taime-session-daemon` via Tauri
 * commands (`daemon_query` for the diff/attribution/contention/worktree surface,
 * plus the dedicated daemon commands). The method signatures + return shapes are
 * unchanged, so the components are untouched — only this fetch layer moved.
 *
 * Session/terminal CRUD that was tmux-shaped (createSession/addTerminal/
 * getTerminal/output/input/exit) is gone: agents are launched + driven through
 * the daemon transport (see `store.launchAgentDaemon` + `pty.ts`).
 */

// ---------------------------------------------------------------------------
// Types (the daemon returns these directly)
// ---------------------------------------------------------------------------

/** One row from the daemon's `agents` query (liveness/reconcile surface).
 *  `agent_id` is THE identity — the attribution anchor everywhere. */
export interface AgentSummary {
  agent_id: string;
  status?: string | null;
}

export interface AgentProfileInfo {
  name: string;
  description: string;
  source: string;
}

/** A cron-triggered schedule as reported by the daemon. */
export interface ScheduleInfo {
  name: string;
  /** 5-field POSIX cron string. */
  schedule: string;
  agent_profile: string;
  provider: string;
  enabled: boolean;
  /** Unix seconds of the last fire / next scheduled fire, or null. */
  last_run: number | null;
  next_run: number | null;
  /** Workspace the fire runs in (null = daemon cwd; no task targeting). */
  workspace_root: string | null;
  /** Explicit task behavior: null = uncategorized, "fixed" = attach to
   *  `task_id`, "per_run" = create a task per fire (explicit opt-in). */
  task_mode: "fixed" | "per_run" | string | null;
  task_id: string | null;
  /** The markdown body fired as the agent's prompt. Daemon-serialized as
   *  `prompt`; absent on older daemons — read tolerantly. */
  prompt?: string | null;
}

/** Fields the Add-schedule dialog sends to create/replace a schedule. */
export interface ScheduleInput {
  name: string;
  schedule: string;
  agent_profile: string;
  provider: string;
  prompt: string;
  script?: string | null;
  workspace_root?: string | null;
  task_mode?: "fixed" | "per_run" | null;
  task_id?: string | null;
}

// ── Tasks (v9): workspace-scoped units of user intent ───────────────────────
// Membership is a nullable task_id on the agent's durable record; a Task owns
// grouping/lifecycle/review rollups — never raw attribution (Agent-ID anchored).

export type TaskStatus = "open" | "in_review" | "done" | "archived";

export interface TaskInfo {
  id: string;
  workspace_root: string;
  title: string;
  description: string;
  status: TaskStatus | string;
  /** Unix seconds. */
  created_at: number;
  updated_at: number;
  archived_at: number | null;
  /** Member-agent count (derived rollup, computed daemon-side). */
  agent_count: number;
}

/** One member agent in a task detail: worktree row + live status + dirty rollup. */
export interface TaskAgent {
  agent_id: string;
  provider: string | null;
  branch: string | null;
  mode: string | null;
  worktree_path: string | null;
  status: string | null;
  alive: boolean;
  dirty_count: number;
  dirty_paths: string[];
  /** Role/profile (orchestrator / product-builder / researcher / …), daemon
   *  -reported from the roles map. null ⇒ unknown (e.g. after a daemon restart). */
  role: string | null;
}

export interface TaskDetail {
  task: TaskInfo;
  agents: TaskAgent[];
  runs: WorkflowRunSummary[];
}

// ── Workflows (the loopable agent step-graph) ───────────────────────────────
export interface WorkflowNode {
  id: string;
  /** Agent profile the node spawns. The daemon serializes `profile`
   *  (workflow.rs); `role` is only a parse alias older payloads carried —
   *  read `profile ?? role`. */
  profile?: string;
  role?: string;
  prompt: string;
  /** Provider override for this node (null/absent ⇒ the run default). */
  provider?: string | null;
  /** Key the node's final output is stored under for downstream prompts. */
  output_key?: string | null;
}
export interface WorkflowEdge {
  from: string;
  to: string;
  /** "always" | "keyword:WORD" | "/regex/" — first match wins; none = terminal. */
  when: string;
}
/** Live state of one node within a run. */
export interface WorkflowNodeState {
  status: "pending" | "running" | "completed" | "failed" | string;
  iteration: number;
  agent_id: string | null;
}
export interface WorkflowRunSummary {
  id: string;
  workflow_name: string;
  status: "running" | "completed" | "failed" | string;
  started_at: number | null;
  ended_at: number | null;
  error: string | null;
  /** Task this run executes inside (null ⇒ Uncategorized). */
  task_id: string | null;
  /** node_id → its latest state in this run. */
  node_states: Record<string, WorkflowNodeState>;
}
export interface WorkflowInfo {
  name: string;
  source: string;
  entry: string;
  nodes: WorkflowNode[];
  edges: WorkflowEdge[];
  /** The most recent run, if any (drives the graph's status colors). */
  last_run: WorkflowRunSummary | null;
}

export interface ProviderInfo {
  name: string;
  binary: string;
  installed: boolean;
}

export interface HealthInfo {
  status: string;
  service: string;
}

export interface InboxMessage {
  id: string;
  sender_id: string;
  receiver_id: string;
  message: string;
  status: "pending" | "delivered" | "failed";
  created_at: string | null;
}

/** A unified working-tree diff for the terminal's worktree. */
export interface TerminalDiff {
  working_directory: string | null;
  is_git: boolean;
  diff: string;
  files_changed: number;
  error?: string | null;
}

export interface FileDiffEntry {
  path: string;
  status: "added" | "modified" | "deleted" | "renamed" | string;
  original: string;
  modified: string;
  additions: number;
  deletions: number;
  binary: boolean;
  old_path: string | null;
}

export interface FileDiffsResponse {
  agent_id: string;
  files: FileDiffEntry[];
}

export interface FileContributor {
  agent_id: string;
  provider: string | null;
  turn_index: number;
  ended_at: string | null;
}
export interface HunkAuthor {
  agent_id: string;
  provider: string | null;
  turn_index: number;
}
export interface AttributionResponse {
  team: {
    agent_id: string;
    provider: string | null;
    mode: string | null;
    member_of: string | null;
  }[];
  files: Record<
    string,
    {
      last: FileContributor | null;
      contributors: FileContributor[];
      hunks?: Record<string, HunkAuthor>;
    }
  >;
}

export interface HunkEntry {
  index: number;
  header: string;
  text: string;
  additions: number;
  deletions: number;
}

export interface HunkedFileEntry {
  path: string;
  old_path: string | null;
  hunks: HunkEntry[];
}

export interface HunkedDiffResponse {
  agent_id: string;
  base: string | null;
  /** Fingerprint of the served patch — echo it back to applySelection as the
   *  reviewed-content binding; the daemon refuses a merge when the worktree
   *  has moved since this response was fetched. */
  digest: string | null;
  files: HunkedFileEntry[];
}

export interface ApplyResult {
  applied: boolean;
  target_dir: string | null;
  files: string[];
  conflicts: string[];
  /** True when the refusal is a digest mismatch — the worktree changed since
   *  the diff was reviewed; reload and re-review. */
  stale?: boolean;
  error: string | null;
}

/** Result of `commit_merge`: the selected hunks landed AND were recorded as one
 *  provenance commit (Co-authored-by + Taime-* trailer + refs/notes/taime note).
 *  Extends the apply result with the commit + push outcome. */
export interface CommitMergeResult extends ApplyResult {
  /** True when the merge was recorded as a commit (false ⇒ refused / conflicted
   *  / stale — see `error`/`conflicts`/`stale`, exactly like ApplyResult). */
  committed: boolean;
  /** Short sha of the provenance commit (present when `committed`). */
  commit?: string;
  /** Whether the `refs/notes/taime` git note was attached (best-effort). */
  note_written?: boolean;
  /** Whether an opt-in push of the target branch succeeded. */
  pushed?: boolean;
  /** A structured push failure (no remote / no upstream / rejected) — the commit
   *  still stands. */
  push_error?: string;
}

/** One recorded provenance merge (the merged-✓ badge / history). */
export interface MergeRecord {
  id: number | null;
  commit: string;
  target: string;
  target_repo: string;
  base_sha: string | null;
  archive_ref: string | null;
  digest: string | null;
  scope: "full" | "partial";
  hunks_selected: number;
  hunks_total: number;
  reviewed: boolean;
  pushed: boolean;
  pr_url: string | null;
  files: string[];
  merged_at: number;
}

/** The portable attribution export for an agent (gap #2) — identity + current
 *  change set + recorded merges + turn count. `schema: "taime.attribution/v1"`. */
export interface AgentAttribution {
  schema: string;
  agent_id: string;
  provider: string;
  base_sha: string | null;
  archive_ref: string | null;
  task_id: string | null;
  turns: number;
  current_files: string[];
  merges: MergeRecord[];
  exported_at: number;
}

export interface WorkspaceInfo {
  path: string;
  exists: boolean;
  is_git: boolean;
  repo_root: string | null;
  branch: string | null;
  head_short: string | null;
}

/** Per-agent worktree provenance, keyed by the agent's id. */
export interface WorktreeInfo {
  agent_id: string;
  mode: "isolated" | "shared";
  worktree_path: string;
  project_root: string | null;
  repo_root: string | null;
  branch: string | null;
  base_sha: string | null;
  provider: string | null;
  member_of?: string | null;
  /** Task membership (null ⇒ Uncategorized). */
  task_id?: string | null;
  /** Archive-then-reclaim state: true once the physical checkout has been
   *  reclaimed and the diff renders from `refs/taime/archive/*`. */
  reclaimed?: boolean;
  archived_at?: number | null;
  reclaimed_at?: number | null;
}

/** A finished agent whose worktree checkout is still on disk and can be reclaimed
 *  (archived into `refs/taime/archive/*`, then the checkout removed). */
export interface ReclaimableAgent {
  agent_id: string;
  project_root: string | null;
  worktree_path: string;
  created_at: number | null;
  task_id: string | null;
}

export interface LaunchIsolation {
  isolate?: boolean;
  projectRoot?: string;
}

/** The four supported CLIs (the daemon's provider registry). */
const PROVIDERS: ProviderInfo[] = [
  { name: "claude_code", binary: "claude", installed: true },
  { name: "codex", binary: "codex", installed: true },
  { name: "gemini_cli", binary: "gemini", installed: true },
  { name: "grok_cli", binary: "grok", installed: true },
];

// ---------------------------------------------------------------------------
// Daemon-backed surface
// ---------------------------------------------------------------------------

export const api = {
  health: async (): Promise<HealthInfo> => ({ status: "ok", service: "taime-session-daemon" }),

  /** The daemon's provider registry (the 4 CLIs) with an accurate `installed`
   *  flag (binary resolvable in the daemon's env). */
  listProviders: () => daemonQuery<ProviderInfo[]>("providers", {}, PROVIDERS),

  /** Agent profiles from the daemon's profile store (`~/.taime/agents/*.toml`
   *  plus the built-in `default`/`orchestrator`). The launcher renders these as
   *  selectable profiles; the chosen name flows back to the daemon at spawn. */
  listProfiles: () =>
    daemonQuery<AgentProfileInfo[]>("profiles", {}, [
      { name: "default", description: "Plain agent — no orchestration tools.", source: "builtin" },
      {
        name: "orchestrator",
        description: "Can assign / handoff to other agents.",
        source: "builtin",
      },
    ]),

  // ── Schedules (cron-triggered unattended agent runs) ──────────────────────
  // All ride the generic daemon Query RPC (reads + mutations), like clear_dirty.
  /** All schedules from the daemon (`~/.taime/schedules/*.md` + UI-created),
   *  with their cron, target role/provider, enabled flag, and last/next run. */
  listSchedules: () => daemonQuery<ScheduleInfo[]>("schedules", {}, []),
  /** Create (or replace) a schedule; the daemon writes its `.md` + computes the
   *  next run. Returns an error string on a bad cron/field, else null. */
  addSchedule: async (input: ScheduleInput): Promise<string | null> => {
    const r = await daemonQuery<{ ok?: boolean; error?: string }>(
      "schedule_add",
      { ...input },
      { error: "daemon unavailable" },
    );
    return r.error ?? null;
  },
  /** Fire a schedule now (manual test run), bypassing the cron + gate. */
  runSchedule: async (name: string): Promise<string | null> => {
    const r = await daemonQuery<{ ok?: boolean; error?: string }>(
      "schedule_run",
      { name },
      { error: "daemon unavailable" },
    );
    return r.error ?? null;
  },
  /** Enable/disable a schedule (disabled schedules don't fire on cron). */
  toggleSchedule: async (name: string, enabled: boolean): Promise<void> => {
    await daemonQuery("schedule_toggle", { name, enabled }, { ok: true });
  },
  /** Delete a schedule (removes its `.md` + row). */
  deleteSchedule: async (name: string): Promise<void> => {
    await daemonQuery("schedule_delete", { name }, { ok: true });
  },

  // ── Workflows (loopable agent step-graphs) ────────────────────────────────
  /** All workflows (`~/.taime/workflows/*.json` + orchestrator-generated), each
   *  with its node/edge graph and most-recent run state. */
  listWorkflows: () => daemonQuery<WorkflowInfo[]>("workflows", {}, []),
  /** Create a workflow from a JSON definition string. The daemon validates +
   *  persists it (source "user"); validation problems come back as
   *  `{ok:false, error}` — never a wire error. */
  createWorkflow: (definition: string) =>
    daemonQuery<{ ok: boolean; name?: string; error?: string }>(
      "workflow_create",
      { definition },
      { ok: false, error: "daemon unreachable" },
    ),
  /** Start a run of a workflow now; returns the run id, or an error string.
   *  `projectRoot` (the active workspace) is where the nodes' worktrees fork
   *  from; `taskId` attaches the run (and its node agents) to a Task. */
  runWorkflow: async (
    name: string,
    projectRoot?: string | null,
    taskId?: string | null,
  ): Promise<{ run_id?: string; error?: string }> =>
    daemonQuery<{ run_id?: string; error?: string }>(
      "workflow_run",
      { name, project_root: projectRoot ?? null, task_id: taskId ?? null },
      { error: "daemon unavailable" },
    ),
  /** Live status of a run (poll while a run is active). */
  getWorkflowRun: (runId: string) =>
    daemonQuery<WorkflowRunSummary | null>("workflow_run_status", { run_id: runId }, null),
  /** Delete a workflow (removes its `.json` + rows). */
  deleteWorkflow: async (name: string): Promise<void> => {
    await daemonQuery("workflow_delete", { name }, { ok: true });
  },

  /** Probe a project dir (app-side git read — no daemon, no boot race). */
  getWorkspaceInfo: async (path: string): Promise<WorkspaceInfo> => {
    if (!inTauri())
      return { path, exists: false, is_git: false, repo_root: null, branch: null, head_short: null };
    try {
      return await invoke<WorkspaceInfo>("workspace_info", { path });
    } catch {
      return { path, exists: false, is_git: false, repo_root: null, branch: null, head_short: null };
    }
  },

  /** Create a brand-new project folder (the "Start something new" flow), then
   *  return its WorkspaceInfo. App-side (mirrors getWorkspaceInfo) so the dialog
   *  can generate a workspace from a typed / not-yet-existing path. Idempotent.
   *  `gitInit` defaults false — the founding agent runs its own git init so
   *  genesis stays attributed to its turn. Outside Tauri it's a no-op probe. */
  initWorkspace: async (path: string, gitInit = false): Promise<WorkspaceInfo> => {
    if (!inTauri()) return api.getWorkspaceInfo(path);
    try {
      return await invoke<WorkspaceInfo>("workspace_init", { path, gitInit });
    } catch {
      return api.getWorkspaceInfo(path);
    }
  },

  /** Permanently delete a directory and all its contents (the "delete workspace
   *  → also remove from disk" path). DESTRUCTIVE — gated in the UI behind a typed
   *  confirmation; the backend additionally refuses root/home/shallow paths.
   *  Returns an error string on failure, else null. */
  deleteDirectory: async (path: string): Promise<string | null> => {
    if (!inTauri()) return "Deleting the folder is only available in the desktop app.";
    try {
      await invoke("delete_directory", { path });
      return null;
    } catch (e) {
      return e instanceof Error ? e.message : String(e);
    }
  },

  /** Tear down a workspace daemon-side (the "delete workspace" flow): stop every
   *  agent provisioned from it, reclaim their checkouts, remove their rows, and
   *  delete the workspace's tasks. Returns counts (or an error).
   *
   *  `destroyArchives` gates the only IRREVERSIBLE part — the durable
   *  `refs/taime/archive/*` snapshots of reclaimed agents' UNMERGED work. Default
   *  (false) = soft, non-lossy: archives are PRESERVED. Pass true only behind the
   *  typed-confirm path. Folder deletion is a separate typed-confirm step
   *  (deleteDirectory). */
  deleteWorkspaceData: (workspaceRoot: string, destroyArchives = false) =>
    daemonQuery<{
      ok?: boolean;
      agents?: number;
      killed?: number;
      tasks?: number;
      error?: string;
    }>(
      "workspace_delete",
      { workspace_root: workspaceRoot, destroy_archives: destroyArchives },
      { ok: false, error: "daemon unavailable" },
    ),

  /** How many of a workspace's agents hold ARCHIVED, unmerged work (a durable
   *  `refs/taime/archive/*` snapshot). This is exactly what a HARD delete
   *  destroys — the dialog shows the count and demands a typed confirm. */
  workspaceArchivedCount: (workspaceRoot: string) =>
    daemonQuery<{ count: number }>(
      "workspace_archived_count",
      { workspace_root: workspaceRoot },
      { count: 0 },
    ),

  // ── Durable review acks (the daemon merge gate's ack) ─────────────────────
  /** Persist that the user acknowledged an agent's current changes — so the ack
   *  survives a UI/daemon restart (it was frontend-local before). Resolves
   *  `false` when the ack was NOT durably recorded (daemon down or persistence
   *  off) — the daemon's merge gate reads the durable row, so callers about to
   *  merge must treat `false` as a hard stop, not a cosmetic miss. */
  markReviewed: (agentId: string) =>
    daemonQuery<boolean>("mark_reviewed", { agent_id: agentId }, false),
  /** Drop an agent's standing ack: its dirty set grew past what was
   *  acknowledged, so new changes need a fresh ack. */
  clearReviewed: (agentId: string) =>
    daemonQuery<boolean>("clear_reviewed", { agent_id: agentId }, true),
  /** Agent ids with a standing review ack — hydrates the ack state on boot. */
  reviewedAgents: () => daemonQuery<string[]>("reviewed", {}, []),

  /** The daemon's agents (its registry is the only roster); the tmux-shaped
   *  session grouping is gone. */
  listAgents: () => daemonQuery<AgentSummary[]>("agents", {}, []),

  getWorkingDirectory: (id: string) =>
    daemonQuery<{ working_directory: string | null }>(
      "worktree",
      { agent_id: id },
      { working_directory: null },
    ).then((w) => {
      const raw = w as unknown as { worktree_path?: string; working_directory?: string | null };
      return { working_directory: raw.worktree_path ?? raw.working_directory ?? null };
    }),

  getTerminalDiff: (id: string) =>
    daemonQuery<TerminalDiff>("terminal_diff", { agent_id: id }, {
      working_directory: null,
      is_git: false,
      diff: "",
      files_changed: 0,
      error: null,
    }),

  getWorktree: (id: string) =>
    daemonQueryStrict<WorktreeInfo | null>("worktree", { agent_id: id }),

  // ── Worktree retention (archive-then-reclaim) ─────────────────────────────
  /** Finished (not-live) agents whose worktree checkout is still on disk —
   *  the cleanup panel's list. Reclaiming archives first, so nothing is lost. */
  listReclaimable: () => daemonQuery<ReclaimableAgent[]>("reclaimable", {}, []),
  /** Archive + reclaim one agent's checkout NOW. The agent stays fully
   *  reviewable and mergeable from its archive afterwards. */
  reclaimAgent: (agentId: string) =>
    daemonQuery<{ ok: boolean; agent_id: string }>(
      "reclaim_agent",
      { agent_id: agentId },
      { ok: false, agent_id: agentId },
    ),

  /** Provision a daemon-owned worktree for an agent (Phase 3). */
  provisionWorktree: async (body: {
    project_root: string;
    provider?: string;
    isolate?: boolean;
    /** Task membership stamped onto the worktree row (null ⇒ Uncategorized). */
    task_id?: string | null;
  }): Promise<WorktreeInfo> => {
    const wt = await daemonProvisionWorktree(
      body.project_root,
      body.provider ?? "claude_code",
      body.isolate ?? false,
      body.task_id ?? null,
    );
    return {
      agent_id: wt.agent_id,
      mode: wt.mode === "isolated" ? "isolated" : "shared",
      worktree_path: wt.worktree_path,
      project_root: wt.project_root,
      repo_root: wt.repo_root,
      branch: wt.branch,
      base_sha: wt.base_sha,
      provider: body.provider ?? null,
      member_of: null,
      task_id: body.task_id ?? null,
    };
  },

  // ── Tasks (workspace-scoped intent grouping; all on the Query RPC) ────────
  /** Tasks of a workspace (newest first; archived hidden unless asked). */
  listTasks: (workspaceRoot: string, includeArchived = false) =>
    daemonQuery<TaskInfo[]>(
      "tasks",
      { workspace_root: workspaceRoot, include_archived: includeArchived },
      [],
    ),
  /** Create a task (status `open`); returns it, or null on error. */
  createTask: async (
    workspaceRoot: string,
    title: string,
    description = "",
  ): Promise<TaskInfo | null> => {
    const r = await daemonQuery<TaskInfo | { error: string }>(
      "task_create",
      { workspace_root: workspaceRoot, title, description },
      { error: "daemon unavailable" },
    );
    return "error" in r ? null : r;
  },
  /** Update title/description/status. Status is the lifecycle enum; `archived`
   *  stamps archived_at. Returns an error string, else null. */
  updateTask: async (
    id: string,
    patch: { title?: string; description?: string; status?: TaskStatus },
  ): Promise<string | null> => {
    const r = await daemonQuery<{ ok?: boolean; error?: string }>(
      "task_update",
      { id, ...patch },
      { error: "daemon unavailable" },
    );
    return r.error ?? null;
  },
  /** Delete a task — members are DEMOTED to Uncategorized (never killed). */
  deleteTask: async (id: string): Promise<string | null> => {
    const r = await daemonQuery<{ ok?: boolean; error?: string }>(
      "task_delete",
      { id },
      { error: "daemon unavailable" },
    );
    return r.error ?? null;
  },
  /** Assign (or unassign with null) an agent to a task — membership is the
   *  nullable task_id pointer; at most one task per agent. */
  assignAgentTask: async (agentId: string, taskId: string | null): Promise<string | null> => {
    const r = await daemonQuery<{ ok?: boolean; error?: string }>(
      "task_assign",
      { agent_id: agentId, task_id: taskId },
      { error: "daemon unavailable" },
    );
    return r.error ?? null;
  },
  /** One task + member agents (live status, dirty rollup) + attached runs —
   *  the Task Review surface. */
  getTaskDetail: (id: string) => daemonQuery<TaskDetail | null>("task_detail", { id }, null),

  // The review-surface reads are STRICT (reject on daemon-down, never a typed
  // fallback): an empty diff from a dead daemon is indistinguishable from "no
  // changes", and rendering it as authoritative would invite an ack of unseen
  // changes.
  getFileDiffs: (id: string) =>
    daemonQueryStrict<FileDiffsResponse>("file_diffs", { agent_id: id }),

  getHunks: (id: string) =>
    daemonQueryStrict<HunkedDiffResponse>("hunked_diff", { agent_id: id }),

  getAttribution: (id: string) =>
    daemonQueryStrict<AttributionResponse>("attribution", { agent_id: id }),

  applySelection: (
    id: string,
    body: {
      target: string;
      mode: "merge" | "revert";
      selections: Record<string, number[] | null>;
      /** The digest of the hunked_diff the user reviewed — required for merge
       *  (the daemon refuses unbound merges and stale views). */
      expectedDigest?: string;
    },
  ) =>
    daemonQuery<ApplyResult>(
      "apply_selection",
      {
        agent_id: id,
        target_dir: body.target,
        mode: body.mode,
        selections: body.selections,
        expected_digest: body.expectedDigest,
      },
      { applied: false, target_dir: body.target, files: [], conflicts: [], error: "daemon unavailable" },
    ),

  /** Merge selected hunks into `target` AND record them as one provenance commit
   *  (Co-authored-by + Taime-* trailer + git note). `expectedDigest` is required
   *  (the integrity floor); `push` optionally pushes the target branch. The merge
   *  records reviewed=true when a standing ack exists, else autonomous. */
  commitMerge: (
    id: string,
    body: {
      target: string;
      selections: Record<string, number[] | null>;
      expectedDigest: string;
      push?: boolean;
    },
  ) =>
    daemonQuery<CommitMergeResult>(
      "commit_merge",
      {
        agent_id: id,
        target_dir: body.target,
        selections: body.selections,
        expected_digest: body.expectedDigest,
        push: body.push ?? false,
      },
      {
        committed: false,
        applied: false,
        target_dir: body.target,
        files: [],
        conflicts: [],
        error: "daemon unavailable",
      },
    ),

  /** An agent's recorded provenance merges, newest first (the merged-✓ badge). */
  mergeHistory: (id: string) =>
    daemonQuery<MergeRecord[]>("merge_history", { agent_id: id }, []),

  /** The portable attribution export for an agent (gap #2). STRICT: a daemon-down
   *  read must reject, never silently return an empty artifact. */
  exportAttribution: (id: string) =>
    daemonQueryStrict<AgentAttribution>("export_attribution", { agent_id: id }),

  getContention: (session: string) =>
    daemonQueryStrict<{ path: string; terminals: string[] }[]>("contention", { session }),

  /** The activity graph: daemon agents + inter-agent edges, mapped to the shape
   *  the ActivityGraph component expects. Pass the active workspace root to scope
   *  the team to that workspace; pass `""` for the daemon-wide roster (used by
   *  agent-row lookups that just project to a single agent). STRICT: rejects on
   *  daemon-down (an empty roster must mean "no agents", not "no daemon"). */
  getGraph: async (workspaceRoot: string): Promise<ActivityGraph> => {
    const g = await daemonQueryStrict<DaemonActivityGraph>("graph", {
      workspace_root: workspaceRoot,
    });
    return {
      session: workspaceRoot,
      agents: g.agents.map((a) => ({
        agent_id: a.agent_id,
        provider: a.provider,
        status: a.status ?? null,
        mode: a.mode ?? null,
        branch: a.branch ?? null,
        member_of: a.member_of ?? null,
        task_id: a.task_id ?? null,
        turns: (a.turns ?? []).map((t) => ({
          id: t.id,
          turn_index: t.turn_index,
          started_at: t.started_at,
          ended_at: t.ended_at,
          files_touched: t.files_touched,
          start_snapshot: t.start_snapshot,
          end_snapshot: t.end_snapshot,
        })),
      })),
      edges: g.edges.map((e) => ({ kind: e.kind, source: e.source, target: e.target, ts: null })),
      contention: g.contention ?? [],
    };
  },

  /** Tell the daemon to reset an agent's accumulated dirty set (the user reviewed
   *  its diff), so the next `FsDirty` push starts fresh. */
  clearDaemonDirty: (agentId: string) =>
    daemonQuery<boolean>("clear_dirty", { agent_id: agentId }, true),
};

export interface GraphTurn {
  id: string;
  turn_index: number;
  started_at: string | null;
  ended_at: string | null;
  files_touched: string[];
  start_snapshot: string | null;
  end_snapshot: string | null;
}

export interface ActivityGraph {
  session: string;
  agents: {
    agent_id: string;
    provider: string | null;
    /** Inferred live status (IDLE/PROCESSING/WAITING_USER_ANSWER/COMPLETED/ERROR). */
    status: string | null;
    mode: string | null;
    branch: string | null;
    member_of: string | null;
    /** Task membership (null ⇒ Uncategorized) — for task-filtered views. */
    task_id: string | null;
    turns: GraphTurn[];
  }[];
  edges: { kind: string; source: string | null; target: string | null; ts: string | null }[];
  contention: { path: string; terminals: string[] }[];
}
