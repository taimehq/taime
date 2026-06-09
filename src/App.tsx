import { lazy, Suspense, useEffect, useState } from "react";
import {
  getBackendStatus,
  onBackendStatus,
  inTauri,
  UNKNOWN_BACKEND,
  type BackendState,
} from "./backend";
import { useBackendSync } from "./hooks/useBackendSync";
import { useStore, type Section } from "./store";
import { TitleBar } from "./chrome/TitleBar";
import { Rail } from "./chrome/Rail";
import { Sidebar } from "./chrome/Sidebar";
import { DashboardScreen } from "./screens/DashboardScreen";
import { TasksScreen } from "./screens/TasksScreen";
import { AgentsScreen } from "./screens/AgentsScreen";
import { WorkflowsScreen } from "./screens/WorkflowsScreen";
import { SchedulesScreen } from "./screens/SchedulesScreen";
import { SettingsScreen } from "./screens/SettingsScreen";
import { LaunchAgentDialog } from "./components/LaunchAgentDialog";
import { NewTaskDialog } from "./components/NewTaskDialog";
import { AddScheduleDialog } from "./components/AddScheduleDialog";
import { NewWorkflowDialog } from "./components/NewWorkflowDialog";
import { SeedDialog } from "./components/SeedDialog";
import { DeleteWorkspaceDialog } from "./components/DeleteWorkspaceDialog";
import { CommandPalette } from "./components/CommandPalette";
import { Snackbar } from "./components/Snackbar";
import { ContextSwitchGuard } from "./components/ContextSwitchGuard";
import { useTerminalFileDrop } from "./hooks/useTerminalFileDrop";
import { useRustPtyReconcile } from "./hooks/useRustPtyReconcile";
import { useGlobalShortcuts } from "./hooks/useGlobalShortcuts";

// Defer the Monaco-heavy overlays out of the initial bundle — they load only
// when the user opens a diff or the activity graph. (The old TaskReviewDrawer
// mount is gone: the Tasks screen's Review tab is the task review surface.)
const DiffView = lazy(() =>
  import("./components/DiffView").then((m) => ({ default: m.DiffView })),
);
const ActivityGraph = lazy(() =>
  import("./components/ActivityGraph").then((m) => ({ default: m.ActivityGraph })),
);

/** Section → screen. */
function Screen({ section }: { section: Section }) {
  switch (section) {
    case "tasks":
      return <TasksScreen />;
    case "agents":
      return <AgentsScreen />;
    case "workflows":
      return <WorkflowsScreen />;
    case "schedules":
      return <SchedulesScreen />;
    case "settings":
      return <SettingsScreen />;
    default:
      return <DashboardScreen />;
  }
}

export default function App() {
  const [rustBackend, setRustBackend] = useState<BackendState>(UNKNOWN_BACKEND);
  // Launch-dialog visibility lives in the store so the command palette can open
  // it too (not just the sidebar button).
  const launchOpen = useStore((s) => s.launchOpen);
  const setLaunchOpen = useStore((s) => s.setLaunchOpen);
  const newTaskOpen = useStore((s) => s.newTaskOpen);
  const setNewTaskOpen = useStore((s) => s.setNewTaskOpen);
  const newScheduleOpen = useStore((s) => s.newScheduleOpen);
  const setNewScheduleOpen = useStore((s) => s.setNewScheduleOpen);
  const newWorkflowOpen = useStore((s) => s.newWorkflowOpen);
  const setNewWorkflowOpen = useStore((s) => s.setNewWorkflowOpen);
  const seedOpen = useStore((s) => s.seedOpen);
  const setSeedOpen = useStore((s) => s.setSeedOpen);
  const deleteWorkspaceTarget = useStore((s) => s.deleteWorkspaceTarget);
  const setDeleteWorkspaceTarget = useStore((s) => s.setDeleteWorkspaceTarget);
  const connected = useStore((s) => s.connected);
  const openDiff = useStore((s) => s.openDiff);
  const section = useStore((s) => s.section);

  // Subscribe to live supervisor status (Tauri only).
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    if (inTauri()) {
      getBackendStatus().then(setRustBackend);
      onBackendStatus(setRustBackend).then((fn) => {
        unlisten = fn;
      });
    }
    return () => unlisten?.();
  }, []);

  useBackendSync();
  useTerminalFileDrop();
  useRustPtyReconcile();
  useGlobalShortcuts();

  // Inside the webview the supervisor (Rust) is the source of truth for the
  // pill. Outside it (dev-in-browser), synthesize it from REST reachability so
  // the UI still reflects the truth instead of a stuck "starting".
  const backend: BackendState = inTauri()
    ? rustBackend
    : {
        status: connected ? "external" : "external_down",
        detail: connected
          ? "Connected to backend (dev browser)"
          : "Backend not reachable",
        external: true,
        pid: null,
        // No HTTP backend exists anymore (the session daemon is a Unix socket);
        // the pill only renders status/detail, never a URL.
        apiUrl: "",
      };

  return (
    <div className="flex h-full flex-col bg-ink-900 text-zinc-200">
      <TitleBar backend={backend} />
      <div className="flex min-h-0 flex-1">
        <Rail />
        <Sidebar />
        <main className="min-h-0 min-w-0 flex-1">
          <Screen section={section} />
        </main>
      </div>
      {launchOpen && <LaunchAgentDialog onClose={() => setLaunchOpen(false)} />}
      {newTaskOpen && <NewTaskDialog onClose={() => setNewTaskOpen(false)} />}
      {newScheduleOpen && (
        // The 5s schedule polls (sidebar + screen) surface the new row.
        <AddScheduleDialog
          onClose={() => setNewScheduleOpen(false)}
          onSaved={() => {}}
        />
      )}
      {newWorkflowOpen && (
        // The 5s workflow polls (sidebar + screen) surface the new row.
        <NewWorkflowDialog onClose={() => setNewWorkflowOpen(false)} />
      )}
      {seedOpen && <SeedDialog onClose={() => setSeedOpen(false)} />}
      {deleteWorkspaceTarget && (
        <DeleteWorkspaceDialog
          path={deleteWorkspaceTarget}
          onClose={() => setDeleteWorkspaceTarget(null)}
        />
      )}
      <CommandPalette />
      <ContextSwitchGuard onReview={openDiff} />
      <Suspense fallback={null}>
        <DiffView />
        <ActivityGraph />
      </Suspense>
      <Snackbar />
    </div>
  );
}
