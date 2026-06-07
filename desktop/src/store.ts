import { create } from "zustand";
import {
  api,
  type Session,
  type SessionDetail,
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
import { providerTitle } from "./lib/providerLabel";

/** Providers the daemon can launch directly (Phase 1 of the CAO replacement). The
 *  default-profile launch routes here; sessions / non-default profiles still go
 *  through CAO until the daemon learns them. */
const DAEMON_PROVIDERS = new Set(["claude_code", "codex", "gemini_cli", "grok_cli"]);

/** Best-effort provider id from a daemon session's program path (for adopting a
 *  crash-surviving session before the daemon reports provider on the wire). */
function providerFromProgram(program: string): string {
  const base = program.split("/").pop() ?? program;
  if (base.includes("codex")) return "codex";
  if (base.includes("gemini")) return "gemini_cli";
  if (base.includes("grok")) return "grok_cli";
  return "claude_code";
}

/** Which transport carries a frame's terminal I/O. Only `daemon` exists now (the
 *  detached session daemon / Rust PTY path; survives crashes). `cao_ws` is a
 *  retired legacy variant kept so older persisted state still parses. */
export type TerminalTransport = "cao_ws" | "daemon";

/** Frame transports that render in the xterm daemon view (vs the CAO WebSocket
 *  terminal). */
export function isDaemonTransport(t: TerminalTransport | undefined): boolean {
  return t === "daemon";
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
  /** Provisioned worktree terminal id — the attribution key (dirty/diff/graph). */
  terminalId: string;
  provider: string;
  branch: string | null;
  cwd: string | null;
  startedAt: number;
  /** Lifecycle: "running" (reattachable) or "exited" (process gone; dismiss only). */
  status: RustPtyStatus;
  /** The owning transport — always the daemon now (kept for forward-compat). */
  transport?: "daemon";
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
  /** Legacy session-removal hook (no daemon equivalent — agents are standalone);
   *  kept for the pipeline UI's empty session list. */
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
  /** Launch a provider on the detached session daemon (the Rust PTY path; survives
   *  app crashes). `profile` is the daemon profile name (`~/.taime/agents/*.toml`
   *  + built-in `default`/`orchestrator`); the daemon resolves it to fill the
   *  system prompt / model / tools and injects the MCP orchestration tools for a
   *  supervisor role. `sessionName` (optional) is the display label of the workspace
   *  session the agent joins; `projectRoot` (optional) is that session's root to
   *  provision the worktree from — so the daemon actually groups the agent there,
   *  not just labels it (defaults to the active `workspaceDir`). Returns true on
   *  success. */
  launchAgentDaemon: (
    provider: string,
    profile?: string,
    sessionName?: string | null,
    projectRoot?: string | null,
  ) => Promise<boolean>;
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
      // Daemon frames carry a CAO terminalId only for the attribution surface
      // (it's a provisioned worktree id, not a tmux terminal). Their lifecycle
      // comes from daemon_list via useRustPtyReconcile — polling CAO /terminals/{id}
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
    // Bucket each session's agents by their LIVE status — the push-maintained
    // `terminalStatuses` map (Phase 4), NOT the retired per-terminal status call.
    // Query each session's detail by its UNIQUE root id (not the basename) so two
    // same-basename workspaces never merge. Unreachable sessions are omitted.
    const results = await Promise.all(
      sessions.map(async (sess) => {
        try {
          const detail = await api.getSession(sess.id);
          const statuses = get().terminalStatuses;
          const roll: SessionStatusRollup = {
            working: 0,
            needsYou: 0,
            error: 0,
            done: 0,
            idle: 0,
            total: detail.terminals.length,
          };
          for (const t of detail.terminals) {
            switch ((statuses[t.id] ?? "").toUpperCase()) {
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
          return [sess.id, roll] as const;
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
    // Daemon-only after the CAO/tmux removal: every supported CLI launches on the
    // detached session daemon (the Rust PTY path), in its own worktree. The
    // chosen profile name flows to the daemon, which resolves it against its
    // profile store (~/.taime/agents/*.toml + built-in default/orchestrator) to
    // fill the system prompt / model / tools and inject orchestration for a
    // supervisor role.
    if (!DAEMON_PROVIDERS.has(provider)) {
      get().showSnackbar({ type: "error", message: `Unknown provider ${provider}` });
      return;
    }
    // launchAgentDaemon reports its own (real) error on failure. `workingDirectory`
    // (the selected session's root) routes provisioning so "Add to session" is real
    // grouping, not just a label.
    // Open the (non-disruptive) team drawer on the FIRST agent so the team view is
    // discovered, then leave it to the user — repeat launches just update the
    // "Team N" badge in the title bar rather than popping the panel each time.
    const firstAgent = Object.keys(get().rustPtySessions).length === 0;
    const ok = await get().launchAgentDaemon(
      provider,
      agentProfile || "default",
      opts?.sessionName ?? null,
      opts?.workingDirectory ?? null,
    );
    if (ok && firstAgent) get().setGraphOpen(true);
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

  launchAgentDaemon: async (provider, profile = "default", sessionName = null, projectRoot = null) => {
    // Provision from the selected session's root when "Add to session" was chosen,
    // else the active workspace — so the daemon groups the agent under the right
    // project (session_root_of keys off the worktree's project_root).
    const dir = projectRoot ?? get().workspaceDir;
    // The built-in "orchestrator" role is a supervisor; the daemon also infers
    // this from a file profile's `orchestrator = true`, but pass the hint so a
    // pre-resolution path still injects the tools.
    const orchestrate = profile === "orchestrator";
    try {
      // Provision a daemon-owned worktree first (git worktree in Rust, persisted
      // to the app-data store) so dirty/diff/graph key off this terminalId, and
      // pass it to the daemon as the attribution_key so turn events carry it.
      // `api.provisionWorktree` routes to the daemon (daemonProvisionWorktree).
      let terminalId: string | null = null;
      let cwd = dir;
      let branch: string | null = null;
      if (dir) {
        const wt = await api.provisionWorktree({
          project_root: dir,
          provider,
          isolate: get().isolationEnabled,
        });
        terminalId = wt.terminal_id;
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
        // that frame instead of adding a duplicate — just focus it + label its role.
        const existing = s.frames.find((f) => f.ptySessionId === sessionId);
        const frames = existing
          ? s.frames.map((f) =>
              f.key === existing.key
                ? { ...f, agentProfile: profile, sessionName: sessionName ?? null }
                : f,
            )
          : [
              ...s.frames,
              {
                key,
                terminalId,
                provider,
                agentProfile: profile,
                sessionName: sessionName ?? null,
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
      };
    }),

  adoptDaemonSession: (summary) =>
    set((s) => {
      // Don't clobber a session we already track (this run or a prior adopt).
      if (s.rustPtySessions[summary.id]) return s;
      const meta: RustPtyMeta = {
        ptySessionId: summary.id,
        terminalId: summary.attribution_key ?? "",
        // Daemon-reported provider (Phase 4); fall back to program inference for
        // a pre-Phase-4 daemon that doesn't report it.
        provider: summary.provider ?? providerFromProgram(summary.program),
        branch: null,
        cwd: summary.cwd || null,
        startedAt: summary.created_at_unix ? summary.created_at_unix * 1000 : Date.now(),
        status: summary.alive ? "running" : "exited",
        transport: "daemon",
      };
      return { rustPtySessions: { ...s.rustPtySessions, [summary.id]: meta } };
    }),

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
          sessionName: null,
          pending: false,
          // Reattach via the backend that owns the session (daemon vs in-app),
          // or the in-app default for pre-existing records without a transport.
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
      // Remember explicitly-closed CAO terminals so the reconciler doesn't
      // immediately reopen them. Rust-PTY frames are excluded — they have their
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
      return {
        terminalStatuses: { ...s.terminalStatuses, [tid]: normalized },
        rustPtySessions:
          m.status === lifecycle
            ? s.rustPtySessions
            : { ...s.rustPtySessions, [sessionId]: { ...m, status: lifecycle } },
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
      return { dirty: { ...s.dirty, [tid]: { count: paths.length, paths } } };
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
