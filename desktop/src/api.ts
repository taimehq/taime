import { getConfig } from "./config";

/**
 * Base URL is resolved once from the Tauri layer (config.ts → get_api_url) and
 * cached. Every request awaits it lazily, so there is no boot-ordering issue and
 * no hardcoded port/host anywhere in the frontend.
 */
async function apiBase(): Promise<string> {
  return (await getConfig()).apiUrl;
}

/** ws://host:port/terminals/{id}/ws — the live PTY stream endpoint. */
export async function terminalWsUrl(terminalId: string): Promise<string> {
  const { wsUrl } = await getConfig();
  return `${wsUrl}/terminals/${terminalId}/ws`;
}

async function fetchJSON<T>(
  path: string,
  opts?: RequestInit & { timeoutMs?: number },
): Promise<T> {
  const base = await apiBase();
  const controller = new AbortController();
  const timeout = setTimeout(
    () => controller.abort(),
    opts?.timeoutMs ?? 10000,
  );
  try {
    const res = await fetch(`${base}${path}`, {
      ...opts,
      signal: controller.signal,
    });
    if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
    return res.json();
  } finally {
    clearTimeout(timeout);
  }
}

// ---------------------------------------------------------------------------
// Types (mirror the CAO backend JSON shapes; verified against a live server)
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
  session: Session;
  terminals: TerminalMeta[];
}

export interface AgentProfileInfo {
  name: string;
  description: string;
  /** Where the profile came from (e.g. "built-in", "local"). */
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

/** A unified working-tree diff for the terminal's cwd (added by Taime; step 5). */
export interface TerminalDiff {
  working_directory: string | null;
  is_git: boolean;
  diff: string;
  files_changed: number;
  error?: string | null;
}

/** One changed file with both sides for a side-by-side review (Taime). */
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

/** One hunk of a file's diff (Taime selective merge/revert). */
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

/** Probe of a candidate project directory for the workspace picker. */
export interface WorkspaceInfo {
  path: string;
  exists: boolean;
  is_git: boolean;
  repo_root: string | null;
  branch: string | null;
  head_short: string | null;
}

/** Per-agent worktree provenance (Taime attribution). */
export interface WorktreeInfo {
  terminal_id: string;
  mode: "worktree" | "shared";
  worktree_path: string;
  project_root: string | null;
  repo_root: string | null;
  branch: string | null;
  base_sha: string | null;
  provider: string | null;
}

/** Options for launching an agent into an isolated git worktree. */
export interface LaunchIsolation {
  isolate?: boolean;
  projectRoot?: string;
}

function isolationQuery(opts?: LaunchIsolation): string {
  if (!opts?.isolate) return "";
  return (
    `&isolate=true` +
    `${opts.projectRoot ? `&project_root=${encodeURIComponent(opts.projectRoot)}` : ""}`
  );
}

// ---------------------------------------------------------------------------
// REST surface
// ---------------------------------------------------------------------------

export const api = {
  health: () => fetchJSON<HealthInfo>("/health"),

  listProviders: () => fetchJSON<ProviderInfo[]>("/agents/providers"),
  listProfiles: () => fetchJSON<AgentProfileInfo[]>("/agents/profiles"),

  /** Taime-added: probe a candidate project dir (exists / git / branch). */
  getWorkspaceInfo: (path: string) =>
    fetchJSON<WorkspaceInfo>(
      `/workspace/info?path=${encodeURIComponent(path)}`,
      { timeoutMs: 8000 },
    ),

  listSessions: () => fetchJSON<Session[]>("/sessions"),
  getSession: (name: string) =>
    fetchJSON<SessionDetail>(`/sessions/${name}`),

  /** Create a new session (and its first terminal). Long timeout: CLI cold-start. */
  createSession: (
    provider: string,
    agentProfile: string,
    sessionName?: string,
    workingDirectory?: string,
    isolation?: LaunchIsolation,
  ) =>
    fetchJSON<Terminal>(
      `/sessions?provider=${provider}&agent_profile=${agentProfile}` +
        `${sessionName ? `&session_name=${encodeURIComponent(sessionName)}` : ""}` +
        `${workingDirectory ? `&working_directory=${encodeURIComponent(workingDirectory)}` : ""}` +
        isolationQuery(isolation),
      { method: "POST", timeoutMs: 90000 },
    ),

  deleteSession: (name: string) =>
    fetchJSON<{ success: boolean; deleted: string[]; errors: unknown[] }>(
      `/sessions/${name}`,
      { method: "DELETE" },
    ),

  /** Add another agent terminal to an existing session. */
  addTerminal: (
    sessionName: string,
    provider: string,
    agentProfile: string,
    workingDirectory?: string,
    isolation?: LaunchIsolation,
  ) =>
    fetchJSON<Terminal>(
      `/sessions/${sessionName}/terminals?provider=${provider}&agent_profile=${agentProfile}` +
        `${workingDirectory ? `&working_directory=${encodeURIComponent(workingDirectory)}` : ""}` +
        isolationQuery(isolation),
      { method: "POST", timeoutMs: 90000 },
    ),

  getTerminal: (id: string) => fetchJSON<Terminal>(`/terminals/${id}`),
  getTerminalStatus: (id: string) =>
    fetchJSON<Terminal>(`/terminals/${id}`).then((t) => t.status),
  getWorkingDirectory: (id: string) =>
    fetchJSON<{ working_directory: string | null }>(
      `/terminals/${id}/working-directory`,
    ),
  getTerminalOutput: (id: string, mode: "full" | "last" = "full") =>
    fetchJSON<{ output: string; mode: string }>(
      `/terminals/${id}/output?mode=${mode}`,
    ),
  sendInput: (id: string, message: string) =>
    fetchJSON<{ success: boolean }>(
      `/terminals/${id}/input?message=${encodeURIComponent(message)}`,
      { method: "POST" },
    ),
  exitTerminal: (id: string) =>
    fetchJSON<{ success: boolean }>(`/terminals/${id}/exit`, {
      method: "POST",
    }),
  deleteTerminal: (id: string) =>
    fetchJSON<{ success: boolean }>(`/terminals/${id}`, { method: "DELETE" }),

  /** Taime-added: unified diff of the terminal's working tree (step 5). */
  getTerminalDiff: (id: string) =>
    fetchJSON<TerminalDiff>(`/terminals/${id}/diff`, { timeoutMs: 20000 }),

  /** Taime-added: per-agent worktree provenance, or null if not isolated. */
  getWorktree: (id: string) =>
    fetchJSON<WorktreeInfo | null>(`/terminals/${id}/worktree`),

  /** Taime-added: structured per-file diff (both sides) for side-by-side review. */
  getFileDiffs: (id: string) =>
    fetchJSON<FileDiffsResponse>(`/terminals/${id}/file-diffs`, {
      timeoutMs: 20000,
    }),

  /** Taime-added: per-file, per-hunk diff for selective merge/revert. */
  getHunks: (id: string) =>
    fetchJSON<HunkedDiffResponse>(`/terminals/${id}/hunks`, { timeoutMs: 20000 }),

  /** Taime-added: selectively merge/revert chosen files/hunks. */
  applySelection: (
    id: string,
    body: {
      target: string;
      mode: "merge" | "revert";
      selections: Record<string, number[] | null>;
    },
  ) =>
    fetchJSON<ApplyResult>(`/terminals/${id}/apply`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
      timeoutMs: 20000,
    }),

