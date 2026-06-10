import { useEffect, useMemo, useState } from "react";
import {
  Check,
  Archive,
  ChevronDown,
  ChevronRight,
  Download,
  FileMinus,
  FilePlus,
  FileText,
  GitBranch,
  GitCommitHorizontal,
  Undo2,
} from "lucide-react";
import { api, type FileDiffEntry, type HunkEntry, type TaskAgent } from "../../api";
import { useStore } from "../../store";
import { providerTitle } from "../../lib/providerLabel";
import { agentLabel } from "../../lib/agentLabel";
import { statusDotClass } from "../../lib/agentStatus";
import {
  AUTHOR_COLORS,
  authorName,
  memberWireStatus,
  middleTruncate,
  type AgentDiffBundle,
} from "./lib";

function StatusIcon({ status }: { status: string }) {
  if (status === "added") return <FilePlus size={12} className="shrink-0 text-emerald-400" />;
  if (status === "deleted") return <FileMinus size={12} className="shrink-0 text-rose-400" />;
  return <FileText size={12} className="shrink-0 text-amber" />;
}

/** Render one hunk's unified-diff text with +/- line coloring (the raw hunk
 *  from `hunked_diff` — same payload apply_selection consumes). */
function HunkText({ hunk }: { hunk: HunkEntry }) {
  const lines = useMemo(() => {
    const ls = hunk.text.replace(/\n$/, "").split("\n");
    // The wire text may repeat the @@ header we already render in the row.
    return ls[0]?.startsWith("@@") ? ls.slice(1) : ls;
  }, [hunk.text]);
  return (
    <pre className="overflow-x-auto bg-ink-900 px-3 py-1.5 font-mono text-[11px] leading-relaxed">
      {lines.map((ln, i) => (
        <div
          key={i}
          className={
            ln.startsWith("+")
              ? "bg-emerald-500/[0.07] text-emerald-300"
              : ln.startsWith("-")
                ? "bg-rose-500/[0.07] text-rose-300"
                : "text-zinc-500"
          }
        >
          {ln || " "}
        </div>
      ))}
    </pre>
  );
}

/**
 * One member agent's worktree diff — DiffView's review semantics, re-homed
 * per agent inside the task's aggregate lens. Merge/Revert/Mark-reviewed are
 * preserved exactly: apply_selection per worktree (merge → main or a sibling,
 * revert → self), review state keyed by Agent ID, hunk-level attribution.
 */
