import { useCallback, useEffect, useRef, useState } from "react";
import { X, GitBranch } from "lucide-react";
import { api, type TaskDetail, type TaskStatus, type WorkflowInfo } from "../api";
import { useStore } from "../store";
import { providerTitle } from "../lib/providerLabel";
import { StatusBadge } from "./StatusBadge";

/** The Task's lifecycle states, as offered in the header select. */
const STATUS_OPTIONS: { value: TaskStatus; label: string }[] = [
  { value: "open", label: "Open" },
  { value: "in_review", label: "In review" },
  { value: "done", label: "Done" },
  { value: "archived", label: "Archived" },
];

/** Run status → dot classes (running pulses; matches the app's lifecycle hues). */
function runDotClass(status: string): string {
  switch (status) {
    case "running":
      return "bg-teal-400 animate-pulse";
    case "completed":
      return "bg-emerald-400";
    case "failed":
      return "bg-rose-400";
    default:
      return "bg-zinc-600";
  }
}

function fmtRunTime(unixSecs: number | null): string {
  if (!unixSecs) return "—";
  try {
    return new Date(unixSecs * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  } catch {
    return "—";
  }
}

/**
 * Task Review: the aggregate review surface for one Task — a non-disruptive
 * right-side drawer (terminals stay visible). A Task groups agents working
 * toward one goal; attribution stays per-agent (Agent ID) — this drawer owns
 * the lifecycle + rollups, and hands off to the per-agent diff for the deltas.
 * Archiving preserves membership; deleting demotes member agents to
 * "Uncategorized" (it never kills agents).
 */
export function TaskReviewDrawer() {
  const taskReviewId = useStore((s) => s.taskReviewId);
  const closeTaskReview = useStore((s) => s.closeTaskReview);
  const openDiff = useStore((s) => s.openDiff);
  const frames = useStore((s) => s.frames);
  const rustPtySessions = useStore((s) => s.rustPtySessions);
  const setActiveFrameGuarded = useStore((s) => s.setActiveFrameGuarded);
  const reopenRustPty = useStore((s) => s.reopenRustPty);
  const workspaceDir = useStore((s) => s.workspaceDir);
  const terminalStatuses = useStore((s) => s.terminalStatuses);

  const [detail, setDetail] = useState<TaskDetail | null>(null);
  const [workflows, setWorkflows] = useState<WorkflowInfo[]>([]);
  const [selectedWorkflow, setSelectedWorkflow] = useState("");
  const [running, setRunning] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const confirmTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const showSnackbar = useStore((s) => s.showSnackbar);

  const load = useCallback(async () => {
    if (!taskReviewId) return;
    const d = await api.getTaskDetail(taskReviewId);
    // The drawer is non-modal: the user can switch tasks while a fetch is in
    // flight. Drop stale responses so task A's payload (and its armed delete
    // button) never renders over task B.
    if (useStore.getState().taskReviewId !== taskReviewId) return;
    setDetail(d);
  }, [taskReviewId]);

  // Live: fetch on open + poll every 2s so rollups track the agents in real time.
  useEffect(() => {
    if (!taskReviewId) return;
    setDetail(null);
    setConfirmDelete(false);
    load();
    api.listWorkflows().then(setWorkflows);
    const t = setInterval(load, 2000);
    return () => clearInterval(t);
  }, [taskReviewId, load]);

  // Esc closes the drawer — unless the DiffView (opened from our own Diff
  // button) is on top: its handler owns that keypress, and closing both at
  // once would dump the user out of the review flow.
  useEffect(() => {
    if (!taskReviewId) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !useStore.getState().diffTerminalId) closeTaskReview();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [taskReviewId, closeTaskReview]);

  // Don't leak the delete-confirm reset timer across unmounts.
  useEffect(
    () => () => {
      if (confirmTimer.current) clearTimeout(confirmTimer.current);
    },
    [],
  );

  /** Focus the member agent's terminal: live frame if present, else reattach
   *  the detached-but-running session via its surviving meta. */
  const focusAgent = useCallback(
    (terminalId: string) => {
      const frame = frames.find((f) => f.terminalId === terminalId);
      if (frame) {
        setActiveFrameGuarded(frame.key);
        return;
      }
      const meta = Object.values(rustPtySessions).find((m) => m.terminalId === terminalId);
      if (meta) reopenRustPty(meta.ptySessionId, { focus: true });
    },
    [frames, rustPtySessions, setActiveFrameGuarded, reopenRustPty],
  );

  const setStatus = useCallback(
    async (status: TaskStatus) => {
      if (!taskReviewId) return;
      const err = await api.updateTask(taskReviewId, { status });
      if (err) showSnackbar({ type: "error", message: `Couldn't update task: ${err}` });
      load();
    },
    [taskReviewId, load, showSnackbar],
  );

  /** Remove from task — membership only; the agent keeps running, uncategorized. */
  const removeAgent = useCallback(
    async (terminalId: string) => {
      const err = await api.assignAgentTask(terminalId, null);
      if (err) showSnackbar({ type: "error", message: `Couldn't remove agent: ${err}` });
      load();
    },
    [load, showSnackbar],
  );

  const runWorkflow = useCallback(async () => {
    if (!detail || !selectedWorkflow) return;
    setRunning(true);
    try {
      const r = await api.runWorkflow(
        selectedWorkflow,
        detail.task.workspace_root || workspaceDir,
        detail.task.id,
      );
      if (r.error) showSnackbar({ type: "error", message: `Workflow failed to start: ${r.error}` });
      load();
    } finally {
      setRunning(false);
    }
  }, [detail, selectedWorkflow, workspaceDir, load, showSnackbar]);

  /** Two-step delete: first click arms (auto-disarms after 3s), second commits. */
  const onDelete = useCallback(async () => {
    if (!taskReviewId) return;
    if (!confirmDelete) {
      setConfirmDelete(true);
      if (confirmTimer.current) clearTimeout(confirmTimer.current);
      confirmTimer.current = setTimeout(() => setConfirmDelete(false), 3000);
      return;
    }
    if (confirmTimer.current) clearTimeout(confirmTimer.current);
    // Only close on actual success — a swallowed failure would close the
    // drawer while the task silently survives in the sidebar.
    const err = await api.deleteTask(taskReviewId);
    if (err) {
      showSnackbar({ type: "error", message: `Couldn't delete task: ${err}` });
      setConfirmDelete(false);
      return;
    }
    closeTaskReview();
  }, [taskReviewId, confirmDelete, closeTaskReview, showSnackbar]);

  if (!taskReviewId) return null;

  const task = detail?.task ?? null;
  const agents = detail?.agents ?? [];
  const runs = detail?.runs ?? [];
  const totalDirty = agents.reduce((sum, a) => sum + a.dirty_count, 0);

  return (
    <aside className="fixed right-0 top-12 bottom-0 z-40 flex w-[420px] flex-col border-l border-t border-ink-600 bg-ink-900 shadow-2xl">
      {/* Header: title + lifecycle select + close */}
      <div className="flex items-center justify-between border-b border-ink-600 bg-ink-800 px-3 py-2.5">
        <span className="min-w-0 truncate text-sm font-medium text-zinc-100">
          {task?.title ?? "Task"}
        </span>
        <div className="flex shrink-0 items-center gap-2">
          <select
            value={(task?.status as TaskStatus) ?? "open"}
            onChange={(e) => setStatus(e.target.value as TaskStatus)}
            className="rounded border border-ink-500 bg-ink-700 px-1.5 py-0.5 text-[11px] text-zinc-300"
            aria-label="Task status"
          >
            {STATUS_OPTIONS.map((o) => (
              <option key={o.value} value={o.value}>
                {o.label}
              </option>
            ))}
          </select>
          <button
            onClick={closeTaskReview}
            title="Close (Esc)"
            aria-label="Close task review"
            className="rounded p-0.5 text-zinc-500 hover:text-zinc-200"
          >
            <X size={16} />
          </button>
        </div>
      </div>

      <div className="min-h-0 flex-1 space-y-4 overflow-y-auto p-3">
        {/* Description */}
        {task?.description && <p className="text-xs text-zinc-500">{task.description}</p>}

        {/* Rollup strip: the Task-level aggregates */}
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="rounded bg-ink-700 px-1.5 py-0.5 text-[10px] text-zinc-400">
            {agents.length} agent{agents.length === 1 ? "" : "s"}
          </span>
          <span className="rounded bg-ink-700 px-1.5 py-0.5 text-[10px] text-zinc-400">
            {totalDirty} dirty file{totalDirty === 1 ? "" : "s"}
          </span>
          <span className="rounded bg-ink-700 px-1.5 py-0.5 text-[10px] text-zinc-400">
            {runs.length} run{runs.length === 1 ? "" : "s"}
          </span>
        </div>

        {/* Member agents — attribution stays per-agent; diffs are per Agent ID */}
        <section>
          <h3 className="mb-1.5 text-[10px] font-semibold uppercase tracking-wide text-zinc-600">
            Agents
          </h3>
          {agents.length === 0 ? (
            <p className="text-[11px] text-zinc-600">No agents in this task yet.</p>
          ) : (
            <div className="space-y-1.5">
              {agents.map((a) => (
                <div
                  key={a.agent_id}
                  className="rounded-lg border border-ink-600 bg-ink-800 px-2.5 py-2"
                >
                  <div className="flex items-center gap-2">
                    <span className="truncate text-[13px] font-medium text-zinc-100">
                      {providerTitle(a.provider ?? "")}
                    </span>
                    {a.dirty_count > 0 && (
                      <span className="rounded bg-amber/20 px-1 text-[9px] font-medium text-amber">
                        {a.dirty_count} dirty
                      </span>
                    )}
                    <span className="ml-auto shrink-0">
                      {/* Prefer the live inferred status; the detail's snapshot is the fallback. */}
                      <StatusBadge status={terminalStatuses[a.agent_id] ?? a.status ?? undefined} />
                    </span>
                  </div>
                  {a.branch && (
                    <div className="mt-0.5 flex items-center gap-1 font-mono text-[10px] text-zinc-500">
                      <GitBranch size={10} className="shrink-0" />
                      <span className="truncate">{a.branch}</span>
                    </div>
                  )}
                  <div className="mt-1.5 flex items-center gap-2">
                    <button
                      onClick={() => focusAgent(a.agent_id)}
                      title="Focus this agent's terminal"
                      className="rounded border border-ink-500 px-2 py-0.5 text-[11px] text-zinc-300 hover:bg-ink-700"
                    >
                      Focus
                    </button>
                    {a.dirty_count > 0 && (
                      <button
                        onClick={() => openDiff(a.agent_id)}
                        title="Review this agent's changes"
                        className="rounded border border-teal-600/50 px-2 py-0.5 text-[11px] text-teal-300 hover:bg-teal-600/10"
                      >
                        Diff
                      </button>
                    )}
                    <button
                      onClick={() => removeAgent(a.agent_id)}
                      title="Remove from task (agent keeps running)"
                      className="ml-auto text-[11px] text-zinc-600 hover:text-rose-400"
                    >
                      Remove
                    </button>
                  </div>
                </div>
              ))}
            </div>
          )}
        </section>

        {/* Workflow runs attached to this task */}
        {runs.length > 0 && (
          <section>
            <h3 className="mb-1.5 text-[10px] font-semibold uppercase tracking-wide text-zinc-600">
              Runs
            </h3>
            <ul className="space-y-1">
              {runs.map((r) => (
                <li key={r.id} className="flex items-center gap-2 text-[11px] text-zinc-400">
                  <span className={`h-2 w-2 shrink-0 rounded-full ${runDotClass(r.status)}`} />
                  <span className="truncate text-zinc-300">{r.workflow_name}</span>
                  <span className="ml-auto shrink-0 font-mono text-[9px] tabular-nums text-zinc-600">
                    {fmtRunTime(r.started_at)}
                  </span>
                </li>
              ))}
            </ul>
          </section>
        )}

        {/* Run a workflow inside this task: its node agents join the membership */}
        <section>
          <h3 className="mb-1.5 text-[10px] font-semibold uppercase tracking-wide text-zinc-600">
            Run workflow in this task
          </h3>
          <div className="flex items-center gap-2">
            <select
              value={selectedWorkflow}
              onChange={(e) => setSelectedWorkflow(e.target.value)}
              className="min-w-0 flex-1 rounded-lg border border-ink-500 bg-ink-700 px-2 py-1.5 text-[12px] text-zinc-200"
              aria-label="Workflow to run"
            >
              <option value="">Pick a workflow…</option>
              {workflows.map((w) => (
                <option key={w.name} value={w.name}>
                  {w.name}
                </option>
              ))}
            </select>
            <button
              onClick={runWorkflow}
              disabled={!selectedWorkflow || running || !detail}
              className="rounded-lg bg-primary px-3 py-1.5 text-sm font-medium text-white hover:bg-primary-hover disabled:opacity-50"
            >
              {running ? "Starting…" : "Run"}
            </button>
          </div>
        </section>
      </div>

      {/* Footer: lifecycle exits. Archive preserves membership; delete demotes
          member agents to Uncategorized — neither ever kills an agent. */}
      <div className="flex items-center gap-2 border-t border-ink-600 p-3">
        {task?.status !== "archived" && (
          <button
            onClick={() => setStatus("archived")}
            className="rounded-lg border border-ink-500 px-3 py-1.5 text-sm text-zinc-400 hover:text-zinc-200"
          >
            Archive
          </button>
        )}
        <button
          onClick={onDelete}
          className="rounded-lg border border-rose-500/50 px-3 py-1.5 text-sm text-rose-300 hover:bg-rose-500/10"
        >
          {confirmDelete ? "Really delete? Agents become Uncategorized." : "Delete"}
        </button>
      </div>
    </aside>
  );
}
