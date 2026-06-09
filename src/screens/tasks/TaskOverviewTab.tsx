import { useEffect, useRef, useState } from "react";
import { GitBranch, Plus } from "lucide-react";
import { api, type TaskDetail, type TaskStatus } from "../../api";
import { useStore } from "../../store";
import { providerTitle } from "../../lib/providerLabel";
import { agentLabel } from "../../lib/agentLabel";
import { profileMeta, displayRole } from "../../lib/profiles";
import { StatusBadge } from "../../components/StatusBadge";
import { fmtUnix, memberWireStatus, middleTruncate, openAgent } from "./lib";

/** The Task's lifecycle states (ported from TaskReviewDrawer). `archived`
 *  preserves membership as a read-only view — it never kills agents. */
const STATUS_OPTIONS: { value: TaskStatus; label: string }[] = [
  { value: "open", label: "Open" },
  { value: "in_review", label: "In review" },
  { value: "done", label: "Done" },
  { value: "archived", label: "Archived" },
];

/**
 * Overview: identity + lifecycle + the member roster. Lifecycle select and the
 * two-step delete (members are DEMOTED to Uncategorized, never killed) are
 * ported from TaskReviewDrawer; the members table routes clicks to the agent's
 * terminal in the Agents section.
 */
export function TaskOverviewTab({
  taskId,
  detail,
  reload,
}: {
  taskId: string;
  detail: TaskDetail;
  reload: () => void;
}) {
  const showSnackbar = useStore((s) => s.showSnackbar);
  const setLaunchOpen = useStore((s) => s.setLaunchOpen);
  const connected = useStore((s) => s.connected);
  const terminalStatuses = useStore((s) => s.terminalStatuses);
  const frames = useStore((s) => s.frames);
  const reviewedFrames = useStore((s) => s.reviewedFrames);

  const [savingStatus, setSavingStatus] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const confirmTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Don't leak the delete-confirm reset timer; disarm when the task switches
  // so task A's armed delete never fires against task B.
  useEffect(() => {
    setConfirmDelete(false);
    return () => {
      if (confirmTimer.current) clearTimeout(confirmTimer.current);
    };
  }, [taskId]);

  const task = detail.task;
  const members = detail.agents;
  const runs = detail.runs;
  const totalDirty = members.reduce((n, a) => n + a.dirty_count, 0);
  const reviewedCount = members.filter((a) => reviewedFrames[a.agent_id]).length;

  const setStatus = async (status: TaskStatus) => {
    if (savingStatus) return;
    setSavingStatus(true);
    try {
      const err = await api.updateTask(taskId, { status });
      if (err) showSnackbar({ type: "error", message: `Couldn't update task: ${err}` });
      reload();
    } finally {
      setSavingStatus(false);
    }
  };

  /** Two-step delete: first click arms (auto-disarms after 3s), second commits.
   *  Delete demotes member agents to Uncategorized — it never kills them. */
  const onDelete = async () => {
    if (deleting) return;
    if (!confirmDelete) {
      setConfirmDelete(true);
      if (confirmTimer.current) clearTimeout(confirmTimer.current);
      confirmTimer.current = setTimeout(() => setConfirmDelete(false), 3000);
      return;
    }
    if (confirmTimer.current) clearTimeout(confirmTimer.current);
    setDeleting(true);
    try {
      const err = await api.deleteTask(taskId);
      if (err) {
        showSnackbar({ type: "error", message: `Couldn't delete task: ${err}` });
        setConfirmDelete(false);
        return;
      }
      showSnackbar({
        type: "success",
        message: "Task deleted — agents demoted to Uncategorized",
      });
      // The row is gone: clear the selection so the screen returns to its
      // select-a-task hint (the sidebar list drops the row on its next poll).
      useStore.getState().clearSelectedTask();
    } finally {
      setDeleting(false);
    }
  };

  /** Role/profile label: daemon-reported role first (covers assigned workers),
   *  falling back to the launch frame's profile, else "—". */
  const roleLabel = (a: { role?: string | null; agent_id: string }): string => {
    const role = displayRole(
      a.role ?? frames.find((f) => f.terminalId === a.agent_id)?.agentProfile,
    );
    return role ? profileMeta(role).label : "—";
  };

  return (
    <div className="h-full space-y-5 overflow-y-auto p-4">
      {/* Description */}
      <section>
        <h3 className="mb-1.5 text-[10px] font-semibold uppercase tracking-wider text-zinc-600">
          Description
        </h3>
        {task.description ? (
          <p className="max-w-2xl text-xs leading-relaxed text-zinc-400">
            {task.description}
          </p>
        ) : (
          <p className="text-xs text-zinc-600">No description.</p>
        )}
      </section>

      {/* Meta + lifecycle */}
      <section className="grid max-w-2xl grid-cols-[110px_minmax(0,1fr)] items-center gap-x-3 gap-y-1.5 text-xs">
        <span className="text-zinc-600">Status</span>
        <span>
          <select
            value={(task.status as TaskStatus) ?? "open"}
            onChange={(e) => void setStatus(e.target.value as TaskStatus)}
            disabled={savingStatus || !connected}
            aria-label="Task status"
            className="rounded border border-ink-500 bg-ink-700 px-1.5 py-0.5 text-[11px] text-zinc-300 disabled:opacity-50"
          >
            {STATUS_OPTIONS.map((o) => (
              <option key={o.value} value={o.value}>
                {o.label}
              </option>
            ))}
          </select>
        </span>
        <span className="text-zinc-600">Workspace</span>
        <span
          title={task.workspace_root}
          className="whitespace-nowrap font-mono text-[11px] text-zinc-400"
        >
          {middleTruncate(task.workspace_root, 56)}
        </span>
        <span className="text-zinc-600">Created</span>
        <span className="tnum font-mono text-[11px] text-zinc-400">
          {fmtUnix(task.created_at)}
        </span>
        <span className="text-zinc-600">Updated</span>
        <span className="tnum font-mono text-[11px] text-zinc-400">
          {fmtUnix(task.updated_at)}
        </span>
        <span className="text-zinc-600">Task ID</span>
        <span
          title={task.id}
          className="truncate whitespace-nowrap font-mono text-[11px] text-zinc-500"
        >
          {task.id}
        </span>
      </section>

      {/* Rollup chips — the Task-level aggregates */}
      <section className="flex flex-wrap items-center gap-1.5">
        <span className="tnum rounded bg-ink-600 px-1.5 py-0.5 text-[10px] text-zinc-400">
          {members.length} agent{members.length === 1 ? "" : "s"}
        </span>
        <span
          className={`tnum rounded px-1.5 py-0.5 text-[10px] ${
            totalDirty > 0 ? "bg-amber/20 text-amber" : "bg-ink-600 text-zinc-400"
          }`}
        >
          {totalDirty} dirty file{totalDirty === 1 ? "" : "s"}
        </span>
        <span className="tnum rounded bg-ink-600 px-1.5 py-0.5 text-[10px] text-zinc-400">
          {reviewedCount} reviewed
        </span>
        <span className="tnum rounded bg-ink-600 px-1.5 py-0.5 text-[10px] text-zinc-400">
          {runs.length} run{runs.length === 1 ? "" : "s"}
        </span>
      </section>

      {/* Member agents — attribution stays per Agent ID; click → terminal */}
      <section>
        <h3 className="mb-1.5 text-[10px] font-semibold uppercase tracking-wider text-zinc-600">
          Member agents
        </h3>
        {members.length === 0 ? (
          <div className="flex max-w-2xl flex-col items-center gap-3 rounded-lg border border-ink-600 py-10">
            <p className="text-xs text-zinc-500">No agents in this task</p>
            <button
              onClick={() => setLaunchOpen(true, taskId)}
              disabled={!connected}
              className="flex items-center gap-1.5 rounded-md border border-ink-500 px-3 py-1.5 text-xs text-zinc-300 hover:bg-ink-600 disabled:cursor-default disabled:opacity-40"
            >
              <Plus size={13} />
              Launch one
            </button>
          </div>
        ) : (
          <div className="max-w-3xl overflow-hidden rounded-lg border border-ink-600">
            <div className="grid grid-cols-[110px_minmax(0,1fr)_110px_120px_56px] gap-2 border-b border-ink-600 bg-ink-800 px-3 py-1.5 text-[10px] font-semibold uppercase tracking-wider text-zinc-600">
              <span>Status</span>
              <span>Agent</span>
              <span>Profile</span>
              <span>Provider</span>
              <span className="text-right">Dirty</span>
            </div>
            {members.map((a) => (
              <button
                key={a.agent_id}
                onClick={() => openAgent(a.agent_id)}
                title={`${a.agent_id}${a.branch ? ` · ${a.branch}` : ""} — open terminal`}
                className="grid w-full grid-cols-[110px_minmax(0,1fr)_110px_120px_56px] items-center gap-2 border-b border-ink-700/50 px-3 py-2 text-left last:border-b-0 hover:bg-ink-700/60"
              >
                <span className="min-w-0 truncate">
                  <StatusBadge status={memberWireStatus(a, terminalStatuses)} />
                </span>
                <span className="min-w-0">
                  <span className="block truncate whitespace-nowrap font-mono text-[11px] text-zinc-200">
                    {agentLabel(a.agent_id)}
                  </span>
                  {a.branch && (
                    <span className="flex items-center gap-1 font-mono text-[10px] text-zinc-600">
                      <GitBranch size={9} className="shrink-0" />
                      <span className="truncate whitespace-nowrap">{a.branch}</span>
                    </span>
                  )}
                </span>
                <span
                  title={roleLabel(a)}
                  className="truncate whitespace-nowrap text-[11px] text-zinc-400"
                >
                  {roleLabel(a)}
                </span>
                <span className="truncate whitespace-nowrap text-[11px] text-zinc-400">
                  {providerTitle(a.provider ?? "")}
                </span>
                <span className="text-right">
                  {a.dirty_count > 0 ? (
                    <span className="tnum rounded bg-amber/20 px-1 text-[10px] font-medium text-amber">
                      {a.dirty_count}
                    </span>
                  ) : (
                    <span className="text-[10px] text-zinc-700">—</span>
                  )}
                </span>
              </button>
            ))}
          </div>
        )}
      </section>

      {/* Lifecycle exit: delete demotes members to Uncategorized (two-step). */}
      <section className="border-t border-ink-700 pt-4">
        <button
          onClick={() => void onDelete()}
          disabled={deleting || !connected}
          className="rounded-md border border-rose-500/50 px-3 py-1.5 text-xs text-rose-300 hover:bg-rose-500/10 disabled:cursor-default disabled:opacity-40"
        >
          {deleting
            ? "Deleting…"
            : confirmDelete
              ? "Really delete? Agents become Uncategorized."
              : "Delete task"}
        </button>
      </section>
    </div>
  );
}
