import { useEffect } from "react";
import { api } from "../api";
import { useStore } from "../store";
import { resolveWorkingDirectory } from "../lib/terminalRouting";
import { watchTerminal, unwatchTerminal, onDirty, onFsEvent } from "../fswatch";

/**
 * For a live terminal: resolve its working directory, ask Rust to watch it, and
 * stream change signals into the store. This is the React-push half of the
 * terminal↔dir mapping — React owns the id, resolves the dir, and hands both to
 * Rust. Two channels:
 *   - `fs-dirty`  → the badge/inventory dirty SET (count + paths)
 *   - `fs-event`  → the per-file attributed timeline (path/kind/ts), which we
 *     also forward to the backend activity graph for durable attribution.
 * Cleans up the watch + subscriptions on unmount.
 */
export function useFsWatch(terminalId: string | null) {
  const setDirty = useStore((s) => s.setDirty);
  const clearDirtyLocal = useStore((s) => s.clearDirty);
  const appendTimeline = useStore((s) => s.appendTimeline);

  useEffect(() => {
    if (!terminalId) return;
    let unlistenDirty: (() => void) | undefined;
    let unlistenEvent: (() => void) | undefined;
    let cancelled = false;

    (async () => {
      // Subscribe first so we don't miss the initial burst.
      unlistenDirty = await onDirty(terminalId, (p) => {
        setDirty(terminalId, { count: p.count, paths: p.paths });
      });
      unlistenEvent = await onFsEvent(terminalId, (b) => {
        appendTimeline(terminalId, b.events);
        // Forward to the backend activity graph (best-effort; attribution is
        // certain in worktree mode where the watch dir maps 1:1 to this agent).
        api.postFsEvents(terminalId, b.events).catch(() => {
          /* graph ingest is best-effort */
        });
      });
      try {
        // Route by ownership: a daemon-owned terminal resolves to its local cwd
        // (no CAO round-trip; survives worktrees moving daemon-side in Phase 3).
        // `getState()` so the watch keys only off `terminalId`, not the registry.
        const working_directory = await resolveWorkingDirectory(
          terminalId,
          useStore.getState().rustPtySessions,
          async (id) => (await api.getWorkingDirectory(id)).working_directory,
        );
        if (cancelled || !working_directory) return;
        await watchTerminal(terminalId, working_directory);
      } catch {
        /* working dir unavailable — skip watching */
      }
    })();

    return () => {
      cancelled = true;
      unlistenDirty?.();
      unlistenEvent?.();
      unwatchTerminal(terminalId);
      clearDirtyLocal(terminalId);
    };
  }, [terminalId, setDirty, clearDirtyLocal, appendTimeline]);
}
