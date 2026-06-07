/**
 * Pure-logic tests for the Zustand store (src/store.ts).
 *
 * All side-effecting imports are mocked so importing the store is hermetic:
 *  - ./api and ./pty transitively import @tauri-apps/api (invoke/Channel);
 *  - ./lib/recentProjects and ./lib/preferences read localStorage at module
 *    init (the store seeds workspaceDir/font size/sidebar from them).
 * ./lib/providerLabel is a pure lookup table and is left real.
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

vi.mock("./api", () => ({
  api: {
    provisionWorktree: vi.fn(),
    listAgents: vi.fn(async () => []),
    deleteSession: vi.fn(async () => ({ success: true, deleted: [], errors: [] })),
    getTerminalStatus: vi.fn(async () => null),
    clearDaemonDirty: vi.fn(async () => true),
  },
}));

vi.mock("./pty", () => ({
  daemonSpawnAgent: vi.fn(),
  daemonKill: vi.fn(async () => {}),
  daemonCloseView: vi.fn(async () => {}),
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

import { useStore, type Frame, type RustPtyMeta } from "./store";
import { api, type WorktreeInfo } from "./api";
import { daemonSpawnAgent, type DaemonSessionSummary } from "./pty";

const provisionWorktree = vi.mocked(api.provisionWorktree);
const spawnAgent = vi.mocked(daemonSpawnAgent);

// Full pristine snapshot taken once at import time; restored before each test
// (replace: true wipes any keys a test added).
const initialState = useStore.getState();

beforeEach(() => {
  useStore.setState(initialState, true);
  vi.clearAllMocks();
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
    sessionName: null,
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

// ── openTaskReview / setGraphOpen mutual exclusivity ────────────────────────

describe("task review / graph drawer exclusivity", () => {
  it("openTaskReview closes the graph drawer", () => {
    useStore.getState().setGraphOpen(true);
    expect(useStore.getState().graphOpen).toBe(true);

    useStore.getState().openTaskReview("task-1");

    const s = useStore.getState();
    expect(s.taskReviewId).toBe("task-1");
    expect(s.graphOpen).toBe(false);
  });

  it("setGraphOpen(true) nulls the open task review", () => {
    useStore.getState().openTaskReview("task-1");
    expect(useStore.getState().taskReviewId).toBe("task-1");

    useStore.getState().setGraphOpen(true);

    const s = useStore.getState();
    expect(s.graphOpen).toBe(true);
    expect(s.taskReviewId).toBeNull();
  });

  it("setGraphOpen(false) does not touch an open task review", () => {
    useStore.getState().openTaskReview("task-1");

    useStore.getState().setGraphOpen(false);

    const s = useStore.getState();
    expect(s.graphOpen).toBe(false);
    expect(s.taskReviewId).toBe("task-1");
  });

  it("closeTaskReview clears only the review id", () => {
    useStore.getState().openTaskReview("task-1");
    useStore.getState().closeTaskReview();
    expect(useStore.getState().taskReviewId).toBeNull();
  });
});
