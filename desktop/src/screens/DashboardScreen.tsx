import { useEffect, useMemo, useState } from "react";
import {
  Activity,
  Bot,
  Calendar,
  ChevronRight,
  Eye,
  FolderOpen,
  Layers,
  Plus,
  type LucideIcon,
} from "lucide-react";
import { useStore, type RustPtyMeta } from "../store";
import { api, type ScheduleInfo, type TaskInfo } from "../api";
import { uiStatus } from "../lib/agentStatus";
import { StatusBadge } from "../components/StatusBadge";
import { providerTitle } from "../lib/providerLabel";
import { pickDirectory } from "../lib/pickDirectory";

/**
 * The Dashboard: fleet overview. Stat cards (tasks / agents / review /
 * schedules), the task-card grid (in-review first), and the always-visible
 * Uncategorized agents roster. Data: daemon queries (tasks, schedules) +
 * store state (agents, statuses, dirty). The daemon never errors loudly here —
 * `connected` is the reachability signal (the queries fall back silently).
 */

// ─── Local polls (loading flags the shared hooks don't expose) ──────────────

function useDashboardTasks(workspaceDir: string | null): {
  tasks: TaskInfo[];
  loaded: boolean;
} {
  const [tasks, setTasks] = useState<TaskInfo[]>([]);
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    if (!workspaceDir) {
      setTasks([]);
      setLoaded(true);
      return;
    }
    setLoaded(false);
    let alive = true;
    const load = () =>
      api
        .listTasks(workspaceDir, false)
        .then((t) => {
          if (alive) {
            setTasks(t);
            setLoaded(true);
          }
        })
        .catch(() => {
          /* daemon down — surfaced via `connected`; next poll retries */
        });
    load();
    const timer = setInterval(load, 5000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, [workspaceDir]);

  return { tasks, loaded };
}

function useSchedules(): { schedules: ScheduleInfo[]; loaded: boolean } {
  const [schedules, setSchedules] = useState<ScheduleInfo[]>([]);
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    let alive = true;
    const load = () =>
      api
        .listSchedules()
        .then((list) => {
          if (alive) {
            setSchedules(list);
            setLoaded(true);
          }
        })
        .catch(() => {});
    load();
    const timer = setInterval(load, 5000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, []);

  return { schedules, loaded };
}

// ─── Derived rollups ─────────────────────────────────────────────────────────

interface TaskRollup {
  running: number;
  dirtyAgents: number;
  dirtyPaths: number;
}

const EMPTY_ROLLUP: TaskRollup = { running: 0, dirtyAgents: 0, dirtyPaths: 0 };

/** Display order: review needs attention first, then open, then done. */
const TASK_GROUPS: { status: string; label: string }[] = [
  { status: "in_review", label: "In review" },
  { status: "open", label: "Open" },
  { status: "done", label: "Done" },
];

const AGENT_FILTERS = [
  { id: "all", label: "All" },
  { id: "running", label: "Running" },
  { id: "blocked", label: "Blocked" },
  { id: "done", label: "Done" },
] as const;
type AgentFilter = (typeof AGENT_FILTERS)[number]["id"];

const plural = (n: number) => (n === 1 ? "" : "s");

// ─── Screen ──────────────────────────────────────────────────────────────────

