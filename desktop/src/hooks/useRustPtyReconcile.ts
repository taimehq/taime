import { useEffect } from "react";
import { inTauri } from "../backend";
import { useStore } from "../store";
import { daemonList } from "../pty";

/**
 * Keep the daemon-session registry honest: enumerate the daemon on boot to adopt
 * crash-survived agents, then every few seconds mark any tracked session the
 * daemon no longer reports alive as "exited", so the detached-agents list
 * reflects lifecycle (running → exited) instead of silently dropping it. The user
 * dismisses exited entries explicitly.
 */
export function useRustPtyReconcile() {
  useEffect(() => {
    if (!inTauri()) return;
    let alive = true;

    // Boot adoption (crash recovery): enumerate the daemon's surviving sessions
    // and populate the registry so they appear in the detached-agents panel and
    // can be reopened with the daemon transport — no user action needed. Runs
    // regardless of the (initially empty) store; daemonList() returns [] without
    // spawning a daemon when none is running.
    (async () => {
      const sessions = await daemonList();
      if (!alive) return;
      const adopt = useStore.getState().adoptDaemonSession;
      for (const s of sessions) adopt(s);
    })();

    const tick = async () => {
      const meta = useStore.getState().rustPtySessions;
      const ids = Object.keys(meta);
      if (ids.length === 0) return;
      const daemonSessions = await daemonList();
      if (!alive) return;
      const live = new Set(
        daemonSessions.filter((s) => s.alive !== false).map((s) => s.id),
      );
      const framed = new Set(
        useStore
          .getState()
          .frames.map((f) => f.ptySessionId)
          .filter(Boolean) as string[],
      );
      // Mark sessions the daemon no longer reports alive (and that aren't framed —
      // a just-launched frame may briefly precede list visibility) as exited.
      const markExited = useStore.getState().markRustPtyExited;
      for (const id of ids) {
        if (meta[id].status !== "running" || framed.has(id)) continue;
        if (!live.has(id)) markExited(id);
      }
    };

    const interval = setInterval(tick, 4000);
    return () => {
      alive = false;
      clearInterval(interval);
    };
  }, []);
}