  /** Taime-added: files changed by >1 agent in a session (collision risk). */
  getContention: (session: string) =>
    fetchJSON<{ path: string; terminals: string[] }[]>(
      `/worktrees/contention?session=${encodeURIComponent(session)}`,
    ),

  /** Taime-added: snapshot an agent's tree at a turn boundary. */
  postCheckpoint: (terminalId: string, boundary: "turn_start" | "turn_end") =>
    fetchJSON<{ turn_id?: string; files_touched?: string[] }>(
      `/activity/checkpoint`,
      {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ terminal_id: terminalId, boundary }),
      },
    ),

  /** Taime-added: the session's activity graph (nodes + edges + contention). */
  getGraph: (session: string) =>
    fetchJSON<ActivityGraph>(
      `/activity/graph?session=${encodeURIComponent(session)}`,
      { timeoutMs: 20000 },
    ),

  /** Taime-added: forward attributed filesystem events into the activity graph. */
  postFsEvents: (
    terminalId: string,
    events: { path: string; kind?: string; ts?: number }[],
  ) =>
    fetchJSON<{ recorded: number; mode: string; confidence: string }>(
      `/activity/fs-events`,
      {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ terminal_id: terminalId, events }),
      },
    ),

  /** Taime-added: query the activity timeline / graph rows. */
  getActivity: (params?: { session?: string; terminalId?: string; limit?: number }) => {
    const q = new URLSearchParams();
    if (params?.session) q.set("session", params.session);
    if (params?.terminalId) q.set("terminal_id", params.terminalId);
    if (params?.limit) q.set("limit", String(params.limit));
    const qs = q.toString();
    return fetchJSON<ActivityEvent[]>(`/activity${qs ? `?${qs}` : ""}`);
  },
};

/** One turn (processing burst) in the activity graph. */
export interface GraphTurn {
  id: string;
  turn_index: number;
  started_at: string | null;
  ended_at: string | null;
  files_touched: string[];
  start_snapshot: string | null;
  end_snapshot: string | null;
}

/** A session's activity graph: agent nodes + their turns, edges, contention. */
export interface ActivityGraph {
  session: string;
  agents: {
    terminal_id: string;
    provider: string | null;
    mode: string | null;
    branch: string | null;
    turns: GraphTurn[];
  }[];
  edges: { kind: string; source: string | null; target: string | null; ts: string | null }[];
  contention: { path: string; terminals: string[] }[];
}

/** A row of the activity timeline / graph (mirrors backend ActivityEventResponse). */
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
