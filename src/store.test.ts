/**
 * Pure-logic tests for the Zustand store (src/store.ts).
 *
 * All side-effecting imports are mocked so importing the store is hermetic:
 *  - ./api, ./pty, and ./lib/terminalInput transitively import @tauri-apps/api
 *    (invoke/Channel);
 *  - ./backend so tests control inTauri() (the fetchAgents gate);
 *  - ./lib/recentProjects and ./lib/preferences read localStorage at module
 *    init (the store seeds workspaceDir/font size/sidebar from them).
 * ./lib/providerLabel is a pure lookup table and is left real.
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

vi.mock("./api", () => ({
  api: {
    provisionWorktree: vi.fn(),
    listAgents: vi.fn(async () => []),
    clearDaemonDirty: vi.fn(async () => true),
    markReviewed: vi.fn(async () => true),
    reviewedAgents: vi.fn(async () => []),
  },
}));

vi.mock("./pty", () => ({
  daemonSpawnAgent: vi.fn(),
  daemonKill: vi.fn(async () => {}),
  daemonCloseView: vi.fn(async () => {}),
  daemonPing: vi.fn(async () => false),
  daemonIncompatible: vi.fn(async () => false),
  daemonStoreHealth: vi.fn(async () => null),
  daemonRestart: vi.fn(async () => {}),
  daemonCheckpoint: vi.fn(async () => {}),
  daemonSendMessage: vi.fn(async () => 1),
}));

vi.mock("./backend", () => ({
  inTauri: vi.fn(() => false),
}));

vi.mock("./lib/terminalInput", () => ({
  sendToTerminal: vi.fn(() => false),
}));

vi.mock("./lib/recentProjects", () => ({
  loadRecentProjects: vi.fn(() => []),
  saveRecentProjects: vi.fn(),
  loadWorkspaceDir: vi.fn(() => null),
  saveWorkspaceDir: vi.fn(),
  addRecent: vi.fn((list: string[], path: string) => [path, ...list.filter((p) => p !== path)]),
}));

vi.mock("./lib/preferences", () => ({
  TERMINAL_FONT_SIZE_DEFAULT: 13,
  clampTerminalFontSize: vi.fn((n: number) => n),
  loadTerminalFontSize: vi.fn(() => 13),
  saveTerminalFontSize: vi.fn(),
  loadSidebarWidth: vi.fn(() => 320),
  saveSidebarWidth: vi.fn(),
  clampSidebarWidth: vi.fn((n: number) => n),
  loadSidebarCollapsed: vi.fn(() => false),
  saveSidebarCollapsed: vi.fn(),
}));

import {
  useStore,
  unreadCount,
  termModeFor,
  type Frame,
  type RustPtyMeta,
} from "./store";
import { api, type WorktreeInfo } from "./api";
import {
  daemonSpawnAgent,
  daemonPing,
  daemonCheckpoint,
  daemonSendMessage,
  type DaemonSessionSummary,
} from "./pty";
import { inTauri } from "./backend";
import { sendToTerminal } from "./lib/terminalInput";

const provisionWorktree = vi.mocked(api.provisionWorktree);
const listAgents = vi.mocked(api.listAgents);
const markReviewedApi = vi.mocked(api.markReviewed);
const spawnAgent = vi.mocked(daemonSpawnAgent);
const ping = vi.mocked(daemonPing);
const checkpoint = vi.mocked(daemonCheckpoint);
const sendMessage = vi.mocked(daemonSendMessage);
const inTauriMock = vi.mocked(inTauri);
const sendToTerminalMock = vi.mocked(sendToTerminal);

// Full pristine snapshot taken once at import time; restored before each test
// (replace: true wipes any keys a test added).
const initialState = useStore.getState();

beforeEach(() => {
  useStore.setState(initialState, true);
  vi.clearAllMocks();
  // Re-seed defaults (clearAllMocks keeps overridden return values).
  inTauriMock.mockReturnValue(false);
  ping.mockResolvedValue(false);
  listAgents.mockResolvedValue([]);
  sendToTerminalMock.mockReturnValue(false);
  // launchAgentDaemon logs the real error on failure; keep test output quiet.
  vi.spyOn(console, "warn").mockImplementation(() => {});
});

// ── builders ────────────────────────────────────────────────────────────────

function makeSummary(over: Partial<DaemonSessionSummary> = {}): DaemonSessionSummary {
  return {
    id: "sess-1",
    cwd: "/work/wt-1",
    program: "/usr/local/bin/claude",
    alive: true,
    attached: false,
    rows: 24,
    cols: 80,
    created_at_unix: 1700000000,
    agent_id: "term-1",
    provider: "claude_code",
    status: null,
    protocol_version: 10,
    task_id: "task-1",
    role: "default",
    ...over,
  };
}

function makeMeta(over: Partial<RustPtyMeta> = {}): RustPtyMeta {
  return {
    ptySessionId: "sess-1",
    terminalId: "term-1",
    provider: "claude_code",
    branch: "agent/term-1",
    cwd: "/work/wt-1",
    startedAt: 1700000000000,
    status: "running",
    taskId: "task-1",
    role: "default",
    transport: "daemon",
    ...over,
  };
}

function makeFrame(over: Partial<Frame> = {}): Frame {
  return {
    key: "frame-test",
    terminalId: "term-1",
    provider: "claude_code",
    agentProfile: null,
    pending: false,
    transport: "daemon",
    ptySessionId: "sess-1",
    ...over,
  };
}

function makeWorktree(over: Partial<WorktreeInfo> = {}): WorktreeInfo {
  return {
    agent_id: "term-new",
    mode: "isolated",
    worktree_path: "/proj/.taime/wt/term-new",
    project_root: "/proj",
    repo_root: "/proj",
    branch: "agent/term-new",
    base_sha: "abc123",
    provider: "claude_code",
    member_of: null,
    task_id: null,
    ...over,
  };
}

// ── adoptDaemonSession ──────────────────────────────────────────────────────

describe("adoptDaemonSession", () => {
  it("populates RustPtyMeta from a DaemonSessionSummary", () => {
    useStore.getState().adoptDaemonSession(makeSummary());
    const meta = useStore.getState().rustPtySessions["sess-1"];
    expect(meta).toBeDefined();
    expect(meta.ptySessionId).toBe("sess-1");
    expect(meta.terminalId).toBe("term-1"); // from agent_id
    expect(meta.status).toBe("running"); // alive: true
    expect(meta.taskId).toBe("task-1"); // from task_id
    expect(meta.provider).toBe("claude_code");
    expect(meta.cwd).toBe("/work/wt-1");
    expect(meta.startedAt).toBe(1700000000 * 1000);
    expect(meta.transport).toBe("daemon");
  });

  it("maps alive:false to status exited and null fields to fallbacks", () => {
    useStore.getState().adoptDaemonSession(
      makeSummary({
        id: "sess-2",
        alive: false,
        agent_id: null,
        task_id: null,
        provider: null,
        program: "/opt/bin/codex",
      }),
    );
    const meta = useStore.getState().rustPtySessions["sess-2"];
    expect(meta.status).toBe("exited");
    expect(meta.terminalId).toBe(""); // agent_id null → ""
    expect(meta.taskId).toBeNull();
    expect(meta.provider).toBe("codex"); // inferred from program
  });

  it("does NOT clobber an existing entry", () => {
    const existing = makeMeta({ taskId: "task-original", terminalId: "term-original" });
    useStore.setState({ rustPtySessions: { "sess-1": existing } });
    const before = useStore.getState();

    useStore.getState().adoptDaemonSession(
      makeSummary({ agent_id: "term-other", task_id: "task-other" }),
    );

    const after = useStore.getState();
    expect(after.rustPtySessions["sess-1"]).toBe(existing); // same object, untouched
    expect(after).toBe(before); // no-op set() → no state churn at all
  });
});

// ── syncDaemonTaskIds ───────────────────────────────────────────────────────

describe("syncDaemonTaskIds", () => {
  it("updates metas AND matching frames when task_id drifts", () => {
    useStore.setState({
      rustPtySessions: { "sess-1": makeMeta({ taskId: "task-old" }) },
      frames: [
        makeFrame({ key: "f1", ptySessionId: "sess-1", taskId: "task-old" }),
        makeFrame({ key: "f2", ptySessionId: "sess-other", taskId: "task-x", terminalId: "t2" }),
      ],
    });

    useStore.getState().syncDaemonTaskIds([makeSummary({ task_id: "task-new" })]);

    const s = useStore.getState();
    expect(s.rustPtySessions["sess-1"].taskId).toBe("task-new");
    expect(s.frames.find((f) => f.key === "f1")?.taskId).toBe("task-new");
    // Frames for sessions not in the list are left alone (same reference).
    expect(s.frames.find((f) => f.key === "f2")?.taskId).toBe("task-x");
  });

  it("handles drift to null (demotion to Uncategorized)", () => {
    useStore.setState({
      rustPtySessions: { "sess-1": makeMeta({ taskId: "task-1" }) },
      frames: [makeFrame({ key: "f1", taskId: "task-1" })],
    });

    useStore.getState().syncDaemonTaskIds([makeSummary({ task_id: null })]);

    const s = useStore.getState();
    expect(s.rustPtySessions["sess-1"].taskId).toBeNull();
    expect(s.frames[0].taskId).toBeNull();
  });

  it("returns the same state object when nothing changed (no re-render storm)", () => {
    useStore.setState({
      rustPtySessions: { "sess-1": makeMeta({ taskId: "task-1" }) },
      frames: [makeFrame({ key: "f1", taskId: "task-1" })],
    });
    const before = useStore.getState();

    useStore.getState().syncDaemonTaskIds([makeSummary({ task_id: "task-1" })]);

    expect(useStore.getState()).toBe(before); // referential equality
  });

  it("treats undefined and null taskId as equal (no spurious update)", () => {
    useStore.setState({
      rustPtySessions: { "sess-1": makeMeta({ taskId: undefined }) },
      frames: [makeFrame({ key: "f1", taskId: undefined })],
    });
    const before = useStore.getState();

    useStore.getState().syncDaemonTaskIds([makeSummary({ task_id: null })]);

    expect(useStore.getState()).toBe(before);
  });
});

// ── markRustPtyExited ───────────────────────────────────────────────────────

describe("markRustPtyExited", () => {
  it("flips running → exited once", () => {
    useStore.setState({ rustPtySessions: { "sess-1": makeMeta({ status: "running" }) } });
    useStore.getState().markRustPtyExited("sess-1");
    expect(useStore.getState().rustPtySessions["sess-1"].status).toBe("exited");
  });

  it("is idempotent (second call is a no-op, same state object)", () => {
    useStore.setState({ rustPtySessions: { "sess-1": makeMeta({ status: "running" }) } });
    useStore.getState().markRustPtyExited("sess-1");
    const afterFirst = useStore.getState();

    useStore.getState().markRustPtyExited("sess-1");

    expect(useStore.getState()).toBe(afterFirst);
    expect(useStore.getState().rustPtySessions["sess-1"].status).toBe("exited");
  });

  it("no-ops on an unknown session id", () => {
    const before = useStore.getState();
    useStore.getState().markRustPtyExited("nope");
    expect(useStore.getState()).toBe(before);
  });
});

// ── reopenRustPty ───────────────────────────────────────────────────────────

describe("reopenRustPty", () => {
  it("no-ops on a missing meta", () => {
    const before = useStore.getState();
    useStore.getState().reopenRustPty("nope");
    expect(useStore.getState()).toBe(before);
    expect(useStore.getState().frames).toHaveLength(0);
  });

  it("no-ops on an exited meta", () => {
    useStore.setState({ rustPtySessions: { "sess-1": makeMeta({ status: "exited" }) } });
    useStore.getState().reopenRustPty("sess-1");
    expect(useStore.getState().frames).toHaveLength(0);
  });

  it("focuses an existing frame instead of duplicating", () => {
    useStore.setState({
      rustPtySessions: { "sess-1": makeMeta() },
      frames: [makeFrame({ key: "f-existing", ptySessionId: "sess-1" })],
      activeFrameKey: null,
    });

    useStore.getState().reopenRustPty("sess-1");

    const s = useStore.getState();
    expect(s.frames).toHaveLength(1);
    expect(s.activeFrameKey).toBe("f-existing");
  });

  it("opens a new frame carrying terminalId/provider/taskId from the meta", () => {
    useStore.setState({
      rustPtySessions: {
        "sess-1": makeMeta({ terminalId: "term-1", provider: "codex", taskId: "task-7" }),
      },
    });

    useStore.getState().reopenRustPty("sess-1");

    const s = useStore.getState();
    expect(s.frames).toHaveLength(1);
    const f = s.frames[0];
    expect(f.ptySessionId).toBe("sess-1");
    expect(f.terminalId).toBe("term-1");
    expect(f.provider).toBe("codex");
    expect(f.taskId).toBe("task-7");
    expect(f.transport).toBe("daemon");
    expect(f.pending).toBe(false);
    expect(s.activeFrameKey).toBe(f.key); // focus defaults to true
  });

  it("focus:false does not steal activeFrameKey", () => {
    useStore.setState({
      rustPtySessions: { "sess-1": makeMeta() },
      frames: [makeFrame({ key: "f-current", ptySessionId: "sess-other", terminalId: "t-x" })],
      activeFrameKey: "f-current",
    });

    useStore.getState().reopenRustPty("sess-1", { focus: false });

    const s = useStore.getState();
    expect(s.frames).toHaveLength(2);
    expect(s.activeFrameKey).toBe("f-current");
  });

  it("focus:false on an existing frame does not refocus it", () => {
    useStore.setState({
      rustPtySessions: { "sess-1": makeMeta() },
      frames: [
        makeFrame({ key: "f-mine", ptySessionId: "sess-1" }),
        makeFrame({ key: "f-current", ptySessionId: "sess-other", terminalId: "t-x" }),
      ],
      activeFrameKey: "f-current",
    });

    useStore.getState().reopenRustPty("sess-1", { focus: false });

    const s = useStore.getState();
    expect(s.frames).toHaveLength(2);
    expect(s.activeFrameKey).toBe("f-current");
  });
});

// ── launchAgentDaemon ───────────────────────────────────────────────────────

describe("launchAgentDaemon", () => {
  it("failure (provision rejects): returns false, error snackbar, no frame, no session", async () => {
    useStore.setState({ workspaceDir: "/proj" });
    provisionWorktree.mockRejectedValueOnce(new Error("git worktree failed"));

    const ok = await useStore.getState().launchAgentDaemon("claude_code");

    expect(ok).toBe(false);
    const s = useStore.getState();
    expect(s.snackbar?.type).toBe("error");
    expect(s.snackbar?.message).toContain("git worktree failed");
    expect(s.frames).toHaveLength(0);
    expect(s.rustPtySessions).toEqual({});
    expect(spawnAgent).not.toHaveBeenCalled();
  });

  it("failure (spawn rejects after provision): returns false, no frame, no session", async () => {
    useStore.setState({ workspaceDir: "/proj" });
    provisionWorktree.mockResolvedValueOnce(makeWorktree());
    spawnAgent.mockRejectedValueOnce(new Error("daemon unreachable"));

    const ok = await useStore.getState().launchAgentDaemon("claude_code");

    expect(ok).toBe(false);
    const s = useStore.getState();
    expect(s.snackbar?.type).toBe("error");
    expect(s.snackbar?.message).toContain("daemon unreachable");
    expect(s.frames).toHaveLength(0);
    expect(s.rustPtySessions).toEqual({});
  });

  it("success: adds a daemon frame + registry entry and returns true", async () => {
    useStore.setState({ workspaceDir: "/proj" });
    provisionWorktree.mockResolvedValueOnce(makeWorktree({ agent_id: "term-new" }));
    spawnAgent.mockResolvedValueOnce("sess-new");

    const ok = await useStore.getState().launchAgentDaemon("claude_code", "default", "task-9");

    expect(ok).toBe(true);
    const s = useStore.getState();
    expect(s.frames).toHaveLength(1);
    const f = s.frames[0];
    expect(f.ptySessionId).toBe("sess-new");
    expect(f.terminalId).toBe("term-new");
    expect(f.transport).toBe("daemon");
    expect(f.taskId).toBe("task-9");
    expect(f.agentProfile).toBe("default");
    expect(s.activeFrameKey).toBe(f.key);

    const meta = s.rustPtySessions["sess-new"];
    expect(meta).toBeDefined();
    expect(meta.terminalId).toBe("term-new");
    expect(meta.status).toBe("running");
    expect(meta.taskId).toBe("task-9");
    expect(meta.cwd).toBe("/proj/.taime/wt/term-new"); // worktree path, not project root
    expect(meta.branch).toBe("agent/term-new");
    expect(s.snackbar?.type).toBe("success");
  });

  it("frame dedupe: a pre-existing frame with the spawned ptySessionId is relabeled, not duplicated", async () => {
    // Simulate the reconcile tick having already surfaced the session as a frame.
    useStore.setState({
      workspaceDir: "/proj",
      frames: [
        makeFrame({ key: "f-pre", ptySessionId: "sess-race", agentProfile: null, taskId: null }),
      ],
      activeFrameKey: null,
    });
    provisionWorktree.mockResolvedValueOnce(makeWorktree({ agent_id: "term-race" }));
    spawnAgent.mockResolvedValueOnce("sess-race");

    const ok = await useStore
      .getState()
      .launchAgentDaemon("claude_code", "orchestrator", "task-2");

    expect(ok).toBe(true);
    const s = useStore.getState();
    expect(s.frames).toHaveLength(1); // no duplicate
    const f = s.frames[0];
    expect(f.key).toBe("f-pre");
    expect(f.agentProfile).toBe("orchestrator"); // relabeled with the launch profile
    expect(f.taskId).toBe("task-2");
    expect(s.activeFrameKey).toBe("f-pre"); // focuses the existing frame
    expect(s.rustPtySessions["sess-race"]).toBeDefined(); // registry still populated
  });

  it("passes the orchestrate hint for the orchestrator profile", async () => {
    useStore.setState({ workspaceDir: "/proj" });
    provisionWorktree.mockResolvedValueOnce(makeWorktree());
    spawnAgent.mockResolvedValueOnce("sess-orch");

    await useStore.getState().launchAgentDaemon("claude_code", "orchestrator");

    expect(spawnAgent).toHaveBeenCalledWith(
      "claude_code",
      "/proj/.taime/wt/term-new", // spawned in the provisioned worktree
      24,
      80,
      "term-new", // agent id = provisioned worktree row id
      null,
      true, // orchestrate
      "orchestrator",
    );
  });

  it("without a workspaceDir: skips provisioning and tracks no registry entry", async () => {
    useStore.setState({ workspaceDir: null });
    spawnAgent.mockResolvedValueOnce("sess-bare");

    const ok = await useStore.getState().launchAgentDaemon("claude_code");

    expect(ok).toBe(true);
    expect(provisionWorktree).not.toHaveBeenCalled();
    const s = useStore.getState();
    expect(s.frames).toHaveLength(1);
    expect(s.frames[0].terminalId).toBeNull();
    // No terminalId → no attribution → no rustPtySessions entry.
    expect(s.rustPtySessions).toEqual({});
  });
});

// ── launch dialog task preset / task-selection clear ────────────────────────

describe("setLaunchOpen task preset", () => {
  it("stores the preset on open and clears it on close", () => {
    useStore.getState().setLaunchOpen(true, "task-9");
    let s = useStore.getState();
    expect(s.launchOpen).toBe(true);
    expect(s.launchPresetTaskId).toBe("task-9");

    useStore.getState().setLaunchOpen(false);
    s = useStore.getState();
    expect(s.launchOpen).toBe(false);
    expect(s.launchPresetTaskId).toBeNull();
  });

  it("open without a preset defaults to null (Uncategorized)", () => {
    useStore.getState().setLaunchOpen(true, "task-9");
    useStore.getState().setLaunchOpen(false);
    useStore.getState().setLaunchOpen(true);
    expect(useStore.getState().launchPresetTaskId).toBeNull();
  });
});

describe("setSeedOpen", () => {
  it("toggles the Start-something-new dialog flag (default closed)", () => {
    expect(useStore.getState().seedOpen).toBe(false);
    useStore.getState().setSeedOpen(true);
    expect(useStore.getState().seedOpen).toBe(true);
    useStore.getState().setSeedOpen(false);
    expect(useStore.getState().seedOpen).toBe(false);
  });
});

describe("deleteWorkspace", () => {
  it("removes a non-active workspace, leaving the active one untouched", () => {
    useStore.setState({
      workspaceDir: "/a",
      activeWorkspaceRoot: "/a",
      recentProjects: ["/a", "/b", "/c"],
      workspaces: ["/a", "/b", "/c"],
    });
    useStore.getState().deleteWorkspace("/b");
    const s = useStore.getState();
    expect(s.workspaceDir).toBe("/a");
    expect(s.recentProjects).not.toContain("/b");
    expect(s.workspaces).not.toContain("/b");
  });

  it("deleting the ACTIVE workspace switches to the next recent + clears task selection", () => {
    useStore.setState({
      workspaceDir: "/a",
      activeWorkspaceRoot: "/a",
      recentProjects: ["/a", "/b"],
      workspaces: ["/a", "/b"],
      selectedTaskId: "task-1",
      section: "tasks",
    });
    useStore.getState().deleteWorkspace("/a");
    const s = useStore.getState();
    expect(s.workspaceDir).toBe("/b");
    expect(s.recentProjects).not.toContain("/a");
    expect(s.selectedTaskId).toBeNull();
  });

  it("deleting the only (active) workspace leaves none active", () => {
    useStore.setState({
      workspaceDir: "/only",
      activeWorkspaceRoot: "/only",
      recentProjects: ["/only"],
      workspaces: ["/only"],
    });
    useStore.getState().deleteWorkspace("/only");
    const s = useStore.getState();
    expect(s.workspaceDir).toBeNull();
    expect(s.recentProjects).toEqual([]);
  });
});

describe("clearSelectedTask", () => {
  it("clears the selection and any pending deep-link tab", () => {
    useStore.getState().selectTask("task-1", "review");
    expect(useStore.getState().selectedTaskId).toBe("task-1");

    useStore.getState().clearSelectedTask();
    const s = useStore.getState();
    expect(s.selectedTaskId).toBeNull();
    expect(s.taskInitialTab).toBeNull();
    // The section is untouched — only the selection clears.
    expect(s.section).toBe("tasks");
  });
});

// ── section navigation (guard-gated) ────────────────────────────────────────

/** An agents-section state with one dirty, unreviewed active frame. */
function seedUnreviewedAgent() {
  useStore.setState({
    section: "agents",
    frames: [makeFrame({ key: "f1", terminalId: "term-1" })],
    activeFrameKey: "f1",
    dirty: { "term-1": { count: 2, paths: ["a.ts", "b.ts"] } },
    reviewedFrames: {},
  });
}

