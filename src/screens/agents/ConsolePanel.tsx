import { useEffect, useRef, useState, type FormEvent } from "react";
import { ChevronDown, ChevronRight, CornerDownLeft } from "lucide-react";
import type { GraphTurn } from "../../api";
import { useStore } from "../../store";
import { submitConsoleMessage } from "./consoleSubmit";
import { fmtClock, fmtClockMs } from "./format";

/**
 * Console — a PROJECTION of the agent's one PTY stream, never a second stream.
 * Renders the durable attribution turns as Warp-style blocks (timestamp,
 * status sigil, files touched). Input rides the DAEMON INBOX
 * (`daemon_send_message`, the proven Schedule/Workflow delivery path): the
 * daemon types + submits it into the agent's stdin when the agent next goes
 * idle. A direct `daemon_write` is NOT usable here — it requires a live
 * terminal attachment, and the Console mounts INSTEAD of the terminal view,
 * so those bytes were silently dropped while the UI echoed "sent to stdin"
 * (2026-06 review).
 */

interface Echo {
  id: number;
  at: number;
  text: string;
}

let echoSeq = 0;

/** Sigil for a turn block: ▸ in flight, ✓ ended, and for the LATEST ended turn
 *  the agent's live wire status upgrades it to ⚠ (blocked) / ✗ (error). */
function sigilFor(
  turn: GraphTurn,
  isLatest: boolean,
  wireStatus: string | undefined,
): { ch: string; cls: string } {
  if (!turn.ended_at) return { ch: "▸", cls: "text-accent" };
  if (isLatest && wireStatus === "ERROR") return { ch: "✗", cls: "text-red-400" };
  if (isLatest && wireStatus === "WAITING_USER_ANSWER")
    return { ch: "⚠", cls: "text-amber" };
  return { ch: "✓", cls: "text-emerald-400" };
}

