import { useStore } from "../store";
import { AlertTriangle, Eye, ArrowRight } from "lucide-react";
import { providerTitle } from "../lib/providerLabel";

/**
 * Raised when switching execution context away from an agent that left
 * uncommitted changes. Makes the cost of context-switching visible and safe:
 * the user can review the diff first or knowingly proceed.
 */
export function ContextSwitchGuard({
  onReview,
}: {
  onReview: (terminalId: string) => void;
}) {
  const pendingSwitchKey = useStore((s) => s.pendingSwitchKey);
  const frames = useStore((s) => s.frames);
  const activeFrameKey = useStore((s) => s.activeFrameKey);
  const dirty = useStore((s) => s.dirty);
  const resolveSwitch = useStore((s) => s.resolveSwitch);

  if (!pendingSwitchKey) return null;

  const current = frames.find((f) => f.key === activeFrameKey);
  const next = frames.find((f) => f.key === pendingSwitchKey);
  const d = current?.terminalId ? dirty[current.terminalId] : undefined;
  if (!current || !d) return null;

  const fromName = providerTitle(current.provider);
  const toName = next ? providerTitle(next.provider) : "another agent";

  // Contended files: paths this agent changed that ANOTHER live agent also has
  // dirty — a real cross-agent collision signal, surfaced before the switch.
  const otherDirtyPaths = new Set<string>();
  for (const [tid, ds] of Object.entries(dirty)) {
    if (current.terminalId && tid === current.terminalId) continue;
    for (const p of ds.paths) otherDirtyPaths.add(p);
  }
  const contended = new Set(d.paths.filter((p) => otherDirtyPaths.has(p)));

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
      <div className="no-drag w-[30rem] rounded-xl border border-amber/40 bg-ink-800 p-5 shadow-2xl">
        <div className="mb-3 flex items-center gap-2.5">
          <AlertTriangle size={18} className="text-amber" />
          <h2 className="text-sm font-semibold text-zinc-100">
            Uncommitted changes from {fromName}
          </h2>
        </div>
        <p className="mb-3 text-sm leading-relaxed text-zinc-400">
          <span className="font-medium text-amber">{d.count}</span> file
          {d.count === 1 ? "" : "s"} changed by {fromName} haven&apos;t been
          reviewed. Switching to {toName} before review risks one agent building
          on another&apos;s unverified work.
        </p>
        {contended.size > 0 && (
          <p className="mb-2 rounded-md border border-rose-500/40 bg-rose-500/10 px-2.5 py-1.5 text-[11px] text-rose-300">
            ⚠ {contended.size} file{contended.size === 1 ? "" : "s"} also changed
            by another agent — merging risks a collision.
          </p>
        )}
        {d.paths.length > 0 && (
          <ul className="mb-4 max-h-28 overflow-auto rounded-lg border border-ink-600 bg-ink-900 p-2">
            {d.paths.slice(0, 8).map((p) => (
              <li
                key={p}
                className="flex items-center justify-between gap-2 font-mono text-[11px]"
              >
                <span
                  className={`truncate ${contended.has(p) ? "text-rose-300" : "text-zinc-500"}`}
                >
                  {p}
                </span>
                {contended.has(p) && (
                  <span className="shrink-0 text-[10px] font-semibold text-rose-400">
                    contended
                  </span>
                )}
              </li>
            ))}
            {d.paths.length > 8 && (
              <li className="text-[11px] text-zinc-600">
                +{d.paths.length - 8} more
              </li>
            )}
          </ul>
        )}
        <div className="flex items-center justify-end gap-2">
          <button
            onClick={() => resolveSwitch(false)}
            className="rounded-lg px-3 py-1.5 text-sm text-zinc-400 hover:text-zinc-200"
          >
            Stay
          </button>
          <button
            onClick={() => {
              if (current.terminalId) onReview(current.terminalId);
            }}
            className="flex items-center gap-1.5 rounded-lg border border-ink-500 px-3 py-1.5 text-sm text-zinc-200 hover:bg-ink-600"
          >
            <Eye size={14} />
            Review diff
          </button>
          <button
            onClick={() => resolveSwitch(true)}
            className="flex items-center gap-1.5 rounded-lg bg-amber px-3 py-1.5 text-sm font-medium text-ink-900 hover:brightness-110"
          >
            Proceed without review
            <ArrowRight size={14} />
          </button>
        </div>
      </div>
    </div>
  );
}
