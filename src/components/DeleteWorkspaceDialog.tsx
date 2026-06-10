import { useEffect, useState } from "react";
import { AlertTriangle, Archive, FolderX, X } from "lucide-react";
import { api } from "../api";
import { useStore } from "../store";
import { basename, dirname } from "../lib/recentProjects";

const FOCUS_RING =
  "focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-accent";

/**
 * Delete a workspace. The teardown is non-lossy BY DEFAULT — three levels, the
 * destructive two each behind a typed-name confirm (never a bare yes/no):
 *  - Default (soft): stops the workspace's agents, reclaims their disposable
 *    worktree checkouts, and deletes its tasks — but PRESERVES every reclaimed
 *    agent's durable archived work (`refs/taime/archive/*`). Nothing unmerged is
 *    destroyed; the workspace just leaves Taime's list.
 *  - Opt-in (checkbox, shown only when there IS archived work): also PERMANENTLY
 *    DESTROY that unmerged archived work. Irreversible → typed confirm.
 *  - Opt-in (checkbox): also PERMANENTLY DELETE THE FOLDER from disk.
 *    Irreversible → typed confirm. The daemon additionally refuses
 *    root/home/shallow paths.
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
  const connected = useStore((s) => s.connected);

  const [deleteDir, setDeleteDir] = useState(false);
  const [destroyArchives, setDestroyArchives] = useState(false);
  const [confirm, setConfirm] = useState("");
  const [busy, setBusy] = useState(false);
  // How many agents hold archived, unmerged work that a hard delete would erase.
  // `null` until the daemon answers (kept distinct from a real 0).
  const [archivedCount, setArchivedCount] = useState<number | null>(null);

  const name = basename(path);
  const isActive = path === workspaceDir;
  const typedOk = confirm.trim() === name;
  // Either destructive opt-in (destroy archived work / delete the folder) demands
  // the typed name; the soft default needs none.
  const needsTyped = deleteDir || destroyArchives;
  const canRun = !busy && connected && (!needsTyped || typedOk);

  // Fetch the archived-work count on open (and whenever the daemon reconnects) so
  // the destroy-archives opt-in only appears when there's actually work to lose.
  useEffect(() => {
    if (!connected) return;
    let live = true;
    void api.workspaceArchivedCount(path).then((r) => {
      if (live) setArchivedCount(r.count);
    });
    return () => {
      live = false;
    };
  }, [path, connected]);

  const hasArchives = (archivedCount ?? 0) > 0;
  // If the count drops to 0 (or the daemon went away), never leave the opt-in
  // armed — it would require typing for nothing.
  useEffect(() => {
    if (!hasArchives && destroyArchives) {
      setDestroyArchives(false);
      if (!deleteDir) setConfirm("");
    }
  }, [hasArchives, destroyArchives, deleteDir]);

  const run = async () => {
    if (!canRun) return;
    setBusy(true);
    // 1. Daemon teardown: stop the workspace's agents + delete its tasks (and,
    //    when opted in, destroy the archived unmerged work). BEFORE any folder
    //    delete so nothing is writing into the dir as it goes.
    const r = await api.deleteWorkspaceData(path, destroyArchives);
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
      destroyArchives && hasArchives
        ? `destroyed ${plural(archivedCount ?? 0, "agent")}' archived work`
        : null,
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

          <p className="mb-1 text-[11px] leading-relaxed text-zinc-500">
            Stops every agent working in this workspace, reclaims their worktree
            checkouts, and deletes its tasks, then removes it from Taime.
            {isActive &&
              " Taime will switch to your next recent workspace (or none)."}
          </p>
          <p className="mb-4 text-[11px] leading-relaxed text-zinc-500">
            {hasArchives ? (
              <>
                Each reclaimed agent’s unmerged work is{" "}
                <span className="text-zinc-300">kept</span> (recoverable) unless
                you destroy it below.
              </>
            ) : (
              "No unmerged archived work to lose."
            )}
          </p>

          {/* Opt-in: destroy archived unmerged work — only when there is any */}
          {hasArchives && (
            <label className="mb-2 flex cursor-pointer items-start gap-2.5 rounded-lg border border-ink-600 bg-ink-700/30 px-3 py-2.5">
              <input
                type="checkbox"
                checked={destroyArchives}
                onChange={(e) => {
                  setDestroyArchives(e.target.checked);
                  if (!e.target.checked && !deleteDir) setConfirm("");
                }}
                disabled={busy}
                className="mt-0.5 h-3.5 w-3.5 shrink-0 accent-red-500"
              />
              <span className="flex min-w-0 flex-col">
                <span className="flex items-center gap-1.5 text-xs font-medium text-zinc-200">
                  <Archive size={12} className="shrink-0 text-red-400" />
                  Also permanently destroy {archivedCount}{" "}
                  {archivedCount === 1 ? "agent’s" : "agents’"} archived work
                </span>
                <span className="text-[11px] leading-relaxed text-zinc-500">
                  Drops the durable <span className="font-mono">refs/taime/archive/*</span>{" "}
                  snapshots — the only copy of that unmerged work. This cannot be
                  undone.
                </span>
              </span>
            </label>
          )}

          {/* Opt-in: delete the directory too */}
          <label className="flex cursor-pointer items-start gap-2.5 rounded-lg border border-ink-600 bg-ink-700/30 px-3 py-2.5">
            <input
              type="checkbox"
              checked={deleteDir}
              onChange={(e) => {
                setDeleteDir(e.target.checked);
                if (!e.target.checked && !destroyArchives) setConfirm("");
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

          {/* Typed confirmation — whenever a destructive opt-in is armed */}
          {needsTyped && (
            <div className="mt-3 rounded-lg border border-red-500/40 bg-red-500/5 p-3">
              <div className="mb-2 flex items-start gap-2 text-[11px] leading-relaxed text-red-300">
                <AlertTriangle size={13} className="mt-0.5 shrink-0" />
                <span className="min-w-0 break-words">
                  This permanently
                  {destroyArchives && deleteDir
                    ? " destroys the archived unmerged work AND deletes "
                    : destroyArchives
                      ? " destroys the archived unmerged work for "
                      : " deletes "}
                  <span className="font-mono text-red-200">
                    {deleteDir ? path : name}
                  </span>
                  {deleteDir ? " and all its contents." : "."}
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

          {!connected && (
            <p className="mt-3 text-[11px] text-amber">
              daemon unreachable · agents and archived work can’t be torn down —
              retrying
            </p>
          )}
        </div>

        <div className="flex items-center justify-end gap-2 border-t border-ink-600 px-5 py-3">
          <button
            onClick={onClose}
            autoFocus
            className={`rounded-lg px-3 py-1.5 text-sm text-zinc-400 hover:text-zinc-200 ${FOCUS_RING}`}
          >
            Cancel
          </button>
          <button
            onClick={() => void run()}
            disabled={!canRun}
            className={`rounded-lg bg-red-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-red-500 disabled:opacity-50 ${FOCUS_RING}`}
          >
            {busy
              ? "Deleting…"
              : deleteDir
                ? "Delete workspace + folder"
                : destroyArchives
                  ? "Delete workspace + archives"
                  : "Delete workspace"}
          </button>
        </div>
      </div>
    </div>
  );
}
