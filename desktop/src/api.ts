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
   *  flag (binary resolvable in the daemon's env). Profiles aren't yet a daemon
   *  store — the launcher uses the default profile. */
  listProviders: () => daemonQuery<ProviderInfo[]>("providers", {}, PROVIDERS),
  listProfiles: async (): Promise<AgentProfileInfo[]> => [],

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
    session_name?: string;
  }): Promise<WorktreeInfo> => {
    const wt = await daemonProvisionWorktree(
      body.project_root,
      body.provider ?? "claude_code",
      body.isolate ?? false,
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
    };
  },

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
        mode: null,
        branch: null,
        member_of: null,
        turns: [],
      })),
      edges: g.edges.map((e) => ({ kind: e.kind, source: e.source, target: e.target, ts: null })),
      contention: [],
    };
  },

  /** The daemon watches worktrees natively; forwarding is a no-op now. */
  postFsEvents: async (
    _terminalId: string,
    _events: { path: string; kind?: string; ts?: number }[],
  ) => ({ recorded: 0, mode: "daemon", confidence: "certain" }),

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
    mode: string | null;
    branch: string | null;
    member_of: string | null;
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
