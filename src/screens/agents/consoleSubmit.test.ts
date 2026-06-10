/**
 * Tests for the Console's send action — the inbox-routed replacement for the
 * silently-dropped daemonWrite path (2026-06 review, P0 item 10). The rules:
 * an echo is only earned by an accepted enqueue, and a missing Agent ID never
 * touches the daemon (it would dead-letter).
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

vi.mock("../../pty", () => ({
  daemonSendMessage: vi.fn(),
}));

import { submitConsoleMessage } from "./consoleSubmit";
import { daemonSendMessage } from "../../pty";

const sendMock = vi.mocked(daemonSendMessage);

beforeEach(() => {
  vi.clearAllMocks();
});

describe("submitConsoleMessage", () => {
  it("enqueues trimmed text addressed to the agent id and reports ok", async () => {
    sendMock.mockResolvedValueOnce(7);
    const r = await submitConsoleMessage("term-1", "  hello agent  ");
    expect(r).toEqual({ ok: true });
    expect(sendMock).toHaveBeenCalledWith("user", "term-1", "hello agent");
  });

  it("short-circuits without an agent id — never calls the daemon", async () => {
    const r = await submitConsoleMessage(null, "hello");
    expect(r.ok).toBe(false);
    expect(sendMock).not.toHaveBeenCalled();
  });

  it("short-circuits on empty/whitespace text", async () => {
    const r = await submitConsoleMessage("term-1", "   ");
    expect(r.ok).toBe(false);
    expect(sendMock).not.toHaveBeenCalled();
  });

  it("maps an enqueue rejection to an error result — never a false ok", async () => {
    sendMock.mockRejectedValueOnce(new Error("daemon unreachable"));
    const r = await submitConsoleMessage("term-1", "hello");
    expect(r).toEqual({ ok: false, error: "daemon unreachable" });
  });

  it("stringifies non-Error rejections", async () => {
    sendMock.mockRejectedValueOnce("socket closed");
    const r = await submitConsoleMessage("term-1", "hello");
    expect(r).toEqual({ ok: false, error: "socket closed" });
  });
});
