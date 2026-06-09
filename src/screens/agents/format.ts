/** Formatting helpers for the Agents screens (shared by header/console/feed). */

// The truncation contract lives in the shared lib — one implementation.
export { middleTruncate } from "../../lib/format";

/** HH:MM:SS (24h) for an ISO timestamp; "—" when absent/unparseable. */
export function fmtClock(iso: string | null | undefined): string {
  if (!iso) return "—";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "—";
  return d.toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  });
}

/** HH:MM:SS (24h) for an epoch-ms timestamp. */
export function fmtClockMs(at: number): string {
  return new Date(at).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  });
}
