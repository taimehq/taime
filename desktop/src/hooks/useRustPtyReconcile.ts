import { useEffect } from "react";
import { inTauri } from "../backend";
import { useStore } from "../store";
import { ptyList, daemonList } from "../pty";

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
      // Reconcile each session against the backend that OWNS it: in-app sessions
      // via pty_list, daemon sessions via daemon_list. Polling only the in-app
      // list would falsely mark every detached daemon agent as exited.
      const needInapp = ids.some((id) => meta[id].transport !== "daemon");
      const needDaemon = ids.some((id) => meta[id].transport === "daemon");
      const [inappList, daemonSessions] = await Promise.all([
        needInapp ? ptyList() : Promise.resolve([]),
        needDaemon ? daemonList() : Promise.resolve([]),
      ]);
      if (!alive) return;
      const liveInapp = new Set(inappList.map((s) => s.id));
      const liveDaemon = new Set(
        daemonSessions.filter((s) => s.alive !== false).map((s) => s.id),
      );
      const framed = new Set(
        useStore
          .getState()
          .frames.map((f) => f.ptySessionId)
          .filter(Boolean) as string[],
      );
      // Mark sessions that are neither alive in their backend nor currently framed
      // (a just-launched frame may briefly precede list visibility) as exited.
      const markExited = useStore.getState().markRustPtyExited;
      for (const id of ids) {
        if (meta[id].status !== "running" || framed.has(id)) continue;
        const live = meta[id].transport === "daemon" ? liveDaemon : liveInapp;
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
