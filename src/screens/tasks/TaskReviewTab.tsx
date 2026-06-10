import { useCallback, useEffect, useRef, useState } from "react";
import { GitMerge, ListOrdered, RefreshCw } from "lucide-react";
import { api, type TaskAgent, type TaskDetail } from "../../api";
import { useStore } from "../../store";
import { providerTitle } from "../../lib/providerLabel";
import { agentLabel } from "../../lib/agentLabel";
import { AgentDiffSection } from "./AgentDiffSection";
import { loadAgentDiffBundle, type AgentDiffBundle } from "./lib";

/**
 * Review: the AGGREGATE LENS over the task's member worktrees. The task
 * aggregates — it does not own the diff: every section below is one agent's
 * worktree (DiffView's exact data path per Agent ID), and Merge/Revert
 * execute per worktree underneath. The merge queue lists reviewed-not-merged
 * worktrees in member order.
 */
export function TaskReviewTab({
  detail,
  reloadDetail,
}: {
  detail: TaskDetail;
  reloadDetail: () => void;
}) {
  const reviewedFrames = useStore((s) => s.reviewedFrames);
  const setLaunchOpen = useStore((s) => s.setLaunchOpen);
  const connected = useStore((s) => s.connected);

  const members = detail.agents;
  // Identity key for "the member set changed" (joins/leaves), not the 2s
  // detail poll churn — diff bundles only reload on real membership change.
  const memberIds = members.map((a) => a.agent_id).join("\u0000");
  const membersRef = useRef(members);
  membersRef.current = members;
  const rootRef = useRef(detail.task.workspace_root);
  rootRef.current = detail.task.workspace_root;

  const [bundles, setBundles] = useState<Record<string, AgentDiffBundle>>({});
  const [contended, setContended] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(false);
  const [loadedOnce, setLoadedOnce] = useState(false);
  const [loadError, setLoadError] = useState(false);
  const seq = useRef(0);
  const sectionEls = useRef<Record<string, HTMLElement | null>>({});

  const loadAll = useCallback(async () => {
    const mySeq = ++seq.current;
    const ms = membersRef.current;
    setLoading(true);
    try {
      const entries = await Promise.all(
        ms.map(async (a) => [a.agent_id, await loadAgentDiffBundle(a.agent_id)] as const),
      );
      // Contention is workspace-wide (same source DiffView reads) — flag files
      // also changed by another agent so cross-worktree merges aren't blind.
      // Decoration only: its failure must not take down the loaded diffs.
      const cont = rootRef.current
        ? await api.getContention(rootRef.current).catch(() => [])
        : [];
      if (seq.current !== mySeq) return; // stale — membership changed mid-load
      setBundles(Object.fromEntries(entries));
      setContended(new Set(cont.map((r) => r.path)));
      setLoadedOnce(true);
      setLoadError(false);
    } catch {
      // Strict reads reject on daemon-down: keep whatever real bundles we have
      // (stale beats fabricated-empty) and flag the failure so the tab never
      // presents a fallback as a reviewed-clean state.
      if (seq.current === mySeq) setLoadError(true);
    } finally {
      if (seq.current === mySeq) setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (memberIds === "") {
      setBundles({});
      setLoadedOnce(true);
      return;
    }
    void loadAll();
    // `connected` in the deps heals the tab when the daemon comes back (the
    // strict reads reject while it's down) — same wiring as DiffView/DiffPanel.
    // Without it, the "until it answers" copy promised a retry that never ran,
    // and pre-outage bundles silently became trusted again on reconnect.
  }, [memberIds, loadAll, connected]);

  /** After a merge/revert: the worktree changed AND the rollups did. */
  const onApplied = useCallback(() => {
    void loadAll();
    reloadDetail();
  }, [loadAll, reloadDetail]);

  // Aggregate totals across the loaded bundles.
  const loadedBundles = members
    .map((a) => bundles[a.agent_id])
    .filter((b): b is AgentDiffBundle => !!b);
  const totalAdd = loadedBundles.reduce(
    (n, b) => n + b.files.reduce((m, f) => m + f.additions, 0),
    0,
  );
  const totalDel = loadedBundles.reduce(
    (n, b) => n + b.files.reduce((m, f) => m + f.deletions, 0),
    0,
  );
  const totalFiles = loadedBundles.reduce((n, b) => n + b.files.length, 0);
  // Distinct worktrees, not member count — shared-mode agents share one.
  const worktreeCount = new Set(
    members.map((a) => bundles[a.agent_id]?.worktree?.worktree_path ?? a.agent_id),
  ).size;
  const dirtyAgents = members.filter((a) => a.dirty_count > 0).length;
  const reviewedCount = members.filter((a) => reviewedFrames[a.agent_id]).length;

  // The merge queue: reviewed but the worktree still carries changes — i.e.
  // acknowledged, not yet landed. Member order is the queue order.
  const queue = members.filter(
    (a) => reviewedFrames[a.agent_id] && (bundles[a.agent_id]?.files.length ?? 0) > 0,
  );

  const jumpTo = (agentId: string) =>
    sectionEls.current[agentId]?.scrollIntoView({ behavior: "smooth", block: "start" });

  if (members.length === 0) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-3 px-6 text-center">
        <GitMerge size={24} className="text-zinc-700" />
        <p className="text-xs text-zinc-500">
          No agents in this task — nothing to aggregate.
        </p>
        <button
          onClick={() => setLaunchOpen(true, detail.task.id)}
          disabled={!connected}
          className="rounded-md border border-ink-500 px-3 py-1.5 text-xs text-zinc-300 hover:bg-ink-600 disabled:cursor-default disabled:opacity-40"
        >
          Launch one
        </button>
      </div>
    );
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* The aggregate-lens header — the thesis line is canonical copy. */}
      <div className="shrink-0 border-b border-ink-600 px-4 py-2.5">
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
          <span className="shrink-0 rounded border border-ink-500 bg-ink-700 px-1.5 py-0.5 text-[9px] font-semibold uppercase tracking-wide text-zinc-400">
            Aggregate lens
          </span>
          <span className="tnum shrink-0 font-mono text-[11px]">
            <span className="text-emerald-400">+{totalAdd}</span>{" "}
            <span className="text-rose-400">-{totalDel}</span>
          </span>
          <span className="tnum shrink-0 font-mono text-[11px] text-zinc-500">
            {totalFiles} file{totalFiles === 1 ? "" : "s"} · {worktreeCount} worktree
            {worktreeCount === 1 ? "" : "s"}
          </span>
          <span className="tnum shrink-0 rounded bg-ink-600 px-1.5 py-0.5 text-[10px] text-zinc-400">
            {dirtyAgents} dirty · {reviewedCount} reviewed of {members.length}
          </span>
          <span className="ml-auto" />
          <button
            onClick={() => void loadAll()}
            disabled={loading}
            title="Reload every member agent's diff"
            className="flex shrink-0 items-center gap-1.5 rounded border border-ink-500 px-2 py-0.5 text-[11px] text-zinc-400 hover:bg-ink-600 hover:text-zinc-200 disabled:cursor-default disabled:opacity-40"
          >
            <RefreshCw size={11} className={loading ? "animate-spin" : ""} />
            {loading ? "Refreshing…" : "Refresh"}
          </button>
        </div>
        <p className="mt-1 text-[10px] text-zinc-600">
          The task aggregates — it does not own the diff. Merge runs per agent
          worktree.
        </p>
        {loadedOnce && loadError && (
          // A failed refresh after data has rendered: the sections below are
          // STALE, not current — say so, and the per-agent Mark reviewed gates
          // on this via the `stale` prop.
          <p className="mt-1 text-[10px] text-amber">
            {connected
              ? "last refresh failed — the diffs below may be stale; reviewing is disabled until a refresh succeeds"
              : "daemon unreachable — the diffs below may be stale; reviewing is disabled until it answers"}
          </p>
        )}
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto">
        {/* Merge queue: reviewed-not-merged worktrees, in member order. */}
        <section className="border-b border-ink-600 bg-ink-800/40 px-4 py-2.5">
          <div className="flex items-center gap-2">
            <ListOrdered size={12} className="shrink-0 text-zinc-500" />
            <h3 className="text-[10px] font-semibold uppercase tracking-wider text-zinc-500">
              Merge queue
            </h3>
            <span className="min-w-0 truncate text-[10px] text-zinc-600">
              reviewed, not merged — each step lands one Agent ID&apos;s changes
            </span>
          </div>
          {queue.length === 0 ? (
            <p className="mt-1.5 text-[11px] text-zinc-600">
              Empty — mark a worktree reviewed and it queues here until merged.
            </p>
          ) : (
            <ul className="mt-1.5 space-y-1">
              {queue.map((a, i) => (
                <MergeQueueRow
                  key={a.agent_id}
                  index={i}
                  member={a}
                  bundle={bundles[a.agent_id]}
                  onJump={() => jumpTo(a.agent_id)}
                />
              ))}
            </ul>
          )}
        </section>

        {/* Per-agent diff sections — the diff belongs to the Agent ID. */}
        {!loadedOnce && loading ? (
          <p className="px-4 py-6 text-center text-xs text-zinc-500">
            loading diffs…
          </p>
        ) : !loadedOnce && loadError ? (
          <p className="px-4 py-6 text-center text-xs text-zinc-500">
            {connected
              ? "couldn't load the member diffs — refresh to retry"
              : "daemon unreachable — diffs can't be shown or reviewed until it answers"}
          </p>
        ) : (
          members.map((a) => (
            <AgentDiffSection
              key={a.agent_id}
              member={a}
              siblings={members.filter((m) => m.agent_id !== a.agent_id)}
              bundle={bundles[a.agent_id]}
              stale={loadError}
              contended={contended}
              onApplied={onApplied}
              sectionRef={(el) => {
                sectionEls.current[a.agent_id] = el;
              }}
            />
          ))
        )}
      </div>
    </div>
  );
}

