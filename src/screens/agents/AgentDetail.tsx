import { useEffect, useState } from "react";
import { LayoutGrid, Power, X } from "lucide-react";
import { api, type WorktreeInfo } from "../../api";
import {
  useStore,
  termModeFor,
  type Frame,
} from "../../store";
import { TerminalViewRustPty } from "../../components/TerminalViewRustPty";
import { StatusBadge } from "../../components/StatusBadge";
import { statusDotClass } from "../../lib/agentStatus";
import { providerTitle } from "../../lib/providerLabel";
import { profileMeta, displayRole } from "../../lib/profiles";
import { agentLabel } from "../../lib/agentLabel";
import { useTasks } from "../../hooks/useTasks";
import { useAgentTurns } from "./useAgentTurns";
import { ConsolePanel } from "./ConsolePanel";
import { DiffPanel } from "./DiffPanel";
import { ActivityPanel } from "./ActivityPanel";
import { middleTruncate } from "./format";

/** The four content tabs. Terminal/Console mirror the per-agent termMode
 *  (store-persisted); Diff/Activity are view-local. */
type TabId = "terminal" | "console" | "diff" | "activity";

/**
 * The focused agent view (Agents section, focus mode): one agent's full
 * chrome — identity header (Agent ID, Task membership, profile, provider,
 * worktree provenance) over Terminal | Console | Diff | Activity. The
 * Terminal tab is the EXISTING TerminalViewRustPty, mounted unchanged; the
 * Console is a projection of the same stream, never a second one.
 */
