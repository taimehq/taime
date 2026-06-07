import { useEffect, useRef, useState } from "react";
import { Layers } from "lucide-react";
import { useStore, type TaskTab } from "../store";
import { uiStatus } from "../lib/agentStatus";
import { useTaskDetail, memberWireStatus } from "./tasks/lib";
import { TaskOverviewTab } from "./tasks/TaskOverviewTab";
import { TaskReviewTab } from "./tasks/TaskReviewTab";
import { TaskActivityTab } from "./tasks/TaskActivityTab";

/** The three task-detail tabs. `overview`/`review` are deep-linkable (the
 *  store's TaskTab); `activity` is local-only. */
type DetailTab = TaskTab | "activity";

/** Last tab per task, module-local: navigating away and back must not lose the
 *  open tab (safe context switching) without widening the store's surface. */
let lastTab: { taskId: string; tab: DetailTab } | null = null;

/**
 * The Tasks section. The sidebar owns the task LIST; this screen is the detail
 * for store.selectedTaskId — Overview (lifecycle + members), Review (the
 * aggregate lens over per-agent worktree diffs), Activity (task-scoped feed).
 */
export function TasksScreen() {
  const selectedTaskId = useStore((s) => s.selectedTaskId);

  if (!selectedTaskId) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-2 px-6 text-center">
        <Layers size={28} className="text-zinc-700" />
        <p className="text-sm text-zinc-300">Select a task</p>
        <p className="max-w-sm text-xs text-zinc-500">
          Tasks group agents and aggregate review — pick one in the sidebar, or
          create one with the + button.
        </p>
      </div>
    );
  }

  return <TaskDetailView taskId={selectedTaskId} />;
}

const TABS: { id: DetailTab; label: string }[] = [
  { id: "overview", label: "Overview" },
  { id: "review", label: "Review" },
  { id: "activity", label: "Activity" },
];

function TaskDetailView({ taskId }: { taskId: string }) {
  const connected = useStore((s) => s.connected);
  const taskInitialTab = useStore((s) => s.taskInitialTab);
  const clearTaskInitialTab = useStore((s) => s.clearTaskInitialTab);
  const terminalStatuses = useStore((s) => s.terminalStatuses);
  const { detail, loaded, reload } = useTaskDetail(taskId);

  const [tab, setTabState] = useState<DetailTab>(() =>
    lastTab?.taskId === taskId ? lastTab.tab : "overview",
  );
  const setTab = (t: DetailTab) => {
    lastTab = { taskId, tab: t };
    setTabState(t);
  };

  // Switching tasks resets to Overview — unless a deep-link is pending; that
  // effect runs AFTER this one in the same commit and wins (the store flag is
  // still set when this one checks it).
  const prevTask = useRef(taskId);
  useEffect(() => {
    if (prevTask.current === taskId) return;
    prevTask.current = taskId;
    if (useStore.getState().taskInitialTab === null) {
      lastTab = { taskId, tab: "overview" };
      setTabState("overview");
    }
  }, [taskId]);

  // One-shot deep link (selectTask(id, "review")): consume the tab, clear it.
  useEffect(() => {
    if (taskInitialTab === null) return;
    lastTab = { taskId, tab: taskInitialTab };
    setTabState(taskInitialTab);
    clearTaskInitialTab();
  }, [taskInitialTab, taskId, clearTaskInitialTab]);

  // First load (or a vanished task) — render the terse full-pane states.
  if (!detail) {
    return (
      <div className="flex h-full items-center justify-center px-6 text-center">
        <p className="text-xs text-zinc-500">
          {!connected
            ? "daemon unreachable · retrying"
            : loaded
              ? "task not found — it may have been deleted"
              : "loading task…"}
        </p>
      </div>
    );
  }

  const task = detail.task;
  const members = detail.agents;
  const totalDirty = members.reduce((n, a) => n + a.dirty_count, 0);
  const running = members.filter(
    (a) => uiStatus(memberWireStatus(a, terminalStatuses)) === "running",
  ).length;

  return (
    <div className="flex h-full min-h-0 flex-col">
      {!connected && (
        <div className="shrink-0 border-b border-amber/30 bg-amber/10 px-4 py-1 text-[11px] text-amber">
          daemon unreachable · retrying
        </div>
      )}

      {/* Task header: identity + live rollup meta. Lifecycle lives in Overview. */}
      <div className="shrink-0 border-b border-ink-600 px-4 pb-0 pt-3">
        <div className="flex items-baseline gap-2.5">
          <h1
            title={task.title}
            className="min-w-0 truncate whitespace-nowrap text-[15px] font-semibold text-zinc-100"
          >
            {task.title}
          </h1>
          <span className="shrink-0 rounded bg-ink-600 px-1.5 py-0.5 text-[9px] font-semibold uppercase tracking-wide text-zinc-400">
            {String(task.status).replace(/_/g, " ")}
          </span>
        </div>
        <p className="tnum mt-0.5 font-mono text-[10px] text-zinc-600">
          {members.length} agent{members.length === 1 ? "" : "s"}
          {running > 0 && (
            <span className="text-emerald-400"> · {running} running</span>
          )}
          {totalDirty > 0 && (
            <span className="text-amber"> · {totalDirty} dirty file{totalDirty === 1 ? "" : "s"}</span>
          )}
        </p>

        {/* Tabs */}
        <div className="mt-2 flex items-center gap-1">
          {TABS.map((t) => (
            <button
              key={t.id}
              onClick={() => setTab(t.id)}
              aria-selected={tab === t.id}
              className={`-mb-px border-b-2 px-2.5 py-1.5 text-xs ${
                tab === t.id
                  ? "border-accent text-zinc-100"
                  : "border-transparent text-zinc-500 hover:text-zinc-300"
              }`}
            >
              {t.label}
              {t.id === "review" && totalDirty > 0 && (
                <span className="tnum ml-1.5 rounded bg-amber/20 px-1 text-[10px] font-medium text-amber">
                  {totalDirty}
                </span>
              )}
            </button>
          ))}
        </div>
      </div>

      {/* Tab body */}
      <div className="min-h-0 flex-1">
        {tab === "overview" && (
          <TaskOverviewTab taskId={taskId} detail={detail} reload={reload} />
        )}
        {tab === "review" && (
          <TaskReviewTab detail={detail} reloadDetail={reload} />
        )}
        {tab === "activity" && (
          <TaskActivityTab taskId={taskId} detail={detail} />
        )}
      </div>
    </div>
  );
}
