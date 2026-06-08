import { useEffect, useState } from "react";
import { X } from "lucide-react";
import {
  api,
  type ProviderInfo,
  type AgentProfileInfo,
  type TaskInfo,
} from "../api";
import { useStore } from "../store";
import { basename } from "../lib/recentProjects";
import { providerTitle, PROVIDER_ORDER } from "../lib/providerLabel";

/** One-tap cron presets surfaced as chips under the schedule input. */
const CRON_PRESETS: { label: string; value: string }[] = [
  { label: "Every weekday 9am", value: "0 9 * * 1-5" },
  { label: "Hourly", value: "0 * * * *" },
  { label: "Nightly 2am", value: "0 2 * * *" },
];

export function AddScheduleDialog({
  onClose,
  onSaved,
}: {
  onClose: () => void;
  onSaved: () => void;
}) {
  const workspaceDir = useStore((s) => s.workspaceDir);

  const [name, setName] = useState("");
  const [schedule, setSchedule] = useState("");
  const [profiles, setProfiles] = useState<AgentProfileInfo[]>([]);
  const [profile, setProfile] = useState("default");
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [provider, setProvider] = useState("");
  // Workspace-targeted schedules are the common case — default to the active one.
  const [workspace, setWorkspace] = useState(workspaceDir ?? "");
  // "" = Uncategorized, "__per_run__" = fresh task per fire, else a task id.
  const [taskSel, setTaskSel] = useState("");
  const [tasks, setTasks] = useState<TaskInfo[]>([]);
  const [prompt, setPrompt] = useState("");
  const [script, setScript] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api
      .listProfiles()
      .then(setProfiles)
      .catch(() => setProfiles([]));

    api
      .listProviders()
      .then((list) => {
        const installed = list.filter((p) => p.installed);
        const ordered = [...installed].sort((a, b) => {
          const ai = PROVIDER_ORDER.indexOf(a.name);
          const bi = PROVIDER_ORDER.indexOf(b.name);
          return (ai === -1 ? 99 : ai) - (bi === -1 ? 99 : bi);
        });
        setProviders(ordered);
        if (ordered[0]) setProvider(ordered[0].name);
      })
      .catch(() => setProviders([]));
  }, []);

  // Open tasks for the targeted workspace (the Task field hides when none).
  useEffect(() => {
    setTaskSel("");
    if (workspace === "") {
      setTasks([]);
      return;
    }
    let stale = false;
    api
      .listTasks(workspace)
      .then((list) => {
        if (!stale) setTasks(list.filter((t) => t.status === "open"));
      })
      .catch(() => {
        if (!stale) setTasks([]);
      });
    return () => {
      stale = true;
    };
  }, [workspace]);

  const canSubmit =
    !busy && name.trim() !== "" && schedule.trim() !== "" && prompt.trim() !== "";

  const submit = async () => {
    if (!canSubmit) return;
    setBusy(true);
    setError(null);
    const trimmedScript = script.trim();
    const result = await api.addSchedule({
      name: name.trim(),
      schedule: schedule.trim(),
      agent_profile: profile,
      provider,
      prompt: prompt.trim(),
      script: trimmedScript === "" ? null : trimmedScript,
      workspace_root: workspace || null,
      task_mode:
        taskSel === "__per_run__" ? "per_run" : taskSel ? "fixed" : null,
      task_id: taskSel && taskSel !== "__per_run__" ? taskSel : null,
    });
    setBusy(false);
    if (result) {
      setError(result);
      return;
    }
    onSaved();
    onClose();
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
      onClick={onClose}
    >
      <div
        className="no-drag w-[30rem] rounded-xl border border-ink-500 bg-ink-800 p-5 shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-4 flex items-center justify-between">
          <h2 className="text-sm font-semibold text-zinc-100">New schedule</h2>
          <button
            onClick={onClose}
            className="rounded p-1 text-zinc-500 hover:text-zinc-200"
          >
            <X size={16} />
          </button>
        </div>

        {/* Name */}
        <label className="mb-1.5 block text-[11px] font-medium uppercase tracking-wide text-zinc-500">
          Name
        </label>
        <input
          type="text"
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="nightly-triage"
          className="mb-4 w-full rounded-lg border border-ink-500 bg-ink-700 px-3 py-2 text-sm text-zinc-200 placeholder:text-zinc-600"
        />

        {/* Schedule (cron) */}
        <label className="mb-1.5 block text-[11px] font-medium uppercase tracking-wide text-zinc-500">
          Schedule
        </label>
        <input
          type="text"
          value={schedule}
          onChange={(e) => setSchedule(e.target.value)}
          placeholder="0 9 * * 1-5"
          className="w-full rounded-lg border border-ink-500 bg-ink-700 px-3 py-2 font-mono text-sm text-zinc-200 placeholder:text-zinc-600"
        />
        <p className="mt-1.5 text-[11px] text-zinc-600">
          min hour day month weekday — e.g.{" "}
          <span className="font-mono text-zinc-500">0 9 * * 1-5</span> (9am Mon–Fri)
        </p>
        <div className="mb-4 mt-2 flex flex-wrap gap-1.5">
          {CRON_PRESETS.map((preset) => {
            const active = schedule.trim() === preset.value;
            return (
              <button
                key={preset.value}
                type="button"
                onClick={() => setSchedule(preset.value)}
                className={`rounded-full border px-2.5 py-1 text-[11px] transition-colors ${
                  active
                    ? "border-teal-600 bg-teal-600/10 text-teal-300"
                    : "border-ink-500 bg-ink-700/40 text-zinc-400 hover:border-ink-400 hover:text-zinc-200"
                }`}
              >
                {preset.label}
              </button>
            );
          })}
        </div>

        {/* Profile */}
        <label className="mb-1.5 block text-[11px] font-medium uppercase tracking-wide text-zinc-500">
          Profile
        </label>
        <select
          value={profile}
          onChange={(e) => setProfile(e.target.value)}
          className="mb-4 w-full rounded-lg border border-ink-500 bg-ink-700 px-3 py-2 text-sm text-zinc-200"
        >
          {profiles.length === 0 && <option value="default">default</option>}
          {profiles.map((p) => (
            <option key={p.name} value={p.name}>
              {p.name}
            </option>
          ))}
        </select>

        {/* Provider */}
        <label className="mb-1.5 block text-[11px] font-medium uppercase tracking-wide text-zinc-500">
          Runs on
        </label>
        <select
          value={provider}
          onChange={(e) => setProvider(e.target.value)}
          className="mb-4 w-full rounded-lg border border-ink-500 bg-ink-700 px-3 py-2 text-sm text-zinc-200"
        >
          {providers.length === 0 && <option value="">No installed CLIs</option>}
          {providers.map((p) => (
            <option key={p.name} value={p.name}>
              {providerTitle(p.name)}
            </option>
          ))}
        </select>

        {/* Workspace — only the active one is offered; other roots ship as .md. */}
        <label className="mb-1.5 block text-[11px] font-medium uppercase tracking-wide text-zinc-500">
          Workspace
        </label>
        <select
          value={workspace}
          onChange={(e) => setWorkspace(e.target.value)}
          className="mb-4 w-full rounded-lg border border-ink-500 bg-ink-700 px-3 py-2 text-sm text-zinc-200"
        >
          <option value="">None (daemon home)</option>
          {workspaceDir && (
            <option value={workspaceDir}>
              {basename(workspaceDir)} — {workspaceDir}
            </option>
          )}
        </select>

        {/* Task — explicit behavior; default Uncategorized, per-run is opt-in. */}
        {workspace !== "" && (
          <div className="mb-4">
            <label className="mb-1.5 block text-[11px] font-medium uppercase tracking-wide text-zinc-500">
              Task
            </label>
            <select
              value={taskSel}
              onChange={(e) => setTaskSel(e.target.value)}
              className="w-full rounded-lg border border-ink-500 bg-ink-700 px-3 py-2 text-sm text-zinc-200"
            >
              <option value="">Uncategorized</option>
              {tasks.map((t) => (
                <option key={t.id} value={t.id}>
                  {t.title}
                </option>
              ))}
              <option value="__per_run__">New task per run</option>
            </select>
            {taskSel === "__per_run__" && (
              <p className="mt-1 text-[10px] text-zinc-600">
                Each fire creates a fresh task (explicit opt-in).
              </p>
            )}
          </div>
        )}

        {/* Prompt */}
        <label className="mb-1.5 block text-[11px] font-medium uppercase tracking-wide text-zinc-500">
          Prompt
        </label>
        <textarea
          value={prompt}
          onChange={(e) => setPrompt(e.target.value)}
          rows={4}
          placeholder="What should the agent do when this fires?"
          className="mb-4 w-full resize-none rounded-lg border border-ink-500 bg-ink-700 px-3 py-2 text-sm text-zinc-200 placeholder:text-zinc-600"
        />

        {/* Script gate (optional) */}
        <label className="mb-1.5 block text-[11px] font-medium uppercase tracking-wide text-zinc-500">
          Script gate
        </label>
        <input
          type="text"
          value={script}
          onChange={(e) => setScript(e.target.value)}
          placeholder="./health-check.sh — optional gate; non-zero exit skips the run"
          className="w-full rounded-lg border border-ink-500 bg-ink-700 px-3 py-2 font-mono text-sm text-zinc-200 placeholder:text-zinc-600"
        />

        {error && <p className="mt-3 text-[11px] text-rose-400">{error}</p>}

        <div className="mt-4 flex justify-end gap-2">
          <button
            onClick={onClose}
            className="rounded-lg px-3 py-1.5 text-sm text-zinc-400 hover:text-zinc-200"
          >
            Cancel
          </button>
          <button
            onClick={submit}
            disabled={!canSubmit}
            className="rounded-lg bg-primary px-3 py-1.5 text-sm font-medium text-white hover:bg-primary-hover disabled:opacity-50"
          >
            {busy ? "Saving…" : "Create"}
          </button>
        </div>
      </div>
    </div>
  );
}
