import { useCallback, useEffect, useRef, useState } from "react";
import {
  api,
  type AttributionResponse,
  type FileDiffEntry,
  type HunkedFileEntry,
  type TaskAgent,
  type TaskDetail,
  type WorktreeInfo,
} from "../../api";
import { useStore } from "../../store";

// ─── Shared helpers for the Tasks screen ─────────────────────────────────────

// Truncation + attribution grammar live in the shared libs; re-exported here
// so the Tasks screens keep one local import surface.
export { middleTruncate } from "../../lib/format";
export {
  AUTHOR_COLORS,
  authorName,
  type AuthorColor,
} from "../../lib/attribution";

/** Navigate to the Agents section and focus this agent's terminal: live frame
 *  if present, else reattach the detached-but-running session via its meta.
 *  (The TaskReviewDrawer focus logic, plus the section hop.) */
export function openAgent(agentId: string): void {
  const s = useStore.getState();
  s.setSection("agents");
  const frame = s.frames.find((f) => f.terminalId === agentId);
  if (frame) {
    s.setActiveFrameGuarded(frame.key);
    return;
  }
  const meta = Object.values(s.rustPtySessions).find(
    (m) => m.terminalId === agentId,
  );
  if (meta && meta.status === "running") s.reopenRustPty(meta.ptySessionId);
}

/** The wire status to render for a task member: live push first, the detail
 *  snapshot as fallback, and the lifecycle override when the process is gone. */
export function memberWireStatus(
  a: Pick<TaskAgent, "agent_id" | "status" | "alive">,
  terminalStatuses: Record<string, string>,
): string | undefined {
  if (!a.alive) return "EXITED";
  return terminalStatuses[a.agent_id] ?? a.status ?? undefined;
}

/** Unix-seconds → local date+time (header/meta provenance). */
export function fmtUnix(unixSecs: number | null): string {
  if (!unixSecs) return "—";
  try {
    return new Date(unixSecs * 1000).toLocaleString([], {
      year: "numeric",
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  } catch {
    return "—";
  }
}

/** Epoch ms → local clock time (activity feed rows). */
export function fmtClock(ms: number): string {
  try {
    return new Date(ms).toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  } catch {
    return "—";
  }
}

// ─── Task detail poll ────────────────────────────────────────────────────────

/** Poll one task's detail (2s — rollups track the agents in real time, the
 *  cadence the review drawer used). Stale responses are dropped so task A's
 *  payload never renders over task B. */
export function useTaskDetail(taskId: string): {
  detail: TaskDetail | null;
  loaded: boolean;
  reload: () => void;
} {
  const [detail, setDetail] = useState<TaskDetail | null>(null);
  const [loaded, setLoaded] = useState(false);
  const current = useRef(taskId);
  current.current = taskId;

  const reload = useCallback(() => {
    void api.getTaskDetail(taskId).then((d) => {
      if (current.current !== taskId) return; // stale — task switched mid-fetch
      setDetail(d);
      setLoaded(true);
    });
  }, [taskId]);

  useEffect(() => {
    setDetail(null);
    setLoaded(false);
    reload();
    const t = setInterval(reload, 2000);
    return () => clearInterval(t);
  }, [reload]);

  return { detail, loaded, reload };
}

// ─── Per-agent diff bundle (DiffView's data path, per member agent) ──────────

/** Everything the Review tab needs for ONE member agent's worktree: the same
 *  four queries DiffView issues (file diffs / hunked diff / worktree row /
 *  attribution) — the task only aggregates them. */
export interface AgentDiffBundle {
  files: FileDiffEntry[];
  hunks: HunkedFileEntry[];
  worktree: WorktreeInfo | null;
  attribution: AttributionResponse;
}

export async function loadAgentDiffBundle(agentId: string): Promise<AgentDiffBundle> {
  const [fd, hk, wt, attr] = await Promise.all([
    api.getFileDiffs(agentId).catch(() => ({ agent_id: agentId, files: [] })),
    api.getHunks(agentId).catch(() => ({ agent_id: agentId, base: null, files: [] })),
    api.getWorktree(agentId).catch(() => null),
    api.getAttribution(agentId).catch(() => ({ team: [], files: {} })),
  ]);
  return { files: fd.files, hunks: hk.files, worktree: wt, attribution: attr };
}
