import { useState } from "react";
import { AlertTriangle, FolderX, X } from "lucide-react";
import { api } from "../api";
import { useStore } from "../store";
import { basename, dirname } from "../lib/recentProjects";

const FOCUS_RING =
  "focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-accent";

/**
 * Delete (or just unlist) a workspace. Two levels:
 *  - Default: REMOVE FROM TAIME — drops it from the known-workspaces list. The
 *    folder on disk and the daemon's tasks/agents for it are untouched. Safe.
 *  - Opt-in (checkbox): also PERMANENTLY DELETE THE FOLDER from disk. Because
 *    that's irreversible, it requires typing the folder's name to confirm (the
 *    Delete button stays disabled until it matches) — never a bare yes/no. The
 *    daemon additionally refuses root/home/shallow paths.
 *
 * Store-owned target (`deleteWorkspaceTarget`) so any surface can open it.
 */
export function DeleteWorkspaceDialog({
  path,
  onClose,
}: {
  path: string;
  onClose: () => void;
}) {
  const workspaceDir = useStore((s) => s.workspaceDir);
  const deleteWorkspace = useStore((s) => s.deleteWorkspace);

  const [deleteDir, setDeleteDir] = useState(false);
  const [confirm, setConfirm] = useState("");
  const [busy, setBusy] = useState(false);

  const name = basename(path);
  const isActive = path === workspaceDir;
  const typedOk = confirm.trim() === name;
  // Soft-remove needs no typing; the destructive folder delete requires the
  // typed name to match exactly.
  const canRun = !busy && (!deleteDir || typedOk);

  const run = async () => {
    if (!canRun) return;
    setBusy(true);
    // 1. Daemon teardown: stop the workspace's agents + delete its tasks. Do this
    //    BEFORE any folder delete so nothing is writing into the dir as it goes.
    const r = await api.deleteWorkspaceData(path);
    // 2. Optional folder delete. If it fails we still finish (agents are already
    //    stopped) and warn — never strand a half-deleted, still-listed workspace.
    let dirErr: string | null = null;
    if (deleteDir) dirErr = await api.deleteDirectory(path);
    // 3. Remove it from Taime's list (switches away if it was the active one).
    deleteWorkspace(path);
    onClose();
    const plural = (n: number, w: string) => `${n} ${w}${n === 1 ? "" : "s"}`;
    const parts = [
      r.killed ? `stopped ${plural(r.killed, "agent")}` : null,
      r.tasks ? `deleted ${plural(r.tasks, "task")}` : null,
      deleteDir && !dirErr ? "removed the folder" : null,
    ].filter(Boolean);
    useStore.getState().showSnackbar({
      type: dirErr ? "error" : "success",
      message: dirErr
        ? `Removed “${name}”${parts.length ? ` (${parts.join(", ")})` : ""} — but the folder couldn't be deleted: ${dirErr}`
        : `Deleted “${name}”${parts.length ? ` — ${parts.join(", ")}` : ""}`,
    });
  };

  const handleKey = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onClose();
      return;
    }
    if (e.key !== "Enter") return;
    if ((e.target as HTMLElement).tagName === "BUTTON") return; // native activation
    e.preventDefault();
    if (canRun) void run();
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
      onClick={onClose}
    >
      <div
        onKeyDown={handleKey}
        className="no-drag w-[30rem] rounded-xl border border-ink-500 bg-ink-800 shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center gap-2.5 border-b border-ink-600 px-5 py-3.5">
          <FolderX size={15} className="shrink-0 text-zinc-400" />
          <h2 className="text-sm font-semibold text-zinc-100">Delete workspace</h2>
          <span className="flex-1" />
          <button
            onClick={onClose}
            aria-label="Close"
            className={`rounded p-1 text-zinc-500 hover:text-zinc-200 ${FOCUS_RING}`}
          >
            <X size={16} />
          </button>
        </div>

        <div className="p-5">
          {/* Which workspace */}
          <div className="mb-3 flex items-center gap-2 rounded-lg border border-ink-600 bg-ink-700/40 px-3 py-2">
            <span className="flex min-w-0 flex-1 flex-col">
              <span className="truncate text-sm font-medium text-zinc-100" title={path}>
                {name}
              </span>
              <span className="truncate font-mono text-[10px] text-zinc-600">
                {dirname(path)}
              </span>
            </span>
            {isActive && (
              <span className="shrink-0 rounded bg-accent/15 px-1.5 py-0.5 text-[10px] text-accent">
                current
              </span>
            )}
          </div>

          <p className="mb-4 text-[11px] leading-relaxed text-zinc-500">
            Stops every agent working in this workspace, deletes its tasks, and
            removes it from Taime.
            {isActive &&
              " Taime will switch to your next recent workspace (or none)."}
          </p>

          {/* Opt-in: delete the directory too */}
          <label className="flex cursor-pointer items-start gap-2.5 rounded-lg border border-ink-600 bg-ink-700/30 px-3 py-2.5">
            <input
              type="checkbox"
              checked={deleteDir}
              onChange={(e) => {
                setDeleteDir(e.target.checked);
                if (!e.target.checked) setConfirm("");
              }}
              disabled={busy}
              className="mt-0.5 h-3.5 w-3.5 shrink-0 accent-red-500"
            />
            <span className="flex min-w-0 flex-col">
              <span className="text-xs font-medium text-zinc-200">
                Also permanently delete the folder from disk
              </span>
              <span className="text-[11px] leading-relaxed text-zinc-500">
                Erases the directory and everything in it. This cannot be undone.
              </span>
            </span>
          </label>

          {/* Typed confirmation — only when deleting the directory */}
          {deleteDir && (
            <div className="mt-3 rounded-lg border border-red-500/40 bg-red-500/5 p-3">
              <div className="mb-2 flex items-start gap-2 text-[11px] leading-relaxed text-red-300">
                <AlertTriangle size={13} className="mt-0.5 shrink-0" />
                <span className="min-w-0 break-words">
                  This permanently deletes{" "}
                  <span className="font-mono text-red-200">{path}</span> and all
                  its contents.
                </span>
              </div>
              <label className="mb-1 block text-[11px] text-zinc-400">
                Type <span className="font-mono font-semibold text-zinc-200">{name}</span>{" "}
                to confirm
              </label>
              <input
                type="text"
                value={confirm}
                onChange={(e) => setConfirm(e.target.value)}
                placeholder={name}
                autoFocus
                disabled={busy}
                spellCheck={false}
                className={`w-full rounded-lg border bg-ink-900/60 px-3 py-2 font-mono text-sm text-zinc-100 placeholder:text-zinc-700 disabled:opacity-60 ${
                  confirm.length > 0 && !typedOk ? "border-red-500/60" : "border-ink-500"
                } ${FOCUS_RING}`}
              />
            </div>
          )}
        </div>

        <div className="flex items-center justify-end gap-2 border-t border-ink-600 px-5 py-3">
          <button
            onClick={onClose}
            className={`rounded-lg px-3 py-1.5 text-sm text-zinc-400 hover:text-zinc-200 ${FOCUS_RING}`}
          >
            Cancel
          </button>
          <button
            onClick={() => void run()}
            disabled={!canRun}
            className={`rounded-lg bg-red-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-red-500 disabled:opacity-50 ${FOCUS_RING}`}
          >
            {busy ? "Deleting…" : deleteDir ? "Delete workspace + folder" : "Delete workspace"}
          </button>
        </div>
      </div>
    </div>
  );
}
