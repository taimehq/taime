import { useEffect, useRef } from "react";
import { api } from "../api";
import { useStore, isDaemonTransport } from "../store";

/**
 * Derive agent "turn" boundaries from the status signal we already poll and
 * snapshot each turn into the activity graph — no extra polling loop.
 *
 *   non-PROCESSING → PROCESSING   ⇒ turn_start (snapshot the tree, open a turn)
 *   PROCESSING → IDLE | COMPLETED ⇒ turn_end   (snapshot again, record files)
 *
 * Mounted once at the app root.
 */
export function useTurnCheckpoints() {
  const statuses = useStore((s) => s.terminalStatuses);
  const frames = useStore((s) => s.frames);
  const prev = useRef<Record<string, string>>({});

  useEffect(() => {
    // Only CAO frames drive CAO checkpoints. Daemon frames now carry a
    // terminalStatuses entry too (Phase 4 daemon status), but their turn
    // boundaries arrive as daemon turn events (recordTurn) — posting a CAO
    // checkpoint for them would double-attribute, so exclude them here.
    const liveIds = new Set(
      frames
        .filter((f) => !isDaemonTransport(f.transport))
        .map((f) => f.terminalId)
        .filter((x): x is string => !!x),
    );
    for (const tid of liveIds) {
      const cur = statuses[tid];
      if (!cur) continue;
      const before = prev.current[tid];
      if (before === cur) continue;

      if (cur === "PROCESSING" && before !== "PROCESSING") {
        api.postCheckpoint(tid, "turn_start").catch(() => {});
      } else if (
        before === "PROCESSING" &&
        (cur === "IDLE" || cur === "COMPLETED")
      ) {
        api.postCheckpoint(tid, "turn_end").catch(() => {});
      }
      prev.current[tid] = cur;
    }
    // Forget terminals whose frames are gone.
    for (const tid of Object.keys(prev.current)) {
      if (!liveIds.has(tid)) delete prev.current[tid];
    }
  }, [statuses, frames]);
}
