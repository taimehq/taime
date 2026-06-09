import { useEffect, useState } from "react";
import {
  AlertTriangle,
  FolderOpen,
  GitBranch,
  Loader2,
  Sparkles,
  X,
} from "lucide-react";
import { api, type ProviderInfo, type WorkspaceInfo } from "../api";
import { useStore } from "../store";
import { providerTitle, PROVIDER_ORDER } from "../lib/providerLabel";
import { pickDirectory } from "../lib/pickDirectory";
import { inTauri } from "../backend";
import { middleTruncate } from "../lib/format";
import { composeSeedPrompt, seedTaskTitle } from "../lib/seedPrompt";

const FOCUS_RING =
  "focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-accent";

/**
 * "Start something new" — the generative new-workspace experience. One pane:
 * point at a folder (the only required input), optionally say what you're
 * building, press Start. A founding agent launches IN that folder and begins
 * working; structure (a Task, teammates, maybe a Workflow) grows as it does.
 *
 * What it actually does (all on today's daemon — no backend changes):
 *  - The folder is SELECTED as the active workspace (Workspace = the active
 *    dir; there is nothing to "create" daemon-side). Phase: a future
 *    workspace_init RPC would let us mkdir the folder ourselves.
 *  - With an intent: create a Seed Task (api.createTask), then launch the
 *    built-in `orchestrator` profile with composeSeedPrompt(intent) as its
 *    first prompt. The orchestrator carries the MCP tools; the prompt carries
 *    the build/scaffold/commit-early directive + emergence budget.
 *  - Blank intent (the zero-intent escape — never a toll booth): launch a plain
 *    `default` agent with no assignment, no Seed Task.
 *
 * No Approve step: review happens by navigating into the real artifacts (the
 * Task, the agent's diffs, a generated Workflow) — each in its own Section.
 */
