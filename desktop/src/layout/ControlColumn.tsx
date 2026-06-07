import {
  Plus,
  GitBranch,
  Power,
  PanelLeftOpen,
} from "lucide-react";
import { useStore } from "../store";
import { StatusBadge, statusDotClass } from "../components/StatusBadge";
import { FileInventory } from "../components/FileInventory";
import { SchedulesPanel } from "../components/SchedulesPanel";
import { WorkflowsPanel } from "../components/WorkflowsPanel";
import { WorkspacePicker } from "../components/WorkspacePicker";
import { providerTitle } from "../lib/providerLabel";

export function ControlColumn({ onLaunch }: { onLaunch: () => void }) {
  const connected = useStore((s) => s.connected);
  const width = useStore((s) => s.sidebarWidth);
  const collapsed = useStore((s) => s.sidebarCollapsed);
  const setSidebarWidth = useStore((s) => s.setSidebarWidth);
  const toggleSidebar = useStore((s) => s.toggleSidebar);
  const frames = useStore((s) => s.frames);
  const terminalStatuses = useStore((s) => s.terminalStatuses);
  const setActiveFrameGuarded = useStore((s) => s.setActiveFrameGuarded);

  // Collapsed: a thin rail that still surfaces live agent activity — expand,
  // launch, and a status dot per window (click to jump, guarded). Cmd+\ toggles.
  if (collapsed) {
    return (
      <aside className="flex w-10 shrink-0 flex-col items-center gap-2 border-r border-ink-600 bg-ink-800/40 py-2.5">
        <button
          onClick={toggleSidebar}
          title="Show sidebar (⌘\)"
          className="no-drag rounded p-1.5 text-zinc-500 hover:bg-ink-700 hover:text-zinc-200"
        >
          <PanelLeftOpen size={16} />
        </button>
        <button
          onClick={onLaunch}
          disabled={!connected}
          title="Launch agent"
          className="no-drag rounded p-1.5 text-zinc-400 hover:bg-ink-700 hover:text-zinc-200 disabled:opacity-40"
        >
          <Plus size={16} />
        </button>
        <div className="mt-1 flex min-h-0 flex-col items-center gap-2.5 overflow-y-auto">
          {frames.map((f) => {
            const raw = f.pending
              ? "PENDING"
              : f.terminalId
                ? terminalStatuses[f.terminalId]
                : undefined;
            return (
              <button
                key={f.key}
                onClick={() => setActiveFrameGuarded(f.key)}
                title={providerTitle(f.provider)}
                className="no-drag rounded-full p-0.5 hover:bg-ink-700"
              >
                <span
                  className={`block h-2.5 w-2.5 rounded-full ${statusDotClass(raw)}`}
                />
              </button>
            );
          })}
        </div>
      </aside>
    );
  }

  return (
    <aside
      style={{ width }}
      className="relative flex shrink-0 flex-col border-r border-ink-600 bg-ink-800/40"
    >
      {/* Zone 1 — context + primary actions (pinned). Collapse toggle lives in
          the title bar so it costs no sidebar height. */}
      <div className="flex shrink-0 flex-col gap-3 p-4 pb-3">
        <WorkspacePicker />
        <div className="flex flex-col gap-2">
          <button
            onClick={onLaunch}
            disabled={!connected}
            className="no-drag flex items-center justify-center gap-2 rounded-lg bg-primary px-3 py-2 text-sm font-medium text-ink-900 transition-colors hover:bg-primary-hover disabled:cursor-not-allowed disabled:opacity-50"
          >
            <Plus size={16} />
            Launch agent
          </button>
        </div>
      </div>

      {/* Zone 2 — agents + schedules (the only scrolling zone) */}
      <div className="min-h-0 flex-1 space-y-5 overflow-y-auto border-t border-ink-700 px-3 py-3">
        <AgentsSection />
        <WorkflowsPanel />
        <SchedulesPanel />
      </div>

      {/* Zone 3 — changes / review (pinned bottom, capped) */}
      <div className="max-h-[38%] shrink-0 overflow-y-auto border-t border-ink-700 p-3">
        <FileInventory />
      </div>

      <ResizeHandle onResize={setSidebarWidth} />
    </aside>
  );
}

