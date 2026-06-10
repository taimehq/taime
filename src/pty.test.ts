/**
 * Tests for the daemon bridge (src/pty.ts): the query fallback/strict split and
 * the attach channel's control-message dispatch — the transport seams the
 * review surfaces and the agent lifecycle hang off (2026-06 review: coverage
 * stopped exactly here).
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

// Capture every constructed Channel so dispatch tests can drive onmessage.
const channels: Array<{ onmessage: (msg: unknown) => void }> = [];
vi.mock("@tauri-apps/api/core", () => {
  class MockChannel {
    onmessage: (msg: unknown) => void = () => {};
    constructor() {
      channels.push(this);
    }
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
  DaemonUnreachableError,
} from "./pty";

const invokeMock = vi.mocked(invoke);
const inTauriMock = vi.mocked(inTauri);

beforeEach(() => {
  channels.length = 0;
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
