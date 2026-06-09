import { useStore } from "../store";
import { FileWarning, Eye } from "lucide-react";
import { providerTitle } from "../lib/providerLabel";

/**
 * Surfaces uncommitted, agent-driven changes per terminal. Populated by the
 * Rust file watcher (step 4) via `setDirty`. Until a watcher is active this
 * stays empty.
 */
export function FileInventory() {
  const dirty = useStore((s) => s.dirty);
  const frames = useStore((s) => s.frames);
  const openDiff = useStore((s) => s.openDiff);

  const raw = Object.entries(dirty).filter(([, d]) => d.count > 0);

  // Team agents that share ONE worktree (e.g. a supervisor + its delegated
  // sub-agent) each report the same dirty files under their own terminal id —
  // which showed up as duplicate cards. Collapse entries with an identical
  // path-set into a single card, keyed by that set.
  const groups = new Map<
    string,
    { terminalIds: string[]; count: number; paths: string[] }
  >();
  for (const [terminalId, d] of raw) {
    const sig = `${d.count}::${[...d.paths].sort().join("\n")}`;
    const g = groups.get(sig);
    if (g) g.terminalIds.push(terminalId);
    else groups.set(sig, { terminalIds: [terminalId], count: d.count, paths: d.paths });
  }
  const cards = [...groups.values()];

  return (
    <div className="flex flex-col gap-2">
      <h2 className="text-[11px] font-semibold uppercase tracking-wider text-zinc-500">
        File inventory
      </h2>
      {cards.length === 0 ? (
        <p className="text-[11px] text-zinc-600">
          No uncommitted changes detected.
        </p>
      ) : (
        <div className="flex flex-col gap-1.5">
          {cards.map(({ terminalIds, count, paths }) => {
            const primary = terminalIds[0];
            const frame = frames.find(
              (f) => f.terminalId && terminalIds.includes(f.terminalId),
            );
            const provider = frame
              ? providerTitle(frame.provider)
              : primary.slice(0, 8);
            const who =
              terminalIds.length > 1
                ? `${provider} · ${terminalIds.length} agents`
                : provider;
            return (
              <div
                key={primary}
                className="rounded-lg border border-amber/30 bg-amber/5 px-2.5 py-2"
              >
                <div className="flex items-center gap-2">
                  <FileWarning size={13} className="shrink-0 text-amber" />
                  <span className="truncate text-xs text-zinc-200">{who}</span>
                  <span className="ml-auto text-[11px] font-medium text-amber">
                    {count}
                  </span>
                  <button
                    onClick={() => openDiff(primary)}
                    className="flex items-center gap-1 rounded border border-ink-500 px-1.5 py-0.5 text-[10px] text-zinc-300 hover:bg-ink-600"
                    title="Review & selectively merge/revert"
                  >
                    <Eye size={11} />
                    Review
                  </button>
                </div>
                {paths.length > 0 && (
                  <ul className="mt-1.5 flex flex-col gap-0.5">
                    {paths.slice(0, 5).map((p) => (
                      <li
                        key={p}
                        className="truncate font-mono text-[10px] text-zinc-500"
                        title={p}
                      >
                        {p}
                      </li>
                    ))}
                    {paths.length > 5 && (
                      <li className="text-[10px] text-zinc-600">
                        +{paths.length - 5} more
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
