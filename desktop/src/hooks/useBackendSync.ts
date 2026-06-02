import { useEffect } from "react";
import { useStore } from "../store";

/**
 * Periodic backend polling. Connectivity (`store.connected`) is driven by actual
 * REST reachability — `fetchSessions` flips it on success/failure — so the UI
 * reflects whether the API truly responds, independent of the Rust supervisor's
 * self-reported status (which is unavailable when running outside the webview).
 *
 *  - sessions list (10s)  → pipeline overview + connectivity probe
 *  - session status rollups (10s) → collapsed-row at-a-glance status
 *  - terminal statuses (3s) for frames currently open (no-ops when none)
 */
export function useBackendSync() {
  const fetchSessions = useStore((s) => s.fetchSessions);
  const refreshStatuses = useStore((s) => s.refreshStatuses);
  const refreshSessionRollups = useStore((s) => s.refreshSessionRollups);

  useEffect(() => {
    let alive = true;

    // Rollups read the session list, so refresh sessions first each tick.
    const syncSessions = async () => {
      if (!alive) return;
      await fetchSessions();
      if (alive) refreshSessionRollups();
    };

    syncSessions();
    refreshStatuses();

    const sessTimer = setInterval(syncSessions, 10000);
    const statusTimer = setInterval(() => {
      if (alive) refreshStatuses();
    }, 3000);

    return () => {
      alive = false;
      clearInterval(sessTimer);
      clearInterval(statusTimer);
    };
  }, [fetchSessions, refreshStatuses, refreshSessionRollups]);
}
