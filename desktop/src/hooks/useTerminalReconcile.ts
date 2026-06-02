import { useEffect } from "react";
import { api } from "../api";
import { useStore } from "../store";

/** How often to reconcile a session's terminal list against the open frames. */
const RECONCILE_INTERVAL = 10000;
/** Max frames auto-opened in a single tick; the rest are announced, not opened. */
const MAX_SURFACED_PER_TICK = 6;

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
              !!f.sessionName,
          )
          .map((f) => f.sessionName as string),
      );
      if (activeSessions.size === 0) return;

      const shownTerminalIds = new Set(
        state.frames.map((f) => f.terminalId).filter(Boolean) as string[],
      );
      const dismissed = state.dismissedTerminalIds;

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
          for (const t of detail.terminals) {
            if (shownTerminalIds.has(t.id) || dismissed.has(t.id)) continue;
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

      const open = useStore.getState().openTerminalFrame;
      const showSnackbar = useStore.getState().showSnackbar;
      const capped = toOpen.slice(0, MAX_SURFACED_PER_TICK);
      for (const t of capped) open(t);

      if (toOpen.length > MAX_SURFACED_PER_TICK) {
        showSnackbar({
          type: "info",
          message: `${toOpen.length} new agents spawned — open from the sidebar`,
        });
      } else {
        showSnackbar({
          type: "info",
          message:
            capped.length === 1
              ? "1 new agent surfaced"
              : `${capped.length} new agents surfaced`,
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
