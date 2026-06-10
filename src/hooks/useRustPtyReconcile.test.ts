/**
 * Tests for the reconcile tick's demotion decision (sessionsToDemote) — the
 * rule that decides when a tracked session flips to exited against the daemon
 * roster, including the connection-lost override of the framed exemption
 * (without it, a daemon crash wedges a framed agent as "running" forever).
 */
import { describe, it, expect, vi } from "vitest";

// The hook module transitively imports the store/api/pty (Tauri); mock them so
// importing the pure helper is hermetic.
vi.mock("../backend", () => ({ inTauri: vi.fn(() => false) }));
vi.mock("../store", () => ({ useStore: { getState: vi.fn() } }));
vi.mock("../api", () => ({ api: {} }));
vi.mock("../pty", () => ({ daemonList: vi.fn(async () => []) }));

import { sessionsToDemote } from "./useRustPtyReconcile";
import type { RustPtyMeta } from "../store";

function meta(over: Partial<RustPtyMeta> = {}): RustPtyMeta {
  return {
    ptySessionId: "sess-1",
    terminalId: "term-1",
    provider: "claude_code",
    branch: null,
    cwd: null,
    startedAt: 0,
    status: "running",
    ...over,
  };
}

describe("sessionsToDemote", () => {
  it("demotes a running unframed session the daemon no longer lists", () => {
    expect(
      sessionsToDemote({ "sess-1": meta() }, new Set(), new Set()),
    ).toEqual(["sess-1"]);
  });

  it("keeps a running session the daemon still lists", () => {
    expect(
      sessionsToDemote({ "sess-1": meta() }, new Set(), new Set(["sess-1"])),
    ).toEqual([]);
  });

  it("skips already-exited sessions", () => {
    expect(
      sessionsToDemote({ "sess-1": meta({ status: "exited" }) }, new Set(), new Set()),
    ).toEqual([]);
  });

  it("exempts framed sessions (just-launched frames precede list visibility)", () => {
    expect(
      sessionsToDemote({ "sess-1": meta() }, new Set(["sess-1"]), new Set()),
    ).toEqual([]);
  });

  it("demotes a framed session whose attach connection was lost (daemon crash)", () => {
    expect(
      sessionsToDemote(
        { "sess-1": meta({ connectionLost: true }) },
        new Set(["sess-1"]),
        new Set(),
      ),
    ).toEqual(["sess-1"]);
  });

  it("keeps a framed connection-lost session that the daemon still lists (conn dropped, agent alive)", () => {
    expect(
      sessionsToDemote(
        { "sess-1": meta({ connectionLost: true }) },
        new Set(["sess-1"]),
        new Set(["sess-1"]),
      ),
    ).toEqual([]);
  });
});
