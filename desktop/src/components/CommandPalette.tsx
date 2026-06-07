import { useEffect, useMemo, useRef, useState } from "react";
import { useStore, type Frame } from "../store";
import { providerTitle } from "../lib/providerLabel";
import { statusLabel, uiStatus } from "../lib/agentStatus";

/**
 * Cmd+K command palette — the primary navigation/action surface. It is NOT a
 * status replacement: it reads store data directly (frames, terminalStatuses,
 * dirty) and EVERY frame jump goes through setActiveFrameGuarded so the
 * dirty-state guard always applies. Hand-rolled (no cmdk dep) against the store.
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
const GROUP_ORDER = [
  "Needs you",
  "Uncommitted changes",
  "Open frames",
  "Actions",
  "Switch project",
];

function frameLabel(f: Frame): string {
  const profile =
    f.agentProfile && f.agentProfile !== "default"
      ? ` · ${f.agentProfile.replace(/_/g, " ")}`
      : "";
  return `${providerTitle(f.provider)}${profile}`;
}

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

export function CommandPalette() {
  const open = useStore((s) => s.commandPaletteOpen);
  if (!open) return null;
  return <PaletteBody />;
}

function PaletteBody() {
  const frames = useStore((s) => s.frames);
  const terminalStatuses = useStore((s) => s.terminalStatuses);
  const dirty = useStore((s) => s.dirty);
  const recentProjects = useStore((s) => s.recentProjects);
  const workspaceDir = useStore((s) => s.workspaceDir);
  const activeFrameKey = useStore((s) => s.activeFrameKey);

  const setActiveFrameGuarded = useStore((s) => s.setActiveFrameGuarded);
  const openDiff = useStore((s) => s.openDiff);
  const setLaunchOpen = useStore((s) => s.setLaunchOpen);
  const setGraphOpen = useStore((s) => s.setGraphOpen);
  const setWorkspaceDir = useStore((s) => s.setWorkspaceDir);
  const setOpen = useStore((s) => s.setCommandPaletteOpen);

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

    for (const f of frames) {
      const raw = f.pending
        ? "PENDING"
        : f.terminalId
          ? terminalStatuses[f.terminalId]
          : undefined;
      const d = f.terminalId ? dirty[f.terminalId] : undefined;
      const isDirty = !!d && d.count > 0;
      const needsYou = uiStatus(raw) === "blocked";
      const group = needsYou
        ? "Needs you"
        : isDirty
          ? "Uncommitted changes"
          : "Open frames";
      const hint = [
        isDirty ? `${d!.count} dirty` : "",
        raw ? statusLabel(raw) : "",
      ]
        .filter(Boolean)
        .join(" · ");
      out.push({
        id: `frame:${f.key}`,
        group,
        label: frameLabel(f),
        hint: hint || undefined,
        keywords: `${f.provider} ${f.agentProfile ?? ""}`,
        run: () => {
          setActiveFrameGuarded(f.key);
          close();
        },
      });
    }

    // Review current changes — only when the active frame is dirty.
    const active = frames.find((f) => f.key === activeFrameKey);
    const activeDirty = active?.terminalId
      ? dirty[active.terminalId]
      : undefined;
    if (active?.terminalId && activeDirty && activeDirty.count > 0) {
      const tid = active.terminalId;
      out.push({
        id: "action:review",
        group: "Actions",
        label: "Review current changes",
        hint: `${activeDirty.count} files`,
        keywords: "diff review attribution merge revert",
        run: () => {
          openDiff(tid);
          setOpen(false);
        },
      });
    }
    out.push({
      id: "action:launch",
      group: "Actions",
      label: "Launch agent…",
      keywords: "new start spawn claude codex gemini grok",
      run: () => {
        setLaunchOpen(true);
        setOpen(false);
      },
    });
    out.push({
      id: "action:graph",
      group: "Actions",
      label: "View agent team / activity graph",
      hint: "⌘⇧A",
      keywords: "team orchestrator agents flow delegation graph activity who assigned",
      run: () => {
        setGraphOpen(true);
        setOpen(false);
      },
    });

    for (const p of recentProjects) {
      if (p === workspaceDir) continue;
      out.push({
        id: `project:${p}`,
        group: "Switch project",
        label: basename(p),
        hint: p,
        keywords: p,
        run: () => {
          setWorkspaceDir(p);
          setOpen(false);
        },
      });
    }

    out.sort(
      (a, b) => GROUP_ORDER.indexOf(a.group) - GROUP_ORDER.indexOf(b.group),
    );
    return out;
  }, [
    frames,
    terminalStatuses,
    dirty,
    recentProjects,
    workspaceDir,
    activeFrameKey,
    setActiveFrameGuarded,
    openDiff,
    setLaunchOpen,
    setWorkspaceDir,
    setOpen,
  ]);

  const items = useMemo(
    () => commands.filter((c) => matches(query, `${c.label} ${c.keywords ?? ""}`)),
    [commands, query],
  );

  // Keep the selection in range as the filtered list changes.
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
          placeholder="Jump to an agent, review changes, launch…"
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
                  className={`flex w-full items-center justify-between gap-3 px-4 py-1.5 text-left text-sm ${
                    selected ? "bg-ink-600 text-zinc-100" : "text-zinc-300"
                  }`}
                >
                  <span className="min-w-0 truncate">{c.label}</span>
                  {c.hint && (
                    <span className="shrink-0 truncate text-[11px] text-zinc-500">
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
