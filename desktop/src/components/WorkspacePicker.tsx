import { useEffect, useState } from "react";
import {
  Folder,
  FolderSearch,
  GitBranch,
  X,
  ChevronDown,
  ChevronRight,
  AlertTriangle,
  Keyboard,
  Clock,
} from "lucide-react";
import { useStore } from "../store";
import { api, type WorkspaceInfo } from "../api";
import { inTauri } from "../backend";
import { pickDirectory } from "../lib/pickDirectory";
import { basename, dirname } from "../lib/recentProjects";

/**
 * Workspace selector: native folder picker (Tauri), a persisted most-recent
 * history, live git/isolation status, and a manual-path fallback for the dev
 * browser. Replaces the old "type an absolute path" input.
 */
export function WorkspacePicker() {
  const workspaceDir = useStore((s) => s.workspaceDir);
  const setWorkspaceDir = useStore((s) => s.setWorkspaceDir);
  const recentProjects = useStore((s) => s.recentProjects);
  const removeRecentProject = useStore((s) => s.removeRecentProject);
  const clearRecentProjects = useStore((s) => s.clearRecentProjects);
  const isolationEnabled = useStore((s) => s.isolationEnabled);
  const setIsolationEnabled = useStore((s) => s.setIsolationEnabled);

  const [info, setInfo] = useState<WorkspaceInfo | null>(null);
  const [infoLoading, setInfoLoading] = useState(false);
  const [manualOpen, setManualOpen] = useState(false);
  const [draft, setDraft] = useState("");
  const [recentsOpen, setRecentsOpen] = useState(true);

  // Probe the current workspace for git/isolation status + existence.
  useEffect(() => {
    if (!workspaceDir) {
      setInfo(null);
      return;
    }
    let cancelled = false;
    setInfoLoading(true);
    api
      .getWorkspaceInfo(workspaceDir)
      .then((i) => !cancelled && setInfo(i))
      .catch(() => !cancelled && setInfo(null))
      .finally(() => !cancelled && setInfoLoading(false));
    return () => {
      cancelled = true;
    };
  }, [workspaceDir]);

  const browse = async () => {
    const dir = await pickDirectory(workspaceDir ?? undefined);
    if (dir) {
      setWorkspaceDir(dir);
      setManualOpen(false);
    } else if (!inTauri()) {
      // No native dialog in the dev browser → reveal manual entry.
      setManualOpen(true);
      setDraft(workspaceDir ?? "");
    }
  };

  const submitManual = () => {
    setWorkspaceDir(draft.trim() || null);
    setManualOpen(false);
  };

  const others = recentProjects.filter((p) => p !== workspaceDir);

  return (
    <div className="flex flex-col gap-2">
      <h2 className="text-[11px] font-semibold uppercase tracking-wider text-zinc-500">
        Workspace
      </h2>

      {/* Current project card */}
      {workspaceDir ? (
        <div className="rounded-lg border border-ink-500 bg-ink-700/40 p-2.5">
          <div className="flex items-start gap-2">
            <Folder size={15} className="mt-0.5 shrink-0 text-teal" />
            <div className="min-w-0 flex-1">
              <div className="truncate text-sm font-medium text-zinc-100" title={workspaceDir}>
                {basename(workspaceDir)}
              </div>
              <div className="truncate font-mono text-[10px] text-zinc-600" title={workspaceDir}>
                {dirname(workspaceDir)}
              </div>
            </div>
          </div>
          <div className="mt-2 flex items-center justify-between gap-2">
            <WorkspaceBadge info={info} loading={infoLoading} />
            <button
              onClick={browse}
              className="no-drag shrink-0 rounded-md border border-ink-500 px-2 py-1 text-[11px] text-zinc-300 hover:bg-ink-600"
            >
              Change…
            </button>
          </div>
        </div>
      ) : (
        <button
          onClick={browse}
          className="no-drag flex items-center justify-center gap-2 rounded-lg border border-dashed border-ink-500 bg-ink-700/30 px-3 py-3 text-sm text-zinc-300 hover:border-teal/60 hover:text-zinc-100"
        >
          <FolderSearch size={16} className="text-teal" />
          Choose project folder
        </button>
      )}

      {/* Recent projects */}
      {others.length > 0 && (
        <div className="mt-0.5">
          <div className="flex items-center justify-between">
            <button
              onClick={() => setRecentsOpen((v) => !v)}
              className="flex items-center gap-1 text-[10px] font-semibold uppercase tracking-wider text-zinc-600 hover:text-zinc-400"
            >
              {recentsOpen ? <ChevronDown size={11} /> : <ChevronRight size={11} />}
              <Clock size={10} />
              Recent
            </button>
            <button
              onClick={clearRecentProjects}
              className="text-[10px] text-zinc-600 hover:text-zinc-400"
            >
              Clear
            </button>
          </div>
          {recentsOpen && (
            <ul className="mt-1 flex flex-col gap-0.5">
              {others.map((p) => (
                <li key={p} className="group flex items-center">
                  <button
                    onClick={() => setWorkspaceDir(p)}
                    title={p}
                    className="no-drag flex min-w-0 flex-1 items-center gap-2 rounded-md px-2 py-1.5 text-left hover:bg-ink-600/50"
                  >
                    <Folder size={12} className="shrink-0 text-zinc-600" />
                    <span className="flex min-w-0 flex-col">
                      <span className="truncate text-[12px] text-zinc-300">{basename(p)}</span>
                      <span className="truncate font-mono text-[9px] text-zinc-600">
                        {dirname(p)}
                      </span>
                    </span>
                  </button>
                  <button
                    onClick={() => removeRecentProject(p)}
                    aria-label={`Remove ${p} from recent`}
                    className="ml-0.5 shrink-0 rounded p-1 text-zinc-700 opacity-0 hover:text-zinc-300 group-hover:opacity-100"
                  >
                    <X size={12} />
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}

      {/* Manual entry (dev browser / power users) */}
      {manualOpen ? (
        <div className="flex flex-col gap-2 rounded-lg border border-ink-600 bg-ink-900/60 p-2">
          <input
            autoFocus
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            placeholder="/absolute/path/to/project"
            className="w-full rounded-md border border-ink-500 bg-ink-700 px-2.5 py-1.5 font-mono text-xs text-zinc-200"
            onKeyDown={(e) => {
              if (e.key === "Enter") submitManual();
              if (e.key === "Escape") setManualOpen(false);
            }}
          />
          <div className="flex gap-2">
            <button
              onClick={submitManual}
              className="rounded bg-ink-500 px-2 py-1 text-xs text-zinc-200 hover:bg-ink-400"
            >
              Set
            </button>
            <button
              onClick={() => setManualOpen(false)}
              className="rounded px-2 py-1 text-xs text-zinc-500 hover:text-zinc-300"
            >
              Cancel
            </button>
          </div>
        </div>
      ) : (
        <button
          onClick={() => {
            setManualOpen(true);
            setDraft(workspaceDir ?? "");
          }}
          className="flex items-center gap-1.5 self-start text-[10px] text-zinc-600 hover:text-zinc-400"
        >
          <Keyboard size={11} />
          Enter path manually
        </button>
      )}

      {/* Isolation toggle */}
      <label className="mt-1.5 flex cursor-pointer items-start gap-2">
        <input
          type="checkbox"
          checked={isolationEnabled}
          onChange={(e) => setIsolationEnabled(e.target.checked)}
          className="mt-0.5 accent-teal"
        />
        <span className="text-[11px] leading-relaxed text-zinc-400">
          <span className="font-medium text-zinc-300">Isolate agents in worktrees</span>
          <span className="block text-zinc-600">
            {isolationEnabled
              ? "Each agent works its own branch off this repo — changes are provably attributed and safe to review before they land."
              : "Shared dir: all agents edit the same files (live collaboration). Attribution becomes heuristic."}
          </span>
        </span>
      </label>
    </div>
  );
}

function WorkspaceBadge({ info, loading }: { info: WorkspaceInfo | null; loading: boolean }) {
  if (loading) {
    return <span className="text-[10px] text-zinc-600">checking…</span>;
  }
  if (!info) return <span className="text-[10px] text-zinc-700">—</span>;
  if (!info.exists) {
    return (
      <span className="flex items-center gap-1 rounded-md border border-rose-500/40 bg-rose-500/10 px-1.5 py-0.5 text-[10px] text-rose-300">
        <AlertTriangle size={10} /> folder not found
      </span>
    );
  }
  if (info.is_git) {
    return (
      <span className="flex items-center gap-1 rounded-md border border-sky-500/30 bg-sky-500/10 px-1.5 py-0.5 font-mono text-[10px] text-sky-300">
        <GitBranch size={10} /> {info.branch ?? "detached"}
        <span className="text-sky-500/70">· isolates</span>
      </span>
    );
  }
  return (
    <span className="rounded-md border border-amber/40 bg-amber/10 px-1.5 py-0.5 text-[10px] text-amber">
      not a git repo · shared mode
    </span>
  );
}
