import { useEffect } from "react";
import { inTauri } from "../backend";
import { useStore } from "../store";
import { ptyList } from "../pty";

/**
 * Keep the Rust-PTY registry honest: every few seconds, drop any tracked
 * session whose process is no longer alive in the manager (it exited or was
 * killed — the manager removes exited sessions). This prunes the "detached
 * agents" list so it never offers a reopen for a dead agent.
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
      // Prune only sessions that are neither alive in the manager nor currently
      // framed (a just-launched frame may briefly precede pty_list visibility).
      const dead = ids.filter((id) => !live.has(id) && !framed.has(id));
      if (dead.length) {
        useStore.setState((s) => {
          const next = { ...s.rustPtySessions };
          for (const id of dead) delete next[id];
          return { rustPtySessions: next };
        });
      }
    };

    const interval = setInterval(tick, 4000);
    return () => {
      alive = false;
      clearInterval(interval);
    };
  }, []);
}
