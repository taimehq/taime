import { useEffect, useRef, useState } from "react";
import {
  Bell,
  Check,
  ChevronsUpDown,
  FolderGit2,
  Plus,
  Search,
} from "lucide-react";
import { useStore, unreadCount } from "../store";
import { api } from "../api";
import { inTauri, type BackendState } from "../backend";
import { BackendStatusPill } from "../components/BackendStatusPill";
import { StatusBadge } from "../components/StatusBadge";
import { providerTitle } from "../lib/providerLabel";
import { basename, dirname } from "../lib/recentProjects";
import { pickDirectory } from "../lib/pickDirectory";
import { useFullscreen } from "../hooks/useFullscreen";
import { useTasks } from "../hooks/useTasks";
import { NotificationsDrawer } from "./NotificationsDrawer";

/**
 * The 40px unified macOS title bar: [traffic-light reserve + workspace
 * switcher | breadcrumb | status · clock · search · bell] on a true 3-column
 * grid so the breadcrumb stays window-centered.
 *
 * Drag discipline: `data-tauri-drag-region` does NOT inherit — it sits on the
 * bar AND on every non-interactive child; interactive children carry `no-drag`
 * (the -webkit-app-region escape) and no drag attribute.
 *
 * `trafficLightPosition` in tauri.conf.json is y:20 to center the native
 * lights in this h-10 bar — verify visually on notched + non-notched displays.
 */
export function TitleBar({ backend }: { backend: BackendState }) {
  const fullscreen = useFullscreen();
  const setCommandPaletteOpen = useStore((s) => s.setCommandPaletteOpen);
  const unread = useStore((s) => unreadCount(s));
  const [notifOpen, setNotifOpen] = useState(false);

  // Selected agent (active frame) → wire status for the right-cluster badge.
  const activeFrame = useStore((s) =>
    s.frames.find((f) => f.key === s.activeFrameKey),
  );
  const rawStatus = useStore((s) =>
    activeFrame
      ? activeFrame.pending
        ? "PENDING"
        : activeFrame.terminalId
          ? s.terminalStatuses[activeFrame.terminalId]
          : undefined
      : undefined,
  );

  return (
    <header
      data-tauri-drag-region
      className="titlebar-drag relative z-30 grid h-10 shrink-0 grid-cols-[1fr_auto_1fr] items-center gap-3 border-b border-ink-600 bg-ink-800 pr-3.5"
    >
      {/* Left: traffic-light reserve (collapses with the lights in fullscreen) + workspace */}
      <div
        data-tauri-drag-region
        className="flex min-w-0 items-center justify-self-start"
      >
        <div
          data-tauri-drag-region
          aria-hidden
          className={`shrink-0 ${fullscreen ? "w-3" : "w-[88px]"}`}
        />
        <WorkspaceSwitcher />
      </div>

      {/* Center: breadcrumb — fixed center, the task crumb truncates first */}
      <Breadcrumb />

      {/* Right: daemon pill · selected-agent status · clock · search · bell */}
      <div
        data-tauri-drag-region
        className="flex min-w-0 items-center gap-2 justify-self-end"
      >
        <BackendStatusPill state={backend} />
        {activeFrame && <StatusBadge status={rawStatus} />}
        <Clock />
        <button
          onClick={() => setCommandPaletteOpen(true)}
          title="Command palette (⌘K)"
          className="no-drag flex h-[26px] items-center gap-1.5 rounded-md border border-ink-500 bg-ink-700 px-2 text-[11px] text-zinc-500 hover:bg-ink-600 hover:text-zinc-300"
        >
          <Search size={12} />
          Search
          <kbd className="rounded border border-ink-500 bg-ink-600 px-1 font-mono text-[9px] text-zinc-500">
            ⌘K
          </kbd>
        </button>
        <button
          onClick={() => setNotifOpen((v) => !v)}
          title="Notifications"
          aria-label="Notifications"
          className="no-drag relative rounded-md p-1.5 text-zinc-500 hover:bg-ink-600 hover:text-zinc-200"
        >
          <Bell size={14} />
          {unread > 0 && (
            <span className="absolute right-1 top-1 h-1.5 w-1.5 rounded-full bg-amber ring-2 ring-ink-800" />
          )}
        </button>
      </div>

      {notifOpen && <NotificationsDrawer onClose={() => setNotifOpen(false)} />}
    </header>
  );
}

const ADD_WORKSPACE = "__add__";

/**
 * The workspace switcher chip + dropdown. Opens on click or ⌘O (store-owned
 * flag so the global dispatcher reaches it); closes on outside-mousedown, on
 * blur, and on Escape. Full keyboard nav: arrows move, Enter activates.
 */
