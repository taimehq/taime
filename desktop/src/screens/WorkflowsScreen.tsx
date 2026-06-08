import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  GitBranch,
  History,
  Layers,
  Loader2,
  Minus,
  Network,
  Play,
  Plus,
  Trash2,
  Workflow as WorkflowIcon,
} from "lucide-react";
import {
  api,
  type WorkflowInfo,
  type WorkflowNode,
  type WorkflowRunSummary,
} from "../api";
import { useStore } from "../store";
import { useTasks } from "../hooks/useTasks";
import { WorkflowGraph } from "../components/WorkflowGraph";
import { providerTitle } from "../lib/providerLabel";

// ─── Wire-shape helpers ──────────────────────────────────────────────────────

/** The daemon serializes `profile`; `role` is the legacy alias older payloads
 *  carried (api.ts WorkflowNode declares both). */
function nodeProfile(n: WorkflowNode): string {
  return n.profile ?? n.role ?? "default";
}

function nodeProvider(n: WorkflowNode): string | null {
  return n.provider ?? null;
}

// ─── Formatting ──────────────────────────────────────────────────────────────

/** Compact relative time from a unix-seconds timestamp (e.g. "5m ago"). */
function relativeTime(ts: number | null): string {
  if (ts == null) return "—";
  const s = Math.max(0, Math.round((Date.now() - ts * 1000) / 1000));
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.round(s / 60)}m ago`;
  if (s < 86400) return `${Math.round(s / 3600)}h ago`;
  return `${Math.round(s / 86400)}d ago`;
}

/** Absolute short timestamp for title attrs (e.g. "Jun 7, 09:14"). */
function absoluteTime(ts: number | null): string {
  if (ts == null) return "";
  return new Date(ts * 1000).toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/** Run duration (started → ended), compact. */
function duration(start: number | null, end: number | null): string {
  if (start == null || end == null) return "";
  const s = Math.max(0, end - start);
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, "0")}s`;
  return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
}

/** Run / node-run status → chip + dot classes (run statuses, not agent ones). */
function runStatusUi(status: string | undefined): { label: string; chip: string; dot: string } {
  switch (status) {
    case "running":
      return { label: "running", chip: "bg-teal-600/15 text-teal-300", dot: "bg-teal-400 animate-pulse" };
    case "completed":
      return { label: "completed", chip: "bg-emerald-500/15 text-emerald-400", dot: "bg-emerald-400" };
    case "failed":
      return { label: "failed", chip: "bg-red-500/15 text-red-400", dot: "bg-red-400" };
    default:
      return { label: status ?? "pending", chip: "bg-ink-600 text-zinc-500", dot: "bg-zinc-600" };
  }
}

