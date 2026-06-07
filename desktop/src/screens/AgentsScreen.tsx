import { ShellGrid } from "../layout/ShellGrid";

/** The Agents section: the existing multi-terminal shell grid, unchanged —
 *  frames, focus/grid layout, guarded switching, reattach all keep working. */
export function AgentsScreen() {
  return (
    <div className="h-full p-2">
      <ShellGrid />
    </div>
  );
}