export function DashboardScreen() {
  const workspaceDir = useStore((s) => s.workspaceDir);
  const connected = useStore((s) => s.connected);
  const rustPtySessions = useStore((s) => s.rustPtySessions);
  const terminalStatuses = useStore((s) => s.terminalStatuses);
  const dirty = useStore((s) => s.dirty);
  const reviewedFrames = useStore((s) => s.reviewedFrames);
  const setLaunchOpen = useStore((s) => s.setLaunchOpen);
  const selectTask = useStore((s) => s.selectTask);

  const { tasks, loaded: tasksLoaded } = useDashboardTasks(workspaceDir);
  const { schedules, loaded: schedulesLoaded } = useSchedules();

  const [agentFilter, setAgentFilter] = useState<AgentFilter>("all");
  const [draft, setDraft] = useState<string | null>(null); // null = closed
  const [creating, setCreating] = useState(false);

  // Running first (most recent first), then exited — same order as the sidebar.
  const agents = useMemo(
    () =>
      Object.values(rustPtySessions).sort((a, b) => {
        if (a.status !== b.status) return a.status === "running" ? -1 : 1;
        return b.startedAt - a.startedAt;
      }),
    [rustPtySessions],
  );

  const activeTasks = tasks.filter(
    (t) => t.status === "open" || t.status === "in_review",
  );
  const runningAgents = agents.filter((a) => a.status === "running");
  // "Needs review" = agents with dirty changes the user hasn't acknowledged
  // (the same predicate the context-switch guard runs on).
  const needsReview = Object.keys(dirty).filter(
    (id) => dirty[id].count > 0 && !reviewedFrames[id],
  ).length;
  const uncategorized = agents.filter((a) => !a.taskId);

  // Per-task rollup from store-tracked members (running + unreviewed dirty).
  const taskRollups = useMemo(() => {
    const map = new Map<string, TaskRollup>();
    for (const m of agents) {
      if (!m.taskId) continue;
      const r = map.get(m.taskId) ?? { ...EMPTY_ROLLUP };
      if (m.status === "running") r.running += 1;
      const d = dirty[m.terminalId];
      if (d && d.count > 0 && !reviewedFrames[m.terminalId]) {
        r.dirtyAgents += 1;
        r.dirtyPaths += d.count;
      }
      map.set(m.taskId, r);
    }
    return map;
  }, [agents, dirty, reviewedFrames]);

  const uncatFiltered = uncategorized.filter((m) => {
    if (agentFilter === "all") return true;
    const ui = uiStatus(
      m.status === "exited" ? "EXITED" : terminalStatuses[m.terminalId],
    );
    if (agentFilter === "running") return ui === "running";
    if (agentFilter === "blocked") return ui === "blocked";
    return ui === "done" || ui === "exited"; // done
  });

  const submitNewTask = async () => {
    const title = draft?.trim();
    if (!title || !workspaceDir || creating) return;
    setCreating(true);
    try {
      const t = await api.createTask(workspaceDir, title);
      if (t) {
        setDraft(null);
        useStore.getState().selectTask(t.id);
      } else {
        useStore.getState().showSnackbar({
          type: "error",
          message: "Task create failed — daemon unreachable",
        });
      }
    } finally {
      setCreating(false);
    }
  };

  // Truly empty workspace (daemon reachable, nothing to show) → centered hint.
  const empty =
    connected && tasksLoaded && agents.length === 0 && tasks.length === 0;
  if (empty) return <EmptyWorkspace />;

  return (
    <div className="flex h-full flex-col overflow-hidden">
      {/* ── Header ───────────────────────────────────────────────────── */}
      <div className="flex shrink-0 items-center gap-3 px-6 pb-3 pt-5">
        <div className="flex min-w-0 flex-1 items-baseline gap-2">
          <h1 className="shrink-0 text-sm font-medium text-zinc-100">
            Dashboard
          </h1>
          <span className="truncate text-[11px] text-zinc-500">
            {agents.length} agent{plural(agents.length)} ·{" "}
            <span className="tnum">{runningAgents.length}</span> running
          </span>
          {!connected && (
            <span className="shrink-0 rounded bg-amber/15 px-1.5 py-0.5 text-[10px] text-amber">
              daemon unreachable · retrying
            </span>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <button
            onClick={() => setLaunchOpen(true)}
            disabled={!connected}
            title={connected ? "Launch an agent" : "Daemon unreachable"}
            className="flex items-center gap-1.5 rounded-md border border-ink-500 px-2.5 py-1 text-xs text-zinc-300 hover:bg-ink-600 disabled:cursor-default disabled:opacity-40"
          >
            <Bot size={13} className="shrink-0" />
            Launch agent
          </button>
          <button
            onClick={() => setDraft("")}
            disabled={!workspaceDir || !connected}
            title={
              workspaceDir
                ? "Create a task"
                : "Open a workspace to create tasks"
            }
            className="flex items-center gap-1.5 rounded-md bg-primary px-2.5 py-1 text-xs font-medium text-white hover:bg-primary-hover disabled:cursor-default disabled:opacity-40"
          >
            <Plus size={13} className="shrink-0" />
            New task
          </button>
        </div>
      </div>

      {/* ── Stat cards ───────────────────────────────────────────────── */}
      <div className="flex shrink-0 flex-wrap gap-2 px-6 pb-2">
        <StatCard
          label="Active tasks"
          value={!connected ? "—" : !tasksLoaded ? "…" : activeTasks.length}
          icon={Layers}
          valueClass="text-accent"
          glowClass="bg-accent"
        />
        <StatCard
          label="Running agents"
          value={runningAgents.length}
          icon={Activity}
          valueClass="text-emerald-400"
          glowClass="bg-emerald-400"
        />
        <StatCard
          label="Needs review"
          value={needsReview}
          icon={Eye}
          valueClass="text-amber"
          glowClass="bg-amber"
        />
        <StatCard
          label="Schedules"
          value={!connected ? "—" : !schedulesLoaded ? "…" : schedules.length}
          icon={Calendar}
          valueClass="text-zinc-300"
          glowClass="bg-zinc-500"
        />
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-6 pb-6">
        {/* ── Tasks ──────────────────────────────────────────────────── */}
        <section className="mt-3">
          <div className="mb-2 flex items-center gap-2">
            <SectionLabel>Tasks</SectionLabel>
            <span className="tnum text-[10px] text-zinc-600">
              · {tasks.length} total
            </span>
          </div>

          {draft !== null && (
            <input
              autoFocus
              value={draft}
              disabled={creating}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void submitNewTask();
                if (e.key === "Escape") setDraft(null);
              }}
              placeholder="Task title — Enter to create, Esc to cancel"
              className="mb-2 w-full max-w-md rounded-md border border-ink-500 bg-ink-700 px-2 py-1.5 text-xs text-zinc-200 placeholder:text-zinc-600 disabled:opacity-60"
            />
          )}

          {!tasksLoaded ? (
            <p className="py-2 text-[11px] text-zinc-600">loading…</p>
          ) : !workspaceDir ? (
            <p className="py-2 text-[11px] text-zinc-600">
              Open a workspace to scope tasks.
            </p>
          ) : tasks.length === 0 ? (
            <p className="py-2 text-[11px] text-zinc-600">
              No tasks yet — New task creates one.
            </p>
          ) : (
            TASK_GROUPS.map((g) => {
              const group = tasks
                .filter((t) => t.status === g.status)
                .sort((a, b) => b.updated_at - a.updated_at);
              if (group.length === 0) return null;
              return (
                <div key={g.status} className="mb-3">
                  <div className="mb-1.5 flex items-center gap-1.5">
                    <span className="text-[10px] font-medium uppercase tracking-wide text-zinc-600">
                      {g.label}
                    </span>
                    <span className="tnum text-[10px] text-zinc-700">
                      {group.length}
                    </span>
                  </div>
                  <div className="grid grid-cols-[repeat(auto-fill,minmax(260px,1fr))] gap-2">
                    {group.map((t) => (
                      <TaskCard
                        key={t.id}
                        task={t}
                        rollup={taskRollups.get(t.id) ?? EMPTY_ROLLUP}
                        onOpen={() => selectTask(t.id)}
                      />
                    ))}
                  </div>
                </div>
              );
            })
          )}
        </section>

        {/* ── Uncategorized agents (always visible, never error-styled) ── */}
        <section className="mt-4">
          <div className="mb-2 flex items-center gap-2">
            <SectionLabel>Uncategorized agents</SectionLabel>
            <span className="tnum text-[10px] text-zinc-600">
              · {uncategorized.length} agent{plural(uncategorized.length)}
            </span>
            <div
              role="group"
              aria-label="Filter uncategorized agents"
              className="ml-auto flex items-center gap-0.5 rounded-md border border-ink-600 bg-ink-800 p-0.5"
            >
              {AGENT_FILTERS.map((f) => (
                <button
                  key={f.id}
                  onClick={() => setAgentFilter(f.id)}
                  aria-pressed={agentFilter === f.id}
                  className={`rounded px-2 py-0.5 text-[11px] ${
                    agentFilter === f.id
                      ? "bg-ink-500 text-zinc-100"
                      : "text-zinc-500 hover:text-zinc-300"
                  }`}
                >
                  {f.label}
                </button>
              ))}
            </div>
          </div>

          {uncategorized.length === 0 ? (
            <p className="py-2 text-[11px] text-zinc-600">
              No uncategorized agents — all agents are assigned to tasks.
            </p>
          ) : (
            <div className="divide-y divide-ink-600 overflow-hidden rounded-md border border-ink-600 bg-ink-700/40">
              {uncatFiltered.length === 0 ? (
                <p className="px-3 py-3 text-center text-[11px] text-zinc-600">
                  No agents match filter.
                </p>
              ) : (
                uncatFiltered.map((m) => (
                  <UncatAgentRow key={m.ptySessionId} meta={m} />
                ))
              )}
            </div>
          )}
        </section>
      </div>
    </div>
  );
}

