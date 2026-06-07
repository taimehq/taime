import { useCallback, useEffect, useRef, useState } from "react";
import { Play, Trash2, Network, Loader2, Workflow } from "lucide-react";
import { api, type WorkflowInfo } from "../api";
import { useStore } from "../store";
import { WorkflowGraph } from "./WorkflowGraph";

/** Last-run status → dot color (design-system status palette). */
function runDot(status: string | undefined): string {
  switch (status) {
    case "running":
      return "bg-teal-400";
    case "completed":
      return "bg-emerald-400";
    case "failed":
      return "bg-rose-400";
    default:
      return "bg-zinc-600";
  }
}

/** Compact relative time from a unix-seconds timestamp (e.g. "5m ago"). */
function relativeTime(ts: number | null): string {
  if (ts == null) return "";
  const s = Math.max(0, Math.round((Date.now() - ts * 1000) / 1000));
  let value: number;
  let unit: string;
  if (s < 60) {
    value = s;
    unit = "s";
  } else if (s < 3600) {
    value = Math.round(s / 60);
    unit = "m";
  } else if (s < 86400) {
    value = Math.round(s / 3600);
    unit = "h";
  } else {
    value = Math.round(s / 86400);
    unit = "d";
  }
  return `${value}${unit} ago`;
}

/** The most relevant timestamp for a run summary (ended if settled, else started). */
function runTime(wf: WorkflowInfo): number | null {
  const r = wf.last_run;
  if (!r) return null;
  return r.ended_at ?? r.started_at ?? null;
}

export function WorkflowsPanel() {
  const [workflows, setWorkflows] = useState<WorkflowInfo[]>([]);
  const [selected, setSelected] = useState<WorkflowInfo | null>(null);
  const [runningName, setRunningName] = useState<string | null>(null);
  const mounted = useRef(true);

  const refresh = useCallback(async () => {
    try {
      const list = await api.listWorkflows();
      if (mounted.current) setWorkflows(list);
    } catch {
      /* surfaced by the next poll */
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    refresh();
    const id = window.setInterval(refresh, 5000);
    return () => {
      mounted.current = false;
      window.clearInterval(id);
    };
  }, [refresh]);

  // Keep the open graph in sync with the latest poll so live state flows through.
  useEffect(() => {
    if (!selected) return;
    const fresh = workflows.find((w) => w.name === selected.name);
    if (fresh && fresh !== selected) setSelected(fresh);
  }, [workflows, selected]);

  const run = async (wf: WorkflowInfo) => {
    setRunningName(wf.name);
    try {
      const result = await api.runWorkflow(wf.name, useStore.getState().workspaceDir);
      if (result.error) {
        // runWorkflow resolves with `.error` instead of throwing — surface it.
        useStore.getState().showSnackbar({
          type: "error",
          message: `Couldn't run "${wf.name}": ${result.error}`,
        });
        return;
      }
      if (!mounted.current) return;
      // Open the graph for the live view (it polls the run itself).
      setSelected(wf);
      refresh();
    } finally {
      if (mounted.current) setRunningName(null);
    }
  };

  const remove = async (name: string) => {
    if (!window.confirm(`Delete workflow "${name}"?`)) return;
    await api.deleteWorkflow(name);
    if (selected?.name === name) setSelected(null);
    refresh();
  };

  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center justify-between">
        <h2 className="text-[10px] font-semibold uppercase tracking-wide text-zinc-600">
          Workflows · {workflows.length}
        </h2>
      </div>

      {workflows.length === 0 ? (
        <p className="flex items-start gap-1.5 text-[11px] text-zinc-600">
          <Workflow size={12} className="mt-0.5 shrink-0" />
          No workflows yet — an orchestrator can author one, or drop a JSON in
          ~/.taime/workflows.
        </p>
      ) : (
        <div className="flex flex-col gap-1">
          {workflows.map((wf) => {
            const isRunning = runningName === wf.name;
            const lastStatus = wf.last_run?.status;
            return (
              <div
                key={wf.name}
                className="rounded-lg border border-ink-600 bg-ink-800 px-2.5 py-2"
              >
                <div className="flex items-start gap-2">
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-[13px] text-zinc-200">{wf.name}</div>
                    <div className="mt-0.5 flex items-center gap-1.5 text-[10px] text-zinc-500">
                      <span>
                        {wf.nodes.length} step{wf.nodes.length === 1 ? "" : "s"}
                      </span>
                      {wf.last_run && (
                        <>
                          <span className="text-zinc-700">·</span>
                          <span
                            className={`h-1.5 w-1.5 shrink-0 rounded-full ${runDot(lastStatus)}`}
                          />
                          <span className="truncate text-zinc-600">
                            {relativeTime(runTime(wf))}
                          </span>
                        </>
                      )}
                    </div>
                  </div>

                  <div className="flex shrink-0 items-center gap-1.5">
                    {/* View graph */}
                    <button
                      onClick={() => setSelected(wf)}
                      title="View graph"
                      className="rounded p-0.5 text-zinc-500 hover:text-zinc-200"
                    >
                      <Network size={13} />
                    </button>

                    {/* Run now */}
                    <button
                      onClick={() => run(wf)}
                      disabled={isRunning}
                      title="Run now"
                      className="rounded p-0.5 text-zinc-500 hover:text-zinc-200 disabled:opacity-60"
                    >
                      {isRunning ? (
                        <Loader2 size={13} className="animate-spin" />
                      ) : (
                        <Play size={13} />
                      )}
                    </button>

                    {/* Delete */}
                    <button
                      onClick={() => remove(wf.name)}
                      title="Delete workflow"
                      className="rounded p-0.5 text-zinc-600 hover:text-rose-400"
                    >
                      <Trash2 size={13} />
                    </button>
                  </div>
                </div>
              </div>
            );
          })}
        </div>
      )}

      {selected && (
        <WorkflowGraph workflow={selected} onClose={() => setSelected(null)} />
      )}
    </div>
  );
}
