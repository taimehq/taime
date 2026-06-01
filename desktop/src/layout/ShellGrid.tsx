import { useStore, type Frame } from "../store";
import { TerminalView } from "../components/TerminalView";
import { StatusBadge } from "../components/StatusBadge";
import { useFsWatch } from "../hooks/useFsWatch";
import { Loader2, X, TerminalSquare } from "lucide-react";

const TARGET_NAME: Record<string, string> = {
  claude_code: "Claude Code",
  codex: "Codex CLI",
  gemini_cli: "Gemini CLI",
  grok_cli: "Grok Build CLI",
};

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

  return (
    <div className={`grid h-full w-full gap-2 ${gridClass(frames.length)}`}>
      {frames.map((f) => (
        <FrameCell key={f.key} frame={f} />
      ))}
    </div>
  );
}

function FrameCell({ frame }: { frame: Frame }) {
  const activeFrameKey = useStore((s) => s.activeFrameKey);
  const setActiveFrameGuarded = useStore((s) => s.setActiveFrameGuarded);
  const closeFrame = useStore((s) => s.closeFrame);
  const status = useStore((s) =>
    frame.terminalId ? s.terminalStatuses[frame.terminalId] : undefined,
  );
  const dirty = useStore((s) =>
    frame.terminalId ? s.dirty[frame.terminalId] : undefined,
  );

  // Watch this terminal's working dir while the frame is mounted.
  useFsWatch(frame.terminalId);

  const active = activeFrameKey === frame.key;
  const title =
    TARGET_NAME[frame.provider] ?? frame.provider.replace(/_/g, " ");

  return (
    <div
      onMouseDown={() => setActiveFrameGuarded(frame.key)}
      className={`flex min-h-0 min-w-0 flex-col overflow-hidden rounded-lg border ${
        active ? "border-teal-600/70" : "border-ink-600"
      } bg-ink-900`}
    >
      <header className="flex h-8 shrink-0 items-center justify-between border-b border-ink-600 bg-ink-800 px-3">
        <div className="flex min-w-0 items-center gap-2">
          <span className="truncate text-xs font-medium text-zinc-200">
            {title}
          </span>
          {frame.terminalId && (
            <span className="truncate font-mono text-[10px] text-zinc-600">
              {frame.terminalId.slice(0, 8)}
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
          <button
            onMouseDown={(e) => e.stopPropagation()}
            onClick={() => closeFrame(frame.key)}
            className="rounded p-0.5 text-zinc-500 hover:text-zinc-200"
            title="Close frame (agent keeps running)"
          >
            <X size={14} />
          </button>
        </div>
      </header>

      <div className="min-h-0 flex-1">
        {frame.pending || !frame.terminalId ? (
          <div className="flex h-full flex-col items-center justify-center gap-2 text-zinc-500">
            <Loader2 size={20} className="animate-spin text-teal-400" />
            <span className="text-xs">Starting {title}…</span>
            <span className="text-[10px] text-zinc-600">
              cold-starting the CLI
            </span>
          </div>
        ) : (
          <TerminalView terminalId={frame.terminalId} />
        )}
      </div>
    </div>
  );
}
