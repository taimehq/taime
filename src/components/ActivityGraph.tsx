import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { X, GitBranch, RefreshCw, AlertTriangle, FileText, Users, CornerDownRight } from "lucide-react";
import { api, type ActivityGraph as Graph } from "../api";
import { useStore } from "../store";
import { providerTitle } from "../lib/providerLabel";

/** Edges that define the team tree (who spawned / handed work to whom). */
const TREE_KINDS = new Set(["assign", "handoff"]);

/** Color + human label per edge kind (matches the daemon's emitted kinds).
 *  Palette: blue (#5b8def) is the primary structural edge; amber for handoffs;
 *  light blue + zinc neutrals for the chatter so it stays legible without new hues. */
const EDGE: Record<string, { color: string; label: string }> = {
  assign: { color: "#5b8def", label: "assigned" },
  handoff: { color: "#e3a93a", label: "handed off" },
  message: { color: "#8db1f5", label: "messaged" },
  request: { color: "#8a919d", label: "asked" },
  reply: { color: "#6f7681", label: "replied" },
};
const edgeStyle = (kind: string) => EDGE[kind] ?? { color: "#6f7681", label: kind };

/** Status → dot color. Accepts the live inferred status (SCREAMING_SNAKE) or the
 *  graph's lifecycle value (running/exited) as a fallback. */
function statusDot(status: string | null): string {
  switch ((status ?? "").toUpperCase()) {
    case "PROCESSING":
      return "bg-teal-400";
    case "WAITING_USER_ANSWER":
      return "bg-amber";
    case "ERROR":
      return "bg-rose-400";
    case "COMPLETED":
      return "bg-emerald-400";
    case "IDLE":
    case "RUNNING":
      return "bg-zinc-400";
    case "EXITED":
      return "bg-zinc-700";
    default:
      return "bg-zinc-600";
  }
}
function statusText(status: string | null): string {
  const s = (status ?? "").toUpperCase();
  if (s === "WAITING_USER_ANSWER") return "needs you";
  if (s === "PROCESSING") return "working";
  if (s === "COMPLETED") return "done";
  if (s === "ERROR") return "error";
  if (s === "IDLE") return "idle";
  if (s === "EXITED") return "exited";
  if (s === "RUNNING") return "running";
  return "—";
}

function fmtTime(iso: string | null): string {
  if (!iso) return "";
  try {
    return new Date(iso).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  } catch {
    return "";
  }
}

/**
 * The team panel: a non-disruptive right-side drawer (terminals stay visible) that
 * shows the live agent team as a vertical tree — workers nested under whoever
 * assigned/handed-off to them — plus the recent flow of messages/handoffs between
 * them. Polls while open so the team forms in real time; click an agent for its diff.
 */
