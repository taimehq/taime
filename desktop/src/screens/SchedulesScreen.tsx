import { useCallback, useEffect, useRef, useState } from "react";
import {
  Bot,
  Calendar,
  CalendarClock,
  Check,
  Clock,
  Cpu,
  Folder,
  Layers,
  Loader2,
  Minus,
  Play,
  Plus,
  PlusCircle,
  Repeat,
  Shield,
  Trash2,
  type LucideIcon,
} from "lucide-react";
import { api, type ScheduleInfo } from "../api";
import { useStore } from "../store";
import { providerTitle } from "../lib/providerLabel";
import { middleTruncate } from "../lib/format";

// ─── Formatting ──────────────────────────────────────────────────────────────

/** Compact relative time from a unix-seconds timestamp ("in 3h" / "5m ago"). */
function relativeTime(ts: number | null): string {
  if (ts == null) return "—";
  const deltaMs = ts * 1000 - Date.now();
  const future = deltaMs >= 0;
  const s = Math.abs(Math.round(deltaMs / 1000));
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
  return future ? `in ${value}${unit}` : `${value}${unit} ago`;
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

/** The schedule's markdown body. The daemon serializes `prompt`; `body` is the
 *  alias older payloads carried — read tolerantly. Null (absent/blank) keeps
 *  the screen's fallback copy. */
function scheduleBody(sc: ScheduleInfo): string | null {
  const row = sc as ScheduleInfo & { body?: string | null };
  const body = row.prompt ?? row.body ?? null;
  return typeof body === "string" && body.trim() !== "" ? body : null;
}

// ─── Screen ──────────────────────────────────────────────────────────────────

/** Schedules section: detail for store.selectedSchedule (the sidebar owns the
 *  list). Meta cards, task-behavior display, the markdown body, enable/disable
 *  + Run now + delete, and New schedule via the existing dialog. */
export function SchedulesScreen() {
  const selectedName = useStore((s) => s.selectedSchedule);
  const connected = useStore((s) => s.connected);
  // The Add-schedule dialog is store-owned (App mounts it) so the palette and
  // the sidebar "+" share the same surface.
  const setNewScheduleOpen = useStore((s) => s.setNewScheduleOpen);
  const [schedules, setSchedules] = useState<ScheduleInfo[] | null>(null);
  const mounted = useRef(true);

  const refresh = useCallback(async () => {
    try {
      const list = await api.listSchedules();
      if (mounted.current) setSchedules(list);
    } catch {
      /* surfaced by the next poll */
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    refresh();
    const timer = setInterval(refresh, 5000);
    return () => {
      mounted.current = false;
      clearInterval(timer);
    };
  }, [refresh]);

  // Optimistic enable/disable: flip locally, reconcile on the next poll.
  const toggle = async (sc: ScheduleInfo) => {
    setSchedules(
      (prev) =>
        prev?.map((x) => (x.name === sc.name ? { ...x, enabled: !x.enabled } : x)) ??
        prev,
    );
    await api.toggleSchedule(sc.name, !sc.enabled);
    refresh();
  };

  const sc = schedules?.find((s) => s.name === selectedName) ?? null;

  return (
    <div className="flex h-full flex-col overflow-hidden">
      {!connected && (
        <div className="shrink-0 border-b border-amber/30 bg-amber/10 px-4 py-1 text-[11px] text-amber">
          daemon unreachable · retrying
        </div>
      )}
      {schedules === null ? (
        <CenterNote text="loading schedules…" />
      ) : schedules.length === 0 ? (
        <CenterNote
          headline="No schedules yet"
          action={{ label: "New schedule", onClick: () => setNewScheduleOpen(true), enabled: connected }}
        />
      ) : !selectedName ? (
        <CenterNote
          text="Select a schedule in the sidebar."
          action={{ label: "New schedule", onClick: () => setNewScheduleOpen(true), enabled: connected }}
        />
      ) : !sc ? (
        <CenterNote text={`Schedule "${selectedName}" not found — removed or renamed.`} />
      ) : (
        <ScheduleDetail
          sc={sc}
          connected={connected}
          onToggle={() => toggle(sc)}
          onChanged={refresh}
          onNew={() => setNewScheduleOpen(true)}
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
      <div className="flex max-w-sm flex-col items-center gap-3 text-center">
        <CalendarClock size={22} className="text-zinc-700" />
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
            className="flex items-center gap-1 rounded-md border border-ink-500 px-2.5 py-1 text-xs text-zinc-300 hover:bg-ink-600 disabled:cursor-default disabled:opacity-50"
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

function ScheduleDetail({
  sc,
  connected,
  onToggle,
  onChanged,
  onNew,
}: {
  sc: ScheduleInfo;
  connected: boolean;
  onToggle: () => void;
  onChanged: () => void;
  onNew: () => void;
}) {
  const [running, setRunning] = useState(false);
  const [justRan, setJustRan] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [deleting, setDeleting] = useState(false);
  // Title of the fixed-target task (a Task's user-facing identity is its
  // title, never the raw id) — resolved from the schedule's own workspace.
  const [fixedTaskTitle, setFixedTaskTitle] = useState<string | null>(null);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  // Selection switch: clear per-schedule transient state.
  useEffect(() => {
    setConfirmDelete(false);
    setJustRan(false);
  }, [sc.name]);

  useEffect(() => {
    setFixedTaskTitle(null);
    if (sc.task_mode !== "fixed" || !sc.task_id || !sc.workspace_root) return;
    let stale = false;
    api
      .listTasks(sc.workspace_root, true)
      .then((tasks) => {
        if (!stale) {
          setFixedTaskTitle(tasks.find((t) => t.id === sc.task_id)?.title ?? null);
        }
      })
      .catch(() => {});
    return () => {
      stale = true;
    };
  }, [sc.name, sc.task_mode, sc.task_id, sc.workspace_root]);

  const runNow = async () => {
    setRunning(true);
    const err = await api.runSchedule(sc.name);
    if (!mounted.current) return;
    setRunning(false);
    if (err) {
      useStore.getState().showSnackbar({ type: "error", message: `Run failed: ${err}` });
      return;
    }
    setJustRan(true);
    window.setTimeout(() => {
      if (mounted.current) setJustRan(false);
    }, 1500);
    onChanged();
  };

  const remove = async () => {
    setDeleting(true);
    await api.deleteSchedule(sc.name);
    if (!mounted.current) return;
    setDeleting(false);
    setConfirmDelete(false);
    useStore.getState().setSelectedSchedule(null);
    onChanged();
  };

  const body = scheduleBody(sc);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {/* Header */}
      <div className="flex shrink-0 items-start gap-3 border-b border-ink-600 px-4 py-3">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <CalendarClock size={15} className="shrink-0 text-accent" />
            <h1
              title={sc.name}
              className="min-w-0 truncate font-mono text-sm font-semibold text-zinc-100"
            >
              {sc.name}
            </h1>
            {sc.enabled ? (
              <span className="flex shrink-0 items-center gap-1 rounded-full bg-emerald-500/15 px-2 py-0.5 text-[10px] font-medium text-emerald-400">
                <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
                active
              </span>
            ) : (
              <span className="shrink-0 rounded-full bg-ink-600 px-2 py-0.5 text-[10px] font-medium text-zinc-500">
                paused
              </span>
            )}
          </div>
          <div className="mt-1 flex items-center gap-1.5 text-[11px] text-zinc-500">
            <span className="tnum font-mono">{sc.schedule}</span>
            <span className="text-zinc-700">·</span>
            <span className="tnum" title={absoluteTime(sc.next_run)}>
              {sc.enabled ? `next ${relativeTime(sc.next_run)}` : "paused — won't fire"}
            </span>
          </div>
        </div>

        <div className="flex shrink-0 items-center gap-2">
          {/* Run now */}
          <button
            onClick={runNow}
            disabled={running || !connected}
            title={connected ? "Fire this schedule now (bypasses cron + gate)" : "Daemon unreachable"}
            className="flex items-center gap-1 rounded-md border border-ink-500 px-2.5 py-1 text-xs text-zinc-300 hover:bg-ink-600 disabled:cursor-default disabled:opacity-50"
          >
            {running ? (
              <Loader2 size={12} className="animate-spin" />
            ) : justRan ? (
              <Check size={12} className="text-emerald-400" />
            ) : (
              <Play size={12} />
            )}
            Run now
          </button>

          {/* Enable / disable */}
          <button
            onClick={onToggle}
            disabled={!connected}
            role="switch"
            aria-checked={sc.enabled}
            title={sc.enabled ? "Enabled — click to disable" : "Disabled — click to enable"}
            className={`relative h-4 w-7 shrink-0 rounded-full transition-colors disabled:cursor-default disabled:opacity-50 ${
              sc.enabled ? "bg-accent" : "bg-ink-500"
            }`}
          >
            <span
              className={`absolute top-0.5 h-3 w-3 rounded-full bg-zinc-100 transition-all ${
                sc.enabled ? "left-3.5" : "left-0.5"
              }`}
            />
          </button>

          {/* Delete (two-step) */}
          {confirmDelete ? (
            <span className="flex items-center gap-1">
              <button
                onClick={remove}
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
              title="Delete schedule"
              aria-label="Delete schedule"
              className="rounded-md p-1.5 text-zinc-600 hover:bg-ink-600 hover:text-red-400 disabled:cursor-default disabled:opacity-50"
            >
              <Trash2 size={13} />
            </button>
          )}

          <span className="h-4 w-px bg-ink-600" />

          {/* New schedule */}
          <button
            onClick={onNew}
            disabled={!connected}
            title={connected ? "New schedule" : "Daemon unreachable"}
            className="flex items-center gap-1 rounded-md border border-ink-500 px-2.5 py-1 text-xs text-zinc-300 hover:bg-ink-600 disabled:cursor-default disabled:opacity-50"
          >
            <Plus size={12} />
            New schedule
          </button>
        </div>
      </div>

      {/* Body */}
      <div className="min-h-0 flex-1 overflow-y-auto p-4">
        <div className="flex max-w-3xl flex-col gap-4">
          {/* Meta grid */}
          <div className="grid grid-cols-3 gap-2">
            <MetaCard icon={Bot} label="Target profile" value={sc.agent_profile} />
            <MetaCard icon={Cpu} label="Provider" value={providerTitle(sc.provider)} />
            <MetaCard
              icon={Folder}
              label="Workspace root"
              value={sc.workspace_root ? middleTruncate(sc.workspace_root) : "none · daemon home"}
              title={sc.workspace_root ?? undefined}
              dim={!sc.workspace_root}
            />
            <MetaCard
              icon={Clock}
              label="Last run"
              value={sc.last_run != null ? relativeTime(sc.last_run) : "never"}
              title={absoluteTime(sc.last_run) || undefined}
              dim={sc.last_run == null}
            />
            <MetaCard
              icon={Calendar}
              label="Next run"
              value={sc.enabled ? relativeTime(sc.next_run) : "— paused"}
              title={sc.enabled ? absoluteTime(sc.next_run) || undefined : undefined}
              dim={!sc.enabled}
            />
            <MetaCard icon={Repeat} label="Cron" value={sc.schedule} />
          </div>

          {/* Task behavior */}
          <section>
            <h2 className="mb-2 text-[10px] font-semibold uppercase tracking-wider text-zinc-600">
              Task behavior on fire
            </h2>
            <TaskBehavior sc={sc} fixedTaskTitle={fixedTaskTitle} />
            <p className="mt-2 flex items-center gap-1.5 text-[10px] text-zinc-600">
              <Shield size={10} className="shrink-0" />
              Spawned agents keep their own agent id and worktree — attribution stays
              per-agent.
            </p>
          </section>

          {/* Definition body */}
          <section>
            <div className="mb-2 flex items-baseline gap-2">
              <h2 className="text-[10px] font-semibold uppercase tracking-wider text-zinc-600">
                Definition body
              </h2>
              <span className="min-w-0 truncate font-mono text-[10px] text-zinc-700">
                ~/.taime/schedules
              </span>
            </div>
            {body ? (
              <pre className="max-h-80 overflow-auto whitespace-pre-wrap rounded-lg border border-ink-600 bg-ink-900 p-3 font-mono text-[11px] leading-relaxed text-zinc-300">
                {body}
              </pre>
            ) : (
              <p className="rounded-lg border border-ink-600 bg-ink-800 px-3 py-2.5 text-[11px] text-zinc-600">
                Body not exposed by the daemon — edit the schedule's .md in
                ~/.taime/schedules.
              </p>
            )}
          </section>
        </div>
      </div>
    </div>
  );
}

/** One meta card: icon + uppercase label + mono value (tnum — live numbers). */
function MetaCard({
  icon: Icon,
  label,
  value,
  title,
  dim,
}: {
  icon: LucideIcon;
  label: string;
  value: string;
  title?: string;
  dim?: boolean;
}) {
  return (
    <div className="rounded-lg border border-ink-600 bg-ink-800 px-3 py-2.5">
      <div className="flex items-center gap-1.5 text-[10px] font-semibold uppercase tracking-wider text-zinc-600">
        <Icon size={11} className="shrink-0" />
        {label}
      </div>
      <div
        title={title ?? value}
        className={`tnum mt-1 truncate font-mono text-xs ${dim ? "text-zinc-600" : "text-zinc-200"}`}
      >
        {value}
      </div>
    </div>
  );
}

/** Read-only display of the schedule's task behavior (set at creation;
 *  uncategorized is the default, per_run is explicit — never the default). */
function TaskBehavior({
  sc,
  fixedTaskTitle,
}: {
  sc: ScheduleInfo;
  fixedTaskTitle: string | null;
}) {
  if (sc.task_mode === "per_run") {
    return (
      <div className="rounded-lg border border-amber/40 bg-amber/10 px-3 py-2.5">
        <div className="flex items-center gap-2">
          <PlusCircle size={13} className="shrink-0 text-amber" />
          <span className="text-xs font-medium text-zinc-200">New task per run</span>
          <span className="shrink-0 rounded bg-amber/20 px-1.5 py-0.5 text-[9px] font-semibold uppercase tracking-wide text-amber">
            explicit · never the default
          </span>
        </div>
        <p className="mt-1 text-[11px] leading-relaxed text-zinc-500">
          Each fire creates a fresh task titled "{sc.name} — {"{date time}"}"; a failed
          fire rolls it back.
        </p>
      </div>
    );
  }

  if (sc.task_mode === "fixed") {
    return (
      <div className="rounded-lg border border-ink-600 bg-ink-800 px-3 py-2.5">
        <div className="flex items-center gap-2">
          <Layers size={13} className="shrink-0 text-accent" />
          <span className="text-xs font-medium text-zinc-200">Attach to task</span>
          <span
            title={sc.task_id ?? undefined}
            className="min-w-0 truncate rounded bg-accent/10 px-1.5 py-0.5 text-[10px] text-accent"
          >
            {fixedTaskTitle ?? sc.task_id ?? "—"}
          </span>
        </div>
        <p className="mt-1 text-[11px] leading-relaxed text-zinc-500">
          Each fire joins this task. Re-validated at fire time — a missing, archived, or
          foreign task degrades the fire to Uncategorized.
        </p>
      </div>
    );
  }

  return (
    <div className="rounded-lg border border-ink-600 bg-ink-800 px-3 py-2.5">
      <div className="flex items-center gap-2">
        <Minus size={13} className="shrink-0 text-zinc-600" />
        <span className="text-xs font-medium text-zinc-200">Uncategorized</span>
        <span className="shrink-0 rounded bg-ink-600 px-1.5 py-0.5 text-[9px] font-semibold uppercase tracking-wide text-zinc-500">
          default
        </span>
      </div>
      <p className="mt-1 text-[11px] leading-relaxed text-zinc-500">
        Spawned agents run with <span className="font-mono">task_id=null</span> — no task
        is created.
      </p>
    </div>
  );
}