export function AgentDetail({ frame }: { frame: Frame }) {
  // The Agent ID (attribution anchor); pre-provision agents fall back to the
  // PTY session id — the same coalescing the daemon's graph queries use.
  const agentId = frame.terminalId;
  const anchorId = agentId ?? frame.ptySessionId ?? frame.key;
  const sessionId = frame.ptySessionId!; // AgentsScreen only routes daemon frames here

  const workspaceDir = useStore((s) => s.workspaceDir);
  const connected = useStore((s) => s.connected);
  const meta = useStore((s) => s.rustPtySessions[sessionId]);
  const rawStatus = useStore((s) =>
    meta?.status === "exited"
      ? "EXITED"
      : agentId
        ? s.terminalStatuses[agentId]
        : undefined,
  );
  const dirtyCount = useStore((s) =>
    agentId ? (s.dirty[agentId]?.count ?? 0) : 0,
  );
  const termMode = useStore((s) => termModeFor(s, anchorId));
  const setTermMode = useStore((s) => s.setTermMode);
  const selectTask = useStore((s) => s.selectTask);
  const openDiff = useStore((s) => s.openDiff);
  const closeFrame = useStore((s) => s.closeFrame);
  const killRustPty = useStore((s) => s.killRustPty);

  const { tasks } = useTasks(workspaceDir);
  const turnsState = useAgentTurns(anchorId);

  // Tab state is view-local; on agent change it re-opens on that agent's
  // persisted termMode (Terminal vs Console — the store remembers per agent).
  const [tab, setTab] = useState<TabId>(termMode);
  useEffect(() => {
    setTab(termModeFor(useStore.getState(), anchorId));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [anchorId]);

  // Worktree provenance (real path + mode + branch) from the worktree row.
  const [wt, setWt] = useState<WorktreeInfo | null>(null);
  useEffect(() => {
    let alive = true;
    setWt(null);
    if (!agentId) return;
    api
      .getWorktree(agentId)
      .then((w) => {
        if (alive) setWt(w);
      })
      // Strict read: rejects on daemon-down. Keep wt null — the provenance
      // line already renders "daemon unreachable · retrying" off `connected`.
      .catch(() => {});
    return () => {
      alive = false;
    };
    // Re-probe when the daemon comes back (connected flips false → true).
  }, [agentId, connected]);

  // Stop is destructive (terminates the process): explicit two-step with an
  // in-flight state. The confirm arms for 3s, then disarms.
  const [confirmStop, setConfirmStop] = useState(false);
  const [stopping, setStopping] = useState(false);
  useEffect(() => {
    if (!confirmStop) return;
    const t = setTimeout(() => setConfirmStop(false), 3000);
    return () => clearTimeout(t);
  }, [confirmStop]);
  const onStop = async () => {
    if (stopping) return;
    if (!confirmStop) {
      setConfirmStop(true);
      return;
    }
    setStopping(true);
    try {
      await killRustPty(frame.key); // removes the frame; this view unmounts
    } finally {
      setStopping(false);
    }
  };

  const exited = meta?.status === "exited";
  const taskId = frame.taskId ?? meta?.taskId ?? null;
  const task = taskId ? tasks.find((t) => t.id === taskId) : undefined;
  // Lead with the ROLE (resolved from the launch profile or the daemon's role
  // for an adopted/assigned worker); provider + model are secondary.
  const role = displayRole(frame.agentProfile ?? meta?.role);
  const rmeta = role ? profileMeta(role) : null;
  const RoleIcon = rmeta?.icon;
  const worktreePath = wt?.worktree_path ?? meta?.cwd ?? null;
  const worktreeMode = wt?.mode ?? null;
  const branch = wt?.branch ?? meta?.branch ?? null;

  const selectTab = (id: TabId) => {
    setTab(id);
    // Terminal/Console are the persisted per-agent terminal mode.
    if (id === "terminal" || id === "console") setTermMode(anchorId, id);
    // The Diff tab's review surface is the existing DiffView (store-mounted,
    // scoped to this agent); the tab body keeps the change summary.
    if (id === "diff" && agentId) openDiff(agentId);
  };

  const tabClass = (on: boolean, disabled = false) =>
    `flex items-center gap-1.5 border-b-2 px-2.5 py-1.5 text-[11px] ${
      on
        ? "border-accent text-zinc-100"
        : disabled
          ? "border-transparent text-zinc-700"
          : "border-transparent text-zinc-500 hover:text-zinc-300"
    } disabled:cursor-default`;

  return (
    <div className="flex h-full w-full flex-col gap-2">
      <FrameStrip activeKey={frame.key} />

      <div className="flex min-h-0 flex-1 flex-col overflow-hidden rounded-lg border border-teal-600/70 bg-ink-900">
        {/* ── Identity header ─────────────────────────────────────────── */}
        <header className="flex shrink-0 flex-col gap-1 border-b border-ink-600 bg-ink-800 px-3 py-2">
          <div className="flex items-center gap-2.5">
            {RoleIcon && <RoleIcon size={14} className="shrink-0 text-accent" />}
            <span className="shrink-0 text-[13px] font-semibold text-zinc-100">
              {rmeta ? rmeta.label : providerTitle(frame.provider)}
            </span>
            <span
              className="min-w-0 truncate whitespace-nowrap font-mono text-[11px] text-zinc-500"
              title={anchorId}
            >
              {agentLabel(anchorId)}
            </span>
            <StatusBadge status={frame.pending ? "PENDING" : rawStatus} />
            {frame.model && (
              <span
                className="shrink-0 rounded bg-ink-600 px-1.5 text-[10px] font-medium text-zinc-300"
                title="Model (from the agent's startup banner)"
              >
                {frame.model}
              </span>
            )}
            {dirtyCount > 0 && (
              <span
                className="shrink-0 rounded bg-amber/20 px-1.5 py-0.5 text-[10px] font-medium tabular-nums text-amber"
                title={`${dirtyCount} changed path(s) since last review`}
              >
                {dirtyCount} dirty
              </span>
            )}
            <span className="flex-1" />
            {!turnsState.loading && !turnsState.error && (
              <span className="shrink-0 font-mono text-[10px] tabular-nums text-zinc-600">
                {turnsState.turns.length} turn
                {turnsState.turns.length === 1 ? "" : "s"}
              </span>
            )}
            <button
              onClick={() => void onStop()}
              disabled={stopping || exited}
              title={
                exited
                  ? "Process already exited"
                  : confirmStop
                    ? "Click again to terminate the process"
                    : "Stop agent (terminate process)"
              }
              className={`flex shrink-0 items-center gap-1 rounded-md border px-2 py-0.5 text-[11px] ${
                confirmStop
                  ? "border-rose-500/70 bg-rose-500/15 text-rose-300"
                  : "border-ink-500 text-rose-400/80 enabled:hover:bg-rose-500/10"
              } disabled:cursor-default disabled:opacity-40`}
            >
              <Power size={11} />
              {stopping ? "stopping…" : confirmStop ? "confirm stop" : "stop"}
            </button>
            <button
              onClick={() => void closeFrame(frame.key)}
              title="Close view (agent keeps running)"
              aria-label="Close view"
              className="shrink-0 rounded p-0.5 text-zinc-500 hover:text-zinc-200"
            >
              <X size={14} />
            </button>
          </div>

          {/* Membership + provenance line. (A per-agent assignment line goes
              here when the daemon stores one — a known backend gap.) */}
          <div className="flex min-w-0 items-center gap-2 text-[11px]">
            {taskId ? (
              <button
                onClick={() => selectTask(taskId)}
                title={task ? task.title : taskId}
                className="min-w-0 max-w-[220px] truncate whitespace-nowrap text-left text-accent hover:underline"
              >
                Task: {task ? task.title : taskId}
              </button>
            ) : (
              <span className="shrink-0 text-zinc-600">Task: Uncategorized</span>
            )}
            {rmeta && (
              <>
                <span className="shrink-0 text-zinc-700">·</span>
                <span className="shrink-0 text-zinc-400" title="Agent role / profile">
                  Role: {rmeta.label}
                </span>
              </>
            )}
            <span className="shrink-0 text-zinc-700">·</span>
            <span className="shrink-0 font-mono text-[10px] text-zinc-400">
              {providerTitle(frame.provider)}
            </span>
            <span className="shrink-0 text-zinc-700">·</span>
            {worktreePath ? (
              <span
                className="shrink-0 whitespace-nowrap font-mono text-[10px] text-zinc-500"
                title={`${worktreePath}${branch ? ` · ${branch}` : ""}`}
              >
                {middleTruncate(worktreePath, 46)}
              </span>
            ) : (
              <span className="shrink-0 text-[10px] text-zinc-600">
                {connected ? "no worktree" : "daemon unreachable · retrying"}
              </span>
            )}
            {worktreeMode && (
              <span
                className="shrink-0 rounded bg-ink-600 px-1 text-[9px] font-medium text-zinc-400"
                title={`Worktree mode: ${worktreeMode}`}
              >
                {worktreeMode} worktree
              </span>
            )}
          </div>
        </header>

        {/* ── Tabs ────────────────────────────────────────────────────── */}
        <div className="flex shrink-0 items-center border-b border-ink-600 bg-ink-800/60 px-2">
          <button
            onClick={() => selectTab("terminal")}
            className={tabClass(tab === "terminal")}
          >
            Terminal
          </button>
          <button
            onClick={() => selectTab("console")}
            className={tabClass(tab === "console")}
          >
            Console
          </button>
          <button
            onClick={() => selectTab("diff")}
            disabled={!agentId}
            title={agentId ? undefined : "No worktree row — diff unavailable"}
            className={tabClass(tab === "diff", !agentId)}
          >
            Diff
            {dirtyCount > 0 && (
              <span className="rounded bg-amber/20 px-1 text-[9px] font-medium tabular-nums text-amber">
                {dirtyCount}
              </span>
            )}
          </button>
          <button
            onClick={() => selectTab("activity")}
            className={tabClass(tab === "activity")}
          >
            Activity
          </button>
          <span className="flex-1" />
          {/* PTY status — locked lexicon copy, from the session lifecycle.
              connectionLost is the third state: the attach channel died
              without an exit (the agent may be alive) — claiming "attached ·
              live" then would assert a stream that's dead and silently eat
              keystrokes. */}
          {exited ? (
            <span className="flex shrink-0 items-center gap-1.5 pr-1 text-[10px] text-zinc-600">
              <span className="h-1.5 w-1.5 rounded-full bg-zinc-700" />
              re-attached · last screen
            </span>
          ) : meta?.connectionLost ? (
            <span
              className="flex shrink-0 items-center gap-1.5 pr-1 text-[10px] text-amber"
              title="The view's connection to the daemon dropped without the agent exiting — close and reopen this view to reattach"
            >
              <span className="h-1.5 w-1.5 rounded-full bg-amber" />
              connection lost · reopen to reattach
            </span>
          ) : (
            <span className="flex shrink-0 items-center gap-1.5 pr-1 text-[10px] text-emerald-400">
              <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-emerald-400" />
              PTY attached · live
            </span>
          )}
        </div>

        {/* ── Tab content ─────────────────────────────────────────────── */}
        <div className="min-h-0 flex-1">
          {tab === "terminal" && (
            // data-term-key keeps Finder file/screenshot drop hit-testing
            // working in the detail view (same contract as the grid cells).
            <div data-term-key={sessionId} className="h-full">
              <TerminalViewRustPty sessionId={sessionId} frameKey={frame.key} />
            </div>
          )}
          {tab === "console" && (
            <ConsolePanel
              key={sessionId}
              agentId={agentId ?? null}
              anchorId={anchorId}
              exited={exited}
              wireStatus={rawStatus}
              turns={turnsState.turns}
              loading={turnsState.loading}
              error={turnsState.error}
            />
          )}
          {tab === "diff" && <DiffPanel agentId={agentId} />}
          {tab === "activity" && (
            <ActivityPanel
              anchorId={anchorId}
              turns={turnsState.turns}
              loading={turnsState.loading}
              error={turnsState.error}
            />
          )}
        </div>
      </div>
    </div>
  );
}

/**
 * Compact window strip for focus mode: back-to-grid + one tab per frame —
 * the same store actions the shell-grid tabs drive (guarded switching), so
 * keyboard (⌘1–9, ⌘⇧⏎) and mouse stay equivalent.
 */
function FrameStrip({ activeKey }: { activeKey: string }) {
  const frames = useStore((s) => s.frames);
  const setActiveFrameGuarded = useStore((s) => s.setActiveFrameGuarded);
  const setLayoutMode = useStore((s) => s.setLayoutMode);
  const terminalStatuses = useStore((s) => s.terminalStatuses);
  const dirty = useStore((s) => s.dirty);

  const tabClass = (on: boolean) =>
    `flex shrink-0 items-center gap-1.5 rounded-md border px-2 py-1 text-[11px] ${
      on
        ? "border-teal-600/70 bg-ink-700 text-zinc-100"
        : "border-ink-600 bg-ink-800 text-zinc-400 hover:text-zinc-200"
    }`;

  return (
    <div className="flex shrink-0 items-center gap-1 overflow-x-auto pb-0.5">
      <button
        onClick={() => setLayoutMode("grid")}
        title="Show all windows (⌘⇧⏎)"
        className={tabClass(false)}
      >
        <LayoutGrid size={12} className="shrink-0" />
        <span className="font-medium">Grid</span>
      </button>
      <div className="mx-0.5 my-1 w-px shrink-0 self-stretch bg-ink-600" />
      {frames.map((f, i) => {
        const raw = f.pending
          ? "PENDING"
          : f.terminalId
            ? terminalStatuses[f.terminalId]
            : undefined;
        const d = f.terminalId ? dirty[f.terminalId] : undefined;
        return (
          <button
            key={f.key}
            onClick={() => setActiveFrameGuarded(f.key)}
            title={`${providerTitle(f.provider)}${f.model ? " · " + f.model : ""}${
              f.terminalId ? " · " + f.terminalId : ""
            }`}
            className={tabClass(f.key === activeKey)}
          >
            <span className="shrink-0 text-[10px] tabular-nums text-zinc-600">
              {i < 9 ? i + 1 : ""}
            </span>
            <span
              className={`h-1.5 w-1.5 shrink-0 rounded-full ${statusDotClass(raw)}`}
            />
            <span className="max-w-[130px] truncate font-medium">
              {providerTitle(f.provider)}
            </span>
            {d && d.count > 0 && (
              <span className="shrink-0 rounded bg-amber/20 px-1 text-[9px] font-medium tabular-nums text-amber">
                {d.count}
              </span>
            )}
          </button>
        );
      })}
    </div>
  );
}
