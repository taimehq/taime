import { useEffect } from "react";
import { inTauri } from "../backend";
import { useStore, type RustPtyMeta } from "../store";
import { api } from "../api";
import { daemonList, type DaemonSessionSummary } from "../pty";
import { providerTitle } from "../lib/providerLabel";

/** Which tracked sessions the tick should demote to exited: running sessions
 *  the daemon no longer reports alive — EXCEPT framed ones (a just-launched
 *  frame may briefly precede list visibility), UNLESS the frame's attach
 *  connection was lost without an exit (daemon crash/restart): then the exit
 *  can never arrive over the channel and the roster is the only truth left,
 *  so the framed exemption must not wedge the agent as "running" forever. */
export function sessionsToDemote(
  meta: Record<string, RustPtyMeta>,
  framed: Set<string>,
  live: Set<string>,
): string[] {
  return Object.keys(meta).filter((id) => {
    if (meta[id].status !== "running") return false;
    if (framed.has(id) && !meta[id].connectionLost) return false;
    return !live.has(id);
  });
}

/** Mirror daemon-reported status (Phase 4) into the shared terminalStatuses map,
 *  keyed by the agent id the StatusBadge already reads — so a daemon agent's
 *  badge is daemon-driven, no CAO /terminals/{id} poll. */
function applyDaemonStatuses(sessions: DaemonSessionSummary[]) {
  const setStatus = useStore.getState().setTerminalStatus;
  for (const s of sessions) {
    if (s.agent_id && s.status) setStatus(s.agent_id, s.status);
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
      const sessions = (await daemonList()) ?? [];
      if (!alive) return;
      const adopt = useStore.getState().adoptDaemonSession;
      for (const s of sessions) adopt(s);
      applyDaemonStatuses(sessions);
      // Hydrate durable review acks so "I already reviewed this" survives a
      // UI/daemon restart (the guard state was frontend-only before).
      const reviewed = await api.reviewedAgents();
      if (alive && reviewed.length) useStore.getState().hydrateReviewed(reviewed);
      bootDone = true;
    })();

    const tick = async () => {
      const daemonSessions = await daemonList();
      if (!alive) return;
      // A FAILED enumeration is not an empty roster: skip the whole tick
      // (adoption, status mirror, and especially the exit demotion below)
      // rather than treat the error as "every agent is gone".
      if (daemonSessions === null) return;
      applyDaemonStatuses(daemonSessions);
      // Mirror daemon-side Task membership (task_assign / delete demotion) into
      // the tracked metas so the sidebar grouping stays fresh.
      useStore.getState().syncDaemonTaskIds(daemonSessions);

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
            !(s.agent_id && dismissed.has(s.agent_id)),
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

      // Demote sessions the daemon no longer reports alive (see sessionsToDemote
      // for the framed/connection-lost rules) as exited.
      const meta = useStore.getState().rustPtySessions;
      if (Object.keys(meta).length === 0) return;
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
      for (const id of sessionsToDemote(meta, framed, live)) markExited(id);
    };

    const interval = setInterval(tick, 4000);
    return () => {
      alive = false;
      clearInterval(interval);
    };
  }, []);
}
