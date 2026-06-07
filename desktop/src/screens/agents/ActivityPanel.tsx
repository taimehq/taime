import { useMemo } from "react";
import type { GraphTurn } from "../../api";
import { useStore } from "../../store";
import { fmtClock, fmtClockMs } from "./format";

/** One feed row, normalized from either source (turns / notifications). */
interface FeedItem {
  key: string;
  /** Epoch ms for ordering (0 = unknown — sinks to the bottom). */
  at: number;
  time: string;
  dot: string;
  text: string;
  sub?: string;
  subTitle?: string;
}

const KIND_DOT: Record<string, string> = {
  blocked: "bg-amber",
  review: "bg-amber",
  error: "bg-red-400",
  exited: "bg-zinc-600",
};

/**
 * Activity tab — the agent-scoped feed: the durable attribution turns merged
 * with this agent's attention items (blocked/review/exited/error pushes),
 * newest first.
 */
export function ActivityPanel({
  anchorId,
  turns,
  loading,
  error,
}: {
  anchorId: string;
  turns: GraphTurn[];
  loading: boolean;
  error: boolean;
}) {
  const notifications = useStore((s) => s.notifications);

  const items = useMemo<FeedItem[]>(() => {
    const out: FeedItem[] = [];
    for (const t of turns) {
      const iso = t.ended_at ?? t.started_at;
      const at = iso ? Date.parse(iso) || 0 : 0;
      const n = t.files_touched.length;
      const shown = t.files_touched.slice(0, 2);
      const more = n - shown.length;
      out.push({
        key: `turn-${t.id}`,
        at,
        time: fmtClock(iso),
        dot: t.ended_at ? "bg-emerald-400" : "bg-accent animate-pulse",
        text: `turn ${t.turn_index + 1} ${t.ended_at ? "completed" : "running"}`,
        sub:
          n > 0
            ? `${shown.join(" · ")}${more > 0 ? ` +${more} more` : ""}`
            : undefined,
        subTitle: n > 0 ? t.files_touched.join("\n") : undefined,
      });
    }
    for (const n of notifications) {
      if (n.agentId !== anchorId) continue;
      out.push({
        key: n.id,
        at: n.at,
        time: fmtClockMs(n.at),
        dot: KIND_DOT[n.kind] ?? "bg-zinc-600",
        text: n.text,
      });
    }
    return out.sort((a, b) => b.at - a.at).slice(0, 100);
  }, [turns, notifications, anchorId]);

  if (items.length === 0) {
    return (
      <p className="p-4 text-xs text-zinc-600">
        {error
          ? "daemon unreachable · retrying"
          : loading
            ? "loading activity…"
            : "no activity yet"}
      </p>
    );
  }

  return (
    <div className="h-full overflow-y-auto p-2">
      <ul>
        {items.map((it) => (
          <li key={it.key} className="flex gap-2 px-2 py-1.5">
            <span className="w-[58px] shrink-0 pt-px font-mono text-[10px] tabular-nums text-zinc-600">
              {it.time}
            </span>
            <span
              className={`mt-1 h-1.5 w-1.5 shrink-0 rounded-full ${it.dot}`}
            />
            <span className="min-w-0 flex-1">
              <span className="block truncate text-[11px] text-zinc-300">
                {it.text}
              </span>
              {it.sub && (
                <span
                  title={it.subTitle}
                  className="block truncate whitespace-nowrap font-mono text-[10px] text-zinc-600"
                >
                  {it.sub}
                </span>
              )}
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}