export function ConsolePanel({
  agentId,
  anchorId,
  exited,
  wireStatus,
  turns,
  loading,
  error,
}: {
  /** The Agent ID — the inbox address messages deliver to. Null for a
   *  pre-provision session (no worktree row): input disables, because an
   *  inbox message addressed to nobody would dead-letter silently. */
  agentId: string | null;
  /** The display anchor (Agent ID, or the session id pre-provision). */
  anchorId: string;
  /** Process gone — stdin is closed; the input disables. */
  exited: boolean;
  /** Raw wire status (PROCESSING/WAITING_USER_ANSWER/…) for the latest block. */
  wireStatus: string | undefined;
  turns: GraphTurn[];
  loading: boolean;
  error: boolean;
}) {
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);
  const [echoes, setEchoes] = useState<Echo[]>([]);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const scrollRef = useRef<HTMLDivElement>(null);
  // With the daemon unreachable there is no inbox to enqueue on — disable the
  // form instead of letting a submit boot a fresh daemon that has never heard
  // of this agent (the message would dead-letter behind a confident echo).
  const connected = useStore((s) => s.connected);

  // Autoscroll to the newest block (console reads bottom-up like a terminal).
  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [turns.length, echoes.length]);

  const toggle = (id: string) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      next.has(id) ? next.delete(id) : next.add(id);
      return next;
    });

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const v = input.trim();
    if (!v || exited || sending || !agentId || !connected) return;
    setSending(true);
    // Enqueue on the daemon inbox: the daemon types + submits it when the
    // agent next goes idle — no dependency on a terminal attachment this view
    // never has. A failed enqueue is surfaced — a false "queued" echo is
    // exactly the bug this path replaced.
    const r = await submitConsoleMessage(agentId, v);
    if (r.ok) {
      setSendError(null);
      setEchoes((es) =>
        [...es, { id: ++echoSeq, at: Date.now(), text: v }].slice(-20),
      );
      setInput("");
    } else {
      setSendError(r.error);
    }
    setSending(false);
  };

  const empty = turns.length === 0 && echoes.length === 0;

  return (
    <div className="flex h-full flex-col bg-[#070809]">
      <div ref={scrollRef} className="min-h-0 flex-1 overflow-y-auto p-2">
        {empty && error && (
          <p className="px-2 py-3 text-xs text-zinc-600">
            daemon unreachable · retrying
          </p>
        )}
        {empty && !error && loading && (
          <p className="px-2 py-3 text-xs text-zinc-600">loading turn history…</p>
        )}
        {empty && !error && !loading && (
          <p className="px-2 py-3 text-xs text-zinc-600">
            no turns yet · blocks appear as {anchorId} works
          </p>
        )}

        <div className="flex flex-col gap-1">
          {turns.map((t, i) => {
            const sig = sigilFor(t, i === turns.length - 1, wireStatus);
            const open = expanded.has(t.id);
            const n = t.files_touched.length;
            return (
              <div
                key={t.id}
                className="rounded-md border border-ink-600/60 bg-ink-800/40"
              >
                <button
                  onClick={() => toggle(t.id)}
                  disabled={n === 0}
                  title={
                    n > 0
                      ? open
                        ? "Collapse files"
                        : "Show files touched"
                      : undefined
                  }
                  className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left enabled:hover:bg-ink-700/50 disabled:cursor-default"
                >
                  <span
                    className={`w-3 shrink-0 text-center font-mono text-[12px] ${sig.cls}`}
                  >
                    {sig.ch}
                  </span>
                  <span className="shrink-0 font-mono text-[11px] text-zinc-300">
                    turn {t.turn_index + 1}
                  </span>
                  <span className="shrink-0 font-mono text-[10px] tabular-nums text-zinc-600">
                    {fmtClock(t.ended_at ?? t.started_at)}
                  </span>
                  <span className="min-w-0 flex-1 truncate text-[10px] tabular-nums text-zinc-500">
                    {n} file{n === 1 ? "" : "s"} touched
                    {!t.ended_at ? " · in flight" : ""}
                  </span>
                  {n > 0 &&
                    (open ? (
                      <ChevronDown size={11} className="shrink-0 text-zinc-600" />
                    ) : (
                      <ChevronRight size={11} className="shrink-0 text-zinc-600" />
                    ))}
                </button>
                {open && n > 0 && (
                  <ul className="border-t border-ink-600/60 px-2 py-1">
                    {t.files_touched.map((p) => (
                      <li
                        key={p}
                        title={p}
                        className="truncate whitespace-nowrap font-mono text-[10px] leading-5 text-zinc-500"
                      >
                        {p}
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            );
          })}

          {echoes.map((e) => (
            <div
              key={e.id}
              className="flex items-center gap-2 rounded-md border border-ink-600/40 px-2 py-1.5"
            >
              <span className="w-3 shrink-0 text-center font-mono text-[12px] text-violet-400">
                ›
              </span>
              <span
                className="min-w-0 flex-1 truncate font-mono text-[11px] text-zinc-300"
                title={e.text}
              >
                {e.text}
              </span>
              <span className="shrink-0 text-[10px] text-zinc-600">
                queued · delivers when idle
              </span>
              <span className="shrink-0 font-mono text-[10px] tabular-nums text-zinc-600">
                {fmtClockMs(e.at)}
              </span>
            </div>
          ))}
        </div>
      </div>

      {/* Inbox delivery is idle-gated: a blocked agent won't see it until its
          prompt is answered — point at the Terminal tab for that. */}
      {!exited && wireStatus === "WAITING_USER_ANSWER" && (
        <p className="shrink-0 border-t border-ink-600 px-2 py-1 text-[10px] text-amber">
          agent is waiting for an answer — inbox messages deliver when it goes
          idle; answer its prompt in the Terminal tab
        </p>
      )}
      {sendError && (
        <p className="shrink-0 border-t border-ink-600 px-2 py-1 text-[10px] text-rose-400">
          send failed: {sendError}
        </p>
      )}

      {/* Input — enqueues on the daemon inbox addressed to the Agent ID. */}
      <form
        onSubmit={submit}
        className="flex shrink-0 items-center gap-2 border-t border-ink-600 px-2 py-1.5"
      >
        <span className="shrink-0 font-mono text-[12px] text-violet-400">›</span>
        <input
          value={input}
          onChange={(e) => setInput(e.target.value)}
          disabled={exited || sending || !agentId || !connected}
          placeholder={
            exited
              ? "agent exited · stdin closed"
              : !connected
                ? "daemon unreachable · retrying"
                : !agentId
                  ? "no agent id yet — use the Terminal tab"
                  : `message ${anchorId} · delivered when it goes idle…`
          }
          title="Enqueues on the daemon inbox; the daemon types + submits it into the agent's stdin when the agent next goes idle"
          className="min-w-0 flex-1 bg-transparent font-mono text-[12px] text-zinc-200 placeholder:text-zinc-700 focus:outline-none disabled:opacity-50"
        />
        <button
          type="submit"
          disabled={!input.trim() || exited || sending || !agentId || !connected}
          className="flex shrink-0 items-center gap-1 rounded-md border border-ink-500 px-2 py-0.5 text-[11px] text-zinc-300 enabled:hover:bg-ink-600 disabled:cursor-default disabled:opacity-40"
        >
          <CornerDownLeft size={11} />
          {sending ? "queuing…" : "send"}
        </button>
      </form>
    </div>
  );
}
