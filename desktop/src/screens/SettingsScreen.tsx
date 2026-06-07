import { useStore } from "../store";
import { WorkspacePicker } from "../components/WorkspacePicker";

/**
 * Placeholder — the Screens phase replaces this. The Workspace tab mounts the
 * existing picker so workspace browse / recents / manual path / the worktree
 * isolation toggle don't regress while screens land.
 */
export function SettingsScreen() {
  const settingsTab = useStore((s) => s.settingsTab);
  return (
    <div className="flex h-full flex-col gap-3 overflow-y-auto p-6">
      <h1 className="text-sm font-medium text-zinc-100">Settings</h1>
      {settingsTab === "workspace" ? (
        <div className="max-w-sm">
          <WorkspacePicker />
        </div>
      ) : (
        <p className="text-xs text-zinc-500">
          {settingsTab === "providers" ? "Providers" : "Agent profiles"} —
          lands in the Screens phase.
        </p>
      )}
    </div>
  );
}
