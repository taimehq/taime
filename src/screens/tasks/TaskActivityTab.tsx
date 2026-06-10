import { useEffect, useMemo, useRef, useState } from "react";
import { Activity } from "lucide-react";
import { api, type ActivityGraph, type TaskDetail } from "../../api";
import { useStore, type NotificationKind } from "../../store";
import { fmtClock } from "./lib";

/** Notification kind → dot classes (mirrors the bell's attention grammar). */
const KIND_DOT: Record<NotificationKind, string> = {
  blocked: "bg-amber",
  review: "bg-accent",
  error: "bg-red-400",
  exited: "bg-zinc-600",
};

/** Workflow-run status → dot classes (run statuses, not agent statuses). */
function runDotClass(status: string): string {
  switch (status) {
    case "running":
      return "bg-teal-400 animate-pulse";
    case "completed":
      return "bg-emerald-400";
    case "failed":
      return "bg-red-400";
    default:
      return "bg-zinc-600";
  }
}

interface FeedEvent {
  /** Stable key within the feed. */
  key: string;
  /** Epoch ms. */
  at: number;
  dot: string;
  text: string;
  /** Attribution anchor (mono "via …" line): an agent id or a run id. */
  via: string;
  /** Optional hover detail (e.g. the turn's touched files). */
  title?: string;
}

/**
 * Activity: the task-scoped feed, merged from the existing queries — agent
 * turn boundaries (the attribution substrate, via the activity graph filtered
 * to this task's members), the store's daemon-push notifications carrying this
 * taskId, and the task's attached workflow runs. Newest first.
 */
export function TaskActivityTab({
  taskId,
  detail,
}: {
  taskId: string;
  detail: TaskDetail;
}) {
  const notifications = useStore((s) => s.notifications);
  const connected = useStore((s) => s.connected);

  const [graph, setGraph] = useState<ActivityGraph | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [loadError, setLoadError] = useState(false);
  const current = useRef(taskId);
  current.current = taskId;

  // getGraph scopes the roster by WORKSPACE ROOT, not task id — passing the
  // task id matched no workspace, so the roster was always empty and agent
  // turns never rendered (2026-06 review). Query the task's workspace and
  // keep the client-side task_id filter below.
  const workspaceRoot = detail.task.workspace_root;

  // Poll the graph (5s — turns land on turn close, not keystrokes).
  useEffect(() => {
    setGraph(null);
    setLoaded(false);
    setLoadError(false);
    let alive = true;
    const load = () =>
      api
        .getGraph(workspaceRoot)
        .then((g) => {
          if (!alive || current.current !== taskId) return;
          setGraph(g);
          setLoaded(true);
          setLoadError(false);
        })
        .catch(() => {
          // Strict read: daemon-down rejects. Keep polling; the feed shows an
          // unreachable state instead of an authoritative "No activity yet".
          if (alive && current.current === taskId) setLoadError(true);
        });
    load();
    const t = setInterval(load, 5000);
    return () => {
      alive = false;
      clearInterval(t);
    };
  }, [taskId, workspaceRoot]);

  const events = useMemo<FeedEvent[]>(() => {
    const out: FeedEvent[] = [];

    // Agent turns — task-scoped via the graph's task_id membership.
    for (const a of graph?.agents ?? []) {
      if (a.task_id !== taskId) continue;
      for (const t of a.turns) {
        const ts = Date.parse(t.ended_at ?? t.started_at ?? "");
        if (Number.isNaN(ts)) continue;
        const n = t.files_touched.length;
        out.push({
          key: `turn-${a.agent_id}-${t.id}`,
          at: ts,
          dot: t.ended_at ? "bg-emerald-400" : "bg-teal-400 animate-pulse",
          text: `turn ${t.turn_index + 1} ${t.ended_at ? "ended" : "started"} · ${n} file${
            n === 1 ? "" : "s"
          } touched`,
          via: a.agent_id,
          title: t.files_touched.slice(0, 20).join("\n") || undefined,
        });
      }
    }

    // Attention items pushed against this task (blocked/review/error/exited).
    for (const n of notifications) {
      if (n.taskId !== taskId) continue;
      out.push({
        key: n.id,
        at: n.at,
        dot: KIND_DOT[n.kind],
        text: n.text,
        via: n.agentId,
      });
    }

    // Workflow runs attached to this task.
    for (const r of detail.runs) {
      if (!r.started_at) continue;
      out.push({
        key: `run-${r.id}`,
        at: r.started_at * 1000,
        dot: runDotClass(r.status),
        text: `workflow ${r.workflow_name} · ${r.status}`,
        via: `run ${r.id.slice(0, 8)}`,
      });
    }

    return out.sort((x, y) => y.at - x.at).slice(0, 200);
  }, [graph, notifications, detail.runs, taskId]);

  if (!loaded) {
    return (
      <div className="flex h-full items-center justify-center px-6 text-center">
        <p className="text-xs text-zinc-500">
          {!connected || loadError
            ? "daemon unreachable · retrying"
            : "loading activity…"}
        </p>
      </div>
    );
  }

  if (events.length === 0) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-2 px-6 text-center">
        <Activity size={24} className="text-zinc-700" />
        <p className="text-xs text-zinc-500">
          No activity yet — agent turns and attention events land here.
        </p>
      </div>
    );
  }

  return (
    <div className="h-full overflow-y-auto p-4">
      <ul className="max-w-3xl">
        {events.map((e, i) => (
          <li key={e.key} className="flex gap-2.5">
            <span className="tnum w-[64px] shrink-0 pt-px text-right font-mono text-[10px] text-zinc-600">
              {fmtClock(e.at)}
            </span>
            {/* Rail: dot + connecting line */}
            <span className="flex shrink-0 flex-col items-center">
              <span className={`mt-1 h-2 w-2 rounded-full ${e.dot}`} />
              {i < events.length - 1 && <span className="w-px flex-1 bg-ink-600" />}
            </span>
            <span className="min-w-0 flex-1 pb-3">
              <span
                title={e.title}
                className="block truncate whitespace-nowrap text-xs text-zinc-300"
              >
                {e.text}
              </span>
              <span
                title={e.via}
                className="block truncate whitespace-nowrap font-mono text-[10px] text-zinc-600"
              >
                via {e.via}
              </span>
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}
