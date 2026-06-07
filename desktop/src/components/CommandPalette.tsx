import { useEffect, useMemo, useRef, useState } from "react";
import { useStore, type Section } from "../store";
import { providerTitle } from "../lib/providerLabel";
import { statusLabel, uiStatus } from "../lib/agentStatus";
import { useTasks } from "../hooks/useTasks";
import { pickDirectory } from "../lib/pickDirectory";
import { inTauri } from "../backend";

/**
 * Cmd+K command palette — the primary navigation/action surface. It is NOT a
 * status replacement: it indexes the store live (agents, tasks, workspaces)
 * and EVERY jump goes through the guard-routed store actions
 * (setActiveFrameGuarded / setSection / selectTask) so the dirty-state guard
 * always applies. Hand-rolled (no cmdk dep) against the store.
 *
 * Inventory: Workspaces (switch/add) · Sections · Tasks (open / open review)
 * · Agents (open / open console) · Actions (launch agent, new task,
 * new schedule).
 */

interface Cmd {
  id: string;
  group: string;
  label: string;
  /** Right-aligned muted context (status, dirty count, path). */
  hint?: string;
  /** Extra text folded into matching but not shown. */
  keywords?: string;
  run: () => void;
}

// Display order for the section headers.
const GROUP_ORDER = ["Workspaces", "Sections", "Tasks", "Agents", "Actions"];

const SECTIONS: { id: Section; label: string }[] = [
  { id: "dashboard", label: "Go to dashboard" },
  { id: "tasks", label: "Go to tasks" },
  { id: "agents", label: "Go to agents" },
  { id: "workflows", label: "Go to workflows" },
  { id: "schedules", label: "Go to schedules" },
  { id: "settings", label: "Go to settings" },
];

function basename(p: string): string {
  return p.replace(/\/+$/, "").split("/").pop() || p;
}

/** Every token of the query must appear (substring) in the haystack. */
function matches(query: string, hay: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  const h = hay.toLowerCase();
  return q.split(/\s+/).every((t) => h.includes(t));
}

/** Jump to an agent's terminal in the Agents section (guard-routed). */
function openAgentSession(ptySessionId: string): void {
  const s = useStore.getState();
  s.setSection("agents");
  const frame = s.frames.find((f) => f.ptySessionId === ptySessionId);
  if (frame) s.setActiveFrameGuarded(frame.key);
  else if (s.rustPtySessions[ptySessionId]?.status === "running")
    s.reopenRustPty(ptySessionId);
}

export function CommandPalette() {
  const open = useStore((s) => s.commandPaletteOpen);
  if (!open) return null;
  return <PaletteBody />;
}