export function AgentDiffSection({
  member,
  siblings,
  bundle,
  stale,
  contended,
  onApplied,
  sectionRef,
}: {
  member: TaskAgent;
  /** The other members of THIS task — the same-task sibling merge targets
   *  (DiffView's rule: only same-task agents cross-link; Uncategorized never). */
  siblings: TaskAgent[];
  bundle: AgentDiffBundle | undefined;
  /** The aggregate's last refresh failed: `bundle` is a kept-stale snapshot,
   *  not current — Mark reviewed must not acknowledge it. */
  stale: boolean;
  /** Workspace-wide contention: files also changed by another agent. */
  contended: Set<string>;
  onApplied: () => void;
  sectionRef: (el: HTMLElement | null) => void;
}) {
  const terminalStatuses = useStore((s) => s.terminalStatuses);
  const reviewed = useStore((s) => !!s.reviewedFrames[member.agent_id]);
  const markFrameReviewed = useStore((s) => s.markFrameReviewed);
  const showSnackbar = useStore((s) => s.showSnackbar);
  const connected = useStore((s) => s.connected);

  const [sel, setSel] = useState<Record<string, number[]>>({});
  const [target, setTarget] = useState("main");
  const [busy, setBusy] = useState<"merge" | "revert" | null>(null);
  const [collapsedFiles, setCollapsedFiles] = useState<Set<string>>(new Set());

  // A reloaded bundle invalidates hunk indices — drop stale selections.
  useEffect(() => {
    setSel({});
  }, [bundle]);

  const files = bundle?.files ?? [];
  const attribution = bundle?.attribution ?? null;
  const worktree = bundle?.worktree ?? null;

  const hunksByPath = useMemo(() => {
    const m: Record<string, HunkEntry[]> = {};
    for (const h of bundle?.hunks ?? []) m[h.path] = h.hunks;
    return m;
  }, [bundle]);

  // Team ordering (owner first, then members) → stable author colors — the
  // DiffView scheme, computed from this agent's own attribution payload.
  const colorIndexById = useMemo(() => {
    const team = attribution?.team ?? [];
    const order = [
      ...team.filter((t) => !t.member_of),
      ...team.filter((t) => t.member_of),
    ].map((t) => t.agent_id);
    const m: Record<string, number> = {};
    order.forEach((id, i) => (m[id] = i % AUTHOR_COLORS.length));
    return m;
  }, [attribution]);
  const colorFor = (tid: string | null | undefined) =>
    tid && colorIndexById[tid] != null ? AUTHOR_COLORS[colorIndexById[tid]] : null;
  const hunkAuthor = (path: string, index: number) =>
    attribution?.files[path]?.hunks?.[String(index)] ?? null;
  const contribCount = (path: string): number =>
    attribution?.files[path]?.contributors.length ?? 0;

  // Selection helpers (DiffView's, verbatim semantics).
  const allIdx = (path: string) => (hunksByPath[path] ?? []).map((h) => h.index);
  // An EMPTY new file is a real, mergeable change with zero hunks (its patch
  // is header-only) — selectable as a whole file. Binary files stay inert.
  const isSelectableEmpty = (path: string) => {
    const f = files.find((x) => x.path === path);
    return allIdx(path).length === 0 && f?.status === "added" && !f.binary;
  };
  const isFileFull = (path: string) => {
    const all = allIdx(path);
    if (all.length === 0) return isSelectableEmpty(path) && sel[path] !== undefined;
    return (sel[path]?.length ?? 0) === all.length;
  };
  const toggleFile = (path: string) =>
    setSel((s) => {
      const next = { ...s };
      if (allIdx(path).length === 0 && !isSelectableEmpty(path)) return next;
      if (isFileFull(path)) delete next[path];
      else next[path] = allIdx(path);
      return next;
    });
  const toggleHunk = (path: string, idx: number) =>
    setSel((s) => {
      const cur = new Set(s[path] ?? []);
      if (cur.has(idx)) cur.delete(idx);
      else cur.add(idx);
      const next = { ...s };
      if (cur.size === 0) delete next[path];
      else next[path] = [...cur].sort((a, b) => a - b);
      return next;
    });
  // A selected empty file counts as one unit (it has no hunks to count).
  const selectionCount = Object.values(sel).reduce((n, a) => n + Math.max(a.length, 1), 0);

  const agentName = providerTitle(member.provider ?? worktree?.provider ?? "agent");
  const totalAdd = files.reduce((n, f) => n + f.additions, 0);
  const totalDel = files.reduce((n, f) => n + f.deletions, 0);

  // Shared handling for a refused/stale/conflicted apply-or-commit outcome.
  const reportFailure = (res: { stale?: boolean; conflicts: string[]; error: string | null }, verb: string) => {
    if (res.stale) {
      // The worktree moved since this bundle was fetched — reload; the selection
      // is meaningless against the new hunks.
      showSnackbar({ type: "error", message: "The agent changed files after this diff loaded — review the new changes" });
      setSel({});
      onApplied();
    } else if (res.conflicts.length) {
      showSnackbar({ type: "error", message: `Conflicts: ${res.conflicts[0]}` });
      onApplied();
    } else {
      showSnackbar({ type: "error", message: res.error ?? `${verb} failed` });
    }
  };

  /** Commit & merge (→ main or a same-task sibling worktree): land the selected
   *  hunks AND record them as one provenance commit. Merging through the Review
   *  surface IS the review act, so the ack is recorded first (best-effort — the
   *  merge is allowed either way, just labeled reviewed vs autonomous). */
  const commitMerge = async () => {
    if (selectionCount === 0 || busy || !bundle?.digest) return;
    setBusy("merge");
    try {
      await api.markReviewed(member.agent_id);
      const res = await api.commitMerge(member.agent_id, {
        target,
        selections: sel,
        expectedDigest: bundle.digest,
      });
      if (res.committed) {
        const where = target === "main" ? "main" : "the sibling worktree";
        showSnackbar({
          type: "success",
          message: `Committed ${res.commit} → ${where} — ${res.files.length} file(s), provenance recorded`,
        });
        setSel({});
        onApplied();
      } else {
        reportFailure(res, "Merge");
      }
    } catch (e) {
      showSnackbar({ type: "error", message: String(e) });
    } finally {
      setBusy(null);
    }
  };

  /** Revert (reverse-apply from self): discard this agent's own work back to base.
   *  No commit, no ack — the safe direction. */
  const revert = async () => {
    if (selectionCount === 0 || busy) return;
    setBusy("revert");
    try {
      const res = await api.applySelection(member.agent_id, {
        target: "self",
        mode: "revert",
        selections: sel,
        expectedDigest: bundle?.digest ?? undefined,
      });
      if (res.applied) {
        showSnackbar({ type: "success", message: `Reverted ${res.files.length} file(s) from ${agentName}` });
        setSel({});
        onApplied();
      } else {
        reportFailure(res, "Revert");
      }
    } catch (e) {
      showSnackbar({ type: "error", message: String(e) });
    } finally {
      setBusy(null);
    }
  };

  /** Download this agent's attribution as a portable JSON artifact (gap #2). */
  const exportAttribution = async () => {
    try {
      const data = await api.exportAttribution(member.agent_id);
      const blob = new Blob([JSON.stringify(data, null, 2)], { type: "application/json" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `taime-attribution-${member.agent_id.slice(0, 8)}.json`;
      a.click();
      URL.revokeObjectURL(url);
      showSnackbar({ type: "success", message: "Attribution exported" });
    } catch (e) {
      showSnackbar({ type: "error", message: `Export failed: ${String(e)}` });
    }
  };

  /** Acknowledge this agent's changes: clears its dirty set (frontend + the
   *  daemon's accumulated watcher set) and keys review state by Agent ID, in one
   *  ordered action so the dirty-reset and ack-write can't race. */
  const onMarkReviewed = () => {
    markFrameReviewed(member.agent_id);
  };

  // The diff below is authoritative only while the daemon answers, this
  // agent's bundle actually loaded, AND the last refresh succeeded (a kept-
  // stale bundle reads fine but may no longer reflect the worktree). This
  // gates Mark reviewed AND Merge/Revert: selections are POSITIONAL hunk
  // indices that the daemon re-resolves against the live worktree at apply
  // time — applying a selection built on a stale snapshot would merge or
  // revert different lines than the user reviewed.
  const reviewTrusted = connected && bundle !== undefined && !stale;

  const toggleFileCollapsed = (path: string) =>
    setCollapsedFiles((s) => {
      const next = new Set(s);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });

  return (
    <section ref={sectionRef} className="scroll-mt-2 border-b border-ink-700">
      {/* Sticky per-agent header: identity + provenance + the actions. */}
      <div className="sticky top-0 z-10 flex flex-wrap items-center gap-x-2 gap-y-1.5 border-b border-ink-700 bg-ink-800/95 px-4 py-2 backdrop-blur">
        <span
          className={`h-2 w-2 shrink-0 rounded-full ${statusDotClass(
            memberWireStatus(member, terminalStatuses),
          )}`}
        />
        <span className="shrink-0 text-xs font-semibold text-zinc-100">{agentName}</span>
        <span
          title={member.agent_id}
          className="max-w-[160px] truncate whitespace-nowrap font-mono text-[10px] text-zinc-500"
        >
          {agentLabel(member.agent_id)}
        </span>
        {worktree?.reclaimed ? (
          <span
            title="Worktree reclaimed — work preserved in refs/taime/archive/*; this diff renders from the archive."
            className="flex shrink-0 items-center gap-1 rounded border border-ink-600 px-1.5 py-0.5 font-mono text-[10px] text-zinc-400"
          >
            <Archive size={10} />
            archived
          </span>
        ) : worktree?.branch ? (
          <span className="flex shrink-0 items-center gap-1 rounded border border-ink-600 px-1.5 py-0.5 font-mono text-[10px] text-zinc-400">
            <GitBranch size={10} />
            <span title={worktree.branch} className="max-w-[180px] truncate whitespace-nowrap">
              {worktree.branch}
            </span>
          </span>
        ) : worktree?.mode === "isolated" ? (
          <span
            title="Isolated worktree (detached); changes attributed by file."
            className="flex shrink-0 items-center gap-1 rounded border border-ink-600 px-1.5 py-0.5 font-mono text-[10px] text-zinc-400"
          >
            <GitBranch size={10} />
            isolated
          </span>
        ) : null}
        {worktree?.mode === "shared" && (
          <span className="shrink-0 rounded border border-ink-600 px-1.5 py-0.5 text-[10px] text-zinc-500">
            shared dir (heuristic)
          </span>
        )}
        <span
          title={`Attribution anchor: ${member.agent_id}`}
          className="shrink-0 rounded bg-ink-600 px-1.5 py-0.5 font-mono text-[9px] text-zinc-400"
        >
          Attribution: {member.agent_id.slice(0, 8)}
        </span>
        <span className="tnum shrink-0 font-mono text-[10px]">
          <span className="text-emerald-400">+{totalAdd}</span>{" "}
          <span className="text-rose-400">-{totalDel}</span>{" "}
          <span className="text-zinc-600">{files.length}f</span>
        </span>
        {member.dirty_count > 0 && (
          <span className="tnum shrink-0 rounded bg-amber/20 px-1 text-[9px] font-medium text-amber">
            {member.dirty_count} dirty
          </span>
        )}
        {reviewed && (
          <span className="shrink-0 rounded bg-emerald-500/15 px-1 text-[9px] font-medium text-emerald-400">
            reviewed
          </span>
        )}

        <span className="ml-auto" />

        {/* Merge target: main or a same-task sibling's worktree. */}
        <label className="flex shrink-0 items-center gap-1 text-[10px] text-zinc-500">
          into
          <select
            value={target}
            onChange={(e) => setTarget(e.target.value)}
            disabled={busy !== null}
            aria-label="Merge target"
            className="rounded border border-ink-600 bg-ink-900 px-1.5 py-0.5 text-[10px] text-zinc-200 disabled:opacity-50"
          >
            <option value="main">main</option>
            {siblings.map((s) => (
              <option key={s.agent_id} value={s.agent_id}>
                {providerTitle(s.provider ?? "")} ({s.agent_id.slice(0, 6)})
              </option>
            ))}
          </select>
        </label>
        <button
          disabled={busy !== null || selectionCount === 0 || !reviewTrusted || !bundle?.digest}
          onClick={() => void commitMerge()}
          title={
            reviewTrusted
              ? `Records: Co-authored-by ${providerTitle(member.provider ?? "")} · Taime-Agent-Id ${member.agent_id.slice(0, 8)} · reviewed · ${selectionCount} hunk(s) → ${target === "main" ? "main" : "sibling"}`
              : "This diff may be stale — applying its hunk selection could merge the wrong lines"
          }
          className="flex shrink-0 items-center gap-1 rounded-md bg-primary px-2 py-1 text-[11px] font-medium text-white hover:bg-primary-hover disabled:cursor-default disabled:opacity-40"
        >
          <GitCommitHorizontal size={11} />
          {busy === "merge" ? "Committing…" : `Commit & merge${selectionCount > 0 ? ` ${selectionCount}` : ""}`}
        </button>
        <button
          disabled={busy !== null || selectionCount === 0 || !reviewTrusted}
          onClick={() => void revert()}
          title={reviewTrusted ? undefined : "This diff may be stale — applying its hunk selection could revert the wrong lines"}
          className="flex shrink-0 items-center gap-1 rounded-md border border-rose-500/50 px-2 py-1 text-[11px] text-rose-300 hover:bg-rose-500/10 disabled:cursor-default disabled:opacity-40"
        >
          <Undo2 size={11} />
          {busy === "revert" ? "Reverting…" : "Revert"}
        </button>
        <button
          onClick={() => void exportAttribution()}
          disabled={!reviewTrusted}
          title="Download this agent's attribution as a portable JSON artifact"
          className="flex shrink-0 items-center gap-1 rounded-md border border-ink-600 px-2 py-1 text-[11px] text-zinc-300 hover:bg-ink-600 disabled:cursor-default disabled:opacity-40"
        >
          <Download size={11} />
          Export
        </button>
        <button
          onClick={onMarkReviewed}
          disabled={!reviewTrusted}
          title={
            reviewTrusted
              ? "Acknowledge this agent's changes (clears its dirty set)"
              : stale && connected
                ? "Last refresh failed — this diff may be stale, so it can't be acknowledged"
                : "Daemon unreachable — the changes can't be verified, so they can't be acknowledged"
          }
          className="flex shrink-0 items-center gap-1 rounded-md bg-emerald-600/90 px-2 py-1 text-[11px] font-medium text-white hover:brightness-110 disabled:cursor-default disabled:opacity-40"
        >
          <Check size={11} />
          Mark reviewed
        </button>
      </div>

      {/* Body: per-file hunks with selection + attribution chips. */}
      {!connected ? (
        <p className="px-4 py-3 text-[11px] text-zinc-600">
          daemon unreachable — this worktree&apos;s changes can&apos;t be shown
          or reviewed until it answers.
        </p>
      ) : !bundle ? (
        // No bundle ever loaded: an honest split between "in flight" and "the
        // load failed" — an eternal fake loading label is a mislabeled error.
        <p className="px-4 py-3 text-[11px] text-zinc-600">
          {stale ? "couldn't load this worktree's diff — refresh to retry" : "loading diff…"}
        </p>
      ) : files.length === 0 ? (
        <p className="px-4 py-3 text-[11px] text-zinc-600">
          No changes vs base
          {worktree?.branch ? ` (${worktree.branch})` : ""}.
        </p>
      ) : (
        files.map((f) => (
          <FileBlock
            key={f.path}
            file={f}
            hunks={hunksByPath[f.path] ?? []}
            collapsed={collapsedFiles.has(f.path)}
            onToggleCollapsed={() => toggleFileCollapsed(f.path)}
            fileFull={isFileFull(f.path)}
            selectable={allIdx(f.path).length > 0 || isSelectableEmpty(f.path)}
            selIdx={sel[f.path] ?? []}
            onToggleFile={() => toggleFile(f.path)}
            onToggleHunk={(idx) => toggleHunk(f.path, idx)}
            shared={contribCount(f.path) > 1}
            isContended={contended.has(f.path)}
            hunkAuthor={(idx) => hunkAuthor(f.path, idx)}
            colorFor={colorFor}
          />
        ))
      )}
    </section>
  );
}

/** One changed file: select-all checkbox + path + flags, then its hunks. */
function FileBlock({
  file,
  hunks,
  collapsed,
  onToggleCollapsed,
  fileFull,
  selectable,
  selIdx,
  onToggleFile,
  onToggleHunk,
  shared,
  isContended,
  hunkAuthor,
  colorFor,
}: {
  file: FileDiffEntry;
  hunks: HunkEntry[];
  collapsed: boolean;
  onToggleCollapsed: () => void;
  fileFull: boolean;
  /** Whole-file selection allowed: has hunks, or is an empty added file
   *  (header-only patch — still a real, mergeable change). */
  selectable: boolean;
  selIdx: number[];
  onToggleFile: () => void;
  onToggleHunk: (idx: number) => void;
  shared: boolean;
  isContended: boolean;
  hunkAuthor: (idx: number) => { agent_id: string; provider: string | null; turn_index: number } | null;
  colorFor: (tid: string | null | undefined) => { dot: string; text: string; chip: string } | null;
}) {
  return (
    <div className="border-b border-ink-700/50 last:border-b-0">
      <div className="flex items-center gap-2 px-4 py-1.5">
        <input
          type="checkbox"
          checked={fileFull}
          ref={(el) => {
            if (el) el.indeterminate = selIdx.length > 0 && !fileFull;
          }}
          onChange={onToggleFile}
          disabled={!selectable}
          aria-label={`Select all hunks in ${file.path}`}
          className="shrink-0 accent-teal-500"
        />
        <button
          onClick={onToggleCollapsed}
          aria-expanded={!collapsed}
          className="flex min-w-0 flex-1 items-center gap-1.5 text-left"
        >
          {collapsed ? (
            <ChevronRight size={11} className="shrink-0 text-zinc-600" />
          ) : (
            <ChevronDown size={11} className="shrink-0 text-zinc-600" />
          )}
          <StatusIcon status={file.status} />
          <span
            title={file.path}
            className="min-w-0 truncate whitespace-nowrap font-mono text-[11px] text-zinc-300"
          >
            {middleTruncate(file.path, 72)}
          </span>
          {shared && (
            <span
              className="shrink-0 text-[9px] font-semibold text-amber"
              title="changed by more than one teammate"
            >
              shared
            </span>
          )}
          {isContended && (
            <span
              className="shrink-0 text-[9px] font-semibold text-rose-400"
              title="also changed by another agent in this workspace"
            >
              contended
            </span>
          )}
          <span className="tnum ml-auto shrink-0 font-mono text-[10px]">
            <span className="text-emerald-400">+{file.additions}</span>
            <span className="text-rose-400"> -{file.deletions}</span>
          </span>
        </button>
      </div>

      {!collapsed &&
        (file.binary ? (
          <p className="px-4 pb-2 text-[11px] text-zinc-600">Binary file — no preview.</p>
        ) : hunks.length === 0 ? (
          <p className="px-4 pb-2 text-[11px] text-zinc-600">
            {file.status === "added" ? "Empty new file." : "No hunks vs base."}
          </p>
        ) : (
          hunks.map((h) => {
            const ha = hunkAuthor(h.index);
            const hcol = colorFor(ha?.agent_id);
            return (
              <div key={h.index} className="mx-4 mb-2 overflow-hidden rounded border border-ink-700">
                <label className="flex cursor-pointer items-center gap-2 bg-ink-800/80 px-2 py-1 hover:bg-ink-700">
                  <input
                    type="checkbox"
                    checked={selIdx.includes(h.index)}
                    onChange={() => onToggleHunk(h.index)}
                    aria-label={`Select hunk ${h.header}`}
                    className="shrink-0 accent-teal-500"
                  />
                  {ha && (
                    <span
                      className={`flex shrink-0 items-center gap-1 rounded px-1 text-[9px] ${
                        hcol?.chip ?? "bg-ink-600 text-zinc-400"
                      }`}
                      title={`hunk by ${authorName(ha)} (turn ${ha.turn_index + 1})`}
                    >
                      <span className={`h-1.5 w-1.5 rounded-full ${hcol?.dot ?? "bg-zinc-500"}`} />
                      {authorName(ha)} · t{ha.turn_index + 1}
                    </span>
                  )}
                  <span
                    title={h.header}
                    className="min-w-0 truncate whitespace-nowrap font-mono text-[10px] text-zinc-500"
                  >
                    {h.header}
                  </span>
                  <span className="tnum ml-auto shrink-0 font-mono text-[10px]">
                    <span className="text-emerald-400">+{h.additions}</span>
                    <span className="text-rose-400"> -{h.deletions}</span>
                  </span>
                </label>
                <HunkText hunk={h} />
              </div>
            );
          })
        ))}
    </div>
  );
}
