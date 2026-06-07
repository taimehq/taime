import { useEffect, useRef, useState } from "react";
import {
  ArrowLeft,
  Check,
  ChevronRight,
  Layers,
  Minus,
  Plus,
  X,
} from "lucide-react";
import {
  api,
  type ProviderInfo,
  type AgentProfileInfo,
  type TaskInfo,
} from "../api";
import { useStore } from "../store";
import { providerTitle, PROVIDER_ORDER } from "../lib/providerLabel";
import { PROFILE_ORDER, profileMeta } from "../lib/profiles";

const FOCUS_RING =
  "focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-accent";

type WtMode = "isolated" | "shared";

/**
 * Two-step launcher. Step 1 picks the Task (Uncategorized is the zero-click
 * default — Task is never a toll booth); step 2 configures profile /
 * assignment / provider / worktree mode. Launch plumbing is unchanged: task
 * resolution (incl. loud-fail inline creation) happens BEFORE the optimistic
 * close, then the daemon provisions the worktree + mints the agent id.
 */
export function LaunchAgentDialog({ onClose }: { onClose: () => void }) {
  const launchAgent = useStore((s) => s.launchAgent);
  const workspaceDir = useStore((s) => s.workspaceDir);
  const connected = useStore((s) => s.connected);
  const isolationEnabled = useStore((s) => s.isolationEnabled);

  // Tasks are workspace-scoped — without a workspace there is no step 1.
  const hasWorkspace = !!workspaceDir;
  // A preset task (the Task screen's "Launch one") skips straight to config
  // with that task selected — Change still returns to the picker. Read once at
  // mount (lazy initializers); the store clears it when the dialog closes.
  const [step, setStep] = useState<1 | 2>(() =>
    hasWorkspace && !useStore.getState().launchPresetTaskId ? 1 : 2,
  );

  const [providers, setProviders] = useState<ProviderInfo[] | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [profiles, setProfiles] = useState<AgentProfileInfo[]>([]);
  const [profile, setProfile] = useState<string>("default");
  // Task attachment: "" = Uncategorized (the zero-click default), a task id to
  // join an existing task, or "__new__" to create one inline at launch time.
  const [tasks, setTasks] = useState<TaskInfo[] | null>(null); // null = loading
  const [taskSel, setTaskSel] = useState<string>(
    () => useStore.getState().launchPresetTaskId ?? "",
  );
  const [newTaskTitle, setNewTaskTitle] = useState<string>("");
  // The per-agent intent. Collected here; threading it to the daemon as the
  // agent's opening prompt is an integrator step (see `remaining`).
  const [assignment, setAssignment] = useState<string>("");
  const [wtMode, setWtMode] = useState<WtMode>(
    isolationEnabled ? "isolated" : "shared",
  );
  const [busy, setBusy] = useState(false);

  const boxRef = useRef<HTMLDivElement>(null);

  // Re-fetch on reconnect so a daemon restart doesn't strand stale lists.
  useEffect(() => {
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
        setSelected(
          (prev) =>
            prev ??
            ordered.find((p) => PROVIDER_ORDER.includes(p.name))?.name ??
            ordered[0]?.name ??
            null,
        );
      })
      .catch(() => setProviders([]));

    api
      .listProfiles()
      .then(setProfiles)
      .catch(() => setProfiles([]));
  }, [connected]);

  useEffect(() => {
    if (!workspaceDir) return;
    api
      .listTasks(workspaceDir)
      .then((list) =>
        setTasks(
          list.filter((t) => t.status === "open" || t.status === "in_review"),
        ),
      )
      .catch(() => setTasks([]));
  }, [workspaceDir, connected]);

  // Step 1 has no focusable default — focus the dialog so Esc/Enter work.
  useEffect(() => {
    if (step === 1) boxRef.current?.focus();
  }, [step]);

  // The daemon's profile store already includes the built-in default/orchestrator
  // profiles, but guarantee they exist (and aren't duplicated) so the grid is
  // never empty mid-load.
  const byName = new Map(profiles.map((p) => [p.name, p]));
  if (!byName.has("default"))
    byName.set("default", {
      name: "default",
      description: "Plain agent — no orchestration tools.",
      source: "builtin",
    });
  if (!byName.has("orchestrator"))
    byName.set("orchestrator", {
      name: "orchestrator",
      description: "Can assign / handoff to other agents.",
      source: "builtin",
    });
  // Known profiles in canonical order, customs after (alphabetical).
  const displayProfiles = [...byName.values()].sort((a, b) => {
    const ai = PROFILE_ORDER.indexOf(a.name);
    const bi = PROFILE_ORDER.indexOf(b.name);
    if (ai !== -1 || bi !== -1)
      return (ai === -1 ? 99 : ai) - (bi === -1 ? 99 : bi);
    return a.name.localeCompare(b.name);
  });

  const selectedTask =
    taskSel && taskSel !== "__new__"
      ? (tasks ?? []).find((t) => t.id === taskSel)
      : undefined;
  const taskLabel =
    taskSel === ""
      ? "Uncategorized"
      : taskSel === "__new__"
        ? `New task: ${newTaskTitle.trim() || "untitled"}`
        : (selectedTask?.title ?? taskSel);
  const profileLabel = profileMeta(
    profile,
    byName.get(profile)?.description,
  ).label;

  const canContinue = taskSel !== "__new__" || newTaskTitle.trim().length > 0;

  const submit = async () => {
    if (!selected || busy) return;
    // "+ Create new task" with no title: don't silently launch Uncategorized —
    // keep the dialog open so the user can finish (or clear) the task choice.
    if (taskSel === "__new__" && !newTaskTitle.trim()) {
      useStore.getState().showSnackbar({
        type: "error",
        message: "Give the new task a title (or pick Uncategorized).",
      });
      setStep(1);
      return;
    }
    setBusy(true);
    // Resolve the task BEFORE the optimistic close, so a failed creation can
    // abort loudly instead of demoting the launch to Uncategorized behind a
    // success toast. null = Uncategorized — Task is never a toll booth.
    let taskId: string | null = null;
    if (taskSel === "__new__" && workspaceDir) {
      const t = await api.createTask(workspaceDir, newTaskTitle.trim());
      if (!t) {
        setBusy(false);
        useStore.getState().showSnackbar({
          type: "error",
          message: "Couldn't create the task — agent not launched.",
        });
        return;
      }
      taskId = t.id;
    } else if (taskSel && taskSel !== "__new__") {
      taskId = taskSel;
    }
    // Worktree mode rides the existing isolation flag — provision reads it.
    const s = useStore.getState();
    if (s.isolationEnabled !== (wtMode === "isolated"))
      s.setIsolationEnabled(wtMode === "isolated");
    onClose(); // optimistic: close immediately, frame appears as pending
    // TODO(daemon): thread `assignment` as the agent's opening prompt. The wire
    // field exists (AgentSpawnSpec.seed_prompt, taime-protocol) but the daemon's
    // LaunchOpts marks it reserved/dead-code — providers never write it to the
    // PTY. Once the daemon seeds it, thread assignment through store.launchAgent
    // → daemonSpawnAgent → the Tauri command's default_agent_spec.
    void assignment;
    await launchAgent(selected, profile, { taskId });
  };

  const handleKey = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      if (step === 2 && hasWorkspace) setStep(1);
      else onClose();
      return;
    }
    if (e.key !== "Enter") return;
    const tag = (e.target as HTMLElement).tagName;
    if (tag === "BUTTON" || tag === "SELECT") return; // native activation
    if (tag === "TEXTAREA" && !(e.metaKey || e.ctrlKey)) return; // newline
    e.preventDefault();
    if (step === 1) {
      if (canContinue) setStep(2);
    } else {
      void submit();
    }
  };

  // ── Step 1: task picker ────────────────────────────────────────────────────
  const taskCard = (active: boolean) =>
    `flex w-full items-center gap-2.5 rounded-lg border px-3 py-2 text-left transition-colors ${
      active
        ? "border-accent bg-accent/10"
        : "border-ink-500 bg-ink-700/40 hover:border-ink-400"
    } ${FOCUS_RING}`;

  const step1 = (
    <>
      <p className="mb-3 text-[11px] text-zinc-500">
        Pick the task this agent works under. Optional — uncategorized is fully
        supported.
      </p>

      {/* Uncategorized — the visually-primary zero-click default */}
      <button onClick={() => setTaskSel("")} aria-pressed={taskSel === ""} className={`${taskCard(taskSel === "")} mb-1.5`}>
        <span
          className={`flex h-7 w-7 shrink-0 items-center justify-center rounded-full ${
            taskSel === "" ? "bg-accent/15 text-accent" : "bg-ink-600 text-zinc-500"
          }`}
        >
          <Minus size={13} />
        </span>
        <span className="flex min-w-0 flex-1 flex-col">
          <span className="text-sm font-medium text-zinc-100">
            Uncategorized
          </span>
          <span className="text-[11px] text-zinc-500">
            Runs without a task — fully supported
          </span>
        </span>
        {taskSel === "" && <Check size={14} className="shrink-0 text-accent" />}
      </button>

      {/* Existing tasks (workspace-scoped, open / in review) */}
      {!connected ? (
        <p className="mb-1.5 px-1 py-2 text-[11px] text-zinc-600">
          daemon unreachable · retrying
        </p>
      ) : tasks === null ? (
        <p className="mb-1.5 px-1 py-2 text-[11px] text-zinc-600">
          Loading tasks…
        </p>
      ) : tasks.length === 0 ? (
        <p className="mb-1.5 px-1 py-2 text-[11px] text-zinc-600">
          No open tasks in this workspace.
        </p>
      ) : (
        <div className="mb-1.5 flex max-h-56 flex-col gap-1.5 overflow-y-auto">
          {tasks.map((t) => {
            const active = taskSel === t.id;
            return (
              <button
                key={t.id}
                onClick={() => setTaskSel(t.id)}
                aria-pressed={active}
                className={taskCard(active)}
              >
                <span
                  className={`flex h-7 w-7 shrink-0 items-center justify-center rounded-full ${
                    active ? "bg-accent/15 text-accent" : "bg-ink-600 text-zinc-500"
                  }`}
                >
                  <Layers size={13} />
                </span>
                <span className="flex min-w-0 flex-1 flex-col">
                  <span
                    className="truncate text-sm font-medium text-zinc-100"
                    title={t.title}
                  >
                    {t.title}
                  </span>
                  <span className="tnum font-mono text-[11px] text-zinc-500">
                    {t.agent_count} agent{t.agent_count === 1 ? "" : "s"}
                  </span>
                </span>
                {t.status === "in_review" && (
                  <span className="shrink-0 rounded-full bg-amber/15 px-1.5 text-[10px] text-amber">
                    in review
                  </span>
                )}
                {active && <Check size={14} className="shrink-0 text-accent" />}
              </button>
            );
          })}
        </div>
      )}

      {/* Create a task inline at launch time */}
      <button
        onClick={() => setTaskSel("__new__")}
        aria-pressed={taskSel === "__new__"}
        className={taskCard(taskSel === "__new__")}
      >
        <span
          className={`flex h-7 w-7 shrink-0 items-center justify-center rounded-full ${
            taskSel === "__new__"
              ? "bg-accent/15 text-accent"
              : "bg-ink-600 text-zinc-500"
          }`}
        >
          <Plus size={13} />
        </span>
        <span className="text-sm font-medium text-zinc-100">
          Create new task
        </span>
      </button>
      {taskSel === "__new__" && (
        <input
          type="text"
          value={newTaskTitle}
          onChange={(e) => setNewTaskTitle(e.target.value)}
          placeholder="Task title"
          autoFocus
          className={`mt-1.5 w-full rounded-lg border border-ink-500 bg-ink-700 px-3 py-2 text-sm text-zinc-200 placeholder:text-zinc-600 ${FOCUS_RING}`}
        />
      )}
    </>
  );

  // ── Step 2: config ─────────────────────────────────────────────────────────
  const step2 = (
    <>
      {/* Task context — Change returns to step 1 */}
      <div className="mb-4 flex items-center gap-2 rounded-lg border border-ink-600 bg-ink-700/40 px-3 py-2">
        {taskSel === "" ? (
          <Minus size={13} className="shrink-0 text-zinc-500" />
        ) : (
          <Layers size={13} className="shrink-0 text-zinc-500" />
        )}
        <span
          className="min-w-0 flex-1 truncate text-xs text-zinc-300"
          title={taskLabel}
        >
          {taskLabel}
        </span>
        {hasWorkspace && (
          <button
            onClick={() => setStep(1)}
            className={`shrink-0 rounded px-1.5 py-0.5 text-[11px] text-zinc-500 hover:text-zinc-300 ${FOCUS_RING}`}
          >
            Change
          </button>
        )}
      </div>

      {/* Profile */}
      <label className="mb-1.5 block text-[11px] font-medium uppercase tracking-wide text-zinc-500">
        Profile
      </label>
      <div className="mb-4 grid grid-cols-2 gap-1.5">
        {displayProfiles.map((p) => {
          const m = profileMeta(p.name, p.description);
          const Icon = m.icon;
          const active = profile === p.name;
          return (
            <button
              key={p.name}
              onClick={() => setProfile(p.name)}
              aria-pressed={active}
              title={`${m.label} — ${m.desc}`}
              className={`flex items-start gap-2 rounded-lg border px-2.5 py-2 text-left transition-colors ${
                active
                  ? "border-accent bg-accent/10"
                  : "border-ink-500 bg-ink-700/40 hover:border-ink-400"
              } ${FOCUS_RING}`}
            >
              <span
                className={`flex h-6 w-6 shrink-0 items-center justify-center rounded ${
                  active ? "bg-accent/15 text-accent" : "bg-ink-600 text-zinc-500"
                }`}
              >
                <Icon size={13} />
              </span>
              <span className="flex min-w-0 flex-col">
                <span className="truncate text-xs font-medium text-zinc-100">
                  {m.label}
                </span>
                <span className="truncate text-[11px] text-zinc-500">
                  {m.desc}
                </span>
              </span>
            </button>
          );
        })}
      </div>

      {/* Assignment — the per-agent intent */}
      <label className="mb-1.5 block text-[11px] font-medium uppercase tracking-wide text-zinc-500">
        Assignment
      </label>
      <textarea
        value={assignment}
        onChange={(e) => setAssignment(e.target.value)}
        placeholder="What should this agent do?"
        rows={3}
        autoFocus
        className={`mb-4 w-full resize-y rounded-lg border border-ink-500 bg-ink-700 px-3 py-2 text-sm leading-relaxed text-zinc-200 placeholder:text-zinc-600 ${FOCUS_RING}`}
      />

      {/* Provider */}
      <label className="mb-1.5 block text-[11px] font-medium uppercase tracking-wide text-zinc-500">
        Provider
      </label>
      {providers === null ? (
        <p className="mb-4 text-xs text-zinc-600">
          {connected ? "Loading providers…" : "daemon unreachable · retrying"}
        </p>
      ) : providers.length === 0 ? (
        <p className="mb-4 text-xs text-amber">
          {connected
            ? "No installed CLIs detected by the backend."
            : "daemon unreachable · retrying"}
        </p>
      ) : (
        <select
          value={selected ?? ""}
          onChange={(e) => setSelected(e.target.value)}
          className={`mb-4 w-full rounded-lg border border-ink-500 bg-ink-700 px-3 py-2 text-sm text-zinc-200 ${FOCUS_RING}`}
        >
          {providers.map((p) => (
            <option key={p.name} value={p.name}>
              {providerTitle(p.name)}
            </option>
          ))}
        </select>
      )}

      {/* Worktree mode */}
      <label className="mb-1.5 block text-[11px] font-medium uppercase tracking-wide text-zinc-500">
        Worktree
      </label>
      <div className="mb-1.5 flex rounded-lg border border-ink-500 bg-ink-700 p-0.5">
        {(["isolated", "shared"] as const).map((m) => (
          <button
            key={m}
            onClick={() => setWtMode(m)}
            aria-pressed={wtMode === m}
            className={`flex-1 rounded-md px-2 py-1 text-xs capitalize transition-colors ${
              wtMode === m
                ? "bg-ink-500 text-zinc-100"
                : "text-zinc-500 hover:text-zinc-300"
            } ${FOCUS_RING}`}
          >
            {m}
          </button>
        ))}
      </div>
      <p className="mb-4 text-[11px] text-zinc-600">
        {wtMode === "isolated"
          ? "Own git worktree — changes attributed to this agent alone."
          : "Shares the workspace checkout — files contended with other agents."}
      </p>

      {/* Preview */}
      <p className="rounded-lg bg-ink-900/60 px-3 py-2 font-mono text-[11px] leading-relaxed text-zinc-500">
        Launches <span className="text-zinc-300">{profileLabel}</span> on{" "}
        <span className="text-zinc-300">
          {selected ? providerTitle(selected) : "—"}
        </span>{" "}
        · {wtMode} worktree · Task:{" "}
        <span
          className="inline-block max-w-[11rem] truncate align-bottom text-zinc-300"
          title={taskLabel}
        >
          {taskLabel}
        </span>
      </p>
      {!hasWorkspace && (
        <p className="mt-2 text-[11px] text-zinc-600">
          No workspace open — runs in the backend default directory; tasks
          unavailable.
        </p>
      )}
    </>
  );

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
      onClick={onClose}
    >
      <div
        ref={boxRef}
        tabIndex={-1}
        onKeyDown={handleKey}
        className="no-drag flex max-h-[85vh] w-[32rem] flex-col rounded-xl border border-ink-500 bg-ink-800 shadow-2xl outline-none"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex shrink-0 items-center gap-3 border-b border-ink-600 px-5 py-3.5">
          <h2 className="text-sm font-semibold text-zinc-100">Launch agent</h2>
          <span className="flex-1" />
          {hasWorkspace && (
            <span className="flex items-center gap-1 text-[11px]">
              <button
                onClick={() => setStep(1)}
                disabled={step === 1}
                className={`rounded px-0.5 ${
                  step === 1
                    ? "font-medium text-zinc-200"
                    : "text-zinc-600 hover:text-zinc-400"
                } ${FOCUS_RING}`}
              >
                Task
              </button>
              <ChevronRight size={11} className="text-zinc-600" />
              <span
                className={step === 2 ? "font-medium text-zinc-200" : "text-zinc-600"}
              >
                Config
              </span>
            </span>
          )}
          <button
            onClick={onClose}
            aria-label="Close"
            className={`rounded p-1 text-zinc-500 hover:text-zinc-200 ${FOCUS_RING}`}
          >
            <X size={16} />
          </button>
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto p-5">
          {step === 1 ? step1 : step2}
        </div>

        <div className="flex shrink-0 items-center gap-2 border-t border-ink-600 px-5 py-3">
          {step === 2 && hasWorkspace && (
            <button
              onClick={() => setStep(1)}
              className={`flex items-center gap-1 rounded-lg px-2 py-1.5 text-sm text-zinc-400 hover:text-zinc-200 ${FOCUS_RING}`}
            >
              <ArrowLeft size={13} />
              Back
            </button>
          )}
          <span className="flex-1" />
          <button
            onClick={onClose}
            className={`rounded-lg px-3 py-1.5 text-sm text-zinc-400 hover:text-zinc-200 ${FOCUS_RING}`}
          >
            Cancel
          </button>
          {step === 1 ? (
            <button
              onClick={() => setStep(2)}
              disabled={!canContinue}
              className={`rounded-lg bg-primary px-3 py-1.5 text-sm font-medium text-white hover:bg-primary-hover disabled:opacity-50 ${FOCUS_RING}`}
            >
              Continue
            </button>
          ) : (
            <button
              onClick={() => void submit()}
              // The dialog owns this gate: palette/shortcut entry points don't
              // go through the (already-gated) sidebar/dashboard buttons.
              disabled={!selected || busy || !connected}
              title={connected ? undefined : "Daemon unreachable"}
              className={`rounded-lg bg-primary px-3 py-1.5 text-sm font-medium text-white hover:bg-primary-hover disabled:opacity-50 ${FOCUS_RING}`}
            >
              {busy ? "Launching…" : "Launch"}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