function WorkspaceSwitcher() {
  const open = useStore((s) => s.wsSwitcherOpen);
  const setOpen = useStore((s) => s.setWsSwitcherOpen);
  const workspaceDir = useStore((s) => s.workspaceDir);
  const workspaces = useStore((s) => s.workspaces);
  const switchWorkspace = useStore((s) => s.switchWorkspace);

  const rootRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const [cursor, setCursor] = useState(0);
  const [counts, setCounts] = useState<
    Record<string, { tasks: number; agents: number }>
  >({});

  // The navigable items: every known workspace, then "Add workspace…".
  const items = [...workspaces, ADD_WORKSPACE];

  // Outside-mousedown closes (in addition to blur + Escape).
  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, [open, setOpen]);

  // On open: focus the menu (so blur/keys work even when opened via ⌘O), seed
  // the cursor on the active workspace, and fetch per-root task/agent counts.
  useEffect(() => {
    if (!open) return;
    menuRef.current?.focus();
    const s = useStore.getState();
    setCursor(Math.max(0, s.workspaces.indexOf(s.workspaceDir ?? "")));
    let alive = true;
    Promise.all(
      s.workspaces.map(async (root) => {
        try {
          const tasks = await api.listTasks(root, false);
          return [
            root,
            {
              tasks: tasks.length,
              agents: tasks.reduce((n, t) => n + t.agent_count, 0),
            },
          ] as const;
        } catch {
          return [root, null] as const;
        }
      }),
    ).then((pairs) => {
      if (!alive) return;
      setCounts(
        Object.fromEntries(pairs.filter(([, c]) => c !== null)) as Record<
          string,
          { tasks: number; agents: number }
        >,
      );
    });
    return () => {
      alive = false;
    };
  }, [open]);

  const close = (refocus = true) => {
    setOpen(false);
    if (refocus) triggerRef.current?.focus();
  };

  const activate = (idx: number) => {
    const item = items[idx];
    if (!item) return;
    close();
    if (item === ADD_WORKSPACE) {
      void pickDirectory(workspaceDir ?? undefined).then((dir) => {
        if (dir) switchWorkspace(dir);
        // null = cancelled (Tauri) or no native dialog (dev browser) — only
        // the latter needs a pointer to the manual-path fallback.
        else if (!inTauri())
          useStore.getState().showSnackbar({
            type: "info",
            message: "Folder picker unavailable — set the workspace in Settings",
          });
      });
    } else if (item !== workspaceDir) {
      switchWorkspace(item);
    }
  };

  const onMenuKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setCursor((c) => Math.min(c + 1, items.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setCursor((c) => Math.max(c - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      activate(cursor);
    } else if (e.key === "Escape") {
      e.preventDefault();
      close();
    }
  };

  return (
    <div ref={rootRef} className="relative min-w-0">
      <button
        ref={triggerRef}
        onClick={() => setOpen(!open)}
        title={workspaceDir ?? "No workspace open"}
        aria-haspopup="listbox"
        aria-expanded={open}
        className={`no-drag flex max-w-[220px] items-center gap-1.5 rounded-md border px-2 py-1 text-xs ${
          open
            ? "border-accent bg-ink-600"
            : "border-ink-500 bg-ink-700 hover:border-ink-400 hover:bg-ink-600"
        }`}
      >
        <FolderGit2 size={12} className="shrink-0 text-zinc-500" />
        <span className="min-w-0 truncate font-medium text-zinc-100">
          {workspaceDir ? basename(workspaceDir) : "Choose workspace"}
        </span>
        <ChevronsUpDown size={11} className="shrink-0 text-zinc-600" />
      </button>

      {open && (
        <div
          ref={menuRef}
          tabIndex={-1}
          role="listbox"
          aria-label="Workspaces"
          onKeyDown={onMenuKeyDown}
          onBlur={(e) => {
            if (!rootRef.current?.contains(e.relatedTarget as Node)) {
              close(false);
            }
          }}
          className="no-drag absolute left-0 top-[calc(100%+6px)] z-50 w-[280px] rounded-lg border border-ink-500 bg-ink-700 p-1.5 shadow-2xl focus:outline-none"
        >
          <div className="px-2 pb-1 pt-1.5 text-[10px] font-semibold uppercase tracking-wider text-zinc-600">
            Workspaces
          </div>
          {workspaces.length === 0 && (
            <p className="px-2 py-1.5 text-[11px] text-zinc-600">
              No workspaces yet.
            </p>
          )}
          {workspaces.map((root, i) => {
            const active = root === workspaceDir;
            const c = counts[root];
            return (
              <button
                key={root}
                role="option"
                aria-selected={active}
                tabIndex={-1}
                onClick={() => activate(i)}
                onMouseEnter={() => setCursor(i)}
                title={root}
                className={`flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left ${
                  i === cursor ? "bg-ink-500" : ""
                }`}
              >
                <FolderGit2
                  size={13}
                  className={`shrink-0 ${active ? "text-accent" : "text-zinc-600"}`}
                />
                <span className="flex min-w-0 flex-1 flex-col">
                  <span className="truncate font-mono text-xs text-zinc-100">
                    {basename(root)}
                  </span>
                  <span className="truncate text-[10px] text-zinc-600">
                    {c ? `${c.tasks} tasks · ${c.agents} agents` : dirname(root)}
                  </span>
                </span>
                {active && <Check size={13} className="shrink-0 text-accent" />}
              </button>
            );
          })}
          <div className="mx-1 my-1 h-px bg-ink-500" />
          <button
            role="option"
            aria-selected={false}
            tabIndex={-1}
            onClick={() => activate(workspaces.length)}
            onMouseEnter={() => setCursor(workspaces.length)}
            className={`flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs text-zinc-400 ${
              cursor === workspaces.length ? "bg-ink-500" : ""
            }`}
          >
            <Plus size={13} className="shrink-0 text-zinc-600" />
            Add workspace…
          </button>
          <div className="flex items-center gap-1.5 px-2 pb-0.5 pt-1.5 text-[10px] text-zinc-600">
            <kbd className="rounded border border-ink-500 bg-ink-600 px-1 font-mono text-[9px] text-zinc-500">
              ⌘O
            </kbd>
            workspace switcher
          </div>
        </div>
      )}
    </div>
  );
}

/**
 * workspace › Task: <title> › <agent label>. Sections without a task/agent
 * selection collapse to their section label. The middle (task) crumb is the
 * flexible one — it truncates before its neighbors.
 */
function Breadcrumb() {
  const section = useStore((s) => s.section);
  const workspaceDir = useStore((s) => s.workspaceDir);
  const selectedTaskId = useStore((s) => s.selectedTaskId);
  const selectedWorkflow = useStore((s) => s.selectedWorkflow);
  const selectedSchedule = useStore((s) => s.selectedSchedule);
  const activeFrame = useStore((s) =>
    s.frames.find((f) => f.key === s.activeFrameKey),
  );
  const { tasks } = useTasks(workspaceDir);

  const taskTitle = (id: string | null | undefined): string | null => {
    if (!id) return null;
    return tasks.find((t) => t.id === id)?.title ?? id;
  };

  // [workspace?, middle (task — truncates first)?, leaf]
  let middle: string | null = null;
  let leaf: string;
  if (section === "agents") {
    if (activeFrame) {
      const mt = taskTitle(activeFrame.taskId);
      middle = mt ? `Task: ${mt}` : null;
      leaf = `${providerTitle(activeFrame.provider)}${
        activeFrame.terminalId ? ` · ${activeFrame.terminalId}` : ""
      }`;
    } else {
      leaf = "Agents";
    }
  } else if (section === "tasks") {
    leaf = selectedTaskId ? `Task: ${taskTitle(selectedTaskId)}` : "Tasks";
  } else if (section === "workflows") {
    leaf = selectedWorkflow ? `Workflow: ${selectedWorkflow}` : "Workflows";
  } else if (section === "schedules") {
    leaf = selectedSchedule ? `Schedule: ${selectedSchedule}` : "Schedules";
  } else if (section === "settings") {
    leaf = "Settings";
  } else {
    leaf = "Dashboard";
  }

  const sep = (
    <span data-tauri-drag-region className="shrink-0 text-[10px] text-zinc-700">
      ›
    </span>
  );

  return (
    <div
      data-tauri-drag-region
      className="flex min-w-0 max-w-[420px] items-center gap-1.5 justify-self-center overflow-hidden text-xs text-zinc-500"
    >
      {workspaceDir && (
        <>
          <span
            data-tauri-drag-region
            title={workspaceDir}
            className="max-w-[160px] shrink-0 truncate"
          >
            {basename(workspaceDir)}
          </span>
          {sep}
        </>
      )}
      {middle && (
        <>
          <span
            data-tauri-drag-region
            title={middle}
            className="min-w-0 truncate"
          >
            {middle}
          </span>
          {sep}
        </>
      )}
      <span
        data-tauri-drag-region
        title={leaf}
        className="max-w-[220px] shrink-0 truncate font-medium text-zinc-100"
      >
        {leaf}
      </span>
    </div>
  );
}

/** Live HH:MM:SS wall clock — mono + tabular-nums + fixed slot (no jitter). */
function Clock() {
  const [now, setNow] = useState(() => fmtClock());
  useEffect(() => {
    const t = setInterval(() => setNow(fmtClock()), 1000);
    return () => clearInterval(t);
  }, []);
  return (
    <span
      data-tauri-drag-region
      className="tnum w-[60px] shrink-0 text-right font-mono text-[11px] text-zinc-600"
    >
      {now}
    </span>
  );
}

function fmtClock(): string {
  return new Date().toLocaleTimeString("en-US", { hour12: false });
}
