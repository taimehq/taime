import { useCallback, useEffect, useState } from "react";
import { api, type TaskInfo } from "../api";

/**
 * Light poll of the active workspace's tasks (the daemon store is the source
 * of truth; daemon-side changes — orchestrator inherits, schedule per-run
 * creates — must surface without a reload). Archived tasks are included:
 * "archive preserves membership", so members keep their group (dimmed) rather
 * than masquerading as Uncategorized.
 */
export function useTasks(workspaceDir: string | null): {
  tasks: TaskInfo[];
  reload: () => void;
} {
  const [tasks, setTasks] = useState<TaskInfo[]>([]);

  const reload = useCallback(() => {
    if (!workspaceDir) return;
    api
      .listTasks(workspaceDir, true)
      .then(setTasks)
      .catch(() => {
        /* daemon down — surfaced by the backend pill; next poll retries */
      });
  }, [workspaceDir]);

  useEffect(() => {
    if (!workspaceDir) {
      setTasks([]);
      return;
    }
    let alive = true;
    const load = () =>
      api
        .listTasks(workspaceDir, true)
        .then((t) => {
          if (alive) setTasks(t);
        })
        .catch(() => {});
    load();
    const timer = setInterval(load, 5000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, [workspaceDir]);

  return { tasks, reload };
}
