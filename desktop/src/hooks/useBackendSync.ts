import { useEffect } from "react";
import { useStore } from "../store";

/**
 * Periodic backend polling. Connectivity (`store.connected`) is driven by actual
 * REST reachability — `fetchSessions` flips it on success/failure — so the UI
 * reflects whether the API truly responds, independent of the Rust supervisor's
 * self-reported status (which is unavailable when running outside the webview).
 *
 *  - sessions list (10s)  → pipeline overview + connectivity probe
 *  - terminal statuses (3s) for frames currently open (no-ops when none)
 */
export function useBackendSync() {
  const fetchSessions = useStore((s) => s.fetchSessions);
  const refreshStatuses = useStore((s) => s.refreshStatuses);

  useEffect(() => {
    let alive = true;

    fetchSessions();
    refreshStatuses();

    const sessTimer = setInterval(() => {
      if (alive) fetchSessions();
    }, 10000);
    const statusTimer = setInterval(() => {
      if (alive) refreshStatuses();
    }, 3000);

    return () => {
      alive = false;
      clearInterval(sessTimer);
      clearInterval(statusTimer);
    };
  }, [fetchSessions, refreshStatuses]);
}