/** Re-render guard for run merges: only update when the row actually changed. */
function jsonEqual(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

// ─── Screen ──────────────────────────────────────────────────────────────────

/** Workflows section: detail for store.selectedWorkflow (the sidebar owns the
 *  list). Header (name · Definition badge · run-scope · Run), then
 *  Definition | Runs tabs. */
export function WorkflowsScreen() {
  const selectedName = useStore((s) => s.selectedWorkflow);
  const connected = useStore((s) => s.connected);
  // The New-workflow dialog is store-owned (App mounts it) so this screen and
  // the sidebar "+" share the same surface.
  const setNewWorkflowOpen = useStore((s) => s.setNewWorkflowOpen);
  const [workflows, setWorkflows] = useState<WorkflowInfo[] | null>(null);

  useEffect(() => {
    let alive = true;
    const load = () =>
      api
        .listWorkflows()
        .then((w) => {
          if (alive) setWorkflows(w);
        })
        .catch(() => {});
    load();
    const timer = setInterval(load, 5000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, []);

  const wf = workflows?.find((w) => w.name === selectedName) ?? null;

  return (
    <div className="flex h-full flex-col overflow-hidden">
      {!connected && (
        <div className="shrink-0 border-b border-amber/30 bg-amber/10 px-4 py-1 text-[11px] text-amber">
          daemon unreachable · retrying
        </div>
      )}
      {workflows === null ? (
        <CenterNote text="loading workflows…" />
      ) : workflows.length === 0 ? (
        <CenterNote
          headline="No workflows yet"
          action={{
            label: "New workflow",
            onClick: () => setNewWorkflowOpen(true),
            enabled: connected,
          }}
        />
      ) : !selectedName ? (
        <CenterNote
          text="Select a workflow in the sidebar."
          action={{
            label: "New workflow",
            onClick: () => setNewWorkflowOpen(true),
            enabled: connected,
          }}
        />
      ) : !wf ? (
        <CenterNote text={`Workflow "${selectedName}" not found — removed or renamed.`} />
      ) : (
        <WorkflowDetail
          wf={wf}
          connected={connected}
          onNew={() => setNewWorkflowOpen(true)}
        />
      )}
    </div>
  );
}

function CenterNote({
  text,
  headline,
  action,
}: {
  text?: string;
  headline?: string;
  action?: { label: string; onClick: () => void; enabled: boolean };
}) {
  return (
    <div className="flex flex-1 items-center justify-center p-6">
      <div className="flex max-w-sm flex-col items-center gap-2 text-center">
        <WorkflowIcon size={22} className="text-zinc-700" />
        {headline ? (
          <p className="text-base font-semibold tracking-tight text-zinc-200">
            {headline}
          </p>
        ) : (
          <p className="text-xs leading-relaxed text-zinc-500">{text}</p>
        )}
        {action && (
          <button
            onClick={action.onClick}
            disabled={!action.enabled}
            title={action.enabled ? action.label : "Daemon unreachable"}
            className="mt-1 flex items-center gap-1 rounded-md border border-ink-500 px-2.5 py-1 text-xs text-zinc-300 hover:bg-ink-600 disabled:cursor-default disabled:opacity-50"
          >
            <Plus size={12} />
            {action.label}
          </button>
        )}
      </div>
    </div>
  );
}

// ─── Detail ──────────────────────────────────────────────────────────────────

type Tab = "definition" | "runs";

function WorkflowDetail({
  wf,
  connected,
  onNew,
}: {
  wf: WorkflowInfo;
  connected: boolean;
  onNew: () => void;
}) {
  const workspaceDir = useStore((s) => s.workspaceDir);
  const { tasks } = useTasks(workspaceDir);
  const [tab, setTab] = useState<Tab>("definition");
  // "" = Uncategorized (task_id = null on the run), else a task id.
  const [runScope, setRunScope] = useState("");
  const [running, setRunning] = useState(false);
  const [graphOpen, setGraphOpen] = useState(false);
  // Run history: the definition's last_run + runs started this session, by id.
  // (The daemon keeps full history but only exposes the latest run per
  // workflow plus per-id status — see `remaining`.)
  const [runs, setRuns] = useState<Record<string, WorkflowRunSummary>>({});
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  // Two-step delete (no surface elsewhere since WorkflowsPanel retired).
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [deleting, setDeleting] = useState(false);

  // Selection switch: reset per-workflow view state (scope must NOT leak
  // across workflows — the prototype's known bug).
  useEffect(() => {
    setTab("definition");
    setRunScope("");
    setGraphOpen(false);
    setRuns({});
    setConfirmDelete(false);
  }, [wf.name]);

  const mergeRun = useCallback((r: WorkflowRunSummary) => {
    setRuns((prev) => {
      if (prev[r.id] && jsonEqual(prev[r.id], r)) return prev;
      return { ...prev, [r.id]: r };
    });
  }, []);

  // Fold the (polled) last_run into the history map.
  useEffect(() => {
    if (wf.last_run && wf.last_run.workflow_name === wf.name) mergeRun(wf.last_run);
  }, [wf.last_run, wf.name, mergeRun]);

  // Poll live status for runs still marked running.
  const activeIds = useMemo(
    () =>
      Object.values(runs)
        .filter((r) => r.status === "running")
        .map((r) => r.id)
        .sort()
        .join(","),
    [runs],
  );
  useEffect(() => {
    if (!activeIds) return;
    const ids = activeIds.split(",");
    const tick = async () => {
      for (const id of ids) {
        try {
          const r = await api.getWorkflowRun(id);
          if (r && mounted.current) mergeRun(r);
        } catch {
          /* retried next tick */
        }
      }
    };
    const timer = setInterval(tick, 2000);
    return () => clearInterval(timer);
  }, [activeIds, mergeRun]);

  const openTasks = tasks.filter((t) => t.status === "open" || t.status === "in_review");
  const scopeTask = runScope ? (openTasks.find((t) => t.id === runScope) ?? null) : null;

  const onRun = async () => {
    setRunning(true);
    try {
      const res = await api.runWorkflow(wf.name, workspaceDir, runScope || null);
      if (!mounted.current) return;
      if (res.error || !res.run_id) {
        useStore.getState().showSnackbar({
          type: "error",
          message: `Run failed: ${res.error ?? "no run id returned"}`,
        });
        return;
      }
      mergeRun({
        id: res.run_id,
        workflow_name: wf.name,
        status: "running",
        started_at: Math.floor(Date.now() / 1000),
        ended_at: null,
        error: null,
        task_id: runScope || null,
        node_states: {},
      });
      setTab("runs");
    } finally {
      if (mounted.current) setRunning(false);
    }
  };

  const onDelete = async () => {
    if (deleting) return;
    setDeleting(true);
    try {
      await api.deleteWorkflow(wf.name);
      useStore.getState().setSelectedWorkflow(null);
      useStore.getState().showSnackbar({
        type: "success",
        message: `Workflow ${wf.name} deleted`,
      });
    } finally {
      setDeleting(false);
      setConfirmDelete(false);
    }
  };

  const sortedRuns = Object.values(runs).sort(
    (a, b) => (b.started_at ?? 0) - (a.started_at ?? 0),
  );

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {/* Header */}
      <div className="flex shrink-0 items-start gap-3 border-b border-ink-600 px-4 py-3">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <WorkflowIcon size={15} className="shrink-0 text-accent" />
            <h1
              title={wf.name}
              className="min-w-0 truncate text-sm font-semibold text-zinc-100"
            >
              {wf.name}
            </h1>
            <span className="shrink-0 rounded bg-ink-600 px-1.5 py-0.5 text-[9px] font-semibold uppercase tracking-wide text-zinc-400">
              definition
            </span>
          </div>
          <div className="mt-1 flex items-center gap-1.5 text-[11px] text-zinc-500">
            <span>{wf.source}</span>
            <span className="text-zinc-700">·</span>
            <span className="tnum">
              {wf.nodes.length} node{wf.nodes.length === 1 ? "" : "s"}
            </span>
            <span className="text-zinc-700">·</span>
            <span className="tnum">
              {wf.edges.length} edge{wf.edges.length === 1 ? "" : "s"}
            </span>
            <span className="text-zinc-700">·</span>
            <span className="min-w-0 truncate" title={wf.entry}>
              entry <span className="font-mono text-zinc-400">{wf.entry}</span>
            </span>
          </div>
        </div>

        {/* Run scope + Run */}
        <div className="flex shrink-0 items-center gap-2">
          <label
            htmlFor="wf-run-scope"
            className="text-[10px] font-semibold uppercase tracking-wider text-zinc-600"
          >
            Run scope
          </label>
          <select
            id="wf-run-scope"
            value={runScope}
            onChange={(e) => setRunScope(e.target.value)}
            disabled={running}
            className="max-w-[200px] rounded-md border border-ink-500 bg-ink-700 px-2 py-1 text-xs text-zinc-200 disabled:opacity-50"
          >
            <option value="">Uncategorized</option>
            {openTasks.map((t) => (
              <option key={t.id} value={t.id}>
                Task: {t.title}
              </option>
            ))}
          </select>
          <button
            onClick={onRun}
            disabled={running || !connected}
            title={connected ? "Run this workflow now" : "Daemon unreachable"}
            className="flex items-center gap-1 rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-white hover:bg-primary-hover disabled:cursor-default disabled:opacity-50"
          >
            {running ? <Loader2 size={12} className="animate-spin" /> : <Play size={12} />}
            {running ? "Starting…" : "Run"}
          </button>

          {/* Delete (two-step — removes the definition .json + rows) */}
          {confirmDelete ? (
            <span className="flex items-center gap-1">
              <button
                onClick={() => void onDelete()}
                disabled={deleting || !connected}
                className="flex items-center gap-1 rounded-md bg-red-500/15 px-2 py-1 text-[11px] font-medium text-red-400 hover:bg-red-500/25 disabled:cursor-default disabled:opacity-50"
              >
                {deleting && <Loader2 size={11} className="animate-spin" />}
                Delete
              </button>
              <button
                onClick={() => setConfirmDelete(false)}
                disabled={deleting}
                className="rounded-md px-2 py-1 text-[11px] text-zinc-500 hover:text-zinc-300 disabled:opacity-50"
              >
                Cancel
              </button>
            </span>
          ) : (
            <button
              onClick={() => setConfirmDelete(true)}
              disabled={!connected}
              title="Delete workflow"
              aria-label="Delete workflow"
              className="rounded-md p-1.5 text-zinc-600 hover:bg-ink-600 hover:text-red-400 disabled:cursor-default disabled:opacity-50"
            >
              <Trash2 size={13} />
            </button>
          )}

          <span className="h-4 w-px bg-ink-600" />

          {/* New workflow */}
          <button
            onClick={onNew}
            disabled={!connected}
            title={connected ? "New workflow" : "Daemon unreachable"}
            className="flex items-center gap-1 rounded-md border border-ink-500 px-2.5 py-1 text-xs text-zinc-300 hover:bg-ink-600 disabled:cursor-default disabled:opacity-50"
          >
            <Plus size={12} />
            New workflow
          </button>
        </div>
      </div>

      {/* Tabs */}
      <div className="flex shrink-0 items-center gap-1 border-b border-ink-600 px-4">
        <TabButton
          active={tab === "definition"}
          onClick={() => setTab("definition")}
          icon={<GitBranch size={12} />}
          label="Definition"
        />
        <TabButton
          active={tab === "runs"}
          onClick={() => setTab("runs")}
          icon={<History size={12} />}
          label="Runs"
          count={sortedRuns.length}
        />
      </div>

      {/* Body */}
      <div className="min-h-0 flex-1 overflow-y-auto p-4">
        {tab === "definition" ? (
          <DefinitionTab
            wf={wf}
            scopeTask={scopeTask}
            onOpenGraph={() => setGraphOpen(true)}
          />
        ) : (
          <RunsTab
            wf={wf}
            runs={sortedRuns}
            taskTitle={(id) => tasks.find((t) => t.id === id)?.title ?? id}
          />
        )}
      </div>

      {/* The existing graph drawer (renders cycles correctly; self-polls runs).
          The screen's run scope rides along so its Run matches ours. */}
      {graphOpen && (
        <WorkflowGraph
          workflow={wf}
          taskId={runScope || null}
          onClose={() => setGraphOpen(false)}
        />
      )}
    </div>
  );
}

function TabButton({
  active,
  onClick,
  icon,
  label,
  count,
}: {
  active: boolean;
  onClick: () => void;
  icon: React.ReactNode;
  label: string;
  count?: number;
}) {
  return (
    <button
      onClick={onClick}
      aria-selected={active}
      className={`-mb-px flex items-center gap-1.5 border-b-2 px-2 py-2 text-xs ${
        active
          ? "border-accent text-zinc-100"
          : "border-transparent text-zinc-500 hover:text-zinc-300"
      }`}
    >
      {icon}
      {label}
      {count !== undefined && (
        <span className="tnum rounded-full bg-ink-600 px-1.5 text-[10px] text-zinc-500">
          {count}
        </span>
      )}
    </button>
  );
}

// ─── Definition tab ──────────────────────────────────────────────────────────

function DefinitionTab({
  wf,
  scopeTask,
  onOpenGraph,
}: {
  wf: WorkflowInfo;
  scopeTask: { id: string; title: string } | null;
  onOpenGraph: () => void;
}) {
  return (
    <div className="flex max-w-3xl flex-col gap-4">
      {/* Run-scope inheritance banner */}
      <div
        className={`flex items-start gap-2 rounded-lg border px-3 py-2.5 ${
          scopeTask ? "border-accent/30 bg-accent/10" : "border-ink-600 bg-ink-800"
        }`}
      >
        {scopeTask ? (
          <Layers size={13} className="mt-0.5 shrink-0 text-accent" />
        ) : (
          <Minus size={13} className="mt-0.5 shrink-0 text-zinc-600" />
        )}
        <p className="min-w-0 text-[11px] leading-relaxed text-zinc-400">
          Node agents spawn with{" "}
          <span className={`font-mono ${scopeTask ? "text-accent" : "text-zinc-500"}`}>
            task_id={scopeTask ? scopeTask.id : "null"}
          </span>
          {scopeTask && (
            <>
              {" "}
              — <span className="text-zinc-300">{scopeTask.title}</span>
            </>
          )}
          . Each keeps its own agent id and worktree — attribution stays per-agent.
        </p>
      </div>

      {/* Graph */}
      <section>
        <div className="mb-2 flex items-center gap-2">
          <h2 className="text-[10px] font-semibold uppercase tracking-wider text-zinc-600">
            Execution graph
          </h2>
        </div>
        <div className="flex items-center gap-3 rounded-lg border border-ink-600 bg-ink-800 px-3 py-2.5">
          <Network size={14} className="shrink-0 text-zinc-600" />
          <p className="min-w-0 flex-1 truncate text-[11px] text-zinc-500">
            <span className="tnum">{wf.nodes.length}</span> nodes ·{" "}
            <span className="tnum">{wf.edges.length}</span> edges — loops render as
            amber back-edges
          </p>
          <button
            onClick={onOpenGraph}
            className="shrink-0 rounded-md border border-ink-500 px-2.5 py-1 text-[11px] text-zinc-300 hover:bg-ink-600"
          >
            Open graph
          </button>
        </div>
      </section>

      {/* Node table */}
      <section>
        <h2 className="mb-2 text-[10px] font-semibold uppercase tracking-wider text-zinc-600">
          Nodes
        </h2>
        <div className="overflow-hidden rounded-lg border border-ink-600">
          <table className="w-full text-left text-xs">
            <thead className="bg-ink-800 text-[10px] uppercase tracking-wider text-zinc-600">
              <tr>
                <th className="px-3 py-2 font-semibold">Node</th>
                <th className="px-3 py-2 font-semibold">Profile</th>
                <th className="px-3 py-2 font-semibold">Provider</th>
                <th className="px-3 py-2 font-semibold">Prompt</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-ink-700">
              {wf.nodes.map((n) => {
                const provider = nodeProvider(n);
                return (
                  <tr key={n.id} className="bg-ink-800/40">
                    <td className="whitespace-nowrap px-3 py-2">
                      <span className="flex items-center gap-1.5">
                        <span
                          title={n.id}
                          className="max-w-[160px] truncate font-mono text-zinc-200"
                        >
                          {n.id}
                        </span>
                        {n.id === wf.entry && (
                          <span className="shrink-0 rounded bg-ink-600 px-1 text-[9px] uppercase tracking-wide text-zinc-500">
                            entry
                          </span>
                        )}
                      </span>
                    </td>
                    <td className="whitespace-nowrap px-3 py-2 text-zinc-300">
                      <span title={nodeProfile(n)} className="block max-w-[140px] truncate">
                        {nodeProfile(n)}
                      </span>
                    </td>
                    <td className="whitespace-nowrap px-3 py-2">
                      {provider ? (
                        <span className="text-zinc-300">{providerTitle(provider)}</span>
                      ) : (
                        <span className="text-zinc-600">run default</span>
                      )}
                    </td>
                    <td className="px-3 py-2">
                      <span
                        title={n.prompt}
                        className="block max-w-[380px] truncate text-zinc-500"
                      >
                        {n.prompt}
                      </span>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      </section>
    </div>
  );
}

// ─── Runs tab ────────────────────────────────────────────────────────────────

function RunsTab({
  wf,
  runs,
  taskTitle,
}: {
  wf: WorkflowInfo;
  runs: WorkflowRunSummary[];
  /** Task title for a task id (falls back to the id for foreign workspaces). */
  taskTitle: (id: string) => string;
}) {
  if (runs.length === 0) {
    return (
      <p className="text-xs text-zinc-500">
        No runs recorded — Run executes this definition in the active workspace.
      </p>
    );
  }
  return (
    <div className="flex max-w-3xl flex-col gap-2">
      {runs.map((r) => (
        <RunCard key={r.id} wf={wf} run={r} taskTitle={taskTitle} />
      ))}
      <p className="mt-1 text-[10px] text-zinc-600">
        Showing the most recent run plus runs started this session.
      </p>
    </div>
  );
}

function RunCard({
  wf,
  run,
  taskTitle,
}: {
  wf: WorkflowInfo;
  run: WorkflowRunSummary;
  taskTitle: (id: string) => string;
}) {
  const ui = runStatusUi(run.status);
  // Node states in definition order, then any ids the definition no longer has.
  const ordered: [string, WorkflowRunSummary["node_states"][string]][] = [];
  for (const n of wf.nodes) {
    if (run.node_states[n.id]) ordered.push([n.id, run.node_states[n.id]]);
  }
  for (const [id, st] of Object.entries(run.node_states)) {
    if (!wf.nodes.some((n) => n.id === id)) ordered.push([id, st]);
  }

  return (
    <div className="rounded-lg border border-ink-600 bg-ink-800 px-3 py-2.5">
      <div className="flex items-center gap-2">
        <span className={`h-2 w-2 shrink-0 rounded-full ${ui.dot}`} />
        <span title={run.id} className="min-w-0 truncate font-mono text-xs text-zinc-200">
          {run.id}
        </span>
        <span
          className={`shrink-0 rounded-full px-2 py-0.5 text-[10px] font-medium ${ui.chip}`}
        >
          {ui.label}
        </span>
        <span className="flex-1" />
        <span
          title={absoluteTime(run.started_at)}
          className="tnum shrink-0 text-[10px] text-zinc-600"
        >
          {relativeTime(run.started_at)}
        </span>
        {run.ended_at != null && (
          <span className="tnum shrink-0 text-[10px] text-zinc-600">
            {duration(run.started_at, run.ended_at)}
          </span>
        )}
      </div>

      {/* Task scope */}
      <div className="mt-1.5 flex items-center gap-1.5 text-[10px]">
        {run.task_id ? (
          <span className="flex min-w-0 items-center gap-1 rounded bg-accent/10 px-1.5 py-0.5 text-accent">
            <Layers size={10} className="shrink-0" />
            <span title={run.task_id} className="truncate">
              Task: {taskTitle(run.task_id)}
            </span>
          </span>
        ) : (
          <span className="flex items-center gap-1 rounded bg-ink-600 px-1.5 py-0.5 text-zinc-500">
            <Minus size={10} className="shrink-0" />
            Uncategorized
          </span>
        )}
      </div>

      {/* Per-node states + spawned agents */}
      {ordered.length > 0 ? (
        <div className="mt-2 flex flex-wrap items-center gap-1.5">
          {ordered.map(([id, st]) => {
            const stUi = runStatusUi(st.status);
            return (
              <span
                key={id}
                className="flex items-center gap-1.5 rounded border border-ink-700 bg-ink-900/60 px-1.5 py-1"
              >
                <span className={`h-1.5 w-1.5 shrink-0 rounded-full ${stUi.dot}`} />
                <span title={id} className="max-w-[120px] truncate font-mono text-[10px] text-zinc-400">
                  {id}
                </span>
                {st.iteration > 1 && (
                  <span className="tnum text-[9px] text-amber">×{st.iteration}</span>
                )}
                {st.agent_id && (
                  <button
                    onClick={() => useStore.getState().openDiff(st.agent_id as string)}
                    title={`Open diff for agent ${st.agent_id}`}
                    className="max-w-[110px] truncate rounded font-mono text-[10px] text-accent hover:underline"
                  >
                    {st.agent_id}
                  </button>
                )}
              </span>
            );
          })}
        </div>
      ) : (
        <p className="mt-2 text-[10px] text-zinc-600">No node states yet.</p>
      )}

      {run.error && (
        <p className="mt-2 break-words text-[10px] text-red-400">{run.error}</p>
      )}
    </div>
  );
}
