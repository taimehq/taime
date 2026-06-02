/** Normalized terminal status → dot color + label. */
const CONFIG: Record<string, { dot: string; label: string }> = {
  IDLE: { dot: "bg-zinc-500", label: "idle" },
  PROCESSING: { dot: "bg-teal-400 animate-pulse", label: "working" },
  COMPLETED: { dot: "bg-emerald-400", label: "done" },
  WAITING_USER_ANSWER: { dot: "bg-amber animate-pulse", label: "needs you" },
  ERROR: { dot: "bg-red-500", label: "error" },
  PENDING: { dot: "bg-amber animate-pulse", label: "launching" },
  UNKNOWN: { dot: "bg-zinc-600", label: "—" },
};

/** The normalized, human label for a raw terminal status (e.g. "working"). */
export function statusLabel(status: string | undefined): string {
  return (CONFIG[status ?? "UNKNOWN"] ?? CONFIG.UNKNOWN).label;
}

/** Tailwind classes for the status dot (color + any pulse). */
export function statusDotClass(status: string | undefined): string {
  return (CONFIG[status ?? "UNKNOWN"] ?? CONFIG.UNKNOWN).dot;
}

export function StatusBadge({ status }: { status: string | undefined }) {
  const cfg = CONFIG[status ?? "UNKNOWN"] ?? CONFIG.UNKNOWN;
  return (
    <span className="inline-flex items-center gap-1.5 text-[11px] text-zinc-400">
      <span className={`h-2 w-2 rounded-full ${cfg.dot}`} />
      {cfg.label}
    </span>
  );
}
