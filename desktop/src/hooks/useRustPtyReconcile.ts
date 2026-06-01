import { useEffect } from "react";
import { inTauri } from "../backend";
import { useStore } from "../store";
import { ptyList } from "../pty";

/**
 * Keep the Rust-PTY registry honest: every few seconds, mark any tracked
 * session whose process is no longer alive in the manager (it exited or was
 * killed — the manager removes exited sessions) as "exited", so the detached-
 * agents list reflects lifecycle (running → exited) instead of silently
 * dropping it. The user dismisses exited entries explicitly.
 */
export function useRustPtyReconcile() {
  useEffect(() => {
    if (!inTauri()) return;
    let alive = true;

    const tick = async () => {
      const meta = useStore.getState().rustPtySessions;
      const ids = Object.keys(meta);
      if (ids.length === 0) return;
      const live = new Set((await ptyList()).map((s) => s.id));
      if (!alive) return;
      const framed = new Set(
        useStore
          .getState()
          .frames.map((f) => f.ptySessionId)
          .filter(Boolean) as string[],
      );
      // Mark sessions that are neither alive in the manager nor currently framed
      // (a just-launched frame may briefly precede pty_list visibility) as exited.
      const markExited = useStore.getState().markRustPtyExited;
      for (const id of ids) {
        if (!live.has(id) && !framed.has(id) && meta[id].status === "running") {
          markExited(id);
        }
      }
    };

    const interval = setInterval(tick, 4000);
    return () => {
      alive = false;
      clearInterval(interval);
    };
  }, []);
}