export function SeedDialog({ onClose }: { onClose: () => void }) {
  const connected = useStore((s) => s.connected);
  const isolationEnabled = useStore((s) => s.isolationEnabled);

  const [folder, setFolder] = useState<string | null>(null);
  const [manualMode, setManualMode] = useState(false);
  const [wsInfo, setWsInfo] = useState<WorkspaceInfo | null>(null);
  const [intent, setIntent] = useState("");
  const [providers, setProviders] = useState<ProviderInfo[] | null>(null);
  const [provider, setProvider] = useState<string | null>(null);
  const [isolate, setIsolate] = useState(isolationEnabled);
  const [busy, setBusy] = useState(false);

  // Installed providers (re-fetch on reconnect so a daemon restart doesn't
  // strand a stale list) — same pattern as the launcher / workflow dialog.
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
        setProvider((prev) => prev ?? ordered[0]?.name ?? null);
      })
      .catch(() => setProviders([]));
  }, [connected]);

  // Probe the chosen folder so the badge tells the truth about isolation
  // (an empty/new folder provisions in SHARED mode until the first commit).
  useEffect(() => {
    if (!folder) {
      setWsInfo(null);
      return;
    }
    let alive = true;
    api
      .getWorkspaceInfo(folder)
      .then((info) => {
        if (alive) setWsInfo(info);
      })
      .catch(() => {
        if (alive) setWsInfo(null);
      });
    return () => {
      alive = false;
    };
  }, [folder]);

  // A folder that is already a git repo WITH history is established work, not a
  // brand-new project — nudge toward opening it as a workspace + a Task instead.
  const reseed = !!wsInfo?.is_git && !!wsInfo?.head_short;

  const chooseFolder = async () => {
    const dir = await pickDirectory(folder ?? undefined);
    if (dir) {
      setFolder(dir);
      setManualMode(false);
    } else if (!inTauri()) {
      // No native dialog in the dev browser — reveal the manual-path fallback.
      setManualMode(true);
    }
  };

  const canStart = !!folder && !!provider && connected && !busy;

  const start = async () => {
    if (!folder || !provider || busy) return;
    setBusy(true);
    const s = useStore.getState();
    const trimmed = intent.trim();

    // Ensure the folder exists (a typed / brand-new path), then activate it as
    // the workspace. Idempotent + git_init=false — the founding agent runs its
    // own git init so genesis stays attributed to its turn.
    await api.initWorkspace(folder, false);
    // Activate the folder as the workspace (persist + recents + derived list).
    s.switchWorkspace(folder);

    // With an intent, mint the Seed Task BEFORE the optimistic close so a failed
    // creation aborts loudly instead of launching an orphaned founding agent.
    let taskId: string | null = null;
    if (trimmed) {
      const task = await api.createTask(folder, seedTaskTitle(trimmed), trimmed);
      if (!task) {
        setBusy(false);
        s.showSnackbar({
          type: "error",
          message: "Couldn't create the seed task — nothing launched.",
        });
        return;
      }
      taskId = task.id;
    }

    // Worktree mode rides the existing isolation flag — provision reads it.
    if (s.isolationEnabled !== isolate) s.setIsolationEnabled(isolate);

    onClose(); // optimistic: the founding agent's frame appears as it spawns
    // Land the user ON the founding agent's terminal — that attaches its view,
    // which is what lets the first-prompt (assignment) delivery actually fire.
    s.setSection("agents");

    const profile = trimmed ? "orchestrator" : "default";
    const assignment = trimmed ? composeSeedPrompt(trimmed) : null;
    await s.launchAgent(provider, profile, {
      taskId,
      workingDirectory: folder,
      assignment,
      // The founding agent should start working whether or not you're watching
      // its terminal — deliver the seed prompt via the daemon inbox, not the
      // mount-dependent keystroke path.
      seedViaInbox: true,
    });

    // launchAgent surfaced its own error snackbar on failure — don't mask it.
    const after = useStore.getState();
    if (after.snackbar?.type !== "error")
      after.showSnackbar({
        type: "info",
        message: trimmed
          ? "Founding agent launched — it'll start building once its terminal is ready."
          : "Agent launched in your new workspace.",
      });
  };

  /** Re-seed escape: open the folder as a workspace and start a Task instead. */
  const openAsWorkspace = () => {
    if (!folder) return;
    const s = useStore.getState();
    s.switchWorkspace(folder);
    onClose();
    s.setNewTaskOpen(true);
  };

  const handleKey = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onClose();
      return;
    }
    if (e.key !== "Enter") return;
    const tag = (e.target as HTMLElement).tagName;
    if (tag === "BUTTON" || tag === "SELECT") return; // native activation
    if (tag === "TEXTAREA" && !(e.metaKey || e.ctrlKey)) return; // newline
    e.preventDefault();
    if (canStart) void start();
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
      onClick={onClose}
    >
      <div
        onKeyDown={handleKey}
        className="no-drag flex max-h-[85vh] w-[34rem] flex-col rounded-xl border border-ink-500 bg-ink-800 shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex shrink-0 items-center gap-2.5 border-b border-ink-600 px-5 py-3.5">
          <Sparkles size={15} className="shrink-0 text-accent" />
          <h2 className="text-sm font-semibold text-zinc-100">
            Start something new
          </h2>
          <span className="flex-1" />
          <button
            onClick={onClose}
            aria-label="Close"
            className={`rounded p-1 text-zinc-500 hover:text-zinc-200 ${FOCUS_RING}`}
          >
            <X size={16} />
          </button>
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto p-5">
          <p className="mb-4 text-[11px] leading-relaxed text-zinc-500">
            Point at a folder and go. A founding agent starts working there;
            structure grows as the work does.
          </p>

          {/* Where it lives — the only required input */}
          <label className="mb-1.5 block text-[11px] font-medium uppercase tracking-wide text-zinc-500">
            Where should it live?
          </label>
          {folder && !manualMode ? (
            <div className="mb-1.5 flex items-center gap-2 rounded-lg border border-ink-500 bg-ink-700/40 px-3 py-2">
              <FolderOpen size={14} className="shrink-0 text-zinc-500" />
              <span
                className="min-w-0 flex-1 truncate font-mono text-xs text-zinc-200"
                title={folder}
              >
                {middleTruncate(folder, 46)}
              </span>
              <button
                onClick={() => void chooseFolder()}
                disabled={busy}
                className={`shrink-0 rounded px-1.5 py-0.5 text-[11px] text-zinc-500 hover:text-zinc-300 disabled:opacity-50 ${FOCUS_RING}`}
              >
                Change
              </button>
            </div>
          ) : manualMode ? (
            <input
              type="text"
              value={folder ?? ""}
              onChange={(e) => setFolder(e.target.value || null)}
              placeholder="/absolute/path/to/new-project"
              autoFocus
              disabled={busy}
              className={`mb-1.5 w-full rounded-lg border border-ink-500 bg-ink-700 px-3 py-2 font-mono text-xs text-zinc-200 placeholder:text-zinc-600 disabled:opacity-60 ${FOCUS_RING}`}
            />
          ) : (
            <button
              onClick={() => void chooseFolder()}
              disabled={busy}
              className={`mb-1.5 flex w-full items-center gap-2 rounded-lg border border-dashed border-ink-500 bg-ink-700/40 px-3 py-2.5 text-left text-xs text-zinc-400 hover:border-ink-400 hover:text-zinc-200 disabled:opacity-60 ${FOCUS_RING}`}
            >
              <FolderOpen size={14} className="shrink-0 text-zinc-500" />
              Choose folder…
            </button>
          )}

          {/* Honest provisioning badge */}
          {folder && wsInfo && (
            <div className="mb-4">
              {reseed ? (
                <div className="flex items-start gap-2 rounded-lg bg-amber/10 px-3 py-2 text-[11px] leading-relaxed text-amber">
                  <AlertTriangle size={12} className="mt-0.5 shrink-0" />
                  <span className="min-w-0">
                    This folder is already a git repo with history
                    {wsInfo.branch ? ` (${wsInfo.branch})` : ""}. That's
                    established work, not a brand-new project —{" "}
                    <button
                      onClick={openAsWorkspace}
                      className="underline underline-offset-2 hover:text-amber/80"
                    >
                      open it as a workspace and add a task
                    </button>{" "}
                    instead, or start fresh anyway.
                  </span>
                </div>
              ) : !wsInfo.exists ? (
                <p className="text-[11px] text-zinc-600">
                  New folder — the founding agent will scaffold it and{" "}
                  <span className="font-mono">git init</span> on its first turn.
                </p>
              ) : (
                <p className="flex items-center gap-1.5 text-[11px] text-zinc-600">
                  <GitBranch size={11} className="shrink-0" />
                  New project · runs in shared mode until the first commit, then
                  per-agent isolation sharpens.
                </p>
              )}
            </div>
          )}

          {/* What are you building — optional (the zero-intent escape) */}
          <label className="mb-1.5 block text-[11px] font-medium uppercase tracking-wide text-zinc-500">
            What are you building?{" "}
            <span className="font-normal normal-case text-zinc-600">
              · optional
            </span>
          </label>
          <textarea
            value={intent}
            onChange={(e) => setIntent(e.target.value)}
            placeholder="e.g. A CLI that converts Markdown files to styled PDFs. Leave blank to just launch an agent and type your own first prompt."
            rows={3}
            disabled={busy}
            className={`mb-1.5 w-full resize-y rounded-lg border border-ink-500 bg-ink-700 px-3 py-2 text-sm leading-relaxed text-zinc-200 placeholder:text-zinc-600 disabled:opacity-60 ${FOCUS_RING}`}
          />
          <p className="mb-4 text-[11px] text-zinc-600">
            {intent.trim()
              ? "Launches an orchestrator that scaffolds the project, commits early, and grows a small team only as needed."
              : "Blank → a plain agent in the new workspace; you drive it yourself."}
          </p>

          {/* Build with — the founding agent's CLI */}
          <label className="mb-1.5 block text-[11px] font-medium uppercase tracking-wide text-zinc-500">
            Build with
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
              value={provider ?? ""}
              onChange={(e) => setProvider(e.target.value)}
              disabled={busy}
              className={`mb-4 w-full rounded-lg border border-ink-500 bg-ink-700 px-3 py-2 text-sm text-zinc-200 disabled:opacity-60 ${FOCUS_RING}`}
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
            {(["isolated", "shared"] as const).map((m) => {
              const active = isolate === (m === "isolated");
              return (
                <button
                  key={m}
                  onClick={() => setIsolate(m === "isolated")}
                  aria-pressed={active}
                  disabled={busy}
                  className={`flex-1 rounded-md px-2 py-1 text-xs capitalize transition-colors disabled:opacity-60 ${
                    active
                      ? "bg-ink-500 text-zinc-100"
                      : "text-zinc-500 hover:text-zinc-300"
                  } ${FOCUS_RING}`}
                >
                  {m}
                </button>
              );
            })}
          </div>
          <p className="text-[11px] text-zinc-600">
            {isolate
              ? "Own git worktree once there's a commit to fork from — changes attributed to this agent alone."
              : "Shares the workspace checkout — simplest for a single founding agent."}
          </p>
        </div>

        <div className="flex shrink-0 items-center justify-end gap-2 border-t border-ink-600 px-5 py-3">
          {!connected && (
            <span className="mr-auto text-[11px] text-zinc-600">
              daemon unreachable · retrying
            </span>
          )}
          <button
            onClick={onClose}
            className={`rounded-lg px-3 py-1.5 text-sm text-zinc-400 hover:text-zinc-200 ${FOCUS_RING}`}
          >
            Cancel
          </button>
          <button
            onClick={() => void start()}
            disabled={!canStart}
            title={
              !folder
                ? "Choose a folder first"
                : connected
                  ? "Launch the founding agent"
                  : "Daemon unreachable"
            }
            className={`flex items-center gap-1.5 rounded-lg bg-primary px-3 py-1.5 text-sm font-medium text-white hover:bg-primary-hover disabled:opacity-50 ${FOCUS_RING}`}
          >
            {busy ? <Loader2 size={13} className="animate-spin" /> : <Sparkles size={13} />}
            {busy ? "Starting…" : "Start"}
          </button>
        </div>
      </div>
    </div>
  );
}
