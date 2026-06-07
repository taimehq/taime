import { create } from "zustand";
import {
  api,
  type AgentSummary,
} from "./api";
import {
  loadRecentProjects,
  saveRecentProjects,
  loadWorkspaceDir,
  saveWorkspaceDir,
  addRecent,
} from "./lib/recentProjects";
import {
  loadTerminalFontSize,
  saveTerminalFontSize,
  clampTerminalFontSize,
  TERMINAL_FONT_SIZE_DEFAULT,
  loadSidebarWidth,
  saveSidebarWidth,
  clampSidebarWidth,
  loadSidebarCollapsed,
  saveSidebarCollapsed,
} from "./lib/preferences";
import {
  daemonSpawnAgent,
  daemonKill,
  daemonCloseView,
  type TurnEvent,
  type DaemonSessionSummary,
} from "./pty";
import { providerTitle, PROVIDER_ORDER } from "./lib/providerLabel";

/** Providers the daemon can launch via its provider registry. Every launch —
 *  any profile — routes through the daemon; it is the only transport. */
const DAEMON_PROVIDERS = new Set(PROVIDER_ORDER);

/** Best-effort provider id from a daemon session's program path (for adopting a
 *  crash-surviving session before the daemon reports provider on the wire). */
function providerFromProgram(program: string): string {
  const base = program.split("/").pop() ?? program;
  if (base.includes("codex")) return "codex";
  if (base.includes("gemini")) return "gemini_cli";
  if (base.includes("grok")) return "grok_cli";
  return "claude_code";
}

/** Which transport carries a frame's terminal I/O. Only `daemon` exists (the
 *  detached session daemon / Rust PTY path; survives crashes). */
export type TerminalTransport = "daemon";

/** Frame transports that render in the xterm daemon view (a frame without a
 *  transport is a transient placeholder, not a live daemon view). */
export function isDaemonTransport(t: TerminalTransport | undefined): boolean {
  return t === "daemon";
}

/** Shell-grid layout: an auto-grid of all frames, or one focused frame with a
 *  tab strip of the rest. A view flag only — frames remain the source of truth. */
export type LayoutMode = "grid" | "focus";

/** Top-level app sections (the rail). Section selection persists per section —
 *  navigating away and back must NOT lose selections (safe context switching). */
export type Section =
  | "dashboard"
  | "tasks"
  | "agents"
  | "workflows"
  | "schedules"
  | "settings";

/** Task-screen tab a navigation can deep-link to (null = no preference; the
 *  screen keeps its own tab state). */
export type TaskTab = "overview" | "review";

/** A context switch held pending review of the current agent's unreviewed work.
 *  One pending switch at a time — the guard owns the invariant; further switch
 *  requests are ignored until it resolves. */
export type PendingSwitch =
  | { kind: "frame"; key: string }
  | { kind: "section"; section: Section }
  | { kind: "task"; taskId: string; tab: TaskTab | null };

/** Per-agent terminal rendering mode: raw PTY (xterm) or the structured
 *  console projection of the same stream. Absent ⇒ "terminal". */
export type TermMode = "terminal" | "console";

export type NotificationKind = "blocked" | "review" | "exited" | "error";

/** An attention item derived from daemon pushes (status/fs-dirty/exit).
 *  Frontend-local for now (a daemon-side notification log is a filed decision). */
export interface AppNotification {
  id: string;
  kind: NotificationKind;
  /** The agent id (attribution anchor) the event belongs to. */
  agentId: string;
  taskId: string | null;
  text: string;
  /** Epoch ms when the item was pushed. */
  at: number;
  read: boolean;
}

