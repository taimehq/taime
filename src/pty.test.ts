/**
 * Tests for the daemon bridge (src/pty.ts): the query fallback/strict split and
 * the attach channel's control-message dispatch — the transport seams the
 * review surfaces and the agent lifecycle hang off (2026-06 review: coverage
 * stopped exactly here).
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => {
  class MockChannel {
    onmessage: (msg: unknown) => void = () => {};
  }
  return { invoke: vi.fn(), Channel: MockChannel };
});

vi.mock("./backend", () => ({
  inTauri: vi.fn(() => true),
}));

import { invoke } from "@tauri-apps/api/core";
import { inTauri } from "./backend";
import {
  daemonQuery,
  daemonQueryStrict,
  daemonAttach,
  DaemonUnreachableError,
  type TurnEvent,
} from "./pty";

const invokeMock = vi.mocked(invoke);
const inTauriMock = vi.mocked(inTauri);

beforeEach(() => {
  vi.clearAllMocks();
  inTauriMock.mockReturnValue(true);
  vi.spyOn(console, "warn").mockImplementation(() => {});
});

describe("daemonQuery (tolerant)", () => {
  it("returns the invoke result and passes the JSON-stringified fallback", async () => {
    invokeMock.mockResolvedValueOnce([{ agent_id: "a1" }]);
    const r = await daemonQuery("agents", { x: 1 }, []);
    expect(r).toEqual([{ agent_id: "a1" }]);
    expect(invokeMock).toHaveBeenCalledWith("daemon_query", {
      kind: "agents",
      args: { x: 1 },
      fallback: "[]",
    });
  });

  it("returns the fallback outside Tauri without invoking", async () => {
    inTauriMock.mockReturnValue(false);
    const r = await daemonQuery("agents", {}, ["fb"]);
    expect(r).toEqual(["fb"]);
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("swallows invoke errors into the fallback (never rejects)", async () => {
    invokeMock.mockRejectedValueOnce("daemon unreachable");
    const r = await daemonQuery("agents", {}, ["fb"]);
    expect(r).toEqual(["fb"]);
  });
});

describe("daemonQueryStrict", () => {
  it("returns the invoke result and requests NO fallback", async () => {
    invokeMock.mockResolvedValueOnce({ files: [{ path: "a.ts" }] });
    const r = await daemonQueryStrict("file_diffs", { agent_id: "a1" });
    expect(r).toEqual({ files: [{ path: "a.ts" }] });
    expect(invokeMock).toHaveBeenCalledWith("daemon_query", {
      kind: "file_diffs",
      args: { agent_id: "a1" },
      fallback: null,
    });
  });

  it("throws DaemonUnreachableError outside Tauri", async () => {
    inTauriMock.mockReturnValue(false);
    await expect(daemonQueryStrict("file_diffs", {})).rejects.toBeInstanceOf(
      DaemonUnreachableError,
    );
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("maps the command's unreachable error to DaemonUnreachableError", async () => {
    invokeMock.mockRejectedValueOnce("daemon unreachable");
    await expect(daemonQueryStrict("file_diffs", {})).rejects.toBeInstanceOf(
      DaemonUnreachableError,
    );
  });

  it("rethrows non-connectivity errors with their message intact", async () => {
    invokeMock.mockRejectedValueOnce("parse query result: bad json");
    await expect(daemonQueryStrict("file_diffs", {})).rejects.toThrow(
      "parse query result: bad json",
    );
  });
});

describe("daemonAttach channel dispatch", () => {
  const handlers = () => ({
    onBytes: vi.fn(),
    onExit: vi.fn(),
    onTurn: vi.fn(),
    onStatus: vi.fn(),
    onFsDirty: vi.fn(),
    onDisconnected: vi.fn(),
  });

  /** Attach with mocked invoke and return the channel + spies. */
  async function attach() {
    invokeMock.mockResolvedValueOnce(7); // the attach generation
    const h = handlers();
    const res = await daemonAttach(
      "sess-1",
      24,
      80,
      h.onBytes,
      h.onExit,
      h.onTurn,
      h.onStatus,
      h.onFsDirty,
      h.onDisconnected,
    );
    expect(res).not.toBeNull();
    expect(res!.gen).toBe(7); // the gen rides back for gen-scoped close_view
    // Pin the wire contract: rows/cols must reach the daemon BEFORE the grid
    // repaint (the resize-first handoff), alongside the channel sink.
    expect(invokeMock).toHaveBeenCalledWith(
      "daemon_attach",
      expect.objectContaining({ sessionId: "sess-1", rows: 24, cols: 80 }),
    );
    // The channel passed to invoke is the one whose onmessage dispatches.
    const sent = invokeMock.mock.calls[0][1] as { onData: { onmessage: (m: unknown) => void } };
    return { dispatch: sent.onData.onmessage, ...h };
  }

  it("routes raw bytes to onBytes", async () => {
    const t = await attach();
    t.dispatch(new Uint8Array([104, 105]).buffer);
    expect(t.onBytes).toHaveBeenCalledTimes(1);
    expect([...t.onBytes.mock.calls[0][0]]).toEqual([104, 105]);
  });

  it("routes a process exit to onExit with its code", async () => {
    const t = await attach();
    t.dispatch({ type: "exit", code: 0 });
    expect(t.onExit).toHaveBeenCalledWith(0);
    expect(t.onDisconnected).not.toHaveBeenCalled();
  });

  it("routes 'disconnected' to onDisconnected — NEVER onExit (detach/crash ≠ exit)", async () => {
    const t = await attach();
    t.dispatch({ type: "disconnected" });
    expect(t.onDisconnected).toHaveBeenCalledTimes(1);
    expect(t.onExit).not.toHaveBeenCalled();
  });

  it("routes turn/status/fs_dirty pushes to their handlers", async () => {
    const t = await attach();
    const turn: Partial<TurnEvent> & { type: string } = {
      type: "turn",
      epoch: 1,
      startOffset: 0,
      endOffset: 10,
      fsDirtyPaths: [],
    };
    t.dispatch(turn);
    t.dispatch({ type: "status", status: "IDLE" });
    t.dispatch({ type: "fs_dirty", paths: ["a.ts"] });
    expect(t.onTurn).toHaveBeenCalledTimes(1);
    expect(t.onStatus).toHaveBeenCalledWith("IDLE");
    expect(t.onFsDirty).toHaveBeenCalledWith(["a.ts"]);
    expect(t.onExit).not.toHaveBeenCalled();
  });

  it("returns null when the attach invoke rejects", async () => {
    invokeMock.mockRejectedValueOnce("no such session");
    const h = handlers();
    const res = await daemonAttach("sess-x", 24, 80, h.onBytes, h.onExit);
    expect(res).toBeNull();
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });
});
