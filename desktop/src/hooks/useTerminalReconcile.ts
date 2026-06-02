import { useEffect } from "react";
import { api } from "../api";
import { useStore } from "../store";

/** How often to reconcile a session's terminal list against the open frames. */
const RECONCILE_INTERVAL = 10000;
/** Max frames auto-opened in a single tick; the rest are announced, not opened. */
const MAX_SURFACED_PER_TICK = 6;

/**
 * All terminalIds currently represented by a frame — the dedupe keys for
 * reconciliation. This INCLUDES rust_pty frames: a Rust-PTY agent's frame
 * carries its provisioned worktree terminalId, so keying on it here prevents
 * reconcile from opening a second (CAO) frame for the same id. (In practice a
 * provisioned rust_pty id has no tmux pane, so getSession never lists it — see
 * the terminal listing in api/main.py — but excluding it explicitly is robust
 * against that ever changing.)
 */
function shownTerminalIds(
  state: ReturnType<typeof useStore.getState>,
): Set<string> {
  return new Set(
    state.frames.map((f) => f.terminalId).filter(Boolean) as string[],
  );
}

/**
 * Surface spawned worker agents in the shell grid.
 *
 * Frames are otherwise only created by explicit user actions (launchAgent /
 * openTerminalFrame), so assign/handoff-spawned workers run invisibly. This hook
 * watches the terminal list of sessions the user is *already* looking at — those
 * with ≥1 open, non-pending CAO frame — and opens a frame for any terminal that
 * isn't shown, isn't dismissed, and isn't a Rust-PTY agent (those have their own
 * detached-agent lifecycle). To avoid a stampede, at most six are opened per
 * tick; if more appeared, a single snackbar points the user at the sidebar.
 */
export function useTerminalReconcile() {
  useEffect(() => {
    let alive = true;

    const tick = async () => {
      const state = useStore.getState();
      // Sessions with an in-flight optimistic placeholder (terminalId still
      // null): skip them entirely this tick. The backend terminal may already
      // exist while launchAgent's addTerminal await is pending, and opening it
      // here would race launchAgent into two frames for the same id. (launchAgent
      // also dedupes on resolve as a backstop.)
      const pendingSessions = new Set(
        state.frames
          .filter((f) => f.pending && !!f.sessionName)
          .map((f) => f.sessionName as string),
      );
      // Strictly scope to sessions that already have an open frame, so we never
      // auto-open every backend session — only reveal workers within the
      // sessions the user is actively working in.
      const activeSessions = new Set(
        state.frames
          .filter(
            (f) =>
              f.transport !== "rust_pty" &&
              !f.pending &&
              !!f.terminalId &&
              !!f.sessionName &&
              !pendingSessions.has(f.sessionName),
          )
          .map((f) => f.sessionName as string),
      );
      if (activeSessions.size === 0) return;

      const toOpen: {
        terminalId: string;
        provider: string;
        agentProfile: string | null;
        sessionName: string;
      }[] = [];

      for (const sessionName of activeSessions) {
        try {
          const detail = await api.getSession(sessionName);
          if (!alive) return;
          // Re-read live state per session: frames/dismissals can change across
          // the awaits above.
          const live = useStore.getState();
          const shown = shownTerminalIds(live);
          for (const t of detail.terminals) {
            if (shown.has(t.id) || live.dismissedTerminalIds.has(t.id)) continue;
            toOpen.push({
              terminalId: t.id,
              provider: t.provider,
              agentProfile: t.agent_profile,
              sessionName: t.tmux_session,
            });
          }
        } catch {
          /* skip a momentarily-unreachable session */
        }
      }

      if (!alive || toOpen.length === 0) return;

      const showSnackbar = useStore.getState().showSnackbar;
      const capped = toOpen.slice(0, MAX_SURFACED_PER_TICK);
      let surfaced = 0;
      for (const t of capped) {
        // Re-check immediately before opening: a frame may have been closed
        // (→ dismissed) or another tick may have opened this id mid-loop.
        const live = useStore.getState();
        if (shownTerminalIds(live).has(t.terminalId)) continue;
        if (live.dismissedTerminalIds.has(t.terminalId)) continue;
        live.openTerminalFrame(t);
        surfaced++;
      }

      if (surfaced === 0) return;
      if (toOpen.length > MAX_SURFACED_PER_TICK) {
        showSnackbar({
          type: "info",
          message: `${toOpen.length} new agents spawned — open from the sidebar`,
        });
      } else {
        showSnackbar({
          type: "info",
          message:
            surfaced === 1
              ? "1 new agent surfaced"
              : `${surfaced} new agents surfaced`,
        });
      }
    };

    tick();
    const interval = setInterval(tick, RECONCILE_INTERVAL);
    return () => {
      alive = false;
      clearInterval(interval);
    };
  }, []);
}
