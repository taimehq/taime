import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Play, X, Loader2 } from "lucide-react";
import {
  api,
  type WorkflowInfo,
  type WorkflowNodeState,
  type WorkflowRunSummary,
} from "../api";
import { useStore } from "../store";

/** Status → node fill/border/dot color. Mirrors the design-system status palette:
 *  pending=zinc, running=blue (teal-*), completed=emerald, failed=rose. */
const NODE_STYLE: Record<
  string,
  { dot: string; border: string; fill: string; text: string }
> = {
  pending: { dot: "#6f6f6f", border: "#2a2a2a", fill: "#0f0f0f", text: "#a1a1a1" },
  running: { dot: "#4493f8", border: "#4493f8", fill: "#0f0f0f", text: "#ededed" },
  completed: { dot: "#3fb950", border: "#3fb950", fill: "#0f0f0f", text: "#ededed" },
  failed: { dot: "#fb7185", border: "#fb7185", fill: "#0f0f0f", text: "#ededed" },
};
function nodeStyle(status: string | undefined) {
  return NODE_STYLE[status ?? "pending"] ?? NODE_STYLE.pending;
}

/** A run-status pill (header). */
function statusPill(status: string | undefined): { label: string; cls: string } {
  switch (status) {
    case "running":
      return { label: "Running", cls: "bg-teal-400/15 text-teal-300" };
    case "completed":
      return { label: "Completed", cls: "bg-emerald-400/15 text-emerald-300" };
    case "failed":
      return { label: "Failed", cls: "bg-rose-400/15 text-rose-300" };
    default:
      return { label: "Idle", cls: "bg-ink-700 text-zinc-500" };
  }
}

/** Render an edge `when` as a compact human label. */
function whenLabel(when: string): { text: string; faint: boolean } {
  if (!when || when === "always") return { text: "always", faint: true };
  if (when.startsWith("keyword:")) return { text: `if ${when.slice(8)}`, faint: false };
  return { text: when, faint: false }; // /regex/ — show verbatim
}

// Layout constants.
const NODE_W = 150;
const NODE_H = 46;
const COL_GAP = 64; // vertical gap between depth layers
const ROW_GAP = 22; // horizontal gap between siblings in a layer
const PAD_X = 24;
const PAD_TOP = 16;
const PAD_BOTTOM = 28;

interface Placed {
  id: string;
  depth: number;
  row: number;
  x: number;
  y: number;
}

/**
 * The Workflow graph drawer: a read-only node-link diagram of a workflow's
 * step-graph, laid out top-to-bottom by topological depth from `entry`. Nodes are
 * colored by the current run's per-node state; forward edges curve down with a
 * `when` label, back-edges (loops) arc in amber. A Run button starts a run and the
 * drawer polls live status while it runs. Clicking a node that has an `agent_id`
 * opens that agent's diff.
 */
