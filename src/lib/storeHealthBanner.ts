import type { StoreHealth } from "../pty";

/** What the status pill should say about degraded persistence, or null when
 *  the store is healthy (or unknown). Pure — extracted from BackendStatusPill
 *  so the copy/branching is unit-testable without component-test infra. */
export function storeHealthBanner(
  health: StoreHealth | null,
): { off: boolean; label: string; title: string } | null {
  if (!health || health.status === "ok") return null;
  if (health.status === "unavailable") {
    return {
      off: true,
      label: "Attribution not recording",
      title: `Persistence is OFF for this backend run — agents work, but no turns, file events, or reviews are being recorded. Store error: ${health.detail ?? "unknown"}`,
    };
  }
  return {
    off: false,
    label: "Store recovered — history archived",
    title: `The attribution database was corrupt and has been moved aside to ${health.detail ?? "a .corrupt file"} in the app data dir. Recording continues on a fresh database; prior history lives in the moved-aside file.`,
  };
}
