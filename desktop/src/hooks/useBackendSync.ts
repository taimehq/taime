import { useEffect } from "react";
import { useStore } from "../store";

/**
 * Periodic backend polling. Connectivity (`store.connected`) is driven by actual
 * daemon reachability — `fetchAgents` flips it on success/failure — so the UI
 * reflects whether the daemon truly responds, independent of the Rust supervisor's
 * self-reported status (which is unavailable when running outside the webview).
 *
 *  - agent roster (10s) → connectivity probe + reconcile input
 *
 * Per-frame status arrives over the daemon attach channel + reconcile tick
 * (useRustPtyReconcile), not from polling here.
 */
export function useBackendSync() {
  const fetchAgents = useStore((s) => s.fetchAgents);

  useEffect(() => {
    let alive = true;

    fetchAgents();

    const agentsTimer = setInterval(() => {
      if (alive) fetchAgents();
    }, 10000);

    return () => {
      alive = false;
      clearInterval(agentsTimer);
    };
  }, [fetchAgents]);
}
