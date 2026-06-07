import { useEffect, useState } from "react";
import {
  Plus,
  GitBranch,
  Power,
  PanelLeftOpen,
  ClipboardList,
} from "lucide-react";
import { useStore, type RustPtyMeta } from "../store";
import { api, type TaskInfo } from "../api";
import { StatusBadge, statusDotClass } from "../components/StatusBadge";
import { FileInventory } from "../components/FileInventory";
import { SchedulesPanel } from "../components/SchedulesPanel";
import { WorkflowsPanel } from "../components/WorkflowsPanel";
import { WorkspacePicker } from "../components/WorkspacePicker";
import { providerTitle } from "../lib/providerLabel";

export function ControlColumn({ onLaunch }: { onLaunch: () => void }) {
  const connected = useStore((s) => s.connected);
  const width = useStore((s) => s.sidebarWidth);
  const collapsed = useStore((s) => s.sidebarCollapsed);
  const setSidebarWidth = useStore((s) => s.setSidebarWidth);
  const toggleSidebar = useStore((s) => s.toggleSidebar);
  const frames = useStore((s) => s.frames);
  const terminalStatuses = useStore((s) => s.terminalStatuses);
  const setActiveFrameGuarded = useStore((s) => s.setActiveFrameGuarded);

  // Collapsed: a thin rail that still surfaces live agent activity — expand,
  // launch, and a status dot per window (click to jump, guarded). Cmd+\ toggles.
  if (collapsed) {
    return (
      <aside className="flex w-10 shrink-0 flex-col items-center gap-2 border-r border-ink-600 bg-ink-800/40 py-2.5">
        <button
          onClick={toggleSidebar}
          title="Show sidebar (⌘\)"
          className="no-drag rounded p-1.5 text-zinc-500 hover:bg-ink-700 hover:text-zinc-200"
        >
          <PanelLeftOpen size={16} />
        </button>
        <button
          onClick={onLaunch}
          disabled={!connected}
          title="Launch agent"
          className="no-drag rounded p-1.5 text-zinc-400 hover:bg-ink-700 hover:text-zinc-200 disabled:opacity-40"
        >
          <Plus size={16} />
        </button>
        <div className="mt-1 flex min-h-0 flex-col items-center gap-2.5 overflow-y-auto">
          {frames.map((f) => {
            const raw = f.pending
              ? "PENDING"
              : f.terminalId
                ? terminalStatuses[f.terminalId]
                : undefined;
            return (
              <button
                key={f.key}
                onClick={() => setActiveFrameGuarded(f.key)}
                title={providerTitle(f.provider)}
                className="no-drag rounded-full p-0.5 hover:bg-ink-700"
              >
                <span
                  className={`block h-2.5 w-2.5 rounded-full ${statusDotClass(raw)}`}
                />
              </button>
            );
          })}
        </div>
      </aside>
    );
  }

  return (
    <aside
      style={{ width }}
      className="relative flex shrink-0 flex-col border-r border-ink-600 bg-ink-800/40"
    >
      {/* Zone 1 — context + primary actions (pinned). Collapse toggle lives in
          the title bar so it costs no sidebar height. */}
      <div className="flex shrink-0 flex-col gap-3 p-4 pb-3">
        <WorkspacePicker />
        <div className="flex flex-col gap-2">
          <button
            onClick={onLaunch}
            disabled={!connected}
            className="no-drag flex items-center justify-center gap-2 rounded-lg bg-primary px-3 py-2 text-sm font-medium text-white transition-colors hover:bg-primary-hover disabled:cursor-not-allowed disabled:opacity-50"
          >
            <Plus size={16} />
            Launch agent
          </button>
        </div>
      </div>

      {/* Zone 2 — agents + schedules (the only scrolling zone) */}
      <div className="min-h-0 flex-1 space-y-5 overflow-y-auto border-t border-ink-700 px-3 py-3">
        <AgentsSection />
        <WorkflowsPanel />
        <SchedulesPanel />
      </div>

      {/* Zone 3 — changes / review (pinned bottom, capped) */}
      <div className="max-h-[38%] shrink-0 overflow-y-auto border-t border-ink-700 p-3">
        <FileInventory />
      </div>

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
      className="absolute right-0 top-0 z-10 h-full w-1 cursor-col-resize hover:bg-teal-600/40"
    />
  );
}

/**
 * Agents grouped by **Task** (workspace-scoped units of intent). Tasks render as
 * clickable group headers (→ Task Review drawer); agents without a task sit
 * under "Uncategorized". With no tasks at all this stays the flat agent list —
 * Task is never a toll booth. Every daemon session appears (close ≠ kill).
 */
