import { useEffect, useState } from "react";
import {
  Folder,
  FolderSearch,
  GitBranch,
  Trash2,
  ChevronDown,
  AlertTriangle,
  Keyboard,
  Info,
} from "lucide-react";
import { useStore } from "../store";
import { api, type WorkspaceInfo } from "../api";
import { inTauri } from "../backend";
import { pickDirectory } from "../lib/pickDirectory";
import { basename, dirname } from "../lib/recentProjects";

/**
 * Workspace selector: one compact tappable row (folder + git/mode chip) that
 * opens a disclosure for browse / recents / manual-path. The isolation toggle
 * sits below as a single line with the long explanation on hover.
 */
export function WorkspacePicker() {
  const workspaceDir = useStore((s) => s.workspaceDir);
  const setWorkspaceDir = useStore((s) => s.setWorkspaceDir);
  const recentProjects = useStore((s) => s.recentProjects);
  const setDeleteWorkspaceTarget = useStore((s) => s.setDeleteWorkspaceTarget);
  const clearRecentProjects = useStore((s) => s.clearRecentProjects);
  const isolationEnabled = useStore((s) => s.isolationEnabled);
  const setIsolationEnabled = useStore((s) => s.setIsolationEnabled);

  const [info, setInfo] = useState<WorkspaceInfo | null>(null);
  const [infoLoading, setInfoLoading] = useState(false);
  const [changeOpen, setChangeOpen] = useState(false);
  const [manualOpen, setManualOpen] = useState(false);
  const [draft, setDraft] = useState("");

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
      setChangeOpen(false);
    } else if (!inTauri()) {
      // No native dialog in the dev browser → reveal manual entry.
      setManualOpen(true);
      setDraft(workspaceDir ?? "");
    }
  };

  const submitManual = () => {
    setWorkspaceDir(draft.trim() || null);
    setManualOpen(false);
    setChangeOpen(false);
  };

  const others = recentProjects.filter((p) => p !== workspaceDir);

  return (
    <div className="flex flex-col gap-2">
      {/* Current workspace — one tappable row */}
      {workspaceDir ? (
        <button
          onClick={() => setChangeOpen((v) => !v)}
          className="no-drag group flex items-center gap-2 rounded-lg border border-ink-600 bg-ink-700/30 px-2.5 py-2 text-left hover:border-ink-500"
          title={workspaceDir}
        >
          <Folder size={15} className="shrink-0 text-teal" />
          <span className="flex min-w-0 flex-1 flex-col">
            <span className="truncate text-[13px] font-medium text-zinc-100">
              {basename(workspaceDir)}
            </span>
            <span className="truncate font-mono text-[10px] text-zinc-600">
              {dirname(workspaceDir)}
            </span>
          </span>
          <WorkspaceBadge info={info} loading={infoLoading} />
          <ChevronDown
            size={14}
            className={`shrink-0 text-zinc-600 transition-transform ${changeOpen ? "rotate-180" : ""}`}
          />
        </button>
      ) : (
        <button
          onClick={browse}
          className="no-drag flex items-center justify-center gap-2 rounded-lg border border-dashed border-ink-500 bg-ink-700/30 px-3 py-3 text-sm text-zinc-300 hover:border-teal/60 hover:text-zinc-100"
        >
          <FolderSearch size={16} className="text-teal" />
          Choose project folder
        </button>
      )}

      {/* Change disclosure: browse · recent · manual */}
      {changeOpen && (
        <div className="flex flex-col gap-1 rounded-lg border border-ink-600 bg-ink-900/40 p-1.5">
          <button
            onClick={browse}
            className="no-drag flex items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs text-zinc-200 hover:bg-ink-600/50"
          >
            <FolderSearch size={13} className="text-teal" />
            Browse for folder…
          </button>

          {others.length > 0 && (
            <div className="flex flex-col">
              <div className="flex items-center justify-between px-2 pb-0.5 pt-1.5">
                <span className="text-[10px] font-semibold uppercase tracking-wider text-zinc-600">
                  Recent
                </span>
                <button
                  onClick={clearRecentProjects}
                  className="text-[10px] text-zinc-600 hover:text-zinc-400"
                >
                  Clear
                </button>
              </div>
              <ul className="flex flex-col gap-0.5">
                {others.map((p) => (
                  <li key={p} className="group/recent flex items-center">
                    <button
                      onClick={() => {
                        setWorkspaceDir(p);
                        setChangeOpen(false);
                      }}
                      title={p}
                      className="no-drag flex min-w-0 flex-1 items-center gap-2 rounded-md px-2 py-1.5 text-left hover:bg-ink-600/50"
                    >
                      <Folder size={12} className="shrink-0 text-zinc-600" />
                      <span className="truncate text-[12px] text-zinc-300">
                        {basename(p)}
                      </span>
                    </button>
                    <button
                      onClick={() => setDeleteWorkspaceTarget(p)}
                      aria-label={`Delete workspace ${p}`}
                      title="Delete workspace…"
                      className="ml-0.5 shrink-0 rounded p-1 text-zinc-700 opacity-0 hover:text-red-400 group-hover/recent:opacity-100"
                    >
                      <Trash2 size={12} />
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          )}

          {manualOpen ? (
            <div className="flex flex-col gap-2 p-1">
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
              className="flex items-center gap-1.5 self-start px-2 py-1 text-[10px] text-zinc-600 hover:text-zinc-400"
            >
              <Keyboard size={11} />
              Enter path manually
            </button>
          )}

          {workspaceDir && (
            <>
              <div className="mx-1 my-0.5 h-px bg-ink-700" />
              <button
                onClick={() => setDeleteWorkspaceTarget(workspaceDir)}
                className="flex items-center gap-1.5 self-start rounded-md px-2 py-1 text-[11px] text-zinc-600 hover:text-red-400"
              >
                <Trash2 size={11} />
                Delete this workspace…
              </button>
            </>
          )}
        </div>
      )}

      {/* Isolation toggle — one line, detail on hover */}
      <label
        className="mt-0.5 flex cursor-pointer items-center gap-2"
        title={
          isolationEnabled
            ? "Each agent works its own branch off this repo — changes are provably attributed and safe to review before they land."
            : "Shared dir: all agents edit the same files (live collaboration). Attribution becomes heuristic."
        }
      >
        <input
          type="checkbox"
          checked={isolationEnabled}
          onChange={(e) => setIsolationEnabled(e.target.checked)}
          className="h-3.5 w-3.5 shrink-0 accent-teal"
        />
        <span className="text-[11px] leading-none text-zinc-300">
          Isolate agents in worktrees
        </span>
        <Info size={11} className="shrink-0 text-zinc-600" />
      </label>
    </div>
  );
}

function WorkspaceBadge({
  info,
  loading,
}: {
  info: WorkspaceInfo | null;
  loading: boolean;
}) {
  if (loading) return <span className="shrink-0 text-[10px] text-zinc-600">…</span>;
  if (!info) return null;
  if (!info.exists) {
    return (
      <span className="inline-flex shrink-0 items-center gap-1 rounded-md border border-red-500/40 bg-red-500/10 px-1.5 py-0.5 text-[10px] text-red-300">
        <AlertTriangle size={10} /> missing
      </span>
    );
  }
  if (info.is_git) {
    return (
      <span
        className="inline-flex min-w-0 max-w-[8rem] shrink items-center gap-1 rounded-md border border-teal-600/40 bg-teal-600/10 px-1.5 py-0.5 font-mono text-[10px] text-teal-400"
        title={`git · ${info.branch ?? "detached"} · agents isolate into worktrees`}
      >
        <GitBranch size={10} className="shrink-0" />
        <span className="truncate">{info.branch ?? "detached"}</span>
      </span>
    );
  }
  return (
    <span
      className="inline-flex shrink-0 items-center rounded-md border border-amber/40 bg-amber/10 px-1.5 py-0.5 text-[10px] text-amber"
      title="Not a git repo — agents share the directory (shared mode)"
    >
      shared
    </span>
  );
}