export function ActivityGraph() {
  const open = useStore((s) => s.graphOpen);
  const setGraphOpen = useStore((s) => s.setGraphOpen);
  const openDiff = useStore((s) => s.openDiff);
  const frames = useStore((s) => s.frames);
  const terminalStatuses = useStore((s) => s.terminalStatuses);
  const activeWorkspaceRoot = useStore((s) => s.activeWorkspaceRoot);

  const [graph, setGraph] = useState<Graph | null>(null);
  const [loading, setLoading] = useState(false);
  const firstLoad = useRef(true);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      // Scope the team to the active workspace so the drawer shows this
      // workspace's agents, not every agent the daemon has ever run.
      setGraph(await api.getGraph(activeWorkspaceRoot ?? ""));
    } catch {
      setGraph(null);
    } finally {
      setLoading(false);
      firstLoad.current = false;
    }
  }, [activeWorkspaceRoot]);

  // Live: load on open + poll every 2s while open so the team appears as it forms.
  useEffect(() => {
    if (!open) return;
    firstLoad.current = true;
    load();
    const t = setInterval(load, 2000);
    return () => clearInterval(t);
  }, [open, load]);

  // Esc closes the drawer.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setGraphOpen(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, setGraphOpen]);

  const nameFor = useCallback(
    (tid: string | null) => {
      if (!tid) return "?";
      const g = graph?.agents.find((a) => a.agent_id === tid);
      const fr = frames.find((f) => f.terminalId === tid);
      const prov = g?.provider ?? fr?.provider ?? "";
      return prov ? providerTitle(prov) : tid.slice(0, 6);
    },
    [graph, frames],
  );

  // Order agents as a tree: roots first, each followed by its (indented) workers.
  const tree = useMemo(() => {
    const agents = graph?.agents ?? [];
    const byId = new Map(agents.map((a) => [a.agent_id, a]));
    const parentOf = new Map<string, string>();
    const childrenOf = new Map<string, string[]>();
    for (const e of graph?.edges ?? []) {
      if (!e.source || !e.target) continue;
      if (TREE_KINDS.has(e.kind) && byId.has(e.source) && byId.has(e.target) && !parentOf.has(e.target)) {
        parentOf.set(e.target, e.source);
        const list = childrenOf.get(e.source) ?? [];
        list.push(e.target);
        childrenOf.set(e.source, list);
      }
    }
    const order: { id: string; depth: number }[] = [];
    const seen = new Set<string>();
    const visit = (id: string, depth: number) => {
      if (seen.has(id)) return;
      seen.add(id);
      order.push({ id, depth });
      for (const c of childrenOf.get(id) ?? []) visit(c, depth + 1);
    };
    for (const a of agents) {
      const p = parentOf.get(a.agent_id);
      if (!p || !byId.has(p)) visit(a.agent_id, 0);
    }
    for (const a of agents) visit(a.agent_id, 0); // any cycle orphans
    return { order, byId, parentOf };
  }, [graph]);

  if (!open) return null;

  const agents = graph?.agents ?? [];
  const flow = (graph?.edges ?? []).filter((e) => e.source && e.target);
  const isEmpty = !loading && firstLoad.current === false && agents.length === 0;

  return (
    <aside className="fixed right-0 top-12 bottom-0 z-40 flex w-[380px] flex-col border-l border-t border-ink-600 bg-ink-900 shadow-2xl">
      <div className="flex items-center justify-between border-b border-ink-600 bg-ink-800 px-3 py-2.5">
        <div className="flex items-center gap-2 text-sm">
          <Users size={15} className="text-teal-400" />
          <span className="font-semibold text-zinc-100">Team</span>
          <span className="text-[11px] text-zinc-500">
            {agents.length} agent{agents.length === 1 ? "" : "s"}
          </span>
        </div>
        <div className="flex items-center gap-1">
          <button
            onClick={load}
            title="Refresh"
            className="rounded-lg p-1.5 text-zinc-400 hover:bg-ink-600 hover:text-zinc-200"
          >
            <RefreshCw size={14} className={loading ? "animate-spin" : ""} />
          </button>
          <button
            onClick={() => setGraphOpen(false)}
            title="Close (Esc / ⌘⇧A)"
            className="rounded-lg p-1.5 text-zinc-400 hover:bg-ink-600 hover:text-zinc-200"
            aria-label="Close team panel"
          >
            <X size={16} />
          </button>
        </div>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto p-2">
        {isEmpty && (
          <div className="mt-16 flex flex-col items-center px-6 text-center">
            {/* Silhouette of a 3-node team tree: one root handing to two workers. */}
            <svg
              width="88"
              height="64"
              viewBox="0 0 88 64"
              fill="none"
              className="mb-4"
              aria-hidden="true"
            >
              <path d="M44 18 L24 44 M44 18 L64 44" stroke="#232936" strokeWidth="1.5" />
              <circle cx="44" cy="14" r="10" fill="#0c0e13" stroke="#232936" strokeWidth="1.5" />
              <circle cx="24" cy="50" r="10" fill="#0c0e13" stroke="#232936" strokeWidth="1.5" />
              <circle cx="64" cy="50" r="10" fill="#0c0e13" stroke="#232936" strokeWidth="1.5" />
            </svg>
            <p className="text-[13px] text-zinc-300">No agents yet</p>
            <p className="mt-1.5 text-[11px] leading-relaxed text-zinc-500">
              Launch an agent to see it here. Pick the orchestrator profile and ask
              it to delegate — its workers nest below it as the team forms.
            </p>
            <button
              onClick={() => useStore.getState().setLaunchOpen(true)}
              className="mt-4 rounded-lg border border-ink-500 px-3 py-1.5 text-[12px] text-zinc-300 hover:bg-ink-700"
            >
              Launch agent
            </button>
          </div>
        )}

        {/* Team tree */}
        {tree.order.map(({ id, depth }) => {
          const a = tree.byId.get(id);
          if (!a) return null;
          const filesTouched = new Set(a.turns.flatMap((t) => t.files_touched)).size;
          const isWorker = depth > 0 || !!tree.parentOf.get(id);
          const liveStatus = terminalStatuses[id] ?? a.status;
          return (
            <button
              key={id}
              onClick={() => openDiff(id)}
              style={{ marginLeft: depth * 14 }}
              className={`mb-1.5 flex flex-col gap-1 rounded-lg border bg-ink-800 px-2.5 py-2 text-left transition-colors hover:border-teal-400/40 ${
                isWorker ? "border-ink-500" : "border-ink-600"
              }`}
            >
              <div className="flex items-center gap-2">
                {isWorker && <CornerDownRight size={12} className="shrink-0 text-zinc-500" />}
                <span className={`h-2 w-2 shrink-0 rounded-full ${statusDot(liveStatus)}`} />
                <span className="truncate text-[13px] font-medium text-zinc-100">{nameFor(id)}</span>
                <span className="ml-auto text-[10px] text-zinc-500">{statusText(liveStatus)}</span>
              </div>
              <div className="flex items-center gap-2.5 pl-4 text-[10px] text-zinc-600">
                <span>
                  {a.turns.length} turn{a.turns.length === 1 ? "" : "s"}
                </span>
                {filesTouched > 0 && (
                  <span className="flex items-center gap-1">
                    <FileText size={9} /> {filesTouched}
                  </span>
                )}
                {a.mode === "isolated" && a.branch ? (
                  <span className="flex items-center gap-1 truncate text-teal-300/80">
                    <GitBranch size={9} /> {a.branch}
                  </span>
                ) : (
                  a.mode && <span>{a.mode}</span>
                )}
              </div>
            </button>
          );
        })}

        {/* Flow: recent inter-agent activity */}
        {flow.length > 0 && (
          <section className="mt-4">
            <h3 className="mb-1.5 px-1 text-[10px] font-semibold uppercase tracking-wide text-zinc-600">Activity</h3>
            <ul className="space-y-1">
              {flow.slice(-12).reverse().map((e, i) => {
                const { color, label } = edgeStyle(e.kind);
                return (
                  <li key={i} className="flex items-center gap-1.5 px-1 text-[11px] text-zinc-400">
                    <span className="truncate text-zinc-300">{nameFor(e.source)}</span>
                    <span className="shrink-0" style={{ color }}>
                      {label} →
                    </span>
                    <span className="truncate text-zinc-300">{nameFor(e.target)}</span>
                    {e.ts && <span className="ml-auto shrink-0 font-mono text-[9px] tabular-nums text-zinc-600">{fmtTime(e.ts)}</span>}
                  </li>
                );
              })}
            </ul>
          </section>
        )}

        {/* Contention */}
        {graph && graph.contention.length > 0 && (
          <section className="mt-4 rounded-lg border border-rose-400/30 bg-rose-400/5 p-2.5">
            <h3 className="mb-1.5 flex items-center gap-1.5 text-[10px] font-semibold uppercase tracking-wide text-rose-300">
              <AlertTriangle size={12} /> Contended files
            </h3>
            <ul className="space-y-1">
              {graph.contention.map((c) => (
                <li key={c.path} className="text-[11px]">
                  <span className="font-mono text-rose-200">{c.path}</span>
                  <span className="text-zinc-500"> — {c.terminals.map(nameFor).join(", ")}</span>
                </li>
              ))}
            </ul>
          </section>
        )}
      </div>
    </aside>
  );
}
