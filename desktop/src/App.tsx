import { lazy, Suspense, useEffect, useState } from "react";
import { getConfig, type ResolvedConfig } from "./config";
import {
  getBackendStatus,
  onBackendStatus,
  inTauri,
  UNKNOWN_BACKEND,
  type BackendState,
} from "./backend";
import { useBackendSync } from "./hooks/useBackendSync";
import { useStore } from "./store";
import { BackendStatusPill } from "./components/BackendStatusPill";
import { ControlColumn } from "./layout/ControlColumn";
import { ShellGrid } from "./layout/ShellGrid";
import { LaunchAgentDialog } from "./components/LaunchAgentDialog";
import { CommandPalette } from "./components/CommandPalette";
import { Snackbar } from "./components/Snackbar";
import { ContextSwitchGuard } from "./components/ContextSwitchGuard";
import { useTurnCheckpoints } from "./hooks/useTurnCheckpoints";
import { useTerminalFileDrop } from "./hooks/useTerminalFileDrop";
import { useRustPtyReconcile } from "./hooks/useRustPtyReconcile";
import { useTerminalReconcile } from "./hooks/useTerminalReconcile";
import { useGlobalShortcuts } from "./hooks/useGlobalShortcuts";
import {
  LayoutGrid,
  Maximize2,
  PanelLeftClose,
  PanelLeftOpen,
  Users,
} from "lucide-react";

// Defer the Monaco-heavy overlays out of the initial bundle — they load only
// when the user opens a diff or the activity graph.
const DiffView = lazy(() =>
  import("./components/DiffView").then((m) => ({ default: m.DiffView })),
);
const ActivityGraph = lazy(() =>
  import("./components/ActivityGraph").then((m) => ({ default: m.ActivityGraph })),
);
const TaskReviewDrawer = lazy(() =>
  import("./components/TaskReviewDrawer").then((m) => ({ default: m.TaskReviewDrawer })),
);

export default function App() {
  const [cfg, setCfg] = useState<ResolvedConfig | null>(null);
  const [rustBackend, setRustBackend] = useState<BackendState>(UNKNOWN_BACKEND);
  // Launch-dialog visibility lives in the store so the command palette can open
  // it too (not just the sidebar button).
  const launchOpen = useStore((s) => s.launchOpen);
  const setLaunchOpen = useStore((s) => s.setLaunchOpen);
  const connected = useStore((s) => s.connected);
  const openDiff = useStore((s) => s.openDiff);
  const setGraphOpen = useStore((s) => s.setGraphOpen);

  // Discover backend URL + subscribe to live supervisor status (Tauri only).
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    getConfig().then(setCfg);
    if (inTauri()) {
      getBackendStatus().then(setRustBackend);
      onBackendStatus(setRustBackend).then((fn) => {
        unlisten = fn;
      });
    }
    return () => unlisten?.();
  }, []);

  useBackendSync();
  useTurnCheckpoints();
  useTerminalFileDrop();
  useRustPtyReconcile();
  useTerminalReconcile();
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
        apiUrl: cfg?.apiUrl ?? "",
      };

  return (
    <div className="flex h-full flex-col bg-ink-900 text-zinc-200">
      <TitleBar backend={backend} onOpenGraph={() => setGraphOpen(true)} />
      <div className="flex min-h-0 flex-1">
        <ControlColumn onLaunch={() => setLaunchOpen(true)} />
        <main className="min-h-0 flex-1 p-2">
          <ShellGrid />
        </main>
      </div>
      {launchOpen && <LaunchAgentDialog onClose={() => setLaunchOpen(false)} />}
      <CommandPalette />
      <ContextSwitchGuard onReview={openDiff} />
      <Suspense fallback={null}>
        <DiffView />
        <ActivityGraph />
        <TaskReviewDrawer />
      </Suspense>
      <Snackbar />
    </div>
  );
}

function TitleBar({
  backend,
  onOpenGraph,
}: {
  backend: BackendState;
  onOpenGraph: () => void;
}) {
  // Unified macOS title bar. `trafficLightPosition.y` is aligned to the same
  // 24px vertical center as this h-12 bar; keep it in sync if the bar height
  // changes. Left padding clears the native traffic-light cluster plus a
  // comfortable gap.
  const layoutMode = useStore((s) => s.layoutMode);
  const toggleLayoutMode = useStore((s) => s.toggleLayoutMode);
  const hasFrames = useStore((s) => s.frames.length > 0);
  const sidebarCollapsed = useStore((s) => s.sidebarCollapsed);
  const toggleSidebar = useStore((s) => s.toggleSidebar);
  const runningAgents = useStore(
    (s) => Object.values(s.rustPtySessions).filter((m) => m.status === "running").length,
  );

  return (
    <header className="titlebar-drag flex h-12 shrink-0 items-center justify-between border-b border-ink-600 bg-ink-800 pl-[88px] pr-4">
      <div className="flex items-center gap-3">
        <button
          onClick={toggleSidebar}
          title={sidebarCollapsed ? "Show sidebar (⌘\\)" : "Hide sidebar (⌘\\)"}
          className="no-drag rounded p-1 text-zinc-500 hover:bg-ink-600 hover:text-zinc-200"
        >
          {sidebarCollapsed ? (
            <PanelLeftOpen size={15} />
          ) : (
            <PanelLeftClose size={15} />
          )}
        </button>
        <span className="-translate-y-[2px] font-mono text-[14px] font-medium lowercase leading-none tracking-tight text-zinc-300">
          taime
        </span>
      </div>
      <div className="flex items-center gap-4">
        {hasFrames && (
          <button
            onClick={toggleLayoutMode}
            title={
              layoutMode === "grid"
                ? "Focus a single agent (⌘⇧⏎)"
                : "Show all agents in a grid (⌘⇧⏎)"
            }
            className="no-drag flex items-center gap-2.5 rounded-full border border-ink-600 px-3.5 py-1.5 text-xs text-zinc-300 hover:bg-ink-600"
          >
            {layoutMode === "grid" ? (
              <Maximize2 size={13} />
            ) : (
              <LayoutGrid size={13} />
            )}
            {layoutMode === "grid" ? "Focus" : "Grid"}
          </button>
        )}
        <button
          onClick={onOpenGraph}
          title="Agent team & activity graph (⌘⇧A)"
          className="no-drag flex items-center gap-2 rounded-full border border-ink-600 px-3.5 py-1.5 text-xs text-zinc-300 hover:bg-ink-600"
        >
          <Users size={13} />
          Team
          {runningAgents > 0 && (
            <span className="rounded-full bg-teal-400/20 px-1.5 text-[10px] font-semibold text-teal-300">
              {runningAgents}
            </span>
          )}
        </button>
        <BackendStatusPill state={backend} />
      </div>
    </header>
  );
}