/** Drag handle on the sidebar's right edge. Width is clamped in the store. */
function ResizeHandle({ onResize }: { onResize: (px: number) => void }) {
  const onMouseDown = (e: React.MouseEvent) => {
    e.preventDefault();
    const aside = (e.currentTarget as HTMLElement).parentElement;
    const left = aside?.getBoundingClientRect().left ?? 0;
    const onMove = (ev: MouseEvent) => onResize(ev.clientX - left);
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  return (
    <div
      onMouseDown={onMouseDown}
      title="Drag to resize"
      className="absolute right-0 top-0 z-10 h-full w-1 cursor-col-resize hover:bg-teal-600/40"
    />
  );
}

/**
 * The running/idle agents — every daemon session (close ≠ kill, so a closed frame
 * keeps its agent here). Click an open agent to focus it, a detached one to
 * reattach; an exited one can be dismissed. Replaces the old tmux session list.
 */
function AgentsSection() {
  const rustPtySessions = useStore((s) => s.rustPtySessions);
  const frames = useStore((s) => s.frames);
  const statuses = useStore((s) => s.terminalStatuses);
  const setActiveFrameGuarded = useStore((s) => s.setActiveFrameGuarded);
  const reopenRustPty = useStore((s) => s.reopenRustPty);
  const forgetRustPty = useStore((s) => s.forgetRustPty);

  // Running first (most recent first), then exited.
  const agents = Object.values(rustPtySessions).sort((a, b) => {
    if (a.status !== b.status) return a.status === "running" ? -1 : 1;
    return b.startedAt - a.startedAt;
  });

  return (
    <Section title={`Agents · ${agents.length}`}>
      {agents.length === 0 ? (
        <p className="text-[11px] text-zinc-600">
          No agents yet. Launch one to begin.
        </p>
      ) : (
        <div className="flex flex-col gap-1">
          {agents.map((m) => {
            const frame = frames.find((f) => f.ptySessionId === m.ptySessionId);
            const exited = m.status === "exited";
            const status = m.terminalId ? statuses[m.terminalId] : undefined;
            const stateTag = frame ? "open" : exited ? "exited" : "detached";
            return (
              <div
                key={m.ptySessionId}
                className={`flex items-center gap-2 rounded-lg border px-2.5 py-1.5 ${
                  exited ? "border-ink-600 bg-ink-800/40" : "border-teal-600/30 bg-teal-600/5"
                }`}
              >
                <button
                  onClick={() =>
                    frame ? setActiveFrameGuarded(frame.key) : reopenRustPty(m.ptySessionId)
                  }
                  disabled={exited && !frame}
                  title={frame ? "Focus" : exited ? "Process exited" : "Reopen (reattach)"}
                  className="flex min-w-0 flex-1 flex-col text-left disabled:cursor-default"
                >
                  <span className="flex items-center gap-1.5 truncate text-xs text-zinc-200">
                    {providerTitle(m.provider)}
                    <span
                      className={`rounded px-1 text-[10px] font-semibold uppercase tracking-wide ${
                        exited
                          ? "bg-ink-600 text-zinc-400"
                          : frame
                            ? "bg-ink-600 text-zinc-400"
                            : "bg-teal-600/20 text-teal-400"
                      }`}
                    >
                      {stateTag}
                    </span>
                  </span>
                  {m.branch && (
                    <span className="flex items-center gap-1 truncate font-mono text-[10px] text-zinc-500">
                      <GitBranch size={10} /> {m.branch}
                    </span>
                  )}
                </button>
                {!exited && <StatusBadge status={status} />}
                {exited ? (
                  <button
                    onClick={() => forgetRustPty(m.ptySessionId)}
                    className="shrink-0 rounded border border-ink-500 px-2 py-0.5 text-[11px] text-zinc-400 hover:bg-ink-600"
                  >
                    Dismiss
                  </button>
                ) : (
                  <button
                    onClick={() => forgetRustPty(m.ptySessionId)}
                    aria-label="Kill agent"
                    title="Kill agent (terminate process)"
                    className="shrink-0 rounded p-0.5 text-red-400/80 hover:text-red-300"
                  >
                    <Power size={13} />
                  </button>
                )}
              </div>
            );
          })}
        </div>
      )}
    </Section>
  );
}

function Section({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex flex-col gap-2">
      <h2 className="text-[11px] font-semibold uppercase tracking-wider text-zinc-500">
        {title}
      </h2>
      {children}
    </div>
  );
}
