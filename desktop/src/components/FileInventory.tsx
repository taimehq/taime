import { useStore } from "../store";
import { FileWarning, Eye } from "lucide-react";

const TARGET_NAME: Record<string, string> = {
  claude_code: "Claude Code",
  codex: "Codex CLI",
  gemini_cli: "Gemini CLI",
  grok_cli: "Grok Build CLI",
};

/**
 * Surfaces uncommitted, agent-driven changes per terminal. Populated by the
 * Rust file watcher (step 4) via `setDirty`. Until a watcher is active this
 * stays empty.
 */
export function FileInventory() {
  const dirty = useStore((s) => s.dirty);
  const frames = useStore((s) => s.frames);
  const openDiff = useStore((s) => s.openDiff);
  const entries = Object.entries(dirty).filter(([, d]) => d.count > 0);

  return (
    <div className="flex flex-col gap-2">
      <h2 className="text-[11px] font-semibold uppercase tracking-wider text-zinc-500">
        File inventory
      </h2>
      {entries.length === 0 ? (
        <p className="text-[11px] text-zinc-600">
          No uncommitted changes detected.
        </p>
      ) : (
        <div className="flex flex-col gap-1.5">
          {entries.map(([terminalId, d]) => {
            const frame = frames.find((f) => f.terminalId === terminalId);
            const who = frame
              ? (TARGET_NAME[frame.provider] ?? frame.provider)
              : terminalId.slice(0, 8);
            return (
              <div
                key={terminalId}
                className="rounded-lg border border-amber/30 bg-amber/5 px-2.5 py-2"
              >
                <div className="flex items-center gap-2">
                  <FileWarning size={13} className="shrink-0 text-amber" />
                  <span className="truncate text-xs text-zinc-200">{who}</span>
                  <span className="ml-auto text-[11px] font-medium text-amber">
                    {d.count}
                  </span>
                  <button
                    onClick={() => openDiff(terminalId)}
                    className="flex items-center gap-1 rounded border border-ink-500 px-1.5 py-0.5 text-[10px] text-zinc-300 hover:bg-ink-600"
                    title="Review & selectively merge/revert"
                  >
                    <Eye size={11} />
                    Review
                  </button>
                </div>
                {d.paths.length > 0 && (
                  <ul className="mt-1.5 flex flex-col gap-0.5">
                    {d.paths.slice(0, 5).map((p) => (
                      <li
                        key={p}
                        className="truncate font-mono text-[10px] text-zinc-500"
                        title={p}
                      >
                        {p}
                      </li>
                    ))}
                    {d.paths.length > 5 && (
                      <li className="text-[10px] text-zinc-600">
                        +{d.paths.length - 5} more
                      </li>
                    )}
                  </ul>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
