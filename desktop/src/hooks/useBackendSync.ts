import { useEffect } from "react";
import { useStore } from "../store";

/**
 * Periodic backend polling. Connectivity (`store.connected`) is driven by actual
 * daemon reachability — `fetchAgents` flips it on success/failure — so the UI
 * reflects whether the daemon truly responds, independent of the Rust supervisor's
 * self-reported status (which is unavailable when running outside the webview).
 *
 *  - agent roster (10s)   → connectivity probe + reconcile input
 *  - terminal statuses (3s) for frames currently open (no-ops when none)
 */
export function useBackendSync() {
  const fetchAgents = useStore((s) => s.fetchAgents);
  const refreshStatuses = useStore((s) => s.refreshStatuses);

  useEffect(() => {
    let alive = true;

    fetchAgents();
    refreshStatuses();

    const agentsTimer = setInterval(() => {
      if (alive) fetchAgents();
    }, 10000);
    const statusTimer = setInterval(() => {
      if (alive) refreshStatuses();
    }, 3000);

    return () => {
      alive = false;
      clearInterval(agentsTimer);
      clearInterval(statusTimer);
    };
  }, [fetchAgents, refreshStatuses]);
}