function PaletteBody() {
  // Live store indexing — these subscriptions keep the list fresh while open.
  const rustPtySessions = useStore((s) => s.rustPtySessions);
  const terminalStatuses = useStore((s) => s.terminalStatuses);
  const dirty = useStore((s) => s.dirty);
  const workspaces = useStore((s) => s.workspaces);
  const workspaceDir = useStore((s) => s.workspaceDir);
  const setOpen = useStore((s) => s.setCommandPaletteOpen);
  const { tasks } = useTasks(workspaceDir);

  const [query, setQuery] = useState("");
  const [sel, setSel] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const commands = useMemo<Cmd[]>(() => {
    const close = () => setOpen(false);
    const out: Cmd[] = [];

    // ── Workspaces: switch / add ──────────────────────────────────────────
    for (const root of workspaces) {
      if (root === workspaceDir) continue;
      out.push({
        id: `ws:${root}`,
        group: "Workspaces",
        label: `Switch workspace: ${basename(root)}`,
        hint: root,
        keywords: `${root} project open`,
        run: () => {
          close();
          useStore.getState().switchWorkspace(root);
        },
      });
    }
    out.push({
      id: "ws:add",
      group: "Workspaces",
      label: "Add workspace…",
      keywords: "open folder project directory new",
      run: () => {
        close();
        void pickDirectory(workspaceDir ?? undefined).then((dir) => {
          const s = useStore.getState();
          if (dir) s.switchWorkspace(dir);
          // null = cancelled (Tauri) or no native dialog (dev browser) — only
          // the latter needs a pointer to the manual-path fallback.
          else if (!inTauri())
            s.showSnackbar({
              type: "info",
              message: "Folder picker unavailable — set the workspace in Settings",
            });
        });
      },
    });

    // ── Sections (guard-routed via setSection) ────────────────────────────
    for (const sec of SECTIONS) {
      out.push({
        id: `section:${sec.id}`,
        group: "Sections",
        label: sec.label,
        keywords: `section navigate ${sec.id}`,
        run: () => {
          close();
          useStore.getState().setSection(sec.id);
        },
      });
    }

    // ── Tasks: open / open review (active workspace, non-archived) ────────
    for (const t of tasks) {
      if (t.status === "archived") continue;
      const hint = `${String(t.status).replace(/_/g, " ")} · ${t.agent_count} agent${
        t.agent_count === 1 ? "" : "s"
      }`;
      out.push({
        id: `task:${t.id}`,
        group: "Tasks",
        label: `Open task: ${t.title}`,
        hint,
        keywords: `task ${t.id}`,
        run: () => {
          close();
          useStore.getState().selectTask(t.id);
        },
      });
      out.push({
        id: `task-review:${t.id}`,
        group: "Tasks",
        label: `Review task: ${t.title}`,
        hint,
        keywords: `task review diff merge aggregate ${t.id}`,
        run: () => {
          close();
          // Deep-link straight to the Review tab (one-shot taskInitialTab).
          useStore.getState().selectTask(t.id, "review");
        },
      });
    }

    // ── Agents: open / open console (blocked first — attention routes on it)
    const frames = useStore.getState().frames;
    const agents = Object.values(rustPtySessions).sort((a, b) => {
      const rank = (m: typeof a) => {
        if (m.status === "exited") return 2;
        return uiStatus(terminalStatuses[m.terminalId]) === "blocked" ? 0 : 1;
      };
      if (rank(a) !== rank(b)) return rank(a) - rank(b);
      return b.startedAt - a.startedAt;
    });
    for (const m of agents) {
      const hasFrame = frames.some((f) => f.ptySessionId === m.ptySessionId);
      const exited = m.status === "exited";
      if (exited && !hasFrame) continue; // nothing to open
      const raw = exited ? "EXITED" : terminalStatuses[m.terminalId];
      const d = dirty[m.terminalId]?.count ?? 0;
      const hint = [m.terminalId, statusLabel(raw), d > 0 ? `${d} dirty` : ""]
        .filter(Boolean)
        .join(" · ");
      out.push({
        id: `agent:${m.ptySessionId}`,
        group: "Agents",
        label: `Open agent: ${providerTitle(m.provider)}`,
        hint,
        keywords: `${m.provider} ${m.terminalId} ${m.branch ?? ""} terminal`,
        run: () => {
          close();
          openAgentSession(m.ptySessionId);
        },
      });
      if (!exited && m.terminalId) {
        out.push({
          id: `agent-console:${m.ptySessionId}`,
          group: "Agents",
          label: `Open console: ${providerTitle(m.provider)}`,
          hint: m.terminalId,
          keywords: `${m.provider} ${m.terminalId} console blocks turns stdin`,
          run: () => {
            close();
            const s = useStore.getState();
            // Console is a tab of the focused AgentDetail — persist the mode,
            // then focus the agent (the guard still gates the switch).
            s.setTermMode(m.terminalId, "console");
            s.setLayoutMode("focus");
            openAgentSession(m.ptySessionId);
          },
        });
      }
    }

    // ── Actions ───────────────────────────────────────────────────────────
    out.push({
      id: "action:launch",
      group: "Actions",
      label: "Launch agent…",
      keywords: "new start spawn claude codex gemini grok profile",
      run: () => {
        close();
        useStore.getState().setLaunchOpen(true);
      },
    });
    if (workspaceDir) {
      out.push({
        id: "action:new-task",
        group: "Actions",
        label: "New task",
        keywords: "create task intent group",
        run: () => {
          close();
          useStore.getState().setNewTaskOpen(true);
        },
      });
    }
    out.push({
      id: "action:new-schedule",
      group: "Actions",
      label: "New schedule",
      keywords: "create cron automate recurring schedule",
      run: () => {
        close();
        useStore.getState().setNewScheduleOpen(true);
      },
    });

    out.sort(
      (a, b) => GROUP_ORDER.indexOf(a.group) - GROUP_ORDER.indexOf(b.group),
    );
    return out;
  }, [rustPtySessions, terminalStatuses, dirty, workspaces, workspaceDir, tasks, setOpen]);

  const items = useMemo(
    () => commands.filter((c) => matches(query, `${c.label} ${c.keywords ?? ""}`)),
    [commands, query],
  );

  // Keep the selection in range as the filtered list changes — the cursor is
  // always on a real row (visible in every mode).
  const clampedSel = Math.min(sel, Math.max(0, items.length - 1));

  // Scroll the active row into view on keyboard movement.
  useEffect(() => {
    const el = listRef.current?.querySelector<HTMLElement>(
      `[data-idx="${clampedSel}"]`,
    );
    el?.scrollIntoView({ block: "nearest" });
  }, [clampedSel]);

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setSel((s) => Math.min(s + 1, items.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSel((s) => Math.max(s - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      items[clampedSel]?.run();
    } else if (e.key === "Escape") {
      e.preventDefault();
      setOpen(false);
    }
  };

  let lastGroup = "";

  return (
    <div
      className="fixed inset-0 z-50 flex items-start justify-center bg-black/40 pt-[12vh]"
      onMouseDown={() => setOpen(false)}
    >
      <div
        className="w-[min(640px,90vw)] overflow-hidden rounded-xl border border-ink-500 bg-ink-800 shadow-2xl"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <input
          ref={inputRef}
          value={query}
          onChange={(e) => {
            setQuery(e.target.value);
            setSel(0);
          }}
          onKeyDown={onKeyDown}
          placeholder="Jump to a task or agent, switch workspace, launch…"
          spellCheck={false}
          className="w-full border-b border-ink-600 bg-transparent px-4 py-3 text-sm text-zinc-100 placeholder:text-zinc-500 focus:border-accent/60 focus:outline-none"
        />
        <div ref={listRef} className="max-h-[50vh] overflow-y-auto py-1">
          {items.length === 0 && (
            <div className="px-4 py-6 text-center text-xs text-zinc-500">
              No matches
            </div>
          )}
          {items.map((c, i) => {
            const header = c.group !== lastGroup ? c.group : null;
            lastGroup = c.group;
            const selected = i === clampedSel;
            return (
              <div key={c.id}>
                {header && (
                  <div className="px-4 pb-1 pt-2 text-[10px] font-semibold uppercase tracking-wide text-zinc-600">
                    {header}
                  </div>
                )}
                <button
                  data-idx={i}
                  onMouseDown={(e) => {
                    e.preventDefault();
                    c.run();
                  }}
                  onMouseMove={() => setSel(i)}
                  className={`relative flex w-full items-center justify-between gap-3 px-4 py-1.5 text-left text-sm ${
                    selected ? "bg-ink-600 text-zinc-100" : "text-zinc-300"
                  }`}
                >
                  {selected && (
                    <span className="absolute bottom-1 left-0 top-1 w-[2px] rounded-r bg-accent" />
                  )}
                  <span className="min-w-0 truncate" title={c.label}>
                    {c.label}
                  </span>
                  {c.hint && (
                    <span
                      className="max-w-[45%] shrink-0 truncate font-mono text-[11px] text-zinc-500"
                      title={c.hint}
                    >
                      {c.hint}
                    </span>
                  )}
                </button>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
