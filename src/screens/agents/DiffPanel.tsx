import { useCallback, useEffect, useState } from "react";
import { GitMerge, RefreshCw } from "lucide-react";
import { api, type FileDiffEntry } from "../../api";
import { useStore } from "../../store";

/** Per-file status letter (terse instrument chip; full word in the title). */
function statusChip(status: string): { ch: string; cls: string } {
  if (status === "added") return { ch: "A", cls: "text-emerald-400" };
  if (status === "deleted") return { ch: "D", cls: "text-rose-400" };
  if (status === "renamed") return { ch: "R", cls: "text-violet-400" };
  return { ch: "M", cls: "text-amber" };
}

/**
 * Diff tab — the agent-scoped change summary (same `file_diffs` data the full
 * review reads), with the existing DiffView as the review surface: "open full
 * review" sets the store's diffTerminalId, which mounts the App-level DiffView
 * scoped to this agent (merge/revert/mark-reviewed all live there).
 */
export function DiffPanel({ agentId }: { agentId: string | null }) {
  const connected = useStore((s) => s.connected);
  const openDiff = useStore((s) => s.openDiff);
  // null = first load in flight (or the last load failed — see `failed`).
  const [files, setFiles] = useState<FileDiffEntry[] | null>(null);
  const [failed, setFailed] = useState(false);
  const [refreshing, setRefreshing] = useState(false);

  const load = useCallback(async () => {
    if (!agentId) return;
    setRefreshing(true);
    try {
      // Strict read: rejects on daemon-down instead of serving an empty
      // fallback, so a dead daemon can't render as "no changes vs base".
      const r = await api.getFileDiffs(agentId);
      setFiles(r.files);
      setFailed(false);
    } catch {
      // Keep files null and flag the failure — the body must distinguish a
      // daemon-UP query error from "still loading" (an eternal fake loading
      // label is a mislabeled error state on a trust surface).
      setFiles(null);
      setFailed(true);
    } finally {
      setRefreshing(false);
    }
    // Re-load when the daemon comes back so the tab heals without a manual
    // refresh (mirrors AgentDetail's worktree probe).
  }, [agentId, connected]);

  useEffect(() => {
    setFiles(null);
    setFailed(false);
    void load();
  }, [load]);

  if (!agentId) {
    return (
      <p className="p-4 text-xs text-zinc-600">
        no worktree row · diff unavailable for this agent
      </p>
    );
  }

  const totalAdd = (files ?? []).reduce((n, f) => n + f.additions, 0);
  const totalDel = (files ?? []).reduce((n, f) => n + f.deletions, 0);

  return (
    <div className="flex h-full flex-col">
      <div className="flex shrink-0 items-center gap-2 border-b border-ink-600 px-3 py-1.5">
        <span className="text-[10px] font-semibold uppercase tracking-wider text-zinc-600">
          Changes vs base
        </span>
        {files && files.length > 0 && (
          <span className="font-mono text-[10px] tabular-nums">
            <span className="text-emerald-400">+{totalAdd}</span>{" "}
            <span className="text-rose-400">-{totalDel}</span>
            <span className="text-zinc-600"> · {files.length} files</span>
          </span>
        )}
        <span className="flex-1" />
        <button
          onClick={() => void load()}
          disabled={refreshing}
          title="Refresh diff"
          aria-label="Refresh diff"
          className="rounded p-1 text-zinc-500 enabled:hover:bg-ink-600 enabled:hover:text-zinc-200 disabled:cursor-default disabled:opacity-40"
        >
          <RefreshCw size={12} className={refreshing ? "animate-spin" : ""} />
        </button>
        <button
          onClick={() => openDiff(agentId)}
          className="flex items-center gap-1.5 rounded-md bg-primary px-2.5 py-1 text-[11px] font-medium text-white hover:bg-primary-hover"
        >
          <GitMerge size={12} />
          Open full review
        </button>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto p-1.5">
        {!connected && (
          <p className="px-2 py-2 text-xs text-zinc-600">
            daemon unreachable · retrying
          </p>
        )}
        {connected && files === null && failed && (
          <p className="px-2 py-2 text-xs text-rose-400/80">
            couldn&apos;t load the diff — use refresh to retry
          </p>
        )}
        {connected && files === null && !failed && (
          <p className="px-2 py-2 text-xs text-zinc-600">loading diff…</p>
        )}
        {connected && files !== null && files.length === 0 && (
          <p className="px-2 py-2 text-xs text-zinc-600">
            no changes vs base yet
          </p>
        )}
        {files !== null && files.length > 0 && (
          <ul>
            {files.map((f) => {
              const chip = statusChip(f.status);
              return (
                <li key={f.path}>
                  <button
                    onClick={() => openDiff(agentId)}
                    title={`${f.path} — open full review`}
                    className="flex w-full items-center gap-2 rounded px-2 py-1 text-left hover:bg-ink-700/60"
                  >
                    <span
                      className={`w-3 shrink-0 text-center font-mono text-[10px] font-semibold ${chip.cls}`}
                      title={f.status}
                    >
                      {chip.ch}
                    </span>
                    <span className="min-w-0 flex-1 truncate whitespace-nowrap font-mono text-[11px] text-zinc-300">
                      {f.path}
                    </span>
                    {f.binary && (
                      <span className="shrink-0 text-[9px] text-zinc-600">
                        binary
                      </span>
                    )}
                    <span className="shrink-0 font-mono text-[10px] tabular-nums">
                      <span className="text-emerald-400">+{f.additions}</span>
                      <span className="text-rose-400"> -{f.deletions}</span>
                    </span>
                  </button>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </div>
  );
}
