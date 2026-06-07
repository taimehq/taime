import { SchedulesPanel } from "../components/SchedulesPanel";

/** Placeholder — the Screens phase replaces this. The existing panel is
 *  mounted so schedule create / run / toggle / delete don't regress. */
export function SchedulesScreen() {
  return (
    <div className="flex h-full flex-col gap-3 overflow-y-auto p-6">
      <h1 className="text-sm font-medium text-zinc-100">Schedules</h1>
      <div className="max-w-md">
        <SchedulesPanel />
      </div>
    </div>
  );
}
