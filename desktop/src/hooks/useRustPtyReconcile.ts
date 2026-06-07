import { useEffect } from "react";
import { inTauri } from "../backend";
import { useStore } from "../store";
import { daemonList, type DaemonSessionSummary } from "../pty";
import { providerTitle } from "../lib/providerLabel";

/** Mirror daemon-reported status (Phase 4) into the shared terminalStatuses map,
 *  keyed by the attribution id the StatusBadge already reads — so a daemon
 *  agent's badge is daemon-driven, no CAO /terminals/{id} poll. */
function applyDaemonStatuses(sessions: DaemonSessionSummary[]) {
  const setStatus = useStore.getState().setTerminalStatus;
  for (const s of sessions) {
    if (s.attribution_key && s.status) setStatus(s.attribution_key, s.status);
  }
}

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
    let bootDone = false;
    (async () => {
      const sessions = await daemonList();
      if (!alive) return;
      const adopt = useStore.getState().adoptDaemonSession;
      for (const s of sessions) adopt(s);
      applyDaemonStatuses(sessions);
      bootDone = true;
    })();

    const tick = async () => {
      const daemonSessions = await daemonList();
      if (!alive) return;
      applyDaemonStatuses(daemonSessions);

      // Surface NEW live daemon sessions the moment they appear — e.g. the workers
      // an orchestrator just assigned. Adopt into the Agents panel AND open a
      // NON-FOCUSED frame (so the team visibly forms without stealing focus from
      // the agent you're typing in), then toast. Gated on bootDone so the initial
      // set adopted at boot isn't re-framed.
      if (bootDone) {
        const st = useStore.getState();
        const tracked = st.rustPtySessions;
        const dismissed = st.dismissedTerminalIds;
        const fresh = daemonSessions.filter(
          (s) =>
            s.alive !== false &&
            !tracked[s.id] &&
            !(s.attribution_key && dismissed.has(s.attribution_key)),
        );
        for (const s of fresh) {
          st.adoptDaemonSession(s);
          st.reopenRustPty(s.id, { focus: false });
        }
        if (fresh.length === 1) {
          const p = fresh[0].provider;
          st.showSnackbar({
            type: "info",
            message: `${p ? providerTitle(p) + " " : ""}agent joined the team`,
          });
        } else if (fresh.length > 1) {
          st.showSnackbar({ type: "info", message: `${fresh.length} agents joined the team` });
        }
      }

      // Demote sessions the daemon no longer reports alive (and that aren't framed —
      // a just-launched frame may briefly precede list visibility) as exited.
      const meta = useStore.getState().rustPtySessions;
      const ids = Object.keys(meta);
      if (ids.length === 0) return;
      const live = new Set(
        daemonSessions.filter((s) => s.alive !== false).map((s) => s.id),
      );
      const framed = new Set(
        useStore
          .getState()
          .frames.map((f) => f.ptySessionId)
          .filter(Boolean) as string[],
      );
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
