import { useEffect, useState } from "react";
import { api, type GraphTurn } from "../../api";

/**
 * One agent's attribution turns, polled from the daemon's activity graph (the
 * durable turn store — complete even while no terminal view is attached, which
 * is exactly when the Console/Activity projections need it). Keyed by the
 * agent's anchor id: the graph keys on COALESCE(attribution_key,
 * pty_session_id), so a pre-provision agent still resolves.
 */
export interface AgentTurnsState {
  /** Ascending by turn_index. */
  turns: GraphTurn[];
  loading: boolean;
  /** Last poll failed (daemon unreachable) — the poll keeps retrying. */
  error: boolean;
}

const EMPTY: AgentTurnsState = { turns: [], loading: false, error: false };

export function useAgentTurns(anchorId: string | null): AgentTurnsState {
  const [state, setState] = useState<AgentTurnsState>({
    turns: [],
    loading: true,
    error: false,
  });

  useEffect(() => {
    if (!anchorId) {
      setState(EMPTY);
      return;
    }
    let alive = true;
    setState({ turns: [], loading: true, error: false });
    const load = async () => {
      try {
        // The graph query is agent-roster wide; project to this agent's row.
        const g = await api.getGraph(anchorId);
        if (!alive) return;
        const row = g.agents.find((a) => a.agent_id === anchorId);
        const turns = [...(row?.turns ?? [])].sort(
          (a, b) => a.turn_index - b.turn_index,
        );
        setState((s) => {
          // Re-render guard: same turn count + same last edge ⇒ no change.
          const prev = s.turns;
          if (
            !s.loading &&
            !s.error &&
            prev.length === turns.length &&
            prev[prev.length - 1]?.ended_at === turns[turns.length - 1]?.ended_at &&
            prev[prev.length - 1]?.files_touched.length ===
              turns[turns.length - 1]?.files_touched.length
          ) {
            return s;
          }
          return { turns, loading: false, error: false };
        });
      } catch {
        if (!alive) return;
        setState((s) => ({ ...s, loading: false, error: true }));
      }
    };
    void load();
    const timer = setInterval(load, 5000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, [anchorId]);

  return state;
}
