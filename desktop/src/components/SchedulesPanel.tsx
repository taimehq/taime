import { useCallback, useEffect, useRef, useState } from "react";
import { Plus, Play, Trash2, Clock, Check, Loader2 } from "lucide-react";
import { api, type ScheduleInfo } from "../api";
import { useStore } from "../store";
import { AddScheduleDialog } from "./AddScheduleDialog";
import { basename } from "../lib/recentProjects";

/** Provider display names (the daemon's 4 CLIs). */
const PROVIDER_LABELS: Record<string, string> = {
  claude_code: "Claude Code",
  codex: "Codex CLI",
  gemini_cli: "Gemini CLI",
  grok_cli: "Grok Build CLI",
};

function providerLabel(name: string): string {
  return PROVIDER_LABELS[name] ?? name;
}

/** Compact relative time from a unix-seconds timestamp (e.g. "in 3h", "5m ago"). */
function relativeTime(nextRun: number | null, enabled: boolean): string {
  if (!enabled || nextRun == null) return "—";
  const deltaMs = nextRun * 1000 - Date.now();
  const future = deltaMs >= 0;
  let s = Math.abs(Math.round(deltaMs / 1000));
  let unit: string;
  let value: number;
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

export function SchedulesPanel() {
  const [schedules, setSchedules] = useState<ScheduleInfo[]>([]);
  const [showAdd, setShowAdd] = useState(false);
  const [runningName, setRunningName] = useState<string | null>(null);
  const [ranName, setRanName] = useState<string | null>(null);
  // Task titles for the fixed-task chips: the user-facing identity of a Task
  // is its title, never the raw id.
  const [taskTitles, setTaskTitles] = useState<Record<string, string>>({});
  const workspaceDir = useStore((s) => s.workspaceDir);
  const mounted = useRef(true);

  const refresh = useCallback(async () => {
    try {
      const list = await api.listSchedules();
      if (mounted.current) setSchedules(list);
      if (workspaceDir) {
        const tasks = await api.listTasks(workspaceDir, true);
        if (mounted.current)
          setTaskTitles(Object.fromEntries(tasks.map((t) => [t.id, t.title])));
      }
    } catch {
      /* surfaced by the next poll */
    }
  }, [workspaceDir]);

  useEffect(() => {
    mounted.current = true;
    refresh();
    const id = window.setInterval(refresh, 5000);
    return () => {
      mounted.current = false;
      window.clearInterval(id);
    };
  }, [refresh]);

  const run = async (name: string) => {
    setRunningName(name);
    await api.runSchedule(name);
    if (!mounted.current) return;
    setRunningName(null);
    setRanName(name);
    window.setTimeout(() => {
      if (mounted.current) setRanName((n) => (n === name ? null : n));
    }, 1500);
    refresh();
  };

  const toggle = async (s: ScheduleInfo) => {
    // Optimistic flip so the switch feels instant; reconciled by refresh.
    setSchedules((prev) =>
      prev.map((x) => (x.name === s.name ? { ...x, enabled: !x.enabled } : x)),
    );
    await api.toggleSchedule(s.name, !s.enabled);
    refresh();
  };

  const remove = async (name: string) => {
    if (!window.confirm(`Delete schedule "${name}"?`)) return;
    await api.deleteSchedule(name);
    refresh();
  };

  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center justify-between">
        <h2 className="text-[10px] font-semibold uppercase tracking-wide text-zinc-600">
          Schedules · {schedules.length}
        </h2>
        <button
          onClick={() => setShowAdd(true)}
          title="New schedule"
          className="rounded p-0.5 text-zinc-500 hover:text-zinc-200"
        >
          <Plus size={14} />
        </button>
      </div>

      {schedules.length === 0 ? (
        <p className="flex items-center gap-1.5 text-[11px] text-zinc-600">
          <Clock size={12} className="shrink-0" />
          No schedules yet — automate a recurring agent run.
        </p>
      ) : (
        <div className="flex flex-col gap-1">
          {schedules.map((s) => {
            const isRunning = runningName === s.name;
            const justRan = ranName === s.name;
            return (
              <div
                key={s.name}
                className="rounded-lg border border-ink-600 bg-ink-800 px-2.5 py-2"
              >
                <div className="flex items-start gap-2">
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-[13px] text-zinc-200">
                      {s.name}
                    </div>
                    <div className="mt-0.5 flex items-center gap-1.5">
                      <span className="rounded-md bg-ink-700 px-1.5 py-0.5 font-mono text-[10px] text-zinc-500">
                        {s.schedule}
                      </span>
                      {/* Workspace + task targeting (explicit; set at creation) */}
                      {s.workspace_root && (
                        <span
                          title={s.workspace_root}
                          className="rounded bg-ink-700 px-1 text-[9px] text-zinc-500"
                        >
                          {basename(s.workspace_root)}
                        </span>
                      )}
                      {s.task_mode && (
                        <span
                          title={
                            s.task_mode === "fixed"
                              ? `Task: ${(s.task_id && taskTitles[s.task_id]) ?? s.task_id ?? ""}`
                              : "Creates a new task per run"
                          }
                          className="rounded bg-teal-600/15 px-1 text-[9px] text-teal-400"
                        >
                          {s.task_mode === "per_run"
                            ? "task/run"
                            : ((s.task_id && taskTitles[s.task_id]) ?? "task")}
                        </span>
                      )}
                      <span className="truncate text-[10px] text-zinc-600">
                        {providerLabel(s.provider)} · {s.agent_profile}
                      </span>
                    </div>
                    <div className="mt-0.5 flex items-center gap-1 text-[10px] text-zinc-500">
                      <Clock size={10} className="shrink-0" />
                      {relativeTime(s.next_run, s.enabled)}
                    </div>
                  </div>

                  <div className="flex shrink-0 items-center gap-1.5">
                    {/* Run now */}
                    <button
                      onClick={() => run(s.name)}
                      disabled={isRunning}
                      title="Run now"
                      className="rounded p-0.5 text-zinc-500 hover:text-zinc-200 disabled:opacity-60"
                    >
                      {isRunning ? (
                        <Loader2 size={13} className="animate-spin" />
                      ) : justRan ? (
                        <Check size={13} className="text-emerald-400" />
                      ) : (
                        <Play size={13} />
                      )}
                    </button>

                    {/* Enable toggle */}
                    <button
                      onClick={() => toggle(s)}
                      title={s.enabled ? "Enabled — click to disable" : "Disabled — click to enable"}
                      role="switch"
                      aria-checked={s.enabled}
                      className={`relative h-3.5 w-6 shrink-0 rounded-full transition-colors ${
                        s.enabled ? "bg-teal-600" : "bg-ink-600"
                      }`}
                    >
                      <span
                        className={`absolute top-0.5 h-2.5 w-2.5 rounded-full bg-zinc-100 transition-all ${
                          s.enabled ? "left-3" : "left-0.5"
                        }`}
                      />
                    </button>

                    {/* Delete */}
                    <button
                      onClick={() => remove(s.name)}
                      title="Delete schedule"
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

      {showAdd && (
        <AddScheduleDialog
          onClose={() => setShowAdd(false)}
          onSaved={refresh}
        />
      )}
    </div>
  );
}
