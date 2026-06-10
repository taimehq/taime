import { useCallback, useEffect, useMemo, useState } from "react";
import "../monacoSetup"; // point Monaco at the bundled (offline) build before use
import { DiffEditor } from "@monaco-editor/react";
import {
  GitBranch,
  X,
  Check,
  FilePlus,
  FileMinus,
  FileText,
  GitMerge,
  Undo2,
} from "lucide-react";
import {
  api,
  type FileDiffEntry,
  type HunkedFileEntry,
  type WorktreeInfo,
  type AttributionResponse,
  type FileContributor,
} from "../api";
import { useStore } from "../store";
import { providerTitle } from "../lib/providerLabel";
import { AUTHOR_COLORS, authorName } from "../lib/attribution";

const UNATTRIBUTED = "unattributed";

const FOCUS_RING =
  "focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-accent";

function langForPath(path: string): string {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  const map: Record<string, string> = {
    ts: "typescript", tsx: "typescript", js: "javascript", jsx: "javascript",
    py: "python", rs: "rust", go: "go", java: "java", rb: "ruby", php: "php",
    c: "c", h: "c", cpp: "cpp", cs: "csharp", json: "json", md: "markdown",
    css: "css", scss: "scss", html: "html", yml: "yaml", yaml: "yaml",
    toml: "ini", sh: "shell", sql: "sql",
  };
  return map[ext] ?? "plaintext";
}

function StatusIcon({ status }: { status: string }) {
  if (status === "added") return <FilePlus size={13} className="text-emerald-400" />;
  if (status === "deleted") return <FileMinus size={13} className="text-rose-400" />;
  return <FileText size={13} className="text-amber" />;
}

/**
 * Full-screen review + selective merge/revert of one agent's changes vs its
 * worktree base. Provenance is explicit (which agent, which branch); the user
 * picks files/hunks and lands them on the main checkout or another agent's
 * worktree — or reverts them from this agent. Contended files (also changed by
 * another agent in the session) are flagged.
 */