// ─── Pieces ──────────────────────────────────────────────────────────────────

function SectionLabel({ children }: { children: React.ReactNode }) {
  return (
    <span className="text-[10px] font-semibold uppercase tracking-wider text-zinc-500">
      {children}
    </span>
  );
}

/** Telemetry stat card: big tabular metric + label + status-tinted corner
 *  glow blob (~18% opacity, per the DS card spec) + inset machined edge. */
function StatCard({
  label,
  value,
  icon: Icon,
  valueClass,
  glowClass,
}: {
  label: string;
  value: number | string;
  icon: LucideIcon;
  valueClass: string;
  glowClass: string;
}) {
  return (
    <div className="relative min-w-[150px] flex-1 overflow-hidden rounded-md border border-ink-600 bg-ink-700 px-3 py-2.5 shadow-[inset_0_1px_0_rgba(255,255,255,0.04)]">
      <span
        aria-hidden
        className={`pointer-events-none absolute -right-4 -top-4 h-14 w-14 rounded-full opacity-[0.18] blur-xl ${glowClass}`}
      />
      <div className="flex items-center gap-2.5">
        <span className="flex h-7 w-7 shrink-0 items-center justify-center rounded bg-ink-500">
          <Icon size={14} className={valueClass} />
        </span>
        <div className="min-w-0">
          <div
            className={`tnum font-mono text-[28px] font-semibold leading-none tracking-tight ${valueClass}`}
          >
            {value}
          </div>
          <div className="mt-1 truncate text-[10px] font-semibold uppercase tracking-wider text-zinc-600">
            {label}
          </div>
        </div>
      </div>
    </div>
  );
}

