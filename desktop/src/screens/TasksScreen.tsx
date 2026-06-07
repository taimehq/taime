import { useEffect } from "react";
import { useStore } from "../store";

/**
 * Placeholder — the Screens phase replaces this with the full Task screen.
 * It already honors the one-shot review deep-link (taskInitialTab) by routing
 * it to the existing Task Review drawer, and keeps review reachable.
 */
export function TasksScreen() {
  const selectedTaskId = useStore((s) => s.selectedTaskId);
  const taskInitialTab = useStore((s) => s.taskInitialTab);
  const clearTaskInitialTab = useStore((s) => s.clearTaskInitialTab);
  const openTaskReview = useStore((s) => s.openTaskReview);

  useEffect(() => {
    if (taskInitialTab === null) return;
    if (taskInitialTab === "review" && selectedTaskId) {
      openTaskReview(selectedTaskId);
    }
    clearTaskInitialTab();
  }, [taskInitialTab, selectedTaskId, openTaskReview, clearTaskInitialTab]);

  return (
    <div className="flex h-full flex-col gap-2 p-6">
      <h1 className="text-sm font-medium text-zinc-100">Tasks</h1>
      {selectedTaskId ? (
        <>
          <p className="text-xs text-zinc-500">
            Selected task:{" "}
            <span className="font-mono text-zinc-400">{selectedTaskId}</span>
          </p>
          <button
            onClick={() => openTaskReview(selectedTaskId)}
            className="self-start rounded-md border border-ink-500 px-2.5 py-1 text-xs text-zinc-300 hover:bg-ink-600"
          >
            Open task review
          </button>
        </>
      ) : (
        <p className="text-xs text-zinc-500">
          Select a task in the sidebar. The full Task screen lands in the
          Screens phase.
        </p>
      )}
    </div>
  );
}
