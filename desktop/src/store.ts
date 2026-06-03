import { create } from "zustand";
import {
  api,
  type Session,
  type SessionDetail,
  type Terminal,
} from "./api";
import {
  loadRecentProjects,
  saveRecentProjects,
  loadWorkspaceDir,
  saveWorkspaceDir,
  addRecent,
} from "./lib/recentProjects";
import { makeSessionName } from "./lib/sessionName";
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
  ptySpawnClaude,
  ptyKill,
  ptyCloseView,
  daemonSpawnClaude,
  daemonKill,
  daemonCloseView,
  type TurnEvent,
} from "./pty";

/** Which transport carries a frame's terminal I/O.
 *  - `cao_ws`   — CAO/tmux/WebSocket (legacy default).
 *  - `rust_pty` — in-app Rust PtyManager (Step 0).
 *  - `daemon`   — detached session daemon that survives app crashes (Step 2). */
export type TerminalTransport = "cao_ws" | "rust_pty" | "daemon";

/** Frame transports that render in the xterm Rust-PTY view. */
export function isRustPtyTransport(t: TerminalTransport | undefined): boolean {
  return t === "rust_pty" || t === "daemon";
}

/** Shell-grid layout: an auto-grid of all frames, or one focused frame with a
 *  tab strip of the rest. A view flag only — frames remain the source of truth. */
export type LayoutMode = "grid" | "focus";

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
  sessionName: string | null;
  pending: boolean;
  error?: string;
  /** Transport for this frame's terminal (default cao_ws). */
  transport?: TerminalTransport;
  /** Rust PTY session id (when transport === "rust_pty"). */
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
  /** Provisioned worktree terminal id — the attribution key (dirty/diff/graph). */
  terminalId: string;
  provider: string;
  branch: string | null;
  cwd: string | null;
  startedAt: number;
  /** Lifecycle: "running" (reattachable) or "exited" (process gone; dismiss only). */
  status: RustPtyStatus;
  /** Which backend/transport owns the session (used on reopen to pick the right attach path). */
  transport?: "rust_pty" | "daemon";
}

/** Dirty-state surfaced by the Rust file watcher (step 4). */
export interface DirtyState {
  count: number;
  paths: string[];
}

/** One attributed file change in a terminal's activity timeline. */
export interface TimelineEvent {
  path: string;
  kind: string;
  ts: number;
}

/** Cap on retained per-terminal timeline events (most recent kept). */
const TIMELINE_CAP = 200;

/**
 * Per-session status counts shown at-a-glance on a collapsed sidebar row.
 * Buckets mirror the StatusBadge normalization (PROCESSING→working,
 * WAITING_USER_ANSWER→needsYou, ERROR→error, COMPLETED→done, IDLE→idle); other
 * states (PENDING/UNKNOWN) count toward `total` only.
 */
export interface SessionStatusRollup {
  working: number;
  needsYou: number;
  error: number;
  done: number;
  idle: number;
  total: number;
}

let frameCounter = 0;
const nextKey = () => `frame-${++frameCounter}`;

interface Store {
  // backend-derived
  sessions: Session[];
  activeSessionDetail: SessionDetail | null;
  connected: boolean;
  terminalStatuses: Record<string, string>;
  /** Per-session status counts for the collapsed sidebar rows (polled, 10s). */
  sessionStatusRollup: Record<string, SessionStatusRollup>;

  // workspace (single active project)
  workspaceDir: string | null;
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
  /** Per-terminal attributed file-change timeline (most recent last). */
  timeline: Record<string, TimelineEvent[]>;
  /** Terminal ids whose auto-surfaced frame the user explicitly closed; the
   *  reconciler must not reopen these. (Manually reopening clears the flag.) */
  dismissedTerminalIds: Set<string>;
  /** Frames whose dirty changes the user has acknowledged (for the switch guard). */
  reviewedFrames: Record<string, boolean>;
  /** When set, a context switch is blocked pending review of the current frame. */
  pendingSwitchKey: string | null;
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

  // backend sync
  setConnected: (connected: boolean) => void;
  setWorkspaceDir: (dir: string | null) => void;
  removeRecentProject: (path: string) => void;
  clearRecentProjects: () => void;
  setIsolationEnabled: (enabled: boolean) => void;
  fetchSessions: () => Promise<void>;
  selectSessionDetail: (name: string | null) => Promise<void>;
  refreshStatuses: () => Promise<void>;
  /** Poll per-session terminal statuses and rebuild the collapsed-row rollups. */
  refreshSessionRollups: () => Promise<void>;
  /** Delete a CAO session (terminates its tmux + agents) and close its frames. */
  killSession: (name: string) => Promise<void>;

