import { statusDotClass, statusLabel } from "../lib/agentStatus";

/** Dot + label for a raw wire status. The status vocabulary and its styling
 *  live in lib/agentStatus — the single status module. */
export function StatusBadge({ status }: { status: string | undefined }) {
  return (
    <span className="inline-flex items-center gap-1.5 text-[11px] text-zinc-400">
      <span className={`h-2 w-2 rounded-full ${statusDotClass(status)}`} />
      {statusLabel(status)}
    </span>
  );
}
