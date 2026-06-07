import { WorkflowsPanel } from "../components/WorkflowsPanel";

/** Placeholder — the Screens phase replaces this. The existing panel is
 *  mounted so run / graph / delete don't regress while screens land. */
export function WorkflowsScreen() {
  return (
    <div className="flex h-full flex-col gap-3 overflow-y-auto p-6">
      <h1 className="text-sm font-medium text-zinc-100">Workflows</h1>
      <div className="max-w-md">
        <WorkflowsPanel />
      </div>
    </div>
  );
}