  // grid actions
  launchAgent: (
    provider: string,
    agentProfile: string,
    opts?: { sessionName?: string; workingDirectory?: string },
  ) => Promise<void>;
  openTerminalFrame: (t: {
    terminalId: string;
    provider: string;
    agentProfile?: string | null;
    sessionName?: string | null;
  }) => void;
  /** Launch Claude on the Rust-owned PTY transport (CAO path untouched). */
  launchClaudeRustPty: () => Promise<void>;
  /** Launch Claude on the detached session daemon (survives app crashes). */
  launchClaudeDaemon: () => Promise<void>;
  /** Reopen a detached (still-running) Rust-PTY agent in a new frame. */
  reopenRustPty: (ptySessionId: string) => void;
  /** Mark a Rust-PTY session exited (process gone) — keeps it visible as such. */
  markRustPtyExited: (ptySessionId: string) => void;
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
  markReviewed: (key: string) => void;
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
  appendTimeline: (terminalId: string, events: TimelineEvent[]) => void;

  // misc
  setTerminalStatus: (id: string, status: string | null) => void;
  /** Record the model parsed from a frame's startup banner. */
  setFrameModel: (key: string, model: string) => void;
  showSnackbar: (s: Snackbar) => void;
  hideSnackbar: () => void;
}

