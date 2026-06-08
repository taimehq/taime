import { useEffect, useState } from "react";
import {
  Bot,
  ChevronDown,
  ChevronRight,
  Cpu,
  Folder,
  Info,
  Palette,
  Plus,
  type LucideIcon,
} from "lucide-react";
import { useStore, type RustPtyMeta } from "../store";
import { api, type ScheduleInfo, type TaskInfo, type WorkflowInfo } from "../api";
import { statusDotClass, uiStatus } from "../lib/agentStatus";
import { providerTitle } from "../lib/providerLabel";
import { useTasks } from "../hooks/useTasks";

/**
 * The left sidebar (resizable 220–480, persisted). Content follows the rail
 * section: dashboard/tasks/agents share ONE task-first panel (tasks primary,
 * agents + running collapsible below); workflows/schedules/settings get thin
 * list sidebars over the store's selections.
 */
export function Sidebar() {
  const section = useStore((s) => s.section);
  const width = useStore((s) => s.sidebarWidth);
  const collapsed = useStore((s) => s.sidebarCollapsed);
  const setSidebarWidth = useStore((s) => s.setSidebarWidth);

  // Collapsed (⌘\) costs zero width — the rail still navigates.
  if (collapsed) return null;

  return (
    <aside
      style={{ width }}
      className="relative flex shrink-0 flex-col border-r border-hairline bg-ink-800"
    >
      {section === "workflows" ? (
        <WorkflowsSidebar />
      ) : section === "schedules" ? (
        <SchedulesSidebar />
      ) : section === "settings" ? (
        <SettingsSidebar />
      ) : (
        <TaskFirstPanel />
      )}
      <ResizeHandle onResize={setSidebarWidth} />
    </aside>
  );
}

