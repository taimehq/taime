/**
 * Regression tests for the api layer's strict/tolerant query wiring — P0
 * finding 7 lived exactly here. The review's mutation check showed the whole
 * suite stayed green with the strict switches reverted (store.test.ts mocks
 * ./api wholesale), so a "helpful" future re-addition of a fallback or a
 * .catch(() => empty) would silently re-arm "daemon-down renders as No
 * changes to review". These tests pin the wiring.
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

vi.mock("./pty", () => ({
  daemonQuery: vi.fn(),
  daemonQueryStrict: vi.fn(),
  daemonProvisionWorktree: vi.fn(),
}));

vi.mock("./backend", () => ({
  inTauri: vi.fn(() => false),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

import { api } from "./api";
import { daemonQuery, daemonQueryStrict } from "./pty";

const strict = vi.mocked(daemonQueryStrict);
const tolerant = vi.mocked(daemonQuery);

beforeEach(() => {
  vi.clearAllMocks();
});

// The trust-surface reads: every one must be STRICT (reject on daemon-down,
// never resolve a typed fallback indistinguishable from real data).
const STRICT_READS: Array<{
  name: string;
  call: () => Promise<unknown>;
  kind: string;
  args: Record<string, unknown>;
}> = [
  {
    name: "getFileDiffs",
    call: () => api.getFileDiffs("agent-1"),
    kind: "file_diffs",
    args: { agent_id: "agent-1" },
  },
  {
    name: "getHunks",
    call: () => api.getHunks("agent-1"),
    kind: "hunked_diff",
    args: { agent_id: "agent-1" },
  },
  {
    name: "getAttribution",
    call: () => api.getAttribution("agent-1"),
    kind: "attribution",
    args: { agent_id: "agent-1" },
  },
  {
    name: "getWorktree",
    call: () => api.getWorktree("agent-1"),
    kind: "worktree",
    args: { agent_id: "agent-1" },
  },
  {
    name: "getContention",
    call: () => api.getContention("/proj"),
    kind: "contention",
    args: { session: "/proj" },
  },
];

describe("strict review-surface reads", () => {
  for (const r of STRICT_READS) {
    it(`${r.name} queries strictly (kind + args) and never falls back`, async () => {
      strict.mockResolvedValueOnce({});
      await r.call();
      expect(strict).toHaveBeenCalledWith(r.kind, r.args);
      expect(tolerant).not.toHaveBeenCalled();
    });

    it(`${r.name} propagates a daemon-down rejection to the caller`, async () => {
      strict.mockRejectedValueOnce(new Error("daemon unreachable"));
      await expect(r.call()).rejects.toThrow("daemon unreachable");
    });
  }
});

describe("getGraph", () => {
  it("queries strictly, forwarding the workspace root verbatim (P0 item 15: never a task id)", async () => {
    strict.mockResolvedValueOnce({ agents: [], edges: [] });
    await api.getGraph("/work/project");
    expect(strict).toHaveBeenCalledWith("graph", { workspace_root: "/work/project" });
    expect(tolerant).not.toHaveBeenCalled();
  });

  it("maps the daemon graph shape and defaults the optional fields", async () => {
    strict.mockResolvedValueOnce({
      agents: [{ agent_id: "a1", provider: "codex", status: "IDLE" }],
      edges: [{ kind: "assign", source: "a1", target: "a2" }],
    });
    const g = await api.getGraph("");
    expect(g.agents).toEqual([
      {
        agent_id: "a1",
        provider: "codex",
        status: "IDLE",
        mode: null,
        branch: null,
        member_of: null,
        task_id: null,
        turns: [],
      },
    ]);
    expect(g.edges).toEqual([{ kind: "assign", source: "a1", target: "a2", ts: null }]);
    expect(g.contention).toEqual([]);
  });

  it("propagates a daemon-down rejection", async () => {
    strict.mockRejectedValueOnce(new Error("daemon unreachable"));
    await expect(api.getGraph("/work/project")).rejects.toThrow("daemon unreachable");
  });
});

describe("tolerant surfaces stay tolerant", () => {
  it("listAgents rides the fallback path (the connectivity probe is daemonPing, not this)", async () => {
    tolerant.mockResolvedValueOnce([]);
    await api.listAgents();
    expect(tolerant).toHaveBeenCalledWith("agents", {}, []);
    expect(strict).not.toHaveBeenCalled();
  });

  it("clearDaemonDirty is best-effort; markReviewed reports an honest false on daemon-down", async () => {
    // clear_dirty is a tolerant best-effort write (fallback true). mark_reviewed
    // is tolerant too, but its fallback is FALSE: a review ack that never
    // reached a down daemon must not claim success (the gate's "honest acks").
    tolerant.mockResolvedValue(true);
    await api.clearDaemonDirty("agent-1");
    await api.markReviewed("agent-1");
    expect(tolerant).toHaveBeenCalledWith("clear_dirty", { agent_id: "agent-1" }, true);
    expect(tolerant).toHaveBeenCalledWith("mark_reviewed", { agent_id: "agent-1" }, false);
    expect(strict).not.toHaveBeenCalled();
  });
});