describe("section navigation", () => {
  it("defaults to dashboard with empty selections", () => {
    const s = useStore.getState();
    expect(s.section).toBe("dashboard");
    expect(s.selectedTaskId).toBeNull();
    expect(s.taskInitialTab).toBeNull();
    expect(s.selectedWorkflow).toBeNull();
    expect(s.selectedSchedule).toBeNull();
  });

  it("setSection switches freely when no unreviewed work", () => {
    useStore.getState().setSection("tasks");
    expect(useStore.getState().section).toBe("tasks");
    expect(useStore.getState().pendingSwitch).toBeNull();
  });

  it("per-section selection persists across section switches", () => {
    useStore.getState().selectTask("task-1");
    useStore.getState().setSelectedWorkflow("wf-1");
    useStore.getState().setSelectedSchedule("sch-1");

    useStore.getState().setSection("settings");
    useStore.getState().setSection("tasks");

    const s = useStore.getState();
    expect(s.selectedTaskId).toBe("task-1");
    expect(s.selectedWorkflow).toBe("wf-1");
    expect(s.selectedSchedule).toBe("sch-1");
  });

  it("leaving agents with unreviewed work raises the guard instead of switching", () => {
    seedUnreviewedAgent();

    useStore.getState().setSection("dashboard");

    const s = useStore.getState();
    expect(s.section).toBe("agents"); // did NOT navigate
    expect(s.pendingSwitch).toEqual({ kind: "section", section: "dashboard" });
  });

  it("ignores further navigation while the guard is open", () => {
    seedUnreviewedAgent();
    useStore.getState().setSection("dashboard");

    useStore.getState().setSection("settings");
    useStore.getState().selectTask("task-9");

    const s = useStore.getState();
    expect(s.pendingSwitch).toEqual({ kind: "section", section: "dashboard" }); // not retargeted
    expect(s.section).toBe("agents");
    expect(s.selectedTaskId).toBeNull();
  });

  it("resolveSwitch(true) applies the section and marks the agent reviewed by agent id", () => {
    seedUnreviewedAgent();
    useStore.getState().setSection("dashboard");

    useStore.getState().resolveSwitch(true);

    const s = useStore.getState();
    expect(s.section).toBe("dashboard");
    expect(s.pendingSwitch).toBeNull();
    expect(s.reviewedFrames["term-1"]).toBe(true); // keyed by agent id, not frame key
  });

  it("resolveSwitch(false) cancels and stays", () => {
    seedUnreviewedAgent();
    useStore.getState().setSection("dashboard");

    useStore.getState().resolveSwitch(false);

    const s = useStore.getState();
    expect(s.section).toBe("agents");
    expect(s.pendingSwitch).toBeNull();
    expect(s.reviewedFrames["term-1"]).toBeUndefined();
  });

  it("already-reviewed work (by agent id) does not raise the guard", () => {
    seedUnreviewedAgent();
    useStore.setState({ reviewedFrames: { "term-1": true } });

    useStore.getState().setSection("dashboard");

    expect(useStore.getState().section).toBe("dashboard");
    expect(useStore.getState().pendingSwitch).toBeNull();
  });

  it("no guard when leaving a non-agents section, even with dirty work", () => {
    seedUnreviewedAgent();
    useStore.setState({ section: "dashboard" }); // already away from the agent

    useStore.getState().setSection("settings");

    expect(useStore.getState().section).toBe("settings");
    expect(useStore.getState().pendingSwitch).toBeNull();
  });

  it("selectTask navigates to tasks with the deep-link tab when clean", () => {
    useStore.getState().selectTask("task-1", "review");

    const s = useStore.getState();
    expect(s.section).toBe("tasks");
    expect(s.selectedTaskId).toBe("task-1");
    expect(s.taskInitialTab).toBe("review");

    s.clearTaskInitialTab();
    expect(useStore.getState().taskInitialTab).toBeNull();
  });

  it("selectTask is gated leaving agents; resolveSwitch(true) completes the deep link", () => {
    seedUnreviewedAgent();

    useStore.getState().selectTask("task-1", "review");

    let s = useStore.getState();
    expect(s.section).toBe("agents");
    expect(s.selectedTaskId).toBeNull();
    expect(s.pendingSwitch).toEqual({ kind: "task", taskId: "task-1", tab: "review" });

    useStore.getState().resolveSwitch(true);

    s = useStore.getState();
    expect(s.section).toBe("tasks");
    expect(s.selectedTaskId).toBe("task-1");
    expect(s.taskInitialTab).toBe("review");
    expect(s.reviewedFrames["term-1"]).toBe(true);
  });

  it("frame switches still guard (pendingSwitch kind frame) and honor agent-id review state", () => {
    seedUnreviewedAgent();
    useStore.setState({
      frames: [
        makeFrame({ key: "f1", terminalId: "term-1" }),
        makeFrame({ key: "f2", terminalId: "term-2", ptySessionId: "sess-2" }),
      ],
    });

    useStore.getState().setActiveFrameGuarded("f2");
    expect(useStore.getState().pendingSwitch).toEqual({ kind: "frame", key: "f2" });
    expect(useStore.getState().activeFrameKey).toBe("f1");

    useStore.getState().resolveSwitch(true);
    const s = useStore.getState();
    expect(s.activeFrameKey).toBe("f2");
    expect(s.reviewedFrames["term-1"]).toBe(true);
  });
});