/** Drag handle on the sidebar's right edge. Width is clamped in the store. */
function ResizeHandle({ onResize }: { onResize: (px: number) => void }) {
  const onMouseDown = (e: React.MouseEvent) => {
    e.preventDefault();
    const aside = (e.currentTarget as HTMLElement).parentElement;
    const left = aside?.getBoundingClientRect().left ?? 0;
    const onMove = (ev: MouseEvent) => onResize(ev.clientX - left);
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  return (
    <div
      onMouseDown={onMouseDown}
      title="Drag to resize"
      className="absolute right-0 top-0 z-10 h-full w-1 cursor-col-resize hover:bg-accent/40"
    />
  );
}

/** Shared sidebar chrome: header strip with an uppercase title + actions. */
function SidebarHead({
  title,
  children,
}: {
  title: string;
  children?: React.ReactNode;
}) {
  return (
    <div className="flex h-9 shrink-0 items-center gap-1.5 border-b border-hairline px-3">
      <span className="flex-1 text-[10px] font-semibold uppercase tracking-wider text-zinc-500">
        {title}
      </span>
      {children}
    </div>
  );
}

/** Collapsible group: chevron + uppercase title + tabular count pill. */
function Collapsible({
  title,
  count,
  defaultOpen,
  children,
}: {
  title: string;
  count: number;
  defaultOpen: boolean;
  children: React.ReactNode;
}) {
  const [open, setOpen] = useState(defaultOpen);
  return (
    <div className="flex flex-col">
      <button
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        className="flex w-full items-center gap-1.5 rounded-md px-2 py-1.5 hover:bg-ink-600/60"
      >
        {open ? (
          <ChevronDown size={11} className="shrink-0 text-zinc-600" />
        ) : (
          <ChevronRight size={11} className="shrink-0 text-zinc-600" />
        )}
        <span className="min-w-0 flex-1 truncate text-left text-[10px] font-semibold uppercase tracking-wider text-zinc-500">
          {title}
        </span>
        <span className="tnum shrink-0 rounded-full bg-ink-600 px-1.5 text-[10px] text-zinc-500">
          {count}
        </span>
      </button>
      {open && <div className="flex flex-col gap-0.5 pl-1.5">{children}</div>}
    </div>
  );
}

// ─── The unified task-first panel (dashboard / tasks / agents) ──────────────

const TASK_GROUPS: { status: string; label: string; defaultOpen: boolean }[] = [
  { status: "in_review", label: "In review", defaultOpen: true },
  { status: "open", label: "Open", defaultOpen: true },
  { status: "done", label: "Done", defaultOpen: true },
  { status: "archived", label: "Archived", defaultOpen: false },
];

function TaskFirstPanel() {
  const workspaceDir = useStore((s) => s.workspaceDir);
  const connected = useStore((s) => s.connected);
  const setNewTaskOpen = useStore((s) => s.setNewTaskOpen);
  const { tasks } = useTasks(workspaceDir);

  return (
    <>
      <SidebarHead title="Tasks">
        <button
          onClick={() => setNewTaskOpen(true)}
          disabled={!workspaceDir || !connected}
          title={
            workspaceDir ? "New task" : "Open a workspace to create tasks"
          }
          aria-label="New task"
          className="rounded p-1 text-zinc-500 hover:bg-ink-600 hover:text-zinc-200 disabled:cursor-default disabled:opacity-40"
        >
          <Plus size={13} />
        </button>
      </SidebarHead>

      <div className="flex min-h-0 flex-1 flex-col gap-1 overflow-y-auto p-2">
        {TASK_GROUPS.map((g) => {
          const group = tasks.filter((t) => t.status === g.status);
          return (
            <Collapsible
              key={g.status}
              title={g.label}
              count={group.length}
              defaultOpen={g.defaultOpen}
            >
              {group.length === 0 ? (
                <p className="px-2 py-1 text-[10px] text-zinc-700">
                  {g.status === "open" ? "No open tasks." : "None."}
                </p>
              ) : (
                group.map((t) => (
                  <TaskRow key={t.id} task={t} dim={g.status === "archived"} />
                ))
              )}
            </Collapsible>
          );
        })}

        <div className="mx-1 my-1.5 h-px shrink-0 bg-ink-700" />

        <AgentsCollapsible tasks={tasks} />
        <RunningCollapsible />
      </div>
    </>
  );
}

/** One task row: title, lifecycle badge, member count. Click navigates
 *  (guarded) to the Tasks section with this task selected. */
function TaskRow({ task, dim }: { task: TaskInfo; dim: boolean }) {
  const selected = useStore(
    (s) => s.section === "tasks" && s.selectedTaskId === task.id,
  );
  const selectTask = useStore((s) => s.selectTask);
  return (
    <button
      onClick={() => selectTask(task.id)}
      title={task.title}
      className={`relative flex w-full items-center gap-1.5 rounded-md px-2 py-1.5 text-left ${
        selected ? "bg-ink-500" : "hover:bg-ink-600/60"
      } ${dim ? "opacity-60" : ""}`}
    >
      {selected && (
        <span className="absolute bottom-[5px] left-0 top-[5px] w-[2px] rounded-r bg-accent" />
      )}
      <span className="min-w-0 flex-1 truncate text-xs text-zinc-200">
        {task.title}
      </span>
      <TaskStatusChip status={task.status} />
      <span className="tnum shrink-0 text-[10px] text-zinc-600">
        {task.agent_count}
      </span>
    </button>
  );
}

/** Lifecycle chip for a task: open · in_review · done · archived. */
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

/**
 * Agents grouped by Task. Uncategorized is STRICTLY task_id = null (the
 * lexicon definition) and the group is ALWAYS visible; an agent whose task
 * isn't in this workspace's list belongs to another workspace — labeled as
 * such rather than asserting a membership it doesn't have. Archived groups
 * keep their members (dimmed): archive preserves membership.
 */
function AgentsCollapsible({ tasks }: { tasks: TaskInfo[] }) {
  const rustPtySessions = useStore((s) => s.rustPtySessions);
  const connected = useStore((s) => s.connected);
  const setLaunchOpen = useStore((s) => s.setLaunchOpen);

  // Running first (most recent first), then exited.
  const agents = Object.values(rustPtySessions).sort((a, b) => {
    if (a.status !== b.status) return a.status === "running" ? -1 : 1;
    return b.startedAt - a.startedAt;
  });
  const taskIds = new Set(tasks.map((t) => t.id));
  const membersOf = (taskId: string) =>
    agents.filter((m) => m.taskId === taskId);
  const uncategorized = agents.filter((m) => !m.taskId);
  const foreign = agents.filter((m) => m.taskId && !taskIds.has(m.taskId));
  const active = tasks.filter((t) => t.status !== "archived");
  // Archived groups only earn a sub-label while they still have live members.
  const archived = tasks.filter(
    (t) => t.status === "archived" && membersOf(t.id).length > 0,
  );

  return (
    <Collapsible title="Agents" count={agents.length} defaultOpen>
      {[...active, ...archived].map((t) => {
        const members = membersOf(t.id);
        if (members.length === 0) return null;
        const dim = t.status === "archived";
        return (
          <div key={t.id} className={`flex flex-col gap-0.5 ${dim ? "opacity-60" : ""}`}>
            <span
              title={t.title}
              className="truncate px-2 pt-1 text-[10px] font-medium text-zinc-600"
            >
              {t.title}
            </span>
            {members.map((m) => (
              <AgentRow key={m.ptySessionId} meta={m} />
            ))}
          </div>
        );
      })}

      {/* Uncategorized — always visible (taskId strictly null). */}
      <div className="flex flex-col gap-0.5">
        <span className="px-2 pt-1 text-[10px] font-medium uppercase tracking-wider text-zinc-600">
          Uncategorized · {uncategorized.length}
        </span>
        {uncategorized.length === 0 ? (
          <p className="px-2 py-0.5 text-[10px] text-zinc-700">None.</p>
        ) : (
          uncategorized.map((m) => <AgentRow key={m.ptySessionId} meta={m} />)
        )}
      </div>

      {foreign.length > 0 && (
        <div className="flex flex-col gap-0.5">
          <span className="px-2 pt-1 text-[10px] font-medium uppercase tracking-wider text-zinc-600">
            Other workspaces
          </span>
          {foreign.map((m) => (
            <AgentRow key={m.ptySessionId} meta={m} />
          ))}
        </div>
      )}

      <button
        onClick={() => setLaunchOpen(true)}
        disabled={!connected}
        className="flex w-full items-center gap-1.5 rounded-md px-2 py-1.5 text-left text-xs text-zinc-500 hover:bg-ink-600/60 hover:text-zinc-300 disabled:cursor-default disabled:opacity-40"
      >
        <Plus size={12} className="shrink-0" />
        Launch agent
      </button>
    </Collapsible>
  );
}

/** Live agents only, sorted blocked-first (blocked is the state attention
 *  routes on), then most recent first. */
function RunningCollapsible() {
  const rustPtySessions = useStore((s) => s.rustPtySessions);
  const statuses = useStore((s) => s.terminalStatuses);

  const running = Object.values(rustPtySessions)
    .filter((m) => m.status === "running")
    .sort((a, b) => {
      const ab = uiStatus(statuses[a.terminalId]) === "blocked" ? 0 : 1;
      const bb = uiStatus(statuses[b.terminalId]) === "blocked" ? 0 : 1;
      if (ab !== bb) return ab - bb;
      return b.startedAt - a.startedAt;
    });

  return (
    <Collapsible title="Running" count={running.length} defaultOpen={false}>
      {running.length === 0 ? (
        <p className="px-2 py-0.5 text-[10px] text-zinc-700">
          No agents running.
        </p>
      ) : (
        running.map((m) => <AgentRow key={m.ptySessionId} meta={m} />)
      )}
    </Collapsible>
  );
}

/** One agent row: status dot · provider label · mono agent id · dirty chip.
 *  Click focuses (or reopens) the agent in the Agents section — guarded. */
function AgentRow({ meta }: { meta: RustPtyMeta }) {
  const frame = useStore((s) =>
    s.frames.find((f) => f.ptySessionId === meta.ptySessionId),
  );
  const raw = useStore((s) =>
    meta.status === "exited" ? "EXITED" : s.terminalStatuses[meta.terminalId],
  );
  const dirtyCount = useStore((s) => s.dirty[meta.terminalId]?.count ?? 0);

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
      className={`flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left hover:bg-ink-600/60 disabled:cursor-default disabled:opacity-50 ${
        exited ? "opacity-60" : ""
      }`}
    >
      <span
        className={`h-2 w-2 shrink-0 rounded-full ${statusDotClass(raw)}`}
      />
      <span className="min-w-0 truncate text-xs text-zinc-200">
        {providerTitle(meta.provider)}
      </span>
      <span className="min-w-0 flex-1 truncate font-mono text-[10px] text-zinc-600">
        {meta.terminalId}
      </span>
      {dirtyCount > 0 && (
        <span className="tnum shrink-0 rounded bg-amber/20 px-1 text-[10px] font-medium text-amber">
          {dirtyCount}
        </span>
      )}
    </button>
  );
}