export function WorkflowGraph({
  workflow,
  onClose,
}: {
  workflow: WorkflowInfo;
  onClose: () => void;
}) {
  const [runId, setRunId] = useState<string | null>(workflow.last_run?.id ?? null);
  const [run, setRun] = useState<WorkflowRunSummary | null>(workflow.last_run ?? null);
  const [starting, setStarting] = useState(false);
  const [startError, setStartError] = useState<string | null>(null);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  // Esc closes the drawer.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  // Poll live run status while a run is active.
  useEffect(() => {
    if (!runId) return;
    let cancelled = false;
    const tick = async () => {
      try {
        const r = await api.getWorkflowRun(runId);
        if (cancelled || !mounted.current) return;
        if (r) setRun(r);
        // Stop the loop once the run has settled.
        if (r && r.status !== "running") {
          window.clearInterval(timer);
        }
      } catch {
        /* surfaced on the next tick */
      }
    };
    void tick();
    const timer = window.setInterval(tick, 1500);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [runId]);

  const onRun = useCallback(async () => {
    setStarting(true);
    setStartError(null);
    try {
      const res = await api.runWorkflow(workflow.name, useStore.getState().workspaceDir);
      if (!mounted.current) return;
      if (res.error || !res.run_id) {
        setStartError(res.error ?? "Failed to start run");
      } else {
        setRun(null);
        setRunId(res.run_id);
      }
    } catch {
      if (mounted.current) setStartError("Failed to start run");
    } finally {
      if (mounted.current) setStarting(false);
    }
  }, [workflow.name]);

  const nodeStates: Record<string, WorkflowNodeState> = run?.node_states ?? {};

  // ── Layout: BFS from entry, assigning each node its first-seen depth. ──────
  const layout = useMemo(() => {
    const nodes = workflow.nodes;
    const ids = new Set(nodes.map((n) => n.id));
    const adj = new Map<string, string[]>();
    for (const e of workflow.edges) {
      if (!ids.has(e.from) || !ids.has(e.to)) continue;
      const list = adj.get(e.from) ?? [];
      list.push(e.to);
      adj.set(e.from, list);
    }

    const depth = new Map<string, number>();
    const order: string[] = [];
    const enqueue = (start: string) => {
      if (depth.has(start)) return;
      const queue: string[] = [start];
      depth.set(start, depth.get(start) ?? 0);
      while (queue.length) {
        const cur = queue.shift() as string;
        order.push(cur);
        const d = depth.get(cur) ?? 0;
        for (const next of adj.get(cur) ?? []) {
          if (!depth.has(next)) {
            depth.set(next, d + 1); // first depth wins; tolerates cycles
            queue.push(next);
          }
        }
      }
    };
    // Start from entry, then sweep any unreached nodes (disconnected/orphans).
    if (ids.has(workflow.entry)) enqueue(workflow.entry);
    for (const n of nodes) enqueue(n.id);

    // Group by depth, preserving discovery order within each layer.
    const layers = new Map<number, string[]>();
    for (const id of order) {
      const d = depth.get(id) ?? 0;
      const arr = layers.get(d) ?? [];
      arr.push(id);
      layers.set(d, arr);
    }

    const maxRow = Math.max(1, ...[...layers.values()].map((l) => l.length));
    const contentW = maxRow * NODE_W + (maxRow - 1) * ROW_GAP;
    const placed = new Map<string, Placed>();
    const sortedDepths = [...layers.keys()].sort((a, b) => a - b);
    sortedDepths.forEach((d, di) => {
      const row = layers.get(d) as string[];
      const rowW = row.length * NODE_W + (row.length - 1) * ROW_GAP;
      const offset = PAD_X + (contentW - rowW) / 2;
      row.forEach((id, ri) => {
        placed.set(id, {
          id,
          depth: d,
          row: ri,
          x: offset + ri * (NODE_W + ROW_GAP),
          y: PAD_TOP + di * (NODE_H + COL_GAP),
        });
      });
    });

    const width = contentW + PAD_X * 2;
    const height =
      PAD_TOP + sortedDepths.length * NODE_H + (sortedDepths.length - 1) * COL_GAP + PAD_BOTTOM;

    return { placed, depth, width, height };
  }, [workflow.nodes, workflow.edges, workflow.entry]);

  const pill = statusPill(run?.status);

  return (
    <aside className="fixed right-0 top-12 bottom-0 z-40 flex w-[460px] flex-col border-l border-t border-ink-600 bg-ink-900 shadow-2xl">
      {/* Header */}
      <div className="flex items-center gap-2 border-b border-ink-600 bg-ink-800 px-3 py-2.5">
        <div className="flex min-w-0 flex-1 items-center gap-2">
          <span className="truncate text-sm font-semibold text-zinc-100">{workflow.name}</span>
          <span
            className={`shrink-0 rounded-full px-2 py-0.5 text-[10px] font-medium ${pill.cls}`}
          >
            {pill.label}
          </span>
        </div>
        <button
          onClick={onRun}
          disabled={starting || run?.status === "running"}
          title="Run workflow"
          className="flex shrink-0 items-center gap-1 rounded-lg border border-ink-500 px-2 py-1 text-[12px] text-zinc-300 hover:bg-ink-700 disabled:opacity-60"
        >
          {starting ? <Loader2 size={13} className="animate-spin" /> : <Play size={13} />}
          Run
        </button>
        <button
          onClick={onClose}
          title="Close (Esc)"
          aria-label="Close workflow graph"
          className="shrink-0 rounded-lg p-1.5 text-zinc-400 hover:bg-ink-600 hover:text-zinc-200"
        >
          <X size={16} />
        </button>
      </div>

      {/* Graph body */}
      <div className="min-h-0 flex-1 overflow-auto p-3">
        {workflow.nodes.length === 0 ? (
          <div className="mt-16 flex flex-col items-center px-6 text-center">
            <svg width="80" height="56" viewBox="0 0 80 56" fill="none" className="mb-4" aria-hidden>
              <path d="M40 16 L40 36" stroke="#3a3a3a" strokeWidth="1.5" />
              <rect x="22" y="2" width="36" height="14" rx="3" fill="#0f0f0f" stroke="#3a3a3a" strokeWidth="1.5" />
              <rect x="22" y="38" width="36" height="14" rx="3" fill="#0f0f0f" stroke="#3a3a3a" strokeWidth="1.5" />
            </svg>
            <p className="text-[13px] text-zinc-300">No steps</p>
            <p className="mt-1.5 text-[11px] leading-relaxed text-zinc-500">
              This workflow has no nodes yet.
            </p>
          </div>
        ) : (
          <svg
            width={layout.width}
            height={layout.height}
            viewBox={`0 0 ${layout.width} ${layout.height}`}
            className="block"
          >
            <defs>
              <marker
                id="wf-arrow"
                markerWidth="8"
                markerHeight="8"
                refX="6"
                refY="3"
                orient="auto"
                markerUnits="userSpaceOnUse"
              >
                <path d="M0,0 L6,3 L0,6 Z" fill="#3a3a3a" />
              </marker>
              <marker
                id="wf-arrow-loop"
                markerWidth="8"
                markerHeight="8"
                refX="6"
                refY="3"
                orient="auto"
                markerUnits="userSpaceOnUse"
              >
                <path d="M0,0 L6,3 L0,6 Z" fill="#d29922" />
              </marker>
            </defs>

            {/* Edge layer */}
            {workflow.edges.map((e, i) => {
              const a = layout.placed.get(e.from);
              const b = layout.placed.get(e.to);
              if (!a || !b) return null;
              const da = layout.depth.get(e.from) ?? 0;
              const db = layout.depth.get(e.to) ?? 0;
              const isBack = db <= da; // loop / back-edge to an earlier (or equal) layer
              const { text, faint } = whenLabel(e.when);

              if (isBack) {
                // A curved amber arc bowing out to the right so loops read clearly.
                const sx = a.x + NODE_W;
                const sy = a.y + NODE_H / 2;
                const tx = b.x + NODE_W;
                const ty = b.y + NODE_H / 2;
                const bow = 46 + Math.abs(da - db) * 14;
                const cx = Math.max(sx, tx) + bow;
                const path = `M ${sx} ${sy} C ${cx} ${sy}, ${cx} ${ty}, ${tx} ${ty}`;
                const midX = cx - bow * 0.18;
                const midY = (sy + ty) / 2;
                return (
                  <g key={`e-${i}`}>
                    <path
                      d={path}
                      fill="none"
                      stroke="#d29922"
                      strokeWidth="1.5"
                      strokeDasharray="3 3"
                      markerEnd="url(#wf-arrow-loop)"
                    />
                    <text x={midX} y={midY} fontSize="9" fill="#e3b341" textAnchor="start">
                      {text}
                    </text>
                  </g>
                );
              }

              // Forward edge: bottom of source → top of target, curved vertically.
              const sx = a.x + NODE_W / 2;
              const sy = a.y + NODE_H;
              const tx = b.x + NODE_W / 2;
              const ty = b.y;
              const midY = (sy + ty) / 2;
              const path = `M ${sx} ${sy} C ${sx} ${midY}, ${tx} ${midY}, ${tx} ${ty}`;
              const labelX = (sx + tx) / 2;
              return (
                <g key={`e-${i}`}>
                  <path
                    d={path}
                    fill="none"
                    stroke="#3a3a3a"
                    strokeWidth="1.5"
                    markerEnd="url(#wf-arrow)"
                  />
                  <text
                    x={labelX}
                    y={midY - 2}
                    fontSize="9"
                    fill={faint ? "#6f6f6f" : "#a1a1a1"}
                    textAnchor="middle"
                  >
                    {text}
                  </text>
                </g>
              );
            })}

            {/* Node layer */}
            {workflow.nodes.map((n) => {
              const p = layout.placed.get(n.id);
              if (!p) return null;
              const st = nodeStates[n.id];
              const style = nodeStyle(st?.status);
              const clickable = !!st?.agent_id;
              const iter = st && st.iteration > 1 ? `×${st.iteration}` : "";
              return (
                <g
                  key={n.id}
                  transform={`translate(${p.x}, ${p.y})`}
                  onClick={
                    clickable
                      ? () => useStore.getState().openDiff(st!.agent_id as string)
                      : undefined
                  }
                  style={{ cursor: clickable ? "pointer" : "default" }}
                >
                  <rect
                    width={NODE_W}
                    height={NODE_H}
                    rx="8"
                    fill={style.fill}
                    stroke={style.border}
                    strokeWidth="1.5"
                  />
                  <circle cx="13" cy="14" r="3.5" fill={style.dot} />
                  <text
                    x="23"
                    y="17"
                    fontSize="11"
                    fontWeight="600"
                    fill={style.text}
                  >
                    {n.id.length > 18 ? `${n.id.slice(0, 17)}…` : n.id}
                  </text>
                  <text x="13" y="34" fontSize="9" fill="#8f8f8f">
                    {n.role.length > 22 ? `${n.role.slice(0, 21)}…` : n.role}
                  </text>
                  {iter && (
                    <text
                      x={NODE_W - 8}
                      y="17"
                      fontSize="9"
                      fill="#e3b341"
                      textAnchor="end"
                    >
                      {iter}
                    </text>
                  )}
                  {n.id === workflow.entry && (
                    <text x={NODE_W - 8} y="34" fontSize="8" fill="#6f6f6f" textAnchor="end">
                      entry
                    </text>
                  )}
                </g>
              );
            })}
          </svg>
        )}
      </div>

      {/* Footer: legend + errors */}
      <div className="border-t border-ink-600 bg-ink-800 px-3 py-2">
        {startError && (
          <p className="mb-1.5 text-[11px] text-rose-400">{startError}</p>
        )}
        {run?.error && (
          <p className="mb-1.5 break-words text-[11px] text-rose-400">{run.error}</p>
        )}
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[10px] text-zinc-600">
          <Legend dot="#6f6f6f" label="pending" />
          <Legend dot="#4493f8" label="running" />
          <Legend dot="#3fb950" label="completed" />
          <Legend dot="#fb7185" label="failed" />
          <span className="flex items-center gap-1">
            <span className="inline-block h-0.5 w-3" style={{ background: "#d29922" }} />
            loop
          </span>
        </div>
        {(run?.node_states &&
          Object.values(run.node_states).some((s) => s.agent_id)) && (
          <p className="mt-1.5 text-[10px] text-zinc-600">
            Click a node to open its agent's diff.
          </p>
        )}
      </div>
    </aside>
  );
}

function Legend({ dot, label }: { dot: string; label: string }) {
  return (
    <span className="flex items-center gap-1">
      <span className="inline-block h-2 w-2 rounded-full" style={{ background: dot }} />
      {label}
    </span>
  );
}
