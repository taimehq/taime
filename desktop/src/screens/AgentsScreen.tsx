import { useStore, isDaemonTransport } from "../store";
import { ShellGrid } from "../layout/ShellGrid";
import { AgentDetail } from "./agents/AgentDetail";

/**
 * The Agents section. Default view is the existing multi-terminal shell grid
 * (frames, guarded switching, reattach — unchanged). When exactly one agent
 * is focused (the existing focus-mode semantics: layoutMode "focus" + the
 * active frame), the focused AgentDetail view takes over: identity header,
 * Terminal | Console | Diff | Activity. Pending/placeholder frames stay on
 * the grid (its spinner cell is the launch surface).
 */
export function AgentsScreen() {
  const layoutMode = useStore((s) => s.layoutMode);
  const activeFrameKey = useStore((s) => s.activeFrameKey);
  const frames = useStore((s) => s.frames);

  const active = frames.find((f) => f.key === activeFrameKey) ?? frames[0];
  const focused =
    layoutMode === "focus" &&
    active &&
    !active.pending &&
    isDaemonTransport(active.transport) &&
    active.ptySessionId
      ? active
      : null;

  return (
    <div className="h-full p-2">
      {focused ? <AgentDetail frame={focused} /> : <ShellGrid />}
    </div>
  );
}
