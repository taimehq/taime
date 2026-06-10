import { daemonSendMessage } from "../../pty";

/**
 * The Console's send action, extracted from the component so the trust rules
 * are unit-testable (the 2026-06 review found this fix otherwise unguarded):
 * a message only counts as queued when the daemon actually accepted the
 * enqueue, and a missing Agent ID short-circuits without touching the daemon
 * (an inbox message addressed to nobody would dead-letter silently).
 */
export type ConsoleSubmitResult =
  | { ok: true }
  | { ok: false; error: string };

export async function submitConsoleMessage(
  agentId: string | null,
  text: string,
  send: typeof daemonSendMessage = daemonSendMessage,
): Promise<ConsoleSubmitResult> {
  const v = text.trim();
  if (!v) return { ok: false, error: "empty message" };
  if (!agentId) return { ok: false, error: "no agent id — inbox messages need an address" };
  try {
    await send("user", agentId, v);
    return { ok: true };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}