// ── notifications ───────────────────────────────────────────────────────────

describe("notifications", () => {
  it("first fs-dirty push notifies kind review; growth does not re-notify", () => {
    useStore.setState({ rustPtySessions: { "sess-1": makeMeta() } });

    useStore.getState().markDaemonFsDirty("sess-1", ["a.ts"]);

    let s = useStore.getState();
    expect(s.notifications).toHaveLength(1);
    const n = s.notifications[0];
    expect(n.kind).toBe("review");
    expect(n.agentId).toBe("term-1");
    expect(n.taskId).toBe("task-1");
    expect(n.text).toContain("Claude Code");
    expect(n.read).toBe(false);

    useStore.getState().markDaemonFsDirty("sess-1", ["a.ts", "b.ts"]);

    s = useStore.getState();
    expect(s.dirty["term-1"].count).toBe(2); // set still grows
    expect(s.notifications).toHaveLength(1); // no second item
  });

  it("status push WAITING_USER_ANSWER notifies kind blocked, once per transition", () => {
    useStore.setState({ rustPtySessions: { "sess-1": makeMeta() } });

    useStore.getState().setDaemonSessionStatus("sess-1", "WAITING_USER_ANSWER");
    useStore.getState().setDaemonSessionStatus("sess-1", "WAITING_USER_ANSWER");

    const s = useStore.getState();
    expect(s.notifications).toHaveLength(1);
    expect(s.notifications[0].kind).toBe("blocked");
    expect(s.notifications[0].agentId).toBe("term-1");
  });

  it("poll-path setTerminalStatus dedupes against the push path (same status map)", () => {
    useStore.setState({ rustPtySessions: { "sess-1": makeMeta() } });

    useStore.getState().setDaemonSessionStatus("sess-1", "WAITING_USER_ANSWER");
    useStore.getState().setTerminalStatus("term-1", "WAITING_USER_ANSWER");

    expect(useStore.getState().notifications).toHaveLength(1);
  });

  it("setTerminalStatus ERROR notifies kind error", () => {
    useStore.setState({ rustPtySessions: { "sess-1": makeMeta() } });

    useStore.getState().setTerminalStatus("term-1", "ERROR");

    const s = useStore.getState();
    expect(s.notifications).toHaveLength(1);
    expect(s.notifications[0].kind).toBe("error");
  });

  it("exit notifies kind exited exactly once (idempotent with the lifecycle flip)", () => {
    useStore.setState({ rustPtySessions: { "sess-1": makeMeta() } });

    useStore.getState().markRustPtyExited("sess-1");
    useStore.getState().markRustPtyExited("sess-1"); // no-op

    const s = useStore.getState();
    expect(s.notifications).toHaveLength(1);
    expect(s.notifications[0].kind).toBe("exited");
    expect(s.notifications[0].text).toContain("exited");
  });

  it("caps at 200, evicting oldest first (FIFO)", () => {
    useStore.setState({ rustPtySessions: { "sess-1": makeMeta() } });
    for (let i = 0; i < 205; i++) {
      // Alternate to force a real transition (and thus a push) every call.
      useStore
        .getState()
        .setTerminalStatus("term-1", i % 2 === 0 ? "WAITING_USER_ANSWER" : "ERROR");
    }
    const s = useStore.getState();
    expect(s.notifications).toHaveLength(200);
    // Oldest were evicted: the newest item is the 205th push.
    expect(s.notifications[199].kind).toBe("blocked"); // i=204 is even → blocked
  });

  it("markRead marks one item; unknown id is an identity no-op", () => {
    useStore.setState({ rustPtySessions: { "sess-1": makeMeta() } });
    useStore.getState().setTerminalStatus("term-1", "ERROR");
    useStore.getState().markRustPtyExited("sess-1");
    const [first, second] = useStore.getState().notifications;
    expect(unreadCount(useStore.getState())).toBe(2);

    useStore.getState().markRead(first.id);

    let s = useStore.getState();
    expect(s.notifications.find((n) => n.id === first.id)?.read).toBe(true);
    expect(s.notifications.find((n) => n.id === second.id)?.read).toBe(false);
    expect(unreadCount(s)).toBe(1);

    const before = useStore.getState();
    useStore.getState().markRead("nope");
    useStore.getState().markRead(first.id); // already read
    expect(useStore.getState()).toBe(before);
  });

  it("markAllRead clears the unread count; second call is an identity no-op", () => {
    useStore.setState({ rustPtySessions: { "sess-1": makeMeta() } });
    useStore.getState().setTerminalStatus("term-1", "ERROR");
    useStore.getState().markRustPtyExited("sess-1");

    useStore.getState().markAllRead();

    const after = useStore.getState();
    expect(unreadCount(after)).toBe(0);
    expect(after.notifications.every((n) => n.read)).toBe(true);

    useStore.getState().markAllRead();
    expect(useStore.getState()).toBe(after);
  });
});