export const useStore = create<Store>((set, get) => ({
  sessions: [],
  activeSessionDetail: null,
  connected: false,
  terminalStatuses: {},
  sessionStatusRollup: {},

  workspaceDir: loadWorkspaceDir(),
  recentProjects: loadRecentProjects(),
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
  timeline: {},
  dismissedTerminalIds: new Set(),
  reviewedFrames: {},
  pendingSwitchKey: null,
  diffTerminalId: null,
  graphOpen: false,
  commandPaletteOpen: false,
  launchOpen: false,
  snackbar: null,

  setConnected: (connected) => {
    if (get().connected !== connected) set({ connected });
  },

  setWorkspaceDir: (workspaceDir) => {
    saveWorkspaceDir(workspaceDir);
    if (workspaceDir) {
      const recents = addRecent(get().recentProjects, workspaceDir);
      saveRecentProjects(recents);
      set({ workspaceDir, recentProjects: recents });
    } else {
      set({ workspaceDir });
    }
  },

  removeRecentProject: (path) =>
    set((s) => {
      const recents = s.recentProjects.filter((p) => p !== path);
      saveRecentProjects(recents);
      return { recentProjects: recents };
    }),

  clearRecentProjects: () => {
    saveRecentProjects([]);
    set({ recentProjects: [] });
  },

  setIsolationEnabled: (isolationEnabled) => set({ isolationEnabled }),

  fetchSessions: async () => {
    try {
      const sessions = await api.listSessions();
      const prev = get();
      if (!prev.connected || !jsonEqual(prev.sessions, sessions)) {
        set({ sessions, connected: true });
      } else if (!prev.connected) {
        set({ connected: true });
      }
    } catch {
      if (get().connected) set({ connected: false });
    }
  },

  selectSessionDetail: async (name) => {
    if (!name) {
      set({ activeSessionDetail: null });
      return;
    }
    try {
      const detail = await api.getSession(name);
      if (!jsonEqual(get().activeSessionDetail, detail)) {
        set({ activeSessionDetail: detail });
      }
    } catch {
      /* leave previous detail */
    }
  },

  refreshStatuses: async () => {
    const ids = get()
      // Rust-PTY frames carry a CAO terminalId only for the attribution surface
      // (it's a provisioned worktree id, not a tmux terminal). Their lifecycle
      // comes from ptyList via useRustPtyReconcile — polling CAO /terminals/{id}
      // for them just 404s every tick. Skip them here.
      .frames.filter((f) => !f.ptySessionId)
      .map((f) => f.terminalId)
      .filter((x): x is string => !!x);
    if (ids.length === 0) return;
    await Promise.all(
      ids.map(async (id) => {
        try {
          const status = await api.getTerminalStatus(id);
          get().setTerminalStatus(id, status);
        } catch {
          /* ignore transient */
        }
      }),
    );
  },

  refreshSessionRollups: async () => {
    const sessions = get().sessions;
    if (sessions.length === 0) {
      if (Object.keys(get().sessionStatusRollup).length > 0) {
        set({ sessionStatusRollup: {} });
      }
      return;
    }
    // No session endpoint returns per-terminal status, so fan out: list each
    // session's terminals, then fetch each terminal's status, and bucket. Failed
    // sessions are simply omitted from the rebuilt map (stale row clears).
    // TODO: useTerminalReconcile also getSession()s these every 10s — a shared
    // pass could halve the calls, but the two loops schedule independently today.
    const results = await Promise.all(
      sessions.map(async (sess) => {
        try {
          const detail = await api.getSession(sess.name);
          const statuses = await Promise.all(
            detail.terminals.map((t) =>
              api.getTerminalStatus(t.id).catch(() => null),
            ),
          );
          const roll: SessionStatusRollup = {
            working: 0,
            needsYou: 0,
            error: 0,
            done: 0,
            idle: 0,
            total: detail.terminals.length,
          };
          for (const st of statuses) {
            switch ((st ?? "").toUpperCase()) {
              case "PROCESSING":
                roll.working++;
                break;
              case "WAITING_USER_ANSWER":
                roll.needsYou++;
                break;
              case "ERROR":
                roll.error++;
                break;
              case "COMPLETED":
                roll.done++;
                break;
              case "IDLE":
                roll.idle++;
                break;
            }
          }
          return [sess.name, roll] as const;
        } catch {
          return null; // skip unreachable session
        }
      }),
    );
    // Assign in `sessions` order (NOT Promise-resolution order) so the rebuilt
    // map has deterministic key order — otherwise JSON.stringify in jsonEqual
    // sees a different string each tick for identical content and set() churns.
    const next: Record<string, SessionStatusRollup> = {};
    for (const entry of results) {
      if (entry) next[entry[0]] = entry[1];
    }
    if (!jsonEqual(get().sessionStatusRollup, next)) {
      set({ sessionStatusRollup: next });
    }
  },

  killSession: async (name) => {
    // Close any open frames for this session first (UI only); the delete below
    // is what actually terminates the tmux session + its agents on the backend.
    const victims = get().frames.filter((f) => f.sessionName === name);
    for (const f of victims) await get().closeFrame(f.key);
    try {
      await api.deleteSession(name);
      get().showSnackbar({ type: "info", message: `Removed session ${name}` });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      get().showSnackbar({ type: "error", message: `Couldn't remove ${name}: ${msg}` });
    }
    await get().fetchSessions();
  },

  launchAgent: async (provider, agentProfile, opts) => {
    // Optimistic: show a pending frame immediately.
    const key = nextKey();
    const placeholder: Frame = {
      key,
      terminalId: null,
      provider,
      agentProfile,
      sessionName: opts?.sessionName ?? null,
      pending: true,
    };
    set((s) => ({
      frames: [...s.frames, placeholder],
      activeFrameKey: key,
    }));

    const projectRoot = opts?.workingDirectory ?? get().workspaceDir ?? undefined;
    // Request worktree isolation when enabled and we have a project root; the
    // backend transparently falls back to the shared dir for non-git projects.
    const isolate = get().isolationEnabled && !!projectRoot;
    const isolation = { isolate, projectRoot };
    // When isolating, the backend resolves the worktree path itself, so we don't
    // also pin working_directory; in shared mode it uses project_root as the cwd.
    const workingDirectory = isolate ? undefined : projectRoot;
    try {
      let terminal: Terminal;
      if (opts?.sessionName) {
        terminal = await api.addTerminal(
          opts.sessionName,
          provider,
          agentProfile,
          workingDirectory,
          isolation,
        );
      } else {
        // Name new sessions after the project folder so the pipeline reads
        // "myproject-a1b2" instead of an opaque "cao-cea61400" hash.
        terminal = await api.createSession(
          provider,
          agentProfile,
          makeSessionName(projectRoot),
          workingDirectory,
          isolation,
        );
      }
      // Resolve the placeholder to the real terminal.
      set((s) => {
        // A reconcile tick may have opened a frame for this terminal while
        // addTerminal was in flight: the placeholder's terminalId was null, so
        // the reconciler couldn't see the collision and openTerminalFrame's
        // existing-check couldn't either. If a frame now holds the resolved id,
        // drop our placeholder and keep that one — strictly one frame per id.
        const dup = s.frames.find(
          (f) => f.key !== key && f.terminalId === terminal.id,
        );
        if (dup) {
          const frames = s.frames.filter((f) => f.key !== key);
          return {
            frames,
            activeFrameKey: s.activeFrameKey === key ? dup.key : s.activeFrameKey,
          };
        }
        return {
          frames: s.frames.map((f) =>
            f.key === key
              ? {
                  ...f,
                  terminalId: terminal.id,
                  sessionName: terminal.session_name,
                  agentProfile: terminal.agent_profile,
                  pending: false,
                }
              : f,
          ),
        };
      });
      get().showSnackbar({
        type: "success",
        message: `${provider} launched`,
      });
      await get().fetchSessions();
    } catch (e) {
      // Rollback: drop the placeholder, surface the error.
      const msg = e instanceof Error ? e.message : "launch failed";
      set((s) => {
        const frames = s.frames.filter((f) => f.key !== key);
        return {
          frames,
          activeFrameKey:
            s.activeFrameKey === key
              ? (frames[frames.length - 1]?.key ?? null)
              : s.activeFrameKey,
        };
      });
      get().showSnackbar({
        type: "error",
        message: `Launch failed: ${msg}`,
      });
    }
  },

  openTerminalFrame: ({ terminalId, provider, agentProfile, sessionName }) => {
    // Opening a terminal (manually or via reconcile) clears any prior dismissal
    // so its surfacing lifecycle resets.
    const undismiss = (s: Store): Partial<Store> =>
      s.dismissedTerminalIds.has(terminalId)
        ? {
            dismissedTerminalIds: new Set(
              [...s.dismissedTerminalIds].filter((id) => id !== terminalId),
            ),
          }
        : {};
    const existing = get().frames.find((f) => f.terminalId === terminalId);
    if (existing) {
      set((s) => ({ ...undismiss(s), activeFrameKey: existing.key }));
      return;
    }
    const key = nextKey();
    set((s) => ({
      ...undismiss(s),
      frames: [
        ...s.frames,
        {
          key,
          terminalId,
          provider,
          agentProfile: agentProfile ?? null,
          sessionName: sessionName ?? null,
          pending: false,
        },
      ],
      activeFrameKey: key,
    }));
  },

  launchClaudeRustPty: async () => {
    const dir = get().workspaceDir;
    try {
      // Provision a worktree FIRST so the Rust-PTY agent gets the same
      // attribution surface (dirty/diff/timeline/graph) as a CAO terminal —
      // all of which key off this terminalId. Claude runs in the worktree path.
      let terminalId: string | null = null;
      let cwd = dir;
      let branch: string | null = null;
      if (dir) {
        const wt = await api.provisionWorktree({
          project_root: dir,
          provider: "claude_code",
          isolate: get().isolationEnabled,
        });
        terminalId = wt.terminal_id;
        cwd = wt.worktree_path;
        branch = wt.branch;
      }
      const sessionId = await ptySpawnClaude(cwd, 24, 80);
      const key = nextKey();
      set((s) => ({
        frames: [
          ...s.frames,
          {
            key,
            terminalId, // = provisioned worktree id → attribution lights up
            provider: "claude_code",
            agentProfile: null,
            sessionName: null,
            pending: false,
            transport: "rust_pty",
            ptySessionId: sessionId,
          },
        ],
        activeFrameKey: key,
        rustPtySessions: terminalId
          ? {
              ...s.rustPtySessions,
              [sessionId]: {
                ptySessionId: sessionId,
                terminalId,
                provider: "claude_code",
                branch,
                cwd,
                startedAt: Date.now(),
                status: "running",
                transport: "rust_pty",
              },
            }
          : s.rustPtySessions,
      }));
      get().showSnackbar({ type: "success", message: "Claude launched (Rust PTY)" });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      get().showSnackbar({ type: "error", message: `Rust PTY launch failed: ${msg}` });
    }
  },

  launchClaudeDaemon: async () => {
    const dir = get().workspaceDir;
    try {
      // Same attribution surface as the other transports: provision a worktree
      // first so dirty/diff/timeline/graph key off this terminalId. We also pass
      // it to the daemon as the attribution_key so turn events carry it.
      let terminalId: string | null = null;
      let cwd = dir;
      let branch: string | null = null;
      if (dir) {
        const wt = await api.provisionWorktree({
          project_root: dir,
          provider: "claude_code",
          isolate: get().isolationEnabled,
        });
        terminalId = wt.terminal_id;
        cwd = wt.worktree_path;
        branch = wt.branch;
      }
      const sessionId = await daemonSpawnClaude(cwd, 24, 80, terminalId);
      const key = nextKey();
      set((s) => ({
        frames: [
          ...s.frames,
          {
            key,
            terminalId,
            provider: "claude_code",
            agentProfile: null,
            sessionName: null,
            pending: false,
            transport: "daemon",
            ptySessionId: sessionId,
          },
        ],
        activeFrameKey: key,
        rustPtySessions: terminalId
          ? {
              ...s.rustPtySessions,
              [sessionId]: {
                ptySessionId: sessionId,
                terminalId,
                provider: "claude_code",
                branch,
                cwd,
                startedAt: Date.now(),
                status: "running",
                transport: "daemon",
              },
            }
          : s.rustPtySessions,
      }));
      get().showSnackbar({ type: "success", message: "Claude launched (daemon)" });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      get().showSnackbar({ type: "error", message: `Daemon launch failed: ${msg}` });
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
      };
    }),

  recordTurn: (frameKey, turn) =>
    set((s) => {
      const prev = s.frameTurns[frameKey] ?? [];
      const next = [...prev, turn].slice(-100); // cap retained turns per frame
      return { frameTurns: { ...s.frameTurns, [frameKey]: next } };
    }),

  reopenRustPty: (ptySessionId) => {
    const meta = get().rustPtySessions[ptySessionId];
    if (!meta || meta.status === "exited") return;
    const existing = get().frames.find((f) => f.ptySessionId === ptySessionId);
    if (existing) {
      set({ activeFrameKey: existing.key });
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
          sessionName: null,
          pending: false,
          // Reattach via the backend that owns the session (daemon vs in-app),
          // or the in-app default for pre-existing records without a transport.
          transport: meta.transport === "daemon" ? "daemon" : "rust_pty",
          ptySessionId,
        },
      ],
      activeFrameKey: key,
    }));
  },

  killRustPty: async (key) => {
    const frame = get().frames.find((f) => f.key === key);
    const sid = frame?.ptySessionId;
    if (sid) await (frame?.transport === "daemon" ? daemonKill(sid) : ptyKill(sid));
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
    // Route the kill to the owning backend only (avoids spawning the daemon just
    // to kill an in-app session). Safe even if the session already exited.
    const meta = get().rustPtySessions[ptySessionId];
    if (meta?.transport === "daemon") await daemonKill(ptySessionId);
    else await ptyKill(ptySessionId);
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
      // Remember explicitly-closed CAO terminals so the reconciler doesn't
      // immediately reopen them. Rust-PTY frames are excluded — they have their
      // own detached-agent lifecycle (close detaches, doesn't kill).
      const dismissedTerminalIds =
        frame && !isRustPtyTransport(frame.transport) && frame.terminalId
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
    // Rust-PTY / daemon: detach the view but KEEP the agent running + its
    // registry entry, so it shows up under "detached" and can be reopened.
    if (isRustPtyTransport(frame?.transport) && frame?.ptySessionId) {
      await (frame.transport === "daemon"
        ? daemonCloseView(frame.ptySessionId)
        : ptyCloseView(frame.ptySessionId));
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

  clearDirty: (terminalId) =>
    set((s) => {
      if (!s.dirty[terminalId]) return s;
      const next = { ...s.dirty };
      delete next[terminalId];
      return { dirty: next };
    }),

  appendTimeline: (terminalId, events) =>
    set((s) => {
      if (events.length === 0) return s;
      const prev = s.timeline[terminalId] ?? [];
      const merged = [...prev, ...events].slice(-TIMELINE_CAP);
      return { timeline: { ...s.timeline, [terminalId]: merged } };
    }),

  setActiveFrameGuarded: (key) => {
    const s = get();
    // The guard owns the invariant: while a switch is pending review, ignore
    // further switch requests so a second keystroke/click cannot silently
    // retarget the open guard modal.
    if (s.pendingSwitchKey) return;
    if (key === s.activeFrameKey) return;
    const current = s.frames.find((f) => f.key === s.activeFrameKey);
    const curDirty = current?.terminalId
      ? s.dirty[current.terminalId]
      : undefined;
    // If the agent we're switching AWAY from left unreviewed changes, raise the
    // guard instead of switching. The UI resolves it (review or proceed).
    if (current && curDirty && curDirty.count > 0 && !s.reviewedFrames[current.key]) {
      set({ pendingSwitchKey: key });
      return;
    }
    set({ activeFrameKey: key });
  },

  resolveSwitch: (proceed) => {
    const s = get();
    const target = s.pendingSwitchKey;
    if (!target) return;
    if (proceed) {
      const from = s.activeFrameKey;
      set({
        activeFrameKey: target,
        pendingSwitchKey: null,
        reviewedFrames: from
          ? { ...s.reviewedFrames, [from]: true }
          : s.reviewedFrames,
      });
    } else {
      set({ pendingSwitchKey: null });
    }
  },

  markReviewed: (key) =>
    set((s) => ({ reviewedFrames: { ...s.reviewedFrames, [key]: true } })),

  openDiff: (terminalId) => set({ diffTerminalId: terminalId }),
  closeDiff: () => set({ diffTerminalId: null }),
  setGraphOpen: (graphOpen) => set({ graphOpen }),
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
      return {
        terminalStatuses: { ...s.terminalStatuses, [id]: normalized },
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