export function DiffView() {
  const terminalId = useStore((s) => s.diffTerminalId);
  const closeDiff = useStore((s) => s.closeDiff);
  const clearDirty = useStore((s) => s.clearDirty);
  const markReviewed = useStore((s) => s.markReviewed);
  const showSnackbar = useStore((s) => s.showSnackbar);
  const frames = useStore((s) => s.frames);
  const pendingSwitch = useStore((s) => s.pendingSwitch);
  const resolveSwitch = useStore((s) => s.resolveSwitch);
  const connected = useStore((s) => s.connected);

  const [files, setFiles] = useState<FileDiffEntry[]>([]);
  const [hunks, setHunks] = useState<HunkedFileEntry[]>([]);
  const [worktree, setWorktree] = useState<WorktreeInfo | null>(null);
  const [attribution, setAttribution] = useState<AttributionResponse | null>(null);
  const [contended, setContended] = useState<Set<string>>(new Set());
  const [selected, setSelected] = useState<string | null>(null);
  const [sel, setSel] = useState<Record<string, number[]>>({});
  const [target, setTarget] = useState("main");
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const frame = frames.find((f) => f.terminalId === terminalId);

  const load = useCallback(async () => {
    if (!terminalId) return;
    setLoading(true);
    setError(null);
    try {
      // STRICT reads: daemon-down REJECTS (DaemonUnreachableError) instead of
      // resolving empty shapes — so a dead daemon can never render as an
      // authoritative "No changes to review" with a working Mark reviewed.
      const [fd, hk, wt, attr] = await Promise.all([
        api.getFileDiffs(terminalId),
        api.getHunks(terminalId),
        api.getWorktree(terminalId),
        api.getAttribution(terminalId),
      ]);
      setFiles(fd.files);
      setHunks(hk.files);
      setWorktree(wt);
      setAttribution(attr);
      setSelected((prev) => prev ?? fd.files[0]?.path ?? null);
      if (wt?.mode === "shared" || wt?.mode === "isolated") {
        // Contention is workspace-wide: key off the worktree's project root
        // (the workspace-grouping id), not a frame label. Decoration only —
        // a contention failure must not take down the loaded diff.
        const root = wt?.project_root;
        if (root) {
          const c = await api.getContention(root).catch(() => []);
          setContended(new Set(c.map((r) => r.path)));
        }
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
    // Re-load when the daemon connection flips (down → error state shown; back
    // up → the surface heals without reopening). Mirrors AgentDetail's probe.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [terminalId, connected]);

  useEffect(() => {
    setSel({});
    setSelected(null);
    load();
  }, [load]);

  // Esc closes the review surface (mirrors the X button).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") closeDiff();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [closeDiff]);

  const hunksByPath = useMemo(() => {
    const m: Record<string, HunkedFileEntry> = {};
    for (const h of hunks) m[h.path] = h;
    return m;
  }, [hunks]);

  const current = useMemo(
    () => files.find((f) => f.path === selected) ?? null,
    [files, selected],
  );

  // Team ordering (owner first, then members) → stable author colors.
  const teamOrder = useMemo(() => {
    const team = attribution?.team ?? [];
    return [...team.filter((t) => !t.member_of), ...team.filter((t) => t.member_of)].map(
      (t) => t.agent_id,
    );
  }, [attribution]);
  const colorIndexById = useMemo(() => {
    const m: Record<string, number> = {};
    teamOrder.forEach((id, i) => (m[id] = i % AUTHOR_COLORS.length));
    return m;
  }, [teamOrder]);

  if (!terminalId) return null;

  const isTeam = (attribution?.team.length ?? 0) > 1;
  const lastAuthor = (path: string): FileContributor | null =>
    attribution?.files[path]?.last ?? null;
  const contribCount = (path: string): number =>
    attribution?.files[path]?.contributors.length ?? 0;
  const colorFor = (tid: string | null | undefined) =>
    tid && colorIndexById[tid] != null ? AUTHOR_COLORS[colorIndexById[tid]] : null;
  const hunkAuthor = (path: string, index: number) =>
    attribution?.files[path]?.hunks?.[String(index)] ?? null;

  const agentName =
    (frame && providerTitle(frame.provider)) ??
    worktree?.provider ??
    "agent";

  // Sibling agents = same Task (the membership grouping; Uncategorized agents
  // don't cross-link).
  const otherAgents = frames.filter(
    (f) =>
      f.terminalId &&
      f.terminalId !== terminalId &&
      f.taskId &&
      f.taskId === frame?.taskId,
  );

  const allIdx = (path: string) =>
    (hunksByPath[path]?.hunks ?? []).map((h) => h.index);
  const isFileFull = (path: string) => {
    const all = allIdx(path);
    return all.length > 0 && (sel[path]?.length ?? 0) === all.length;
  };
  const toggleFile = (path: string) =>
    setSel((s) => {
      const next = { ...s };
      if (isFileFull(path)) delete next[path];
      else next[path] = allIdx(path);
      return next;
    });
  const toggleHunk = (path: string, idx: number) =>
    setSel((s) => {
      const cur = new Set(s[path] ?? []);
      cur.has(idx) ? cur.delete(idx) : cur.add(idx);
      const next = { ...s };
      if (cur.size === 0) delete next[path];
      else next[path] = [...cur].sort((a, b) => a - b);
      return next;
    });

  const selectionCount = Object.values(sel).reduce((n, a) => n + a.length, 0);
  const buildSelections = (): Record<string, number[]> => sel;

  // Group the changed files by their last author so the reviewer sees, at a
  // glance, "these files were last changed by member X".
  const fileGroups = (() => {
    const byKey = new Map<string, FileDiffEntry[]>();
    for (const f of files) {
      const key = lastAuthor(f.path)?.agent_id ?? UNATTRIBUTED;
      const list = byKey.get(key);
      if (list) list.push(f);
      else byKey.set(key, [f]);
    }
    const order = [
      ...teamOrder.filter((k) => byKey.has(k)),
      ...[...byKey.keys()].filter((k) => !teamOrder.includes(k) && k !== UNATTRIBUTED),
      ...(byKey.has(UNATTRIBUTED) ? [UNATTRIBUTED] : []),
    ];
    return order.map((key) => ({ key, files: byKey.get(key) ?? [] }));
  })();

  const selectGroup = (groupFiles: FileDiffEntry[], on: boolean) =>
    setSel((s) => {
      const next = { ...s };
      for (const f of groupFiles) {
        const idx = allIdx(f.path);
        if (on && idx.length) next[f.path] = idx;
        else delete next[f.path];
      }
      return next;
    });

  const renderFileRow = (f: FileDiffEntry) => {
    const a = lastAuthor(f.path);
    const shared = contribCount(f.path) > 1;
    return (
      <li key={f.path}>
        <div
          className={`flex items-center gap-2 px-2 py-1.5 text-[12px] ${
            selected === f.path ? "bg-ink-600" : "hover:bg-ink-700"
          }`}
        >
          <input
            type="checkbox"
            checked={isFileFull(f.path)}
            ref={(el) => {
              if (el)
                el.indeterminate = (sel[f.path]?.length ?? 0) > 0 && !isFileFull(f.path);
            }}
            onChange={() => toggleFile(f.path)}
            className="accent-teal-500"
          />
          <button
            onClick={() => setSelected(f.path)}
            className="flex min-w-0 flex-1 items-center gap-2 text-left"
          >
            <StatusIcon status={f.status} />
            <span
              className={`flex-1 truncate font-mono ${
                selected === f.path ? "text-zinc-100" : "text-zinc-400"
              }`}
            >
              {f.path}
            </span>
            {isTeam && a && (
              <span className="shrink-0 text-[9px] text-zinc-500">turn {a.turn_index + 1}</span>
            )}
            {shared && (
              <span
                className="shrink-0 text-[9px] font-semibold text-amber"
                title="changed by more than one teammate"
              >
                shared
              </span>
            )}
            {contended.has(f.path) && (
              <span className="shrink-0 text-[10px] font-semibold text-rose-400">contended</span>
            )}
            <span className="shrink-0 font-mono text-[10px] tabular-nums">
              <span className="text-emerald-400">+{f.additions}</span>
              <span className="text-rose-400"> -{f.deletions}</span>
            </span>
          </button>
        </div>
      </li>
    );
  };

  const apply = async (mode: "merge" | "revert") => {
    if (selectionCount === 0) return;
    setBusy(true);
    try {
      const res = await api.applySelection(terminalId, {
        target: mode === "revert" ? "self" : target,
        mode,
        selections: buildSelections(),
      });
      if (res.applied) {
        const where = mode === "revert" ? agentName : target === "main" ? "main" : "agent";
        showSnackbar({
          type: "success",
          message: `${mode === "merge" ? "Merged" : "Reverted"} ${res.files.length} file(s) ${mode === "merge" ? `→ ${where}` : `from ${where}`}`,
        });
        setSel({});
        await load();
      } else if (res.conflicts.length) {
        showSnackbar({ type: "error", message: `Conflicts: ${res.conflicts[0]}` });
        await load();
      } else {
        showSnackbar({ type: "error", message: res.error ?? "Apply failed" });
      }
    } catch (e) {
      showSnackbar({ type: "error", message: String(e) });
    } finally {
      setBusy(false);
    }
  };

  const onMarkReviewed = () => {
    clearDirty(terminalId);
    // Review state is keyed by agent id — it outlives this frame.
    markReviewed(terminalId);
    closeDiff();
    if (pendingSwitch) resolveSwitch(true);
  };

  // The diff on screen is authoritative only while the daemon is answering and
  // the last load succeeded. Acknowledging changes that were never truly shown
  // would disarm the switch guard on a falsehood — so Mark reviewed gates here.
  const reviewTrusted = connected && !error;

  const totalAdd = files.reduce((n, f) => n + f.additions, 0);
  const totalDel = files.reduce((n, f) => n + f.deletions, 0);
  const currentHunks = current ? (hunksByPath[current.path]?.hunks ?? []) : [];

  return (
    <div className="fixed inset-x-0 top-12 bottom-0 z-50 flex flex-col bg-ink-900">
      {/* Header / provenance / actions */}
      <div className="flex items-center justify-between gap-3 border-b border-ink-600 bg-ink-800 px-4 py-2.5">
        <div className="flex min-w-0 items-center gap-3 text-sm">
          <span className="shrink-0 font-semibold text-zinc-100">{agentName}</span>
          {worktree?.mode === "isolated" && worktree.branch && (
            <span className="flex shrink-0 items-center gap-1.5 rounded-md border border-ink-600 px-2 py-0.5 font-mono text-[11px] text-teal-300">
              <GitBranch size={12} />
              {worktree.branch}
            </span>
          )}
          {worktree?.mode === "shared" && (
            <span className="shrink-0 rounded-md border border-ink-600 px-2 py-0.5 text-[11px] text-zinc-500">
              shared dir (heuristic)
            </span>
          )}
          {isTeam && (
            <span
              className="shrink-0 rounded-md border border-teal-600/40 bg-teal-400/10 px-2 py-0.5 text-[11px] text-teal-300"
              title="A delegation team shares this worktree; files are grouped by author below."
            >
              team · {attribution?.team.length} agents
            </span>
          )}
          <span className="shrink-0 text-zinc-500 tabular-nums">{files.length} files</span>
          <span className="shrink-0 font-mono text-[11px] tabular-nums">
            <span className="text-emerald-400">+{totalAdd}</span>{" "}
            <span className="text-rose-400">-{totalDel}</span>
          </span>
        </div>

        <div className="flex shrink-0 items-center gap-2">
          {/* Merge target */}
          <label className="flex items-center gap-1.5 text-[11px] text-zinc-500">
            into
            <select
              value={target}
              onChange={(e) => setTarget(e.target.value)}
              className="rounded-md border border-ink-600 bg-ink-900 px-2 py-1 text-[11px] text-zinc-200"
            >
              <option value="main">main</option>
              {otherAgents.map((a) => (
                <option key={a.terminalId!} value={a.terminalId!}>
                  {providerTitle(a.provider)} ({a.terminalId!.slice(0, 6)})
                </option>
              ))}
            </select>
          </label>
          <button
            disabled={busy || selectionCount === 0}
            onClick={() => apply("merge")}
            className={`flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-sm font-medium text-white hover:bg-primary-hover disabled:cursor-default disabled:opacity-40 ${FOCUS_RING}`}
          >
            <GitMerge size={14} />
            Merge {selectionCount > 0 ? `${selectionCount}` : ""}
          </button>
          <button
            disabled={busy || selectionCount === 0}
            onClick={() => apply("revert")}
            className={`flex items-center gap-1.5 rounded-md border border-rose-500/50 px-3 py-1.5 text-sm text-rose-300 hover:bg-rose-500/10 disabled:cursor-default disabled:opacity-40 ${FOCUS_RING}`}
          >
            <Undo2 size={14} />
            Revert
          </button>
          <div className="mx-1 h-5 w-px bg-ink-600" />
          <button
            onClick={onMarkReviewed}
            disabled={!reviewTrusted || loading}
            title={
              reviewTrusted
                ? undefined
                : "Daemon unreachable — the changes can't be verified, so they can't be acknowledged"
            }
            className={`flex items-center gap-1.5 rounded-md bg-emerald-600/90 px-3 py-1.5 text-sm font-medium text-white hover:brightness-110 disabled:cursor-default disabled:opacity-40 ${FOCUS_RING}`}
          >
            <Check size={14} />
            Mark reviewed
          </button>
          <button
            onClick={closeDiff}
            className={`rounded-md p-1.5 text-zinc-400 hover:bg-ink-600 hover:text-zinc-200 ${FOCUS_RING}`}
            aria-label="Close diff"
          >
            <X size={18} />
          </button>
        </div>
      </div>

      <div className="flex min-h-0 flex-1">
        {/* File list */}
        <aside className="w-72 shrink-0 overflow-auto border-r border-ink-600 bg-ink-800/60">
          {loading && <p className="p-3 text-xs text-zinc-500">Loading diff…</p>}
          {error && <p className="p-3 text-xs text-rose-400">{error}</p>}
          {isTeam ? (
            // Team worktree: group files by who last changed them, so authorship
            // is obvious and you can merge one teammate's work in one click.
            fileGroups.map((g) => {
              const a0 = g.files[0] ? lastAuthor(g.files[0].path) : null;
              const col = g.key === UNATTRIBUTED ? null : colorFor(g.key);
              const name = g.key === UNATTRIBUTED ? "Unattributed" : authorName(a0);
              const groupSelected = g.files.every((f) => isFileFull(f.path));
              return (
                <div key={g.key} className="border-b border-ink-700/50">
                  <div className="flex items-center gap-1.5 bg-ink-800/80 px-2 py-1">
                    <span className={`h-2 w-2 rounded-full ${col?.dot ?? "bg-zinc-600"}`} />
                    <span className={`text-[11px] font-semibold ${col?.text ?? "text-zinc-400"}`}>
                      {name}
                    </span>
                    <span className="text-[10px] text-zinc-600">{g.files.length}</span>
                    <button
                      onClick={() => selectGroup(g.files, !groupSelected)}
                      className="ml-auto text-[10px] text-zinc-500 hover:text-zinc-300"
                    >
                      {groupSelected ? "Deselect" : "Select all"}
                    </button>
                  </div>
                  <ul>{g.files.map(renderFileRow)}</ul>
                </div>
              );
            })
          ) : (
            <ul>{files.map(renderFileRow)}</ul>
          )}
        </aside>

        {/* Diff + hunk selection */}
        <div className="flex min-w-0 flex-1 flex-col">
          <div className="min-h-0 flex-1">
            {!loading && files.length === 0 && !reviewTrusted ? (
              // Daemon down / load failed: the empty file list is a FALLBACK,
              // not a verified "no changes" — say so, and offer no Mark
              // reviewed (acknowledging unseen changes would disarm the guard).
              <div className="flex h-full flex-col items-center justify-center px-6 text-center">
                <GitMerge size={32} className="text-zinc-600" />
                <p className="mt-3 text-sm text-zinc-300">
                  {connected ? "Couldn't load this agent's changes" : "Daemon unreachable"}
                </p>
                <p className="mt-1 max-w-sm text-[12px] text-zinc-400">
                  {connected
                    ? (error ?? "The diff query failed.")
                    : "This agent's changes can't be shown or reviewed until the daemon answers — retrying."}
                </p>
              </div>
            ) : !loading && files.length === 0 ? (
              // Nothing changed: a single centered empty state spans the body so
              // we never render a bare bg-ink-900 void.
              <div className="flex h-full flex-col items-center justify-center px-6 text-center">
                <GitMerge size={32} className="text-zinc-600" />
                <p className="mt-3 text-sm text-zinc-300">No changes to review</p>
                <p className="mt-1 max-w-sm text-[12px] text-zinc-400">
                  {worktree?.mode === "isolated"
                    ? `This agent's worktree (${worktree.branch ?? "branch"}) has no changes vs its base yet.`
                    : "No uncommitted changes in the shared working directory."}
                </p>
                <button
                  onClick={onMarkReviewed}
                  className={`mt-4 flex items-center gap-1.5 rounded-md border border-ink-500 px-3 py-1.5 text-[12px] text-zinc-300 hover:bg-ink-700 hover:text-zinc-100 ${FOCUS_RING}`}
                >
                  <Check size={13} />
                  Mark reviewed
                </button>
              </div>
            ) : (
              <>
                {current && current.binary && (
                  <p className="p-4 text-sm text-zinc-500">Binary file — no preview.</p>
                )}
                {current && !current.binary && (
                  <DiffEditor
                    key={current.path}
                    theme="taime-dark"
                    language={langForPath(current.path)}
                    original={current.original}
                    modified={current.modified}
                    options={{
                      readOnly: true,
                      renderSideBySide: true,
                      minimap: { enabled: false },
                      fontSize: 12,
                      fontFamily:
                        "'Geist Mono', ui-monospace, SFMono-Regular, Menlo, monospace",
                      scrollBeyondLastLine: false,
                    }}
                  />
                )}
                {!current && !loading && (
                  <p className="p-4 text-sm text-zinc-600">Select a file to view its diff.</p>
                )}
              </>
            )}
          </div>

          {/* Per-hunk selection strip */}
          {current && currentHunks.length > 0 && (
            <div className="max-h-40 shrink-0 overflow-auto border-t border-ink-600 bg-ink-800/80 p-2">
              <p className="mb-1 flex items-center gap-1.5 px-1 text-[10px] uppercase tracking-wide text-zinc-600">
                <span>Hunks — select to merge/revert</span>
                {(() => {
                  const a = current ? lastAuthor(current.path) : null;
                  if (!isTeam || !a) return null;
                  const col = colorFor(a.agent_id);
                  return (
                    <span className={`rounded px-1.5 normal-case ${col?.chip ?? "text-zinc-400"}`}>
                      {authorName(a)} · turn {a.turn_index + 1}
                    </span>
                  );
                })()}
                {current && contribCount(current.path) > 1 && (
                  <span className="rounded bg-amber/15 px-1.5 normal-case text-amber">
                    multiple authors — labeled per hunk
                  </span>
                )}
              </p>
              {currentHunks.map((h) => {
                const ha = current ? hunkAuthor(current.path, h.index) : null;
                const hcol = colorFor(ha?.agent_id);
                return (
                <label
                  key={h.index}
                  className="flex cursor-pointer items-center gap-2 rounded px-1.5 py-1 hover:bg-ink-700"
                >
                  <input
                    type="checkbox"
                    checked={(sel[current.path] ?? []).includes(h.index)}
                    onChange={() => toggleHunk(current.path, h.index)}
                    className="accent-teal-500"
                  />
                  {ha && (
                    <span
                      className={`flex shrink-0 items-center gap-1 rounded px-1 text-[9px] ${hcol?.chip ?? "text-zinc-400"}`}
                      title={`hunk by ${authorName(ha)} (turn ${ha.turn_index + 1})`}
                    >
                      <span className={`h-1.5 w-1.5 rounded-full ${hcol?.dot ?? "bg-zinc-500"}`} />
                      {authorName(ha)} · t{ha.turn_index + 1}
                    </span>
                  )}
                  <span className="truncate font-mono text-[11px] text-zinc-400">
                    {h.header}
                  </span>
                  <span className="ml-auto shrink-0 font-mono text-[10px] tabular-nums">
                    <span className="text-emerald-400">+{h.additions}</span>
                    <span className="text-rose-400"> -{h.deletions}</span>
                  </span>
                </label>
                );
              })}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