// ── termModes ───────────────────────────────────────────────────────────────

describe("termModes", () => {
  it("defaults to terminal for any agent", () => {
    expect(termModeFor(useStore.getState(), "agent-x")).toBe("terminal");
    expect(termModeFor(useStore.getState(), null)).toBe("terminal");
  });

  it("setTermMode records the per-agent mode", () => {
    useStore.getState().setTermMode("agent-x", "console");

    const s = useStore.getState();
    expect(s.termModes["agent-x"]).toBe("console");
    expect(termModeFor(s, "agent-x")).toBe("console");
    expect(termModeFor(s, "agent-y")).toBe("terminal"); // others unaffected
  });

  it("setting the current mode is an identity no-op", () => {
    useStore.getState().setTermMode("agent-x", "console");
    const before = useStore.getState();

    useStore.getState().setTermMode("agent-x", "console");
    useStore.getState().setTermMode("agent-y", "terminal"); // already the default

    expect(useStore.getState()).toBe(before);
  });
});

// ── fetchAgents connectivity (daemon_ping is the one source) ────────────────

describe("fetchAgents connectivity", () => {
  it("outside Tauri: never probes, never claims reachability", async () => {
    await useStore.getState().fetchAgents();

    expect(useStore.getState().connected).toBe(false);
    expect(ping).not.toHaveBeenCalled();
    expect(listAgents).not.toHaveBeenCalled();
  });

  it("in Tauri with a live daemon: connected = ping result, roster updates", async () => {
    inTauriMock.mockReturnValue(true);
    ping.mockResolvedValue(true);
    listAgents.mockResolvedValue([{ agent_id: "a1", status: "IDLE" }]);

    await useStore.getState().fetchAgents();

    const s = useStore.getState();
    expect(s.connected).toBe(true);
    expect(s.agents).toEqual([{ agent_id: "a1", status: "IDLE" }]);
  });

  it("in Tauri with a dead daemon: ping=false wins — a resolved listAgents fallback can't claim connectivity", async () => {
    inTauriMock.mockReturnValue(true);
    ping.mockResolvedValue(false);
    listAgents.mockResolvedValue([]); // the daemon-dead fallback shape
    useStore.setState({ connected: true, agents: [{ agent_id: "a1" }] });

    await useStore.getState().fetchAgents();

    const s = useStore.getState();
    expect(s.connected).toBe(false);
    expect(s.agents).toEqual([{ agent_id: "a1" }]); // last real roster kept
    expect(listAgents).not.toHaveBeenCalled(); // fallback never consulted
  });

  it("recovers: a later successful ping flips connected back on", async () => {
    inTauriMock.mockReturnValue(true);
    ping.mockResolvedValue(false);
    await useStore.getState().fetchAgents();
    expect(useStore.getState().connected).toBe(false);

    ping.mockResolvedValue(true);
    await useStore.getState().fetchAgents();
    expect(useStore.getState().connected).toBe(true);
  });
});

