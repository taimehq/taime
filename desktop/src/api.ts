import { invoke } from "@tauri-apps/api/core";
import { daemonQuery, daemonProvisionWorktree, daemonActivityGraph } from "./pty";
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
// Types (unchanged shapes — the daemon returns these directly)
// ---------------------------------------------------------------------------

export interface Session {
  id: string;
  name: string;
  status: string;
}

export interface Terminal {
  id: string;
  name: string;
  provider: string;
  session_name: string;
  agent_profile: string | null;
  status: string | null;
  last_active: string | null;
}

export interface TerminalMeta {
  id: string;
  tmux_session: string;
  tmux_window: string;
  provider: string;
  agent_profile: string | null;
  created_at: string | null;
  last_active: string | null;
}

export interface SessionDetail {
  session: Session | null;
  terminals: TerminalMeta[];
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
  terminal_id: string;
  provider: string | null;
  branch: string | null;
  mode: string | null;
  worktree_path: string | null;
  status: string | null;
  alive: boolean;
  dirty_count: number;
  dirty_paths: string[];
}

export interface TaskDetail {
  task: TaskInfo;
  agents: TaskAgent[];
  runs: WorkflowRunSummary[];
}

// ── Workflows (the loopable agent step-graph) ───────────────────────────────
export interface WorkflowNode {
  id: string;
  role: string;
  prompt: string;
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
  agent_key: string | null;
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
  terminal_id: string;
  files: FileDiffEntry[];
}

export interface FileContributor {
  terminal_id: string;
  provider: string | null;
  turn_index: number;
  ended_at: string | null;
}
export interface HunkAuthor {
  terminal_id: string;
  provider: string | null;
  turn_index: number;
}
export interface AttributionResponse {
  team: {
    terminal_id: string;
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
  terminal_id: string;
  base: string | null;
  files: HunkedFileEntry[];
}

export interface ApplyResult {
  applied: boolean;
  target_dir: string | null;
  files: string[];
  conflicts: string[];
  error: string | null;
}

export interface WorkspaceInfo {
  path: string;
  exists: boolean;
  is_git: boolean;
  repo_root: string | null;
  branch: string | null;
  head_short: string | null;
}

/** Per-agent worktree provenance. `terminal_id` == the daemon `terminal_key`. */
export interface WorktreeInfo {
  terminal_id: string;
  mode: "worktree" | "shared";
  worktree_path: string;
  project_root: string | null;
  repo_root: string | null;
  branch: string | null;
  base_sha: string | null;
  provider: string | null;
  member_of?: string | null;
  /** Task membership (null ⇒ Uncategorized). */
  task_id?: string | null;
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
   *  selectable roles; the chosen name flows back to the daemon at spawn. */
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

  /** Sessions are the daemon's agents (surfaced via the daemon registry); the
   *  tmux-shaped session grouping is gone. */
  listSessions: () => daemonQuery<Session[]>("sessions", {}, []),
  getSession: (name: string) =>
    daemonQuery<SessionDetail>("session_detail", { name }, { session: null, terminals: [] }),
  deleteSession: async (_name: string) => ({ success: true, deleted: [] as string[], errors: [] as unknown[] }),

  getWorkingDirectory: (id: string) =>
    daemonQuery<{ working_directory: string | null }>(
      "worktree",
      { terminal_key: id },
      { working_directory: null },
    ).then((w) => {
      const raw = w as unknown as { worktree_path?: string; working_directory?: string | null };
      return { working_directory: raw.worktree_path ?? raw.working_directory ?? null };
    }),

  getTerminalStatus: (_id: string): Promise<string | null> => Promise.resolve(null),

  getTerminalDiff: (id: string) =>
    daemonQuery<TerminalDiff>("terminal_diff", { terminal_key: id }, {
      working_directory: null,
      is_git: false,
      diff: "",
      files_changed: 0,
      error: null,
    }),

  getWorktree: (id: string) =>
    daemonQuery<WorktreeInfo | null>("worktree", { terminal_key: id }, null),

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
      terminal_id: wt.terminal_key,
      mode: wt.mode === "worktree" ? "worktree" : "shared",
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
  assignAgentTask: async (agentKey: string, taskId: string | null): Promise<string | null> => {
    const r = await daemonQuery<{ ok?: boolean; error?: string }>(
      "task_assign",
      { agent_key: agentKey, task_id: taskId },
      { error: "daemon unavailable" },
    );
    return r.error ?? null;
  },
  /** One task + member agents (live status, dirty rollup) + attached runs —
   *  the Task Review surface. */
  getTaskDetail: (id: string) => daemonQuery<TaskDetail | null>("task_detail", { id }, null),

  getFileDiffs: (id: string) =>
    daemonQuery<FileDiffsResponse>("file_diffs", { terminal_key: id }, { terminal_id: id, files: [] }),

  getHunks: (id: string) =>
    daemonQuery<HunkedDiffResponse>("hunked_diff", { terminal_key: id }, {
      terminal_id: id,
      base: null,
      files: [],
    }),

  getAttribution: (id: string) =>
    daemonQuery<AttributionResponse>("attribution", { terminal_key: id }, { team: [], files: {} }),

  applySelection: (
    id: string,
    body: {
      target: string;
      mode: "merge" | "revert";
      selections: Record<string, number[] | null>;
    },
  ) =>
    daemonQuery<ApplyResult>(
      "apply_selection",
      { terminal_key: id, target_dir: body.target, mode: body.mode, selections: body.selections },
      { applied: false, target_dir: body.target, files: [], conflicts: [], error: "daemon unavailable" },
    ),

  getContention: (session: string) =>
    daemonQuery<{ path: string; terminals: string[] }[]>("contention", { session }, []),

  /** The daemon records turns natively; checkpoints are a no-op now. */
  postCheckpoint: async (_terminalId: string, _boundary: "turn_start" | "turn_end") => ({}),

  /** The activity graph: daemon agents + inter-agent edges, mapped to the shape
   *  the ActivityGraph component expects. */
  getGraph: async (session: string): Promise<ActivityGraph> => {
    const g = await daemonActivityGraph();
    return {
      session,
      agents: g.agents.map((a) => ({
        terminal_id: a.id,
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
  clearDaemonDirty: (terminalId: string) =>
    daemonQuery<boolean>("clear_dirty", { terminal_key: terminalId }, true),

  getActivity: async (_params?: { session?: string; terminalId?: string; limit?: number }): Promise<ActivityEvent[]> => [],
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
    terminal_id: string;
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

export interface ActivityEvent {
  id: string;
  ts: string | null;
  kind: string;
  terminal_id: string | null;
  session_name: string | null;
  agent_profile: string | null;
  provider: string | null;
  target_terminal_id: string | null;
  path: string | null;
  change_kind: string | null;
  turn_id: string | null;
  snapshot_sha: string | null;
  meta: Record<string, unknown> | null;
}