/** Lifecycle chip for a task (same vocabulary as the sidebar). */
function TaskStatusChip({ status }: { status: string }) {
  const cls =
    status === "in_review"
      ? "bg-amber/20 text-amber"
      : status === "done"
        ? "bg-emerald-500/15 text-emerald-400"
        : status === "archived"
          ? "bg-ink-600 text-zinc-500"
          : "bg-ink-600 text-zinc-400"; // open
  return (
    <span
      className={`shrink-0 rounded px-1 text-[9px] font-semibold uppercase tracking-wide ${cls}`}
    >
      {status.replace(/_/g, " ")}
    </span>
  );
}

/** One task card: title, lifecycle chip, member + running counts, and the
 *  unreviewed-dirty rollup when present. Click → Tasks section (guarded). */
function TaskCard({
  task,
  rollup,
  onOpen,
}: {
  task: TaskInfo;
  rollup: TaskRollup;
  onOpen: () => void;
}) {
  return (
    <button
      onClick={onOpen}
      title={task.title}
      className="flex flex-col gap-2 rounded-md border border-ink-600 bg-ink-700 p-3 text-left shadow-[inset_0_1px_0_rgba(255,255,255,0.03)] hover:border-ink-400 hover:bg-ink-600"
    >
      <div className="flex w-full items-start justify-between gap-2">
        <span className="min-w-0 flex-1 truncate text-xs font-medium text-zinc-100">
          {task.title}
        </span>
        <TaskStatusChip status={task.status} />
      </div>
      <div className="flex items-center gap-1.5 text-[11px] text-zinc-500">
        <Bot size={11} className="shrink-0 text-zinc-600" />
        <span className="tnum">
          {task.agent_count} agent{plural(task.agent_count)}
        </span>
        {rollup.running > 0 && (
          <span className="tnum text-emerald-400">
            · {rollup.running} running
          </span>
        )}
      </div>
      {rollup.dirtyAgents > 0 && (
        <div className="flex w-full items-center gap-1.5 rounded bg-amber/10 px-2 py-1 text-[10px] text-amber">
          <Eye size={10} className="shrink-0" />
          <span className="tnum min-w-0 truncate">
            {rollup.dirtyAgents} agent{plural(rollup.dirtyAgents)} pending
            review · {rollup.dirtyPaths} path{plural(rollup.dirtyPaths)}
          </span>
        </div>
      )}
    </button>
  );
}

/** One uncategorized-agent row: status · provider · mono agent id · branch ·
 *  unreviewed-dirty chip. Click focuses (or reopens) it in Agents — guarded. */