// ── one-shot assignment delivery ────────────────────────────────────────────

describe("assignment delivery", () => {
  /** Launch term-new/sess-new with an assignment (the dialog path). */
  async function launchWithAssignment(assignment: string | null = "Fix the flaky test") {
    useStore.setState({ workspaceDir: "/proj" });
    provisionWorktree.mockResolvedValueOnce(makeWorktree({ agent_id: "term-new" }));
    spawnAgent.mockResolvedValueOnce("sess-new");
    await useStore
      .getState()
      .launchAgentDaemon("claude_code", "default", null, null, assignment);
  }

  it("launch stamps pendingAssignments keyed by the minted agent id (trimmed)", async () => {
    await launchWithAssignment("  Fix the flaky test  ");
    expect(useStore.getState().pendingAssignments).toEqual({
      "term-new": "Fix the flaky test",
    });
  });

  it("launch without an assignment (or blank) stamps nothing", async () => {
    await launchWithAssignment(null);
    expect(useStore.getState().pendingAssignments).toEqual({});

    await launchWithAssignment("   ");
    expect(useStore.getState().pendingAssignments).toEqual({});
  });

  it("seedViaInbox delivers via the daemon inbox (enqueue), NOT the keystroke path", async () => {
    useStore.setState({ workspaceDir: "/proj" });
    provisionWorktree.mockResolvedValueOnce(makeWorktree({ agent_id: "term-seed" }));
    spawnAgent.mockResolvedValueOnce("sess-seed");

    await useStore
      .getState()
      .launchAgentDaemon("claude_code", "orchestrator", "task-1", null, "build a thing", true);

    // Daemon inbox owns delivery — no keystroke pending (never double-delivers).
    expect(useStore.getState().pendingAssignments).toEqual({});
    expect(sendMessage).toHaveBeenCalledWith("taime", "term-seed", "build a thing");
  });

  it("IDLE before the view attaches keeps the entry pending (writer not registered)", async () => {
    await launchWithAssignment();
    sendToTerminalMock.mockReturnValue(false); // no writer yet

    useStore.getState().setDaemonSessionStatus("sess-new", "IDLE");

    expect(sendToTerminalMock).toHaveBeenCalledWith("sess-new", "Fix the flaky test");
    expect(useStore.getState().pendingAssignments["term-new"]).toBe(
      "Fix the flaky test",
    ); // retained for the next IDLE report
  });

  it("first IDLE with the view attached writes the text, clears the entry, then submits + checkpoints", async () => {
    vi.useFakeTimers();
    try {
      await launchWithAssignment();
      sendToTerminalMock.mockReturnValue(true);

      useStore.getState().setDaemonSessionStatus("sess-new", "IDLE");

      expect(sendToTerminalMock).toHaveBeenCalledWith("sess-new", "Fix the flaky test");
      expect(useStore.getState().pendingAssignments).toEqual({}); // one-shot: cleared

      vi.advanceTimersByTime(150); // the delayed submit write
      expect(sendToTerminalMock).toHaveBeenLastCalledWith("sess-new", "\r");
      expect(checkpoint).toHaveBeenCalledWith("sess-new", "submit");
    } finally {
      vi.useRealTimers();
    }
  });

  it("delivers at most once: a later IDLE round-trip does not re-send", async () => {
    vi.useFakeTimers();
    try {
      await launchWithAssignment();
      sendToTerminalMock.mockReturnValue(true);
      useStore.getState().setDaemonSessionStatus("sess-new", "IDLE");
      vi.advanceTimersByTime(150);
      const callsAfterFirst = sendToTerminalMock.mock.calls.length;

      useStore.getState().setDaemonSessionStatus("sess-new", "PROCESSING");
      useStore.getState().setDaemonSessionStatus("sess-new", "IDLE");
      vi.advanceTimersByTime(150);

      expect(sendToTerminalMock.mock.calls.length).toBe(callsAfterFirst);
    } finally {
      vi.useRealTimers();
    }
  });

  it("poll-path setTerminalStatus IDLE delivers too (the reconcile-tick retry)", async () => {
    vi.useFakeTimers();
    try {
      await launchWithAssignment();
      sendToTerminalMock.mockReturnValue(true);

      useStore.getState().setTerminalStatus("term-new", "IDLE");

      expect(sendToTerminalMock).toHaveBeenCalledWith("sess-new", "Fix the flaky test");
      expect(useStore.getState().pendingAssignments).toEqual({});
      vi.advanceTimersByTime(150); // flush the submit timer inside fake time
    } finally {
      vi.useRealTimers();
    }
  });

  it("adopted (pre-existing) agents never receive a delivery", () => {
    useStore.getState().adoptDaemonSession(makeSummary());
    sendToTerminalMock.mockReturnValue(true);

    useStore.getState().setDaemonSessionStatus("sess-1", "IDLE");
    useStore.getState().setTerminalStatus("term-1", "IDLE");

    expect(sendToTerminalMock).not.toHaveBeenCalled();
  });
});

