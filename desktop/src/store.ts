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
import { ptySpawnClaude, ptyKill, ptyCloseView } from "./pty";

/** Which transport carries a frame's terminal I/O. */
export type TerminalTransport = "cao_ws" | "rust_pty";

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

let frameCounter = 0;
const nextKey = () => `frame-${++frameCounter}`;

interface Store {
  // backend-derived
  sessions: Session[];
  activeSessionDetail: SessionDetail | null;
  connected: boolean;
  terminalStatuses: Record<string, string>;

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
  /** Known Rust-PTY agents keyed by ptySessionId — survives frame close so a
   * detached (still-running) agent can be listed + reopened (close ≠ kill). */
  rustPtySessions: Record<string, RustPtyMeta>;
  dirty: Record<string, DirtyState>;
  /** Per-terminal attributed file-change timeline (most recent last). */
  timeline: Record<string, TimelineEvent[]>;
  /** Frames whose dirty changes the user has acknowledged (for the switch guard). */
  reviewedFrames: Record<string, boolean>;
  /** When set, a context switch is blocked pending review of the current frame. */
  pendingSwitchKey: string | null;
  /** When set, the Monaco diff-review overlay is open for this terminal. */
  diffTerminalId: string | null;
  /** When true, the activity-graph overlay is open. */
  graphOpen: boolean;
  snackbar: Snackbar | null;

  // backend sync
  setConnected: (connected: boolean) => void;
  setWorkspaceDir: (dir: string | null) => void;
  removeRecentProject: (path: string) => void;
  clearRecentProjects: () => void;
  setIsolationEnabled: (enabled: boolean) => void;
  fetchSessions: () => Promise<void>;
  selectSessionDetail: (name: string | null) => Promise<void>;
  refreshStatuses: () => Promise<void>;

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
  /** Reopen a detached (still-running) Rust-PTY agent in a new frame. */
  reopenRustPty: (ptySessionId: string) => void;
  /** Mark a Rust-PTY session exited (process gone) — keeps it visible as such. */
  markRustPtyExited: (ptySessionId: string) => void;
  /** Explicitly terminate a Rust-PTY agent (distinct from closing its frame). */
  killRustPty: (key: string) => Promise<void>;
  /** Drop a Rust-PTY session from the registry (kill if alive). */
  forgetRustPty: (ptySessionId: string) => Promise<void>;
  closeFrame: (key: string) => Promise<void>;
  setActiveFrame: (key: string | null) => void;
  /** Switch active frame, raising the dirty-state guard if needed. */
  setActiveFrameGuarded: (key: string) => void;
  resolveSwitch: (proceed: boolean) => void;
  markReviewed: (key: string) => void;
  openDiff: (terminalId: string) => void;
  closeDiff: () => void;
  setGraphOpen: (open: boolean) => void;

  // dirty state
  setDirty: (terminalId: string, dirty: DirtyState) => void;
  clearDirty: (terminalId: string) => void;
  appendTimeline: (terminalId: string, events: TimelineEvent[]) => void;

  // misc
  setTerminalStatus: (id: string, status: string | null) => void;
  showSnackbar: (s: Snackbar) => void;
  hideSnackbar: () => void;
}

export const useStore = create<Store>((set, get) => ({
  sessions: [],
  activeSessionDetail: null,
  connected: false,
  terminalStatuses: {},

  workspaceDir: loadWorkspaceDir(),
  recentProjects: loadRecentProjects(),
  isolationEnabled: true,

  frames: [],
  activeFrameKey: null,
  rustPtySessions: {},
  dirty: {},
  timeline: {},
  reviewedFrames: {},
  pendingSwitchKey: null,
  diffTerminalId: null,
  graphOpen: false,
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
      .frames.map((f) => f.terminalId)
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
      set((s) => ({
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
      }));
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
    const existing = get().frames.find((f) => f.terminalId === terminalId);
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
          transport: "rust_pty",
          ptySessionId,
        },
      ],
      activeFrameKey: key,
    }));
  },

  killRustPty: async (key) => {
    const frame = get().frames.find((f) => f.key === key);
    const sid = frame?.ptySessionId;
    if (sid) await ptyKill(sid);
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
    await ptyKill(ptySessionId); // safe even if already exited
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
      return {
        frames,
        activeFrameKey:
          s.activeFrameKey === key
            ? (frames[frames.length - 1]?.key ?? null)
            : s.activeFrameKey,
      };
    });
    // Rust-PTY: detach the view but KEEP the agent running + its registry entry,
    // so it shows up under "detached" and can be reopened. (close ≠ kill.)
    if (frame?.transport === "rust_pty" && frame.ptySessionId) {
      await ptyCloseView(frame.ptySessionId);
    }
    // Best-effort clear dirty marker for the closed terminal.
    if (frame?.terminalId) get().clearDirty(frame.terminalId);
  },

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

  setTerminalStatus: (id, status) =>
    set((s) => {
      const normalized = status ? status.toUpperCase() : "UNKNOWN";
      if (s.terminalStatuses[id] === normalized) return s;
      return {
        terminalStatuses: { ...s.terminalStatuses, [id]: normalized },
      };
    }),

  showSnackbar: (snackbar) => set({ snackbar }),
  hideSnackbar: () => set({ snackbar: null }),
}));