function AgentsSection() {
  const rustPtySessions = useStore((s) => s.rustPtySessions);
  const workspaceDir = useStore((s) => s.workspaceDir);
  const openTaskReview = useStore((s) => s.openTaskReview);
  const [tasks, setTasks] = useState<TaskInfo[]>([]);

  // Tasks live in the daemon store; poll lightly so daemon-side changes
  // (orchestrator inherits, schedule per-run creates) surface without a reload.
  // Archived tasks are fetched too: "archive preserves membership", so their
  // members must keep their group (dimmed) — NOT masquerade as Uncategorized.
  useEffect(() => {
    if (!workspaceDir) {
      setTasks([]);
      return;
    }
    let alive = true;
    const load = () =>
      api.listTasks(workspaceDir, true).then((t) => {
        if (alive) setTasks(t);
      });
    load();
    const timer = setInterval(load, 5000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, [workspaceDir]);

  // Running first (most recent first), then exited.
  const agents = Object.values(rustPtySessions).sort((a, b) => {
    if (a.status !== b.status) return a.status === "running" ? -1 : 1;
    return b.startedAt - a.startedAt;
  });
  const taskIds = new Set(tasks.map((t) => t.id));
  const membersOf = (taskId: string) => agents.filter((m) => m.taskId === taskId);
  // Uncategorized is STRICTLY task_id = null (the lexicon definition). An agent
  // whose task isn't in this workspace's list belongs to another workspace —
  // label it as such rather than asserting a membership it doesn't have.
  const uncategorized = agents.filter((m) => !m.taskId);
  const foreign = agents.filter((m) => m.taskId && !taskIds.has(m.taskId));
  const active = tasks.filter((t) => t.status !== "archived");
  // Archived groups only earn a row while they still have live members; their
  // history stays reachable through the drawer (status select un-archives).
  const archived = tasks.filter(
    (t) => t.status === "archived" && membersOf(t.id).length > 0,
  );
  const grouped = active.length > 0 || archived.length > 0;

  return (
    <Section title={`Agents · ${agents.length}`}>
      {agents.length === 0 && !grouped ? (
        <p className="text-[11px] text-zinc-600">
          No agents yet. Launch one to begin.
        </p>
      ) : !grouped ? (
        <AgentList agents={agents} />
      ) : (
        <div className="flex flex-col gap-2.5">
          {[...active, ...archived].map((task) => {
            const members = membersOf(task.id);
            const dim = task.status === "archived";
            return (
              <div key={task.id} className="flex flex-col gap-1">
                <button
                  onClick={() => openTaskReview(task.id)}
                  title="Open task review"
                  className={`flex w-full items-center gap-1.5 rounded-md px-1 py-0.5 text-left hover:bg-ink-700/60 ${
                    dim ? "opacity-60" : ""
                  }`}
                >
                  <ClipboardList size={11} className="shrink-0 text-zinc-500" />
                  <span className="min-w-0 flex-1 truncate text-xs font-medium text-zinc-300">
                    {task.title}
                  </span>
                  <TaskStatusChip status={task.status} />
                  <span className="shrink-0 text-[10px] tabular-nums text-zinc-600">
                    {members.length}
                  </span>
                </button>
                {members.length > 0 && <AgentList agents={members} />}
              </div>
            );
          })}
          {uncategorized.length > 0 && (
            <div className="flex flex-col gap-1">
              <span className="px-1 text-[10px] font-medium uppercase tracking-wide text-zinc-600">
                Uncategorized
              </span>
              <AgentList agents={uncategorized} />
            </div>
          )}
          {foreign.length > 0 && (
            <div className="flex flex-col gap-1">
              <span className="px-1 text-[10px] font-medium uppercase tracking-wide text-zinc-600">
                Other workspaces
              </span>
              <AgentList agents={foreign} />
            </div>
          )}
        </div>
      )}
    </Section>
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

/** The flat agent rows (focus/reattach, status badge, kill/dismiss). */
function AgentList({ agents }: { agents: RustPtyMeta[] }) {
  const frames = useStore((s) => s.frames);
  const statuses = useStore((s) => s.terminalStatuses);
  const setActiveFrameGuarded = useStore((s) => s.setActiveFrameGuarded);
  const reopenRustPty = useStore((s) => s.reopenRustPty);
  const forgetRustPty = useStore((s) => s.forgetRustPty);

  return (
    <div className="flex flex-col gap-1">
      {agents.map((m) => {
        const frame = frames.find((f) => f.ptySessionId === m.ptySessionId);
        const exited = m.status === "exited";
        const status = m.terminalId ? statuses[m.terminalId] : undefined;
        const stateTag = frame ? "open" : exited ? "exited" : "detached";
        return (
          <div
            key={m.ptySessionId}
            className={`flex items-center gap-2 rounded-lg border px-2.5 py-1.5 ${
              exited ? "border-ink-600 bg-ink-800/40" : "border-teal-600/30 bg-teal-600/5"
            }`}
          >
            <button
              onClick={() =>
                frame ? setActiveFrameGuarded(frame.key) : reopenRustPty(m.ptySessionId)
              }
              disabled={exited && !frame}
              title={frame ? "Focus" : exited ? "Process exited" : "Reopen (reattach)"}
              className="flex min-w-0 flex-1 flex-col text-left disabled:cursor-default"
            >
              <span className="flex items-center gap-1.5 truncate text-xs text-zinc-200">
                {providerTitle(m.provider)}
                <span
                  className={`rounded px-1 text-[10px] font-semibold uppercase tracking-wide ${
                    exited
                      ? "bg-ink-600 text-zinc-400"
                      : frame
                        ? "bg-ink-600 text-zinc-400"
                        : "bg-teal-600/20 text-teal-400"
                  }`}
                >
                  {stateTag}
                </span>
              </span>
              {m.branch && (
                <span className="flex items-center gap-1 truncate font-mono text-[10px] text-zinc-500">
                  <GitBranch size={10} /> {m.branch}
                </span>
              )}
            </button>
            {!exited && <StatusBadge status={status} />}
            {exited ? (
              <button
                onClick={() => forgetRustPty(m.ptySessionId)}
                className="shrink-0 rounded border border-ink-500 px-2 py-0.5 text-[11px] text-zinc-400 hover:bg-ink-600"
              >
                Dismiss
              </button>
            ) : (
              <button
                onClick={() => forgetRustPty(m.ptySessionId)}
                aria-label="Kill agent"
                title="Kill agent (terminate process)"
                className="shrink-0 rounded p-0.5 text-red-400/80 hover:text-red-300"
              >
                <Power size={13} />
              </button>
            )}
          </div>
        );
      })}
    </div>
  );
}

function Section({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex flex-col gap-2">
      <h2 className="text-[11px] font-semibold uppercase tracking-wider text-zinc-500">
        {title}
      </h2>
      {children}
    </div>
  );
}