// ── durable review acks (the flagship safe-context-switch guard) ────────────

describe("durable review acks", () => {
  it("markReviewed sets the local ack AND persists it to the daemon", () => {
    useStore.getState().markReviewed("agent-z");
    expect(useStore.getState().reviewedFrames["agent-z"]).toBe(true);
    expect(markReviewedApi).toHaveBeenCalledWith("agent-z");
  });

  it("resolveSwitch(true) persists the proceeded-past agent's ack", () => {
    seedUnreviewedAgent();
    useStore.getState().setSection("dashboard"); // raises the guard
    useStore.getState().resolveSwitch(true);
    expect(useStore.getState().reviewedFrames["term-1"]).toBe(true);
    expect(markReviewedApi).toHaveBeenCalledWith("term-1");
  });

  it("hydrateReviewed unions daemon acks without clobbering local ones", () => {
    useStore.setState({ reviewedFrames: { local: true } });
    useStore.getState().hydrateReviewed(["a", "b", "local"]);
    expect(useStore.getState().reviewedFrames).toEqual({ local: true, a: true, b: true });
  });

  it("hydrateReviewed is an identity no-op when nothing new", () => {
    useStore.setState({ reviewedFrames: { a: true } });
    const before = useStore.getState();
    useStore.getState().hydrateReviewed(["a"]);
    expect(useStore.getState()).toBe(before);
  });
});
