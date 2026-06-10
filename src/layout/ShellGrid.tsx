import { useStore, isDaemonTransport, type Frame } from "../store";
import { TerminalViewRustPty } from "../components/TerminalViewRustPty";
import { StatusBadge } from "../components/StatusBadge";
import { statusDotClass } from "../lib/agentStatus";
import { providerTitle } from "../lib/providerLabel";
import { profileMeta, displayRole } from "../lib/profiles";
import { Loader2, X, TerminalSquare, Power, LayoutGrid } from "lucide-react";

/** CSS grid template that keeps frames roughly square as count grows. */
function gridClass(n: number): string {
  if (n <= 1) return "grid-cols-1 grid-rows-1";
  if (n === 2) return "grid-cols-2 grid-rows-1";
  if (n <= 4) return "grid-cols-2 grid-rows-2";
  if (n <= 6) return "grid-cols-3 grid-rows-2";
  return "grid-cols-3 grid-rows-3";
}

export function ShellGrid() {
  const frames = useStore((s) => s.frames);
  const layoutMode = useStore((s) => s.layoutMode);
  const activeFrameKey = useStore((s) => s.activeFrameKey);

  if (frames.length === 0) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-3 text-center">
        <TerminalSquare size={32} className="text-ink-400" />
        <div>
          <p className="text-sm text-zinc-400">No agents running</p>
          <p className="mt-1 text-xs text-zinc-600">
            Launch an agent from the left to open a live terminal.
          </p>
        </div>
      </div>
    );
  }

  // Tabs ARE the windows. The "Grid" tab tiles every window; a window tab
  // fullscreens that one frame. In focus, only the active frame is mounted
  // (switching reconnects/replays). Switching routes through the single
  // setActiveFrameGuarded entry point (instant — no gate).
  const active = frames.find((f) => f.key === activeFrameKey) ?? frames[0];

  return (
    <div className="flex h-full w-full flex-col gap-2">
      <ShellTabs frames={frames} layoutMode={layoutMode} activeKey={active.key} />
      <div className="min-h-0 flex-1">
        {layoutMode === "focus" ? (
          <FrameCell
            key={active.key}
            frame={active}
            index={frames.findIndex((f) => f.key === active.key)}
          />
        ) : (
          <div className={`grid h-full w-full gap-2 ${gridClass(frames.length)}`}>
            {frames.map((f, i) => (
              <FrameCell key={f.key} frame={f} index={i} />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

/** The shell tab bar — a "Grid" tab (all windows tiled) + one tab per window.
 *  A view over the frames, never a source of truth. Window-tab clicks route
 *  through setActiveFrameGuarded and fullscreen that frame. */
function ShellTabs({
  frames,
  layoutMode,
  activeKey,
}: {
  frames: Frame[];
  layoutMode: "grid" | "focus";
  activeKey: string;
}) {
  const setLayoutMode = useStore((s) => s.setLayoutMode);
  const setActiveFrameGuarded = useStore((s) => s.setActiveFrameGuarded);
  const terminalStatuses = useStore((s) => s.terminalStatuses);
  const dirty = useStore((s) => s.dirty);
  const rustPtySessions = useStore((s) => s.rustPtySessions);

  const tabClass = (on: boolean) =>
    `flex shrink-0 items-center gap-1.5 rounded-md border px-2 py-1 text-left ${
      on
        ? "border-teal-600/70 bg-ink-700 text-zinc-100"
        : "border-ink-600 bg-ink-800 text-zinc-400 hover:text-zinc-200"
    }`;

  return (
    <div className="flex shrink-0 items-stretch gap-1 overflow-x-auto pb-0.5">
      <button
        onClick={() => setLayoutMode("grid")}
        title="Show all windows (⌘⇧⏎)"
        className={tabClass(layoutMode === "grid")}
      >
        <LayoutGrid size={12} className="shrink-0" />
        <span className="flex flex-col leading-tight">
          <span className="text-[11px] font-medium">Grid</span>
          <span className="text-[9px] text-zinc-500">all windows</span>
        </span>
      </button>
      <div className="mx-0.5 my-1 w-px shrink-0 bg-ink-600" />
      {frames.map((f, i) => {
        const raw = f.pending
          ? "PENDING"
          : f.terminalId
            ? terminalStatuses[f.terminalId]
            : undefined;
        const d = f.terminalId ? dirty[f.terminalId] : undefined;
        // Lead with the ROLE (orchestrator / product-builder / researcher / …) —
        // resolved from the launch profile or the daemon's role for an adopted
        // worker — with provider + model as the secondary line.
        const role = displayRole(
          f.agentProfile ?? (f.ptySessionId ? rustPtySessions[f.ptySessionId]?.role : null),
        );
        const rmeta = role ? profileMeta(role) : null;
        const RoleIcon = rmeta?.icon;
        const sub = [rmeta ? providerTitle(f.provider) : null, f.model]
          .filter(Boolean)
          .join(" · ");
        return (
          <button
            key={f.key}
            onClick={() => {
              setActiveFrameGuarded(f.key);
              setLayoutMode("focus");
            }}
            title={`${rmeta ? rmeta.label + " · " : ""}${providerTitle(f.provider)}${
              f.model ? " · " + f.model : ""
            } — fullscreen`}
            className={tabClass(layoutMode === "focus" && f.key === activeKey)}
          >
            <span className="shrink-0 text-[10px] tabular-nums text-zinc-600">
              {i < 9 ? i + 1 : ""}
            </span>
            <span className="flex min-w-0 flex-col leading-tight">
              <span className="flex items-center gap-1.5">
                <span
                  className={`h-1.5 w-1.5 shrink-0 rounded-full ${statusDotClass(raw)}`}
                />
                {RoleIcon && <RoleIcon size={11} className="shrink-0 text-zinc-400" />}
                <span className="max-w-[130px] truncate text-[11px] font-medium">
                  {rmeta ? rmeta.label : providerTitle(f.provider)}
                </span>
              </span>
              {sub && (
                <span className="max-w-[150px] truncate text-[10px] text-zinc-500">
                  {sub}
                </span>
              )}
            </span>
            {d && d.count > 0 && (
              <span className="shrink-0 rounded bg-amber/20 px-1 text-[9px] font-medium text-amber">
                {d.count}
              </span>
            )}
          </button>
        );
      })}
    </div>
  );
}

function FrameCell({ frame, index }: { frame: Frame; index: number }) {
  const activeFrameKey = useStore((s) => s.activeFrameKey);
  const setActiveFrameGuarded = useStore((s) => s.setActiveFrameGuarded);
  const closeFrame = useStore((s) => s.closeFrame);
  const killRustPty = useStore((s) => s.killRustPty);
  const layoutMode = useStore((s) => s.layoutMode);
  const setLayoutMode = useStore((s) => s.setLayoutMode);
  const status = useStore((s) =>
    frame.terminalId ? s.terminalStatuses[frame.terminalId] : undefined,
  );
  const dirty = useStore((s) =>
    frame.terminalId ? s.dirty[frame.terminalId] : undefined,
  );
  const meta = useStore((s) =>
    frame.ptySessionId ? s.rustPtySessions[frame.ptySessionId] : undefined,
  );

  const isRustPty = isDaemonTransport(frame.transport) && !!frame.ptySessionId;

  // (Filesystem watching now lives in the session daemon — it streams dirty
  // paths over the attach channel as `FsDirty`, so React no longer mounts a
  // watcher per frame.)

  const active = activeFrameKey === frame.key;
  const title = providerTitle(frame.provider);
  // Lead with the ROLE (resolved from the launch profile or the daemon's role
  // for an adopted/assigned worker); provider + model are secondary.
  const role = displayRole(frame.agentProfile ?? meta?.role);
  const rmeta = role ? profileMeta(role) : null;
  const RoleIcon = rmeta?.icon;
  // Key the frame for drag-and-drop hit-testing (file/screenshot drop → path).
  const termKey = isRustPty ? frame.ptySessionId : frame.terminalId;

  return (
    <div
      data-term-key={termKey ?? undefined}
      onMouseDown={() => setActiveFrameGuarded(frame.key)}
      className={`flex h-full min-h-0 min-w-0 flex-col overflow-hidden rounded-lg border ${
        active ? "border-teal-600/70" : "border-ink-600"
      } bg-ink-900`}
    >
      <header className="flex h-8 shrink-0 items-center justify-between border-b border-ink-600 bg-ink-800 px-3">
        <div
          className="flex min-w-0 items-center gap-2"
          title={layoutMode === "focus" ? "Double-click for grid" : "Double-click to fullscreen"}
          onDoubleClick={() => {
            if (layoutMode === "focus") {
              setLayoutMode("grid");
            } else {
              setActiveFrameGuarded(frame.key);
              setLayoutMode("focus");
            }
          }}
        >
          {index < 9 && (
            <span className="shrink-0 text-[10px] tabular-nums text-zinc-600">
              {index + 1}
            </span>
          )}
          {RoleIcon && <RoleIcon size={13} className="shrink-0 text-accent" />}
          <span className="shrink-0 text-xs font-medium text-zinc-100">
            {rmeta ? rmeta.label : title}
          </span>
          {rmeta && (
            <span className="shrink-0 text-[11px] text-zinc-500" title="Provider">
              {title}
            </span>
          )}
          {frame.model && (
            <span
              className="shrink-0 rounded bg-ink-600 px-1.5 text-[10px] font-medium text-zinc-300"
              title="Model (from the agent's startup banner)"
            >
              {frame.model}
            </span>
          )}
        </div>
        <div className="flex items-center gap-2.5">
          {dirty && dirty.count > 0 && (
            <span
              className="rounded bg-amber/20 px-1.5 py-0.5 text-[10px] font-medium text-amber"
              title={`${dirty.count} uncommitted change(s)`}
            >
              {dirty.count} dirty
            </span>
          )}
          <StatusBadge status={frame.pending ? "PENDING" : status} />
          {isRustPty && (
            <button
              onMouseDown={(e) => e.stopPropagation()}
              onClick={() => killRustPty(frame.key)}
              className="rounded p-0.5 text-rose-400/80 hover:text-rose-300"
              title="Kill agent (terminate process)"
            >
              <Power size={13} />
            </button>
          )}
          <button
            onMouseDown={(e) => e.stopPropagation()}
            onClick={() => closeFrame(frame.key)}
            className="rounded p-0.5 text-zinc-500 hover:text-zinc-200"
            title={
              isRustPty
                ? "Close view (agent keeps running)"
                : "Close frame (agent keeps running)"
            }
          >
            <X size={14} />
          </button>
        </div>
      </header>

      <div className="min-h-0 flex-1">
        {isRustPty && frame.ptySessionId ? (
          <TerminalViewRustPty sessionId={frame.ptySessionId} frameKey={frame.key} />
        ) : (
          <div className="flex h-full flex-col items-center justify-center gap-2 text-zinc-500">
            <Loader2 size={20} className="animate-spin text-teal-400" />
            <span className="text-xs">Starting {title}…</span>
            <span className="text-[10px] text-zinc-600">
              cold-starting the CLI
            </span>
          </div>
        )}
      </div>
    </div>
  );
}