/** Re-render guard: only update when data actually changed. */
function jsonEqual(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

export interface Snackbar {
  type: "success" | "error" | "info";
  message: string;
}

/**
 * A terminal "frame" shown in the right-hand shell grid. `pending` frames are
 * optimistic placeholders shown the instant the user launches an agent, before
 * the backend has finished cold-starting the CLI and returned a real id.
 */
export interface Frame {
  /** Local frame id; equals the real terminal id once resolved. */
  key: string;
  terminalId: string | null; // null while pending
  provider: string;
  agentProfile: string | null;
  /** Task membership at launch (null ⇒ Uncategorized). The durable source of
   *  truth is the worktree row; this is the display copy for tabs/headers. */
  taskId?: string | null;
  pending: boolean;
  error?: string;
  /** Transport for this frame's terminal (always the daemon once spawned). */
  transport?: TerminalTransport;
  /** Daemon session id (when transport === "daemon"). */
  ptySessionId?: string;
  /** Model label parsed from the agent's startup banner (best-effort). */
  model?: string;
}

/**
 * Surviving metadata for a Rust-PTY agent, keyed by ptySessionId. Outlives its
 * frame: closing a frame detaches the view (close_view ≠ kill), and this record
 * is what lets the UI list + reopen a still-running detached agent.
 */
export type RustPtyStatus = "running" | "exited";

export interface RustPtyMeta {
  ptySessionId: string;
  /** The agent's id (provisioned worktree row) — the attribution anchor
   *  (dirty/diff/graph). Local name kept as terminalId to limit churn. */
  terminalId: string;
  provider: string;
  branch: string | null;
  cwd: string | null;
  startedAt: number;
  /** Lifecycle: "running" (reattachable) or "exited" (process gone; dismiss only). */
  status: RustPtyStatus;
  /** Task membership (null ⇒ Uncategorized). Daemon-reported from the worktree
   *  row; the reconcile tick keeps it fresh after reassignment. */
  taskId?: string | null;
  /** The owning transport — always the daemon now (kept for forward-compat). */
  transport?: "daemon";
}

/** Dirty-state surfaced by the Rust file watcher (step 4). */
export interface DirtyState {
  count: number;
  paths: string[];
}

let frameCounter = 0;
const nextKey = () => `frame-${++frameCounter}`;

let notificationCounter = 0;
const NOTIFICATION_CAP = 200;

/** Append a notification, evicting the oldest beyond the cap (FIFO). */
function appendNotification(
  items: AppNotification[],
  n: Omit<AppNotification, "id" | "at" | "read">,
): AppNotification[] {
  const item: AppNotification = {
    ...n,
    id: `notif-${++notificationCounter}`,
    at: Date.now(),
    read: false,
  };
  return [...items, item].slice(-NOTIFICATION_CAP);
}

/** Notifications for a wire-status transition (already-deduped by the caller's
 *  status-unchanged early return). Only blocked/error states notify. */
function statusNotifications(
  items: AppNotification[],
  agentId: string,
  taskId: string | null,
  providerName: string,
  normalized: string,
): AppNotification[] | null {
  const kind: NotificationKind | null =
    normalized === "WAITING_USER_ANSWER"
      ? "blocked"
      : normalized === "ERROR"
        ? "error"
        : null;
  if (!kind) return null;
  const text =
    kind === "blocked"
      ? `${providerName} needs your answer`
      : `${providerName} reported an error`;
  return appendNotification(items, { kind, agentId, taskId, text });
}

/** Known workspace roots: the active root first, then the recents history. */
function deriveWorkspaces(dir: string | null, recents: string[]): string[] {
  return dir ? [dir, ...recents.filter((p) => p !== dir)] : [...recents];
}

/** Unread notification count (derived — pass the store state). */
export function unreadCount(s: Pick<Store, "notifications">): number {
  return s.notifications.reduce((n, item) => (item.read ? n : n + 1), 0);
}

/** Terminal mode for an agent (derived — absent ⇒ "terminal"). */
export function termModeFor(
  s: Pick<Store, "termModes">,
  agentId: string | null | undefined,
): TermMode {
  return (agentId && s.termModes[agentId]) || "terminal";
}

interface Store {
  // backend-derived
  agents: AgentSummary[];
  connected: boolean;
  terminalStatuses: Record<string, string>;

  // navigation (the rail + per-section selection; selections persist across
  // section switches — navigating away must not lose them)
  section: Section;
  /** Selected task (Tasks section). Survives section switches. */
  selectedTaskId: string | null;
  /** One-shot deep-link: which tab the Task screen should open on (the screen
   *  consumes + clears it). Null ⇒ no preference. */
  taskInitialTab: TaskTab | null;
  /** Selected workflow definition (Workflows section). */
  selectedWorkflow: string | null;
  /** Selected schedule (Schedules section). */
  selectedSchedule: string | null;
  /** Selected settings nav entry (Settings section sidebar). */
  settingsTab: string;
  /** When true, the title-bar workspace switcher dropdown is open. Store-owned
   *  so the ⌘O global shortcut can toggle it from the dispatcher. */
  wsSwitcherOpen: boolean;

  // workspace (single active project)
  workspaceDir: string | null;
  /** The active workspace root — alias of `workspaceDir` (kept in lockstep by
   *  setWorkspaceDir; new screens read this name, per the lexicon). */
  activeWorkspaceRoot: string | null;
  /** Known workspace roots: active root + recents (derived, kept in sync). */
  workspaces: string[];
  /** Most-recent-first history of opened project directories (persisted). */
  recentProjects: string[];
  /** When true (default), each launched agent runs in its own git worktree for
   * provable per-agent change attribution. Backend falls back to the shared dir
   * for non-git projects. Toggled off for live shared-dir collaboration. */
  isolationEnabled: boolean;

  // UI / grid
  frames: Frame[];
  activeFrameKey: string | null;
  /** Shell-grid layout: auto-grid vs single focused frame + tab strip. */
  layoutMode: LayoutMode;
  /** Known Rust-PTY agents keyed by ptySessionId — survives frame close so a
   * detached (still-running) agent can be listed + reopened (close ≠ kill). */
  rustPtySessions: Record<string, RustPtyMeta>;
  /** Attribution turn boundaries pushed by the daemon, keyed by frame key
   *  (most recent last, capped). The flagship substrate landing in the app. */
  frameTurns: Record<string, TurnEvent[]>;
  dirty: Record<string, DirtyState>;
  /** Terminal ids whose auto-surfaced frame the user explicitly closed; the
   *  reconciler must not reopen these. (Manually reopening clears the flag.) */
  dismissedTerminalIds: Set<string>;
  /** Agents (by agent id) whose dirty changes the user has acknowledged (for
   *  the switch guard). Keyed by agent_id so review state survives frame
   *  close/reopen and is shared by every view of the same agent. */
  reviewedFrames: Record<string, boolean>;
  /** When set, a context switch (frame, section, or task navigation) is
   *  blocked pending review of the current agent's unreviewed work. */
  pendingSwitch: PendingSwitch | null;
  /** Attention items derived from daemon pushes (capped FIFO at 200). */
  notifications: AppNotification[];
  /** Per-agent terminal rendering mode (absent ⇒ "terminal"). */
  termModes: Record<string, TermMode>;
  /** When set, the Monaco diff-review overlay is open for this terminal. */
  diffTerminalId: string | null;
  /** When true, the activity-graph overlay is open. */
  graphOpen: boolean;
  /** When true, the command palette (Cmd+K) overlay is open. */
  commandPaletteOpen: boolean;
  /** When true, the launch-agent dialog is open. Store-owned so both the
   *  sidebar button and the command palette can open it. */
  launchOpen: boolean;
  snackbar: Snackbar | null;

  // preferences (persisted via lib/preferences.ts)
  /** xterm font size for ALL terminal frames (Cmd ±/0). Terminal-only — the
   *  Monaco diff viewer keeps its own sizing. */
  terminalFontSize: number;
  /** Left sidebar width in px (drag-resizable, persisted + clamped). */
  sidebarWidth: number;
  /** When true, the sidebar is collapsed to a thin rail (Cmd+\). */
  sidebarCollapsed: boolean;

  // navigation
  /** Switch the rail section — guarded: leaving the agents section with an
   *  agent's unreviewed work raises the context-switch guard instead. */
  setSection: (section: Section) => void;
  /** Navigate to a task (Tasks section), optionally deep-linking to a tab —
   *  guarded the same way as setSection. */
  selectTask: (taskId: string, tab?: TaskTab) => void;
  /** The Task screen consumes the one-shot deep-link tab, then clears it. */
  clearTaskInitialTab: () => void;
  setSelectedWorkflow: (id: string | null) => void;
  setSelectedSchedule: (id: string | null) => void;
  setSettingsTab: (tab: string) => void;
  setWsSwitcherOpen: (open: boolean) => void;

  // workspaces
  /** Open a workspace (the existing workspace-open flow): persists it, fronts
   *  the recents history, and clears workspace-scoped selections (tasks). */
  switchWorkspace: (root: string) => void;
  /** Record a workspace in the known list without activating it. */
  addWorkspace: (root: string) => void;

  // notifications
  markRead: (id: string) => void;
  markAllRead: () => void;

  // per-agent UI prefs
  setTermMode: (agentId: string, mode: TermMode) => void;

  // backend sync
  setConnected: (connected: boolean) => void;
  setWorkspaceDir: (dir: string | null) => void;
  removeRecentProject: (path: string) => void;
  clearRecentProjects: () => void;
  setIsolationEnabled: (enabled: boolean) => void;
  /** Refresh the daemon agent roster — also the connectivity probe. */
  fetchAgents: () => Promise<void>;

  // grid actions
  launchAgent: (
    provider: string,
    agentProfile: string,
    opts?: { taskId?: string | null; workingDirectory?: string },
  ) => Promise<void>;
  /** Launch a provider on the detached session daemon (the Rust PTY path; survives
   *  app crashes). `profile` is the daemon profile name (`~/.taime/agents/*.toml`
   *  + built-in `default`/`orchestrator`); the daemon resolves it to fill the
   *  system prompt / model / tools and injects the MCP orchestration tools for a
   *  supervisor profile. `taskId` (optional) is the Task the agent joins — stamped
   *  onto its worktree row at provision (null ⇒ Uncategorized); `projectRoot`
   *  (optional) overrides the worktree fork root (defaults to the active
   *  `workspaceDir`). Returns true on success. */
  launchAgentDaemon: (
    provider: string,
    profile?: string,
    taskId?: string | null,
    projectRoot?: string | null,
  ) => Promise<boolean>;
  /** Refresh each tracked agent's Task membership from a daemon list (the
   *  reconcile tick) — keeps the sidebar grouping fresh after reassignment. */
  syncDaemonTaskIds: (sessions: DaemonSessionSummary[]) => void;
  /** Task whose review drawer is open (null = closed). */
  taskReviewId: string | null;
  openTaskReview: (taskId: string) => void;
  closeTaskReview: () => void;
  /** Reopen a detached (still-running) Rust-PTY agent in a new frame. */
  reopenRustPty: (ptySessionId: string, opts?: { focus?: boolean }) => void;
  /** Mark a Rust-PTY session exited (process gone) — keeps it visible as such. */
  markRustPtyExited: (ptySessionId: string) => void;
  /** Adopt a daemon session discovered at boot (crash survival): populate the
   *  registry so it appears in the detached panel and can be reopened. */
  adoptDaemonSession: (summary: DaemonSessionSummary) => void;
  /** Record a daemon-emitted attribution turn boundary against a frame. */
  recordTurn: (frameKey: string, turn: TurnEvent) => void;
  /** Explicitly terminate a Rust-PTY agent (distinct from closing its frame). */
  killRustPty: (key: string) => Promise<void>;
  /** Drop a Rust-PTY session from the registry (kill if alive). */
  forgetRustPty: (ptySessionId: string) => Promise<void>;
  closeFrame: (key: string) => Promise<void>;
  /** Mark a terminal id as dismissed so the reconciler won't reopen it. */
  dismissTerminal: (id: string) => void;
  setActiveFrame: (key: string | null) => void;
  /** Switch active frame, raising the dirty-state guard if needed. */
  setActiveFrameGuarded: (key: string) => void;
  resolveSwitch: (proceed: boolean) => void;
  /** Acknowledge an agent's dirty changes (keyed by agent id). */
  markReviewed: (agentId: string) => void;
  openDiff: (terminalId: string) => void;
  closeDiff: () => void;
  setGraphOpen: (open: boolean) => void;
  setCommandPaletteOpen: (open: boolean) => void;
  setLaunchOpen: (open: boolean) => void;
  setLayoutMode: (mode: LayoutMode) => void;
  toggleLayoutMode: () => void;

  // preferences
  setTerminalFontSize: (size: number) => void;
  adjustTerminalFontSize: (delta: number) => void;
  resetTerminalFontSize: () => void;
  setSidebarWidth: (px: number) => void;
  toggleSidebar: () => void;

  // dirty state
  setDirty: (terminalId: string, dirty: DirtyState) => void;
  clearDirty: (terminalId: string) => void;

  // misc
  setTerminalStatus: (id: string, status: string | null) => void;
  /** Mirror a daemon status PUSH (Phase 4) for `sessionId` into the badge map,
   *  resolving its attribution-keyed terminalId via `rustPtySessions`. */
  setDaemonSessionStatus: (sessionId: string, status: string | null) => void;
  /** Mirror a daemon fs-dirty PUSH (Phase 6) for `sessionId`: the full set of
   *  paths its per-session watcher has seen change since the last review. */
  markDaemonFsDirty: (sessionId: string, paths: string[]) => void;
  /** Record the model parsed from a frame's startup banner. */
  setFrameModel: (key: string, model: string) => void;
  showSnackbar: (s: Snackbar) => void;
  hideSnackbar: () => void;
}

/** Does this frame's agent have dirty changes the user hasn't acknowledged?
 *  (The guard condition — shared by frame switches and section navigation.) */
function hasUnreviewedWork(
  s: Pick<Store, "dirty" | "reviewedFrames">,
  frame: Frame | undefined,
): boolean {
  if (!frame?.terminalId) return false;
  const d = s.dirty[frame.terminalId];
  return !!d && d.count > 0 && !s.reviewedFrames[frame.terminalId];
}

const bootWorkspaceDir = loadWorkspaceDir();
const bootRecents = loadRecentProjects();

export const useStore = create<Store>((set, get) => ({
  agents: [],
  connected: false,
  terminalStatuses: {},

  section: "dashboard",
  selectedTaskId: null,
  taskInitialTab: null,
  selectedWorkflow: null,
  selectedSchedule: null,
  settingsTab: "workspace",
  wsSwitcherOpen: false,

  workspaceDir: bootWorkspaceDir,
  activeWorkspaceRoot: bootWorkspaceDir,
  workspaces: deriveWorkspaces(bootWorkspaceDir, bootRecents),
  recentProjects: bootRecents,
  isolationEnabled: true,

  terminalFontSize: loadTerminalFontSize(),
  sidebarWidth: loadSidebarWidth(),
  sidebarCollapsed: loadSidebarCollapsed(),

  frames: [],
  activeFrameKey: null,
  layoutMode: "grid",
  rustPtySessions: {},
  frameTurns: {},
  dirty: {},
  dismissedTerminalIds: new Set(),
  reviewedFrames: {},
  pendingSwitch: null,
  notifications: [],
  termModes: {},
  diffTerminalId: null,
  graphOpen: false,
  commandPaletteOpen: false,
  launchOpen: false,
  snackbar: null,

  setConnected: (connected) => {
    if (get().connected !== connected) set({ connected });
  },

  setSection: (section) => {
    const s = get();
    // The guard owns the invariant: while a switch is pending review, ignore
    // further navigation so a second keystroke/click cannot silently retarget
    // the open guard modal.
    if (s.pendingSwitch) return;
    if (section === s.section) return;
    // Leaving the agents section away from an agent with unreviewed changes
    // raises the guard instead of navigating (same gate as frame switches).
    if (
      s.section === "agents" &&
      hasUnreviewedWork(s, s.frames.find((f) => f.key === s.activeFrameKey))
    ) {
      set({ pendingSwitch: { kind: "section", section } });
      return;
    }
    set({ section });
  },

  selectTask: (taskId, tab) => {
    const s = get();
    if (s.pendingSwitch) return;
    if (
      s.section === "agents" &&
      hasUnreviewedWork(s, s.frames.find((f) => f.key === s.activeFrameKey))
    ) {
      set({ pendingSwitch: { kind: "task", taskId, tab: tab ?? null } });
      return;
    }
    set({ section: "tasks", selectedTaskId: taskId, taskInitialTab: tab ?? null });
  },

  clearTaskInitialTab: () => {
    if (get().taskInitialTab !== null) set({ taskInitialTab: null });
  },

  setSelectedWorkflow: (selectedWorkflow) => set({ selectedWorkflow }),
  setSelectedSchedule: (selectedSchedule) => set({ selectedSchedule }),
  setSettingsTab: (settingsTab) => set({ settingsTab }),
  setWsSwitcherOpen: (wsSwitcherOpen) => set({ wsSwitcherOpen }),

  switchWorkspace: (root) => {
    if (root === get().workspaceDir) return;
    // The existing workspace-open flow (persist + recents + derived list)…
    get().setWorkspaceDir(root);
    // …plus clearing the workspace-scoped selection: tasks belong to a
    // workspace; workflow/schedule definitions are global and keep theirs.
    set({ selectedTaskId: null, taskInitialTab: null });
  },

  addWorkspace: (root) => {
    const s = get();
    const recents = addRecent(s.recentProjects, root);
    saveRecentProjects(recents);
    set({
      recentProjects: recents,
      workspaces: deriveWorkspaces(s.workspaceDir, recents),
    });
  },

  markRead: (id) =>
    set((s) => {
      const hit = s.notifications.find((n) => n.id === id && !n.read);
      if (!hit) return s;
      return {
        notifications: s.notifications.map((n) =>
          n.id === id ? { ...n, read: true } : n,
        ),
      };
    }),

  markAllRead: () =>
    set((s) => {
      if (s.notifications.every((n) => n.read)) return s;
      return {
        notifications: s.notifications.map((n) =>
          n.read ? n : { ...n, read: true },
        ),
      };
    }),

  setTermMode: (agentId, mode) =>
    set((s) => {
      if (termModeFor(s, agentId) === mode) return s;
      return { termModes: { ...s.termModes, [agentId]: mode } };
    }),

  setWorkspaceDir: (workspaceDir) => {
    saveWorkspaceDir(workspaceDir);
    if (workspaceDir) {
      const recents = addRecent(get().recentProjects, workspaceDir);
      saveRecentProjects(recents);
      set({
        workspaceDir,
        activeWorkspaceRoot: workspaceDir,
        recentProjects: recents,
        workspaces: deriveWorkspaces(workspaceDir, recents),
      });
    } else {
      set({
        workspaceDir,
        activeWorkspaceRoot: workspaceDir,
        workspaces: deriveWorkspaces(workspaceDir, get().recentProjects),
      });
    }
  },

  removeRecentProject: (path) =>
    set((s) => {
      const recents = s.recentProjects.filter((p) => p !== path);
      saveRecentProjects(recents);
      return {
        recentProjects: recents,
        workspaces: deriveWorkspaces(s.workspaceDir, recents),
      };
    }),

  clearRecentProjects: () => {
    saveRecentProjects([]);
    set((s) => ({
      recentProjects: [],
      workspaces: deriveWorkspaces(s.workspaceDir, []),
    }));
  },

  setIsolationEnabled: (isolationEnabled) => set({ isolationEnabled }),

  fetchAgents: async () => {
    try {
      const agents = await api.listAgents();
      const prev = get();
      if (!prev.connected || !jsonEqual(prev.agents, agents)) {
        set({ agents, connected: true });
      }
    } catch {
      if (get().connected) set({ connected: false });
    }
  },

  launchAgent: async (provider, agentProfile, opts) => {
    // Daemon-only after the CAO/tmux removal: every supported CLI launches on the
    // detached session daemon (the Rust PTY path), in its own worktree. The
    // chosen profile name flows to the daemon, which resolves it against its
    // profile store (~/.taime/agents/*.toml + built-in default/orchestrator) to
    // fill the system prompt / model / tools and inject orchestration for a
    // supervisor profile.
    if (!DAEMON_PROVIDERS.has(provider)) {
      get().showSnackbar({ type: "error", message: `Unknown provider ${provider}` });
      return;
    }
    // launchAgentDaemon reports its own (real) error on failure. `taskId` is the
    // Task the agent joins — real membership (stamped on the worktree row at
    // provision), not just a label; null ⇒ Uncategorized.
    // Open the (non-disruptive) team drawer on the FIRST agent so the team view is
    // discovered, then leave it to the user — repeat launches just update the
    // "Team N" badge in the title bar rather than popping the panel each time.
    const firstAgent = Object.keys(get().rustPtySessions).length === 0;
    const ok = await get().launchAgentDaemon(
      provider,
      agentProfile || "default",
      opts?.taskId ?? null,
      opts?.workingDirectory ?? null,
    );
    if (ok && firstAgent) get().setGraphOpen(true);
  },

  launchAgentDaemon: async (provider, profile = "default", taskId = null, projectRoot = null) => {
    // Provision from the active workspace (or an explicit override root) — the
    // worktree row is the durable anchor for attribution AND Task membership.
    const dir = projectRoot ?? get().workspaceDir;
    // The built-in "orchestrator" profile is a supervisor; the daemon also infers
    // this from a file profile's `orchestrator = true`, but pass the hint so a
    // pre-resolution path still injects the tools.
    const orchestrate = profile === "orchestrator";
    try {
      // Provision a daemon-owned worktree first (git worktree in Rust, persisted
      // to the app-data store) so dirty/diff/graph key off this agent id, and
      // pass it to the daemon as the agent_id so turn events carry it.
      // `api.provisionWorktree` routes to the daemon (daemonProvisionWorktree).
      let terminalId: string | null = null;
      let cwd = dir;
      let branch: string | null = null;
      if (dir) {
        const wt = await api.provisionWorktree({
          project_root: dir,
          provider,
          isolate: get().isolationEnabled,
          task_id: taskId,
        });
        terminalId = wt.agent_id;
        cwd = wt.worktree_path;
        branch = wt.branch;
      }
      const sessionId = await daemonSpawnAgent(
        provider,
        cwd,
        24,
        80,
        terminalId,
        null,
        orchestrate,
        profile,
      );
      const key = nextKey();
      set((s) => {
        // If the reconcile tick already surfaced this freshly-spawned session as a
        // frame (a concurrent daemonList can list it before this set() runs), reuse
        // that frame instead of adding a duplicate — just focus it + label its profile.
        const existing = s.frames.find((f) => f.ptySessionId === sessionId);
        const frames = existing
          ? s.frames.map((f) =>
              f.key === existing.key
                ? { ...f, agentProfile: profile, taskId: taskId ?? null }
                : f,
            )
          : [
              ...s.frames,
              {
                key,
                terminalId,
                provider,
                agentProfile: profile,
                taskId: taskId ?? null,
                pending: false,
                transport: "daemon" as const,
                ptySessionId: sessionId,
              },
            ];
        return {
          frames,
          activeFrameKey: existing ? existing.key : key,
          rustPtySessions: terminalId
          ? {
              ...s.rustPtySessions,
              [sessionId]: {
                ptySessionId: sessionId,
                terminalId,
                provider,
                branch,
                cwd,
                startedAt: Date.now(),
                status: "running",
                taskId: taskId ?? null,
                transport: "daemon",
              },
            }
            : s.rustPtySessions,
        };
      });
      get().showSnackbar({
        type: "success",
        message: `${providerTitle(provider)} launched`,
      });
      return true;
    } catch (e) {
      // Surface the REAL error (provision/spawn failure) — the daemon is the only
      // backend now, so there's no silent fallback.
      const msg = e instanceof Error ? e.message : String(e);
      console.warn("[taime] daemon launch failed", e);
      get().showSnackbar({
        type: "error",
        message: `Launch failed: ${msg}`,
      });
      return false;
    }
  },

  markRustPtyExited: (ptySessionId) =>
    set((s) => {
      const m = s.rustPtySessions[ptySessionId];
      if (!m || m.status === "exited") return s;
      return {
        rustPtySessions: {
          ...s.rustPtySessions,
          [ptySessionId]: { ...m, status: "exited" },
        },
        // Surface the lifecycle transition (running → exited) as an attention
        // item. Idempotent with the early return above — one push per exit.
        ...(m.terminalId
          ? {
              notifications: appendNotification(s.notifications, {
                kind: "exited",
                agentId: m.terminalId,
                taskId: m.taskId ?? null,
                text: `${providerTitle(m.provider)} exited`,
              }),
            }
          : {}),
      };
    }),

  adoptDaemonSession: (summary) =>
    set((s) => {
      // Don't clobber a session we already track (this run or a prior adopt).
      if (s.rustPtySessions[summary.id]) return s;
      const meta: RustPtyMeta = {
        ptySessionId: summary.id,
        terminalId: summary.agent_id ?? "",
        // Daemon-reported provider (Phase 4); fall back to program inference for
        // a pre-Phase-4 daemon that doesn't report it.
        provider: summary.provider ?? providerFromProgram(summary.program),
        branch: null,
        cwd: summary.cwd || null,
        startedAt: summary.created_at_unix ? summary.created_at_unix * 1000 : Date.now(),
        status: summary.alive ? "running" : "exited",
        taskId: summary.task_id ?? null,
        transport: "daemon",
      };
      return { rustPtySessions: { ...s.rustPtySessions, [summary.id]: meta } };
    }),

  syncDaemonTaskIds: (sessions) =>
    set((s) => {
      // Membership can change daemon-side (task_assign, task delete demotion);
      // mirror the worktree-row truth into the tracked metas AND any open
      // frames (DiffView's sibling matching reads frames) when it drifts.
      let changed = false;
      const next = { ...s.rustPtySessions };
      const byId = new Map(sessions.map((sum) => [sum.id, sum.task_id ?? null]));
      for (const sum of sessions) {
        const m = next[sum.id];
        const tid = sum.task_id ?? null;
        if (m && (m.taskId ?? null) !== tid) {
          next[sum.id] = { ...m, taskId: tid };
          changed = true;
        }
      }
      let framesChanged = false;
      const frames = s.frames.map((f) => {
        if (!f.ptySessionId || !byId.has(f.ptySessionId)) return f;
        const tid = byId.get(f.ptySessionId) ?? null;
        if ((f.taskId ?? null) === tid) return f;
        framesChanged = true;
        return { ...f, taskId: tid };
      });
      if (!changed && !framesChanged) return s;
      return {
        ...(changed ? { rustPtySessions: next } : {}),
        ...(framesChanged ? { frames } : {}),
      };
    }),

  taskReviewId: null,
  // The two right drawers (Task Review / Team graph) share the same geometry —
  // opening one closes the other so they never stack invisibly.
  openTaskReview: (taskId) => set({ taskReviewId: taskId, graphOpen: false }),
  closeTaskReview: () => set({ taskReviewId: null }),

  recordTurn: (frameKey, turn) =>
    set((s) => {
      const prev = s.frameTurns[frameKey] ?? [];
      const next = [...prev, turn].slice(-100); // cap retained turns per frame
      return { frameTurns: { ...s.frameTurns, [frameKey]: next } };
    }),

  reopenRustPty: (ptySessionId, opts) => {
    const focus = opts?.focus ?? true;
    const meta = get().rustPtySessions[ptySessionId];
    if (!meta || meta.status === "exited") return;
    const existing = get().frames.find((f) => f.ptySessionId === ptySessionId);
    if (existing) {
      if (focus) set({ activeFrameKey: existing.key });
      return;
    }
    const key = nextKey();
    set((s) => ({
      frames: [
        ...s.frames,
        {
          key,
          terminalId: meta.terminalId,
          provider: meta.provider,
          agentProfile: null,
          // Carry membership from the (reconcile-synced) meta so reattached /
          // adopted agents cross-link in DiffView like manually launched ones.
          taskId: meta.taskId ?? null,
          pending: false,
          transport: "daemon",
          ptySessionId,
        },
      ],
      // Auto-surfaced workers don't steal focus from the agent you're typing in.
      activeFrameKey: focus ? key : s.activeFrameKey,
    }));
  },

  killRustPty: async (key) => {
    const frame = get().frames.find((f) => f.key === key);
    const sid = frame?.ptySessionId;
    if (sid) await daemonKill(sid);
    set((s) => {
      const frames = s.frames.filter((f) => f.key !== key);
      const rustPtySessions = { ...s.rustPtySessions };
      if (sid) delete rustPtySessions[sid];
      return {
        frames,
        rustPtySessions,
        activeFrameKey:
          s.activeFrameKey === key ? (frames[frames.length - 1]?.key ?? null) : s.activeFrameKey,
      };
    });
  },

  forgetRustPty: async (ptySessionId) => {
    // Connect-only; safe even if the session already exited (won't spawn a daemon).
    await daemonKill(ptySessionId);
    set((s) => {
      const rustPtySessions = { ...s.rustPtySessions };
      delete rustPtySessions[ptySessionId];
      return { rustPtySessions };
    });
  },

  closeFrame: async (key) => {
    const frame = get().frames.find((f) => f.key === key);
    // Remove from the grid immediately (closing a frame ≠ stopping the agent).
    set((s) => {
      const frames = s.frames.filter((f) => f.key !== key);
      // Remember explicitly-closed non-daemon frames so the reconciler doesn't
      // immediately reopen them. Daemon frames are excluded — they have their
      // own detached-agent lifecycle (close detaches, doesn't kill).
      const dismissedTerminalIds =
        frame && !isDaemonTransport(frame.transport) && frame.terminalId
          ? new Set(s.dismissedTerminalIds).add(frame.terminalId)
          : s.dismissedTerminalIds;
      return {
        frames,
        dismissedTerminalIds,
        activeFrameKey:
          s.activeFrameKey === key
            ? (frames[frames.length - 1]?.key ?? null)
            : s.activeFrameKey,
      };
    });
    // Daemon: detach the view but KEEP the agent running (it survives even an
    // app crash) + its registry entry, so it shows under "detached" and reopens.
    if (isDaemonTransport(frame?.transport) && frame?.ptySessionId) {
      await daemonCloseView(frame.ptySessionId);
    }
    // Best-effort clear dirty marker for the closed terminal.
    if (frame?.terminalId) get().clearDirty(frame.terminalId);
  },

  dismissTerminal: (id) =>
    set((s) => {
      if (s.dismissedTerminalIds.has(id)) return s;
      return { dismissedTerminalIds: new Set(s.dismissedTerminalIds).add(id) };
    }),

  setActiveFrame: (key) => set({ activeFrameKey: key }),

  setDirty: (terminalId, dirty) =>
    set((s) => {
      if (jsonEqual(s.dirty[terminalId], dirty)) return s;
      return { dirty: { ...s.dirty, [terminalId]: dirty } };
    }),

  clearDirty: (terminalId) => {
    // Also reset the daemon's accumulated set so its next FsDirty push doesn't
    // re-surface already-reviewed paths (best-effort; the daemon owns the watch).
    api.clearDaemonDirty(terminalId).catch(() => {
      /* daemon may be down; the local clear below still applies */
    });
    set((s) => {
      if (!s.dirty[terminalId]) return s;
      const next = { ...s.dirty };
      delete next[terminalId];
      return { dirty: next };
    });
  },

  setActiveFrameGuarded: (key) => {
    const s = get();
    // The guard owns the invariant: while a switch is pending review, ignore
    // further switch requests so a second keystroke/click cannot silently
    // retarget the open guard modal.
    if (s.pendingSwitch) return;
    if (key === s.activeFrameKey) return;
    const current = s.frames.find((f) => f.key === s.activeFrameKey);
    // If the agent we're switching AWAY from left unreviewed changes, raise the
    // guard instead of switching. The UI resolves it (review or proceed).
    if (hasUnreviewedWork(s, current)) {
      set({ pendingSwitch: { kind: "frame", key } });
      return;
    }
    set({ activeFrameKey: key });
  },

  resolveSwitch: (proceed) => {
    const s = get();
    const target = s.pendingSwitch;
    if (!target) return;
    if (!proceed) {
      set({ pendingSwitch: null });
      return;
    }
    // Proceeding past the guard acknowledges the current agent's changes
    // (keyed by agent id — review state outlives this frame).
    const from = s.frames.find((f) => f.key === s.activeFrameKey);
    const reviewedFrames = from?.terminalId
      ? { ...s.reviewedFrames, [from.terminalId]: true }
      : s.reviewedFrames;
    if (target.kind === "frame") {
      set({ activeFrameKey: target.key, pendingSwitch: null, reviewedFrames });
    } else if (target.kind === "section") {
      set({ section: target.section, pendingSwitch: null, reviewedFrames });
    } else {
      set({
        section: "tasks",
        selectedTaskId: target.taskId,
        taskInitialTab: target.tab,
        pendingSwitch: null,
        reviewedFrames,
      });
    }
  },

  markReviewed: (agentId) =>
    set((s) => ({ reviewedFrames: { ...s.reviewedFrames, [agentId]: true } })),

  openDiff: (terminalId) => set({ diffTerminalId: terminalId }),
  closeDiff: () => set({ diffTerminalId: null }),
  // Mutually exclusive with the Task Review drawer (same right-edge geometry).
  setGraphOpen: (graphOpen) => set(graphOpen ? { graphOpen, taskReviewId: null } : { graphOpen }),
  setCommandPaletteOpen: (commandPaletteOpen) => set({ commandPaletteOpen }),
  setLaunchOpen: (launchOpen) => set({ launchOpen }),
  setLayoutMode: (layoutMode) => set({ layoutMode }),
  toggleLayoutMode: () =>
    set((s) => ({ layoutMode: s.layoutMode === "grid" ? "focus" : "grid" })),

  setTerminalFontSize: (size) => {
    const next = clampTerminalFontSize(size);
    if (next === get().terminalFontSize) return;
    saveTerminalFontSize(next);
    set({ terminalFontSize: next });
  },
  adjustTerminalFontSize: (delta) =>
    get().setTerminalFontSize(get().terminalFontSize + delta),
  resetTerminalFontSize: () =>
    get().setTerminalFontSize(TERMINAL_FONT_SIZE_DEFAULT),

  setSidebarWidth: (px) => {
    const next = clampSidebarWidth(px);
    if (next === get().sidebarWidth) return;
    saveSidebarWidth(next);
    set({ sidebarWidth: next });
  },
  toggleSidebar: () => {
    const next = !get().sidebarCollapsed;
    saveSidebarCollapsed(next);
    set({ sidebarCollapsed: next });
  },

  setTerminalStatus: (id, status) =>
    set((s) => {
      const normalized = status ? status.toUpperCase() : "UNKNOWN";
      if (s.terminalStatuses[id] === normalized) return s;
      // Blocked/error transitions become attention items. Both status paths
      // (this poll mirror + the per-session push) write the same map, so the
      // unchanged early return above dedupes between them.
      const meta = Object.values(s.rustPtySessions).find(
        (m) => m.terminalId === id,
      );
      const notifications = statusNotifications(
        s.notifications,
        id,
        meta?.taskId ?? null,
        meta ? providerTitle(meta.provider) : id,
        normalized,
      );
      return {
        terminalStatuses: { ...s.terminalStatuses, [id]: normalized },
        ...(notifications ? { notifications } : {}),
      };
    }),

  setDaemonSessionStatus: (sessionId, status) =>
    set((s) => {
      const tid = s.rustPtySessions[sessionId]?.terminalId;
      if (!tid) return s;
      const normalized = status ? status.toUpperCase() : "UNKNOWN";
      if (s.terminalStatuses[tid] === normalized) return s;
      // Keep the daemon-session map's own status field coherent too (used by the
      // Agents panel), mapping the inferred status onto the lifecycle label.
      const m = s.rustPtySessions[sessionId];
      const lifecycle = normalized === "EXITED" ? "exited" : m.status === "exited" ? "exited" : "running";
      // Attention items: blocked/error transitions, plus an exit when THIS push
      // (not markRustPtyExited) is what flips the lifecycle — mutually
      // exclusive with markRustPtyExited's push, so no duplicates.
      let notifications = statusNotifications(
        s.notifications,
        tid,
        m.taskId ?? null,
        providerTitle(m.provider),
        normalized,
      );
      if (m.status === "running" && lifecycle === "exited") {
        notifications = appendNotification(notifications ?? s.notifications, {
          kind: "exited",
          agentId: tid,
          taskId: m.taskId ?? null,
          text: `${providerTitle(m.provider)} exited`,
        });
      }
      return {
        terminalStatuses: { ...s.terminalStatuses, [tid]: normalized },
        rustPtySessions:
          m.status === lifecycle
            ? s.rustPtySessions
            : { ...s.rustPtySessions, [sessionId]: { ...m, status: lifecycle } },
        ...(notifications ? { notifications } : {}),
      };
    }),

  markDaemonFsDirty: (sessionId, paths) =>
    set((s) => {
      const tid = s.rustPtySessions[sessionId]?.terminalId;
      if (!tid || paths.length === 0) return s;
      const prev = s.dirty[tid];
      if (prev && prev.count === paths.length && prev.paths.join(" ") === paths.join(" ")) {
        return s;
      }
      // First dirty since launch / last review-clear → one attention item.
      // Subsequent pushes only grow the set; they don't re-notify.
      const m = s.rustPtySessions[sessionId];
      const notifications = !prev
        ? appendNotification(s.notifications, {
            kind: "review",
            agentId: tid,
            taskId: m.taskId ?? null,
            text: `${providerTitle(m.provider)} · ${paths.length} path${
              paths.length === 1 ? "" : "s"
            } changed`,
          })
        : undefined;
      return {
        dirty: { ...s.dirty, [tid]: { count: paths.length, paths } },
        ...(notifications ? { notifications } : {}),
      };
    }),

  setFrameModel: (key, model) =>
    set((s) => {
      if (s.frames.find((f) => f.key === key)?.model === model) return s;
      return {
        frames: s.frames.map((f) => (f.key === key ? { ...f, model } : f)),
      };
    }),

  showSnackbar: (snackbar) => set({ snackbar }),
  hideSnackbar: () => set({ snackbar: null }),
}));