/** One queue entry: position, identity, size — and a jump to its section
 *  (the merge itself stays in the section: per worktree, never task-level). */
function MergeQueueRow({
  index,
  member,
  bundle,
  onJump,
}: {
  index: number;
  member: TaskAgent;
  bundle: AgentDiffBundle | undefined;
  onJump: () => void;
}) {
  const add = bundle?.files.reduce((n, f) => n + f.additions, 0) ?? 0;
  const del = bundle?.files.reduce((n, f) => n + f.deletions, 0) ?? 0;
  return (
    <li className="flex items-center gap-2">
      <span className="tnum flex h-4 w-4 shrink-0 items-center justify-center rounded-full bg-ink-600 font-mono text-[9px] text-zinc-400">
        {index + 1}
      </span>
      <span className="shrink-0 text-[11px] text-zinc-300">
        {providerTitle(member.provider ?? "")}
      </span>
      <span
        title={member.agent_id}
        className="min-w-0 flex-1 truncate whitespace-nowrap font-mono text-[10px] text-zinc-600"
      >
        {agentLabel(member.agent_id)}
      </span>
      <span className="tnum shrink-0 font-mono text-[10px]">
        <span className="text-emerald-400">+{add}</span>{" "}
        <span className="text-rose-400">-{del}</span>
      </span>
      <button
        onClick={onJump}
        className="shrink-0 rounded border border-ink-500 px-1.5 py-0.5 text-[10px] text-zinc-400 hover:bg-ink-600 hover:text-zinc-200"
      >
        Jump to diff
      </button>
    </li>
  );
}