function UncatAgentRow({ meta }: { meta: RustPtyMeta }) {
  const frame = useStore((s) =>
    s.frames.find((f) => f.ptySessionId === meta.ptySessionId),
  );
  const raw = useStore((s) =>
    meta.status === "exited" ? "EXITED" : s.terminalStatuses[meta.terminalId],
  );
  const dirtyCount = useStore((s) => {
    const d = s.dirty[meta.terminalId];
    return d && d.count > 0 && !s.reviewedFrames[meta.terminalId] ? d.count : 0;
  });

  const exited = meta.status === "exited";
  const open = () => {
    const s = useStore.getState();
    s.setSection("agents");
    if (frame) s.setActiveFrameGuarded(frame.key);
    else if (!exited) s.reopenRustPty(meta.ptySessionId);
  };

  return (
    <button
      onClick={open}
      disabled={exited && !frame}
      title={`${providerTitle(meta.provider)} · ${meta.terminalId}${
        meta.branch ? ` · ${meta.branch}` : ""
      }`}
      className={`flex w-full items-center gap-3 px-3 py-2 text-left hover:bg-ink-600/60 disabled:cursor-default disabled:opacity-50 ${
        exited ? "opacity-60" : ""
      }`}
    >
      <span className="w-24 shrink-0">
        <StatusBadge status={raw} />
      </span>
      <span className="shrink-0 text-xs text-zinc-200">
        {providerTitle(meta.provider)}
      </span>
      <span className="min-w-0 flex-1 truncate whitespace-nowrap font-mono text-[11px] text-zinc-500">
        {meta.terminalId}
      </span>
      {meta.branch && (
        <span
          title={meta.branch}
          className="hidden max-w-[180px] shrink-0 truncate whitespace-nowrap font-mono text-[10px] text-zinc-600 lg:inline"
        >
          {meta.branch}
        </span>
      )}
      {dirtyCount > 0 && (
        <span
          title={`${dirtyCount} changed path${plural(dirtyCount)} pending review`}
          className="tnum shrink-0 rounded bg-amber/20 px-1 text-[10px] font-medium text-amber"
        >
          {dirtyCount}
        </span>
      )}
      <ChevronRight size={12} className="shrink-0 text-zinc-600" />
    </button>
  );
}

/** Centered hint for a truly empty workspace (daemon reachable, no tasks, no
 *  agents): launch the first agent, or open a project folder. */
function EmptyWorkspace() {
  const workspaceDir = useStore((s) => s.workspaceDir);
  const connected = useStore((s) => s.connected);
  const setLaunchOpen = useStore((s) => s.setLaunchOpen);
  const switchWorkspace = useStore((s) => s.switchWorkspace);
  const [picking, setPicking] = useState(false);

  const openWorkspace = async () => {
    if (picking) return;
    setPicking(true);
    try {
      const dir = await pickDirectory(workspaceDir ?? undefined);
      if (dir) switchWorkspace(dir);
    } finally {
      setPicking(false);
    }
  };

  return (
    <div className="flex h-full flex-col items-center justify-center gap-4 p-6">
      <div className="text-center">
        <p className="text-sm font-medium text-zinc-200">
          {workspaceDir ? "Workspace is empty" : "No workspace open"}
        </p>
        <p className="mt-1 max-w-sm text-xs text-zinc-500">
          {workspaceDir
            ? "No tasks or agents yet. Launch an agent to start working — each runs in its own worktree."
            : "Open a project folder to scope tasks, or launch an agent right away."}
        </p>
      </div>
      <div className="flex items-center gap-2">
        <button
          onClick={() => setLaunchOpen(true)}
          disabled={!connected}
          title={connected ? "Launch an agent" : "Daemon unreachable"}
          className="flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-white hover:bg-primary-hover disabled:cursor-default disabled:opacity-40"
        >
          <Bot size={13} className="shrink-0" />
          Launch your first agent
        </button>
        <button
          onClick={() => void openWorkspace()}
          disabled={picking}
          className="flex items-center gap-1.5 rounded-md border border-ink-500 px-3 py-1.5 text-xs text-zinc-300 hover:bg-ink-600 disabled:cursor-default disabled:opacity-40"
        >
          <FolderOpen size={13} className="shrink-0" />
          Open workspace
        </button>
      </div>
    </div>
  );
}