// ─── Workflows / Schedules / Settings list sidebars ─────────────────────────

/** Last-run status → dot color (run statuses, not agent statuses). */
function runDotClass(status: string | undefined): string {
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

function WorkflowsSidebar() {
  const [workflows, setWorkflows] = useState<WorkflowInfo[]>([]);
  const selected = useStore((s) => s.selectedWorkflow);
  const setSelectedWorkflow = useStore((s) => s.setSelectedWorkflow);
  const setNewWorkflowOpen = useStore((s) => s.setNewWorkflowOpen);
  const connected = useStore((s) => s.connected);

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

  return (
    <>
      <SidebarHead title="Workflows">
        <button
          onClick={() => setNewWorkflowOpen(true)}
          disabled={!connected}
          title={connected ? "New workflow" : "Daemon unreachable"}
          aria-label="New workflow"
          className="rounded p-1 text-zinc-500 hover:bg-ink-600 hover:text-zinc-200 disabled:cursor-default disabled:opacity-40"
        >
          <Plus size={13} />
        </button>
      </SidebarHead>
      <div className="flex min-h-0 flex-1 flex-col gap-0.5 overflow-y-auto p-2">
        {workflows.length === 0 ? (
          <div className="flex flex-1 items-center justify-center px-2">
            <p className="text-center text-sm font-semibold tracking-tight text-zinc-300">
              No workflows yet
            </p>
          </div>
        ) : (
          workflows.map((wf) => (
            <button
              key={wf.name}
              onClick={() => setSelectedWorkflow(wf.name)}
              title={wf.name}
              className={`flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left ${
                selected === wf.name ? "bg-ink-500" : "hover:bg-ink-600/60"
              }`}
            >
              <span
                className={`h-2 w-2 shrink-0 rounded-full ${runDotClass(wf.last_run?.status)}`}
              />
              <span className="min-w-0 flex-1 truncate text-xs text-zinc-200">
                {wf.name}
              </span>
              <span className="tnum shrink-0 font-mono text-[10px] text-zinc-600">
                {wf.nodes.length} step{wf.nodes.length === 1 ? "" : "s"}
              </span>
            </button>
          ))
        )}
      </div>
    </>
  );
}

function SchedulesSidebar() {
  const [schedules, setSchedules] = useState<ScheduleInfo[]>([]);
  const selected = useStore((s) => s.selectedSchedule);
  const setSelectedSchedule = useStore((s) => s.setSelectedSchedule);
  const setNewScheduleOpen = useStore((s) => s.setNewScheduleOpen);
  const connected = useStore((s) => s.connected);

  useEffect(() => {
    let alive = true;
    const load = () =>
      api
        .listSchedules()
        .then((list) => {
          if (alive) setSchedules(list);
        })
        .catch(() => {});
    load();
    const timer = setInterval(load, 5000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, []);

  return (
    <>
      <SidebarHead title="Schedules">
        <button
          onClick={() => setNewScheduleOpen(true)}
          disabled={!connected}
          title={connected ? "New schedule" : "Daemon unreachable"}
          aria-label="New schedule"
          className="rounded p-1 text-zinc-500 hover:bg-ink-600 hover:text-zinc-200 disabled:cursor-default disabled:opacity-40"
        >
          <Plus size={13} />
        </button>
      </SidebarHead>
      <div className="flex min-h-0 flex-1 flex-col gap-0.5 overflow-y-auto p-2">
        {schedules.length === 0 ? (
          <div className="flex flex-1 items-center justify-center px-2">
            <p className="text-center text-sm font-semibold tracking-tight text-zinc-300">
              No schedules yet
            </p>
          </div>
        ) : (
          schedules.map((sc) => (
            <button
              key={sc.name}
              onClick={() => setSelectedSchedule(sc.name)}
              title={sc.name}
              className={`flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left ${
                selected === sc.name ? "bg-ink-500" : "hover:bg-ink-600/60"
              }`}
            >
              <span
                className={`h-2 w-2 shrink-0 rounded-full ${
                  sc.enabled ? "bg-emerald-400" : "bg-zinc-600"
                }`}
              />
              <span className="min-w-0 flex-1 truncate text-xs text-zinc-200">
                {sc.name}
              </span>
              <span className="shrink-0 font-mono text-[10px] text-zinc-600">
                {sc.schedule}
              </span>
            </button>
          ))
        )}
      </div>
    </>
  );
}

const SETTINGS_NAV: { id: string; icon: LucideIcon; label: string }[] = [
  { id: "workspace", icon: Folder, label: "Workspace" },
  { id: "providers", icon: Cpu, label: "Providers" },
  { id: "profiles", icon: Bot, label: "Agent profiles" },
  { id: "appearance", icon: Palette, label: "Appearance" },
  { id: "about", icon: Info, label: "About" },
];

function SettingsSidebar() {
  const settingsTab = useStore((s) => s.settingsTab);
  const setSettingsTab = useStore((s) => s.setSettingsTab);
  return (
    <>
      <SidebarHead title="Settings" />
      <div className="flex min-h-0 flex-1 flex-col gap-0.5 overflow-y-auto p-2">
        {SETTINGS_NAV.map((item) => (
          <button
            key={item.id}
            onClick={() => setSettingsTab(item.id)}
            className={`flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs ${
              settingsTab === item.id
                ? "bg-ink-500 text-zinc-100"
                : "text-zinc-300 hover:bg-ink-600/60"
            }`}
          >
            <item.icon size={13} className="shrink-0 text-zinc-500" />
            {item.label}
          </button>
        ))}
      </div>
    </>
  );
}
