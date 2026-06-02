import { useEffect, useState } from "react";
import { Plus, ChevronRight, ChevronDown, GitBranch, Power, Trash2 } from "lucide-react";
import { useStore } from "../store";
import { api, type SessionDetail } from "../api";
import { StatusBadge } from "../components/StatusBadge";
import { FileInventory } from "../components/FileInventory";
import { WorkspacePicker } from "../components/WorkspacePicker";
import { prettySession } from "../lib/sessionName";

export function ControlColumn({ onLaunch }: { onLaunch: () => void }) {
  const sessions = useStore((s) => s.sessions);
  const connected = useStore((s) => s.connected);
  const launchClaudeRustPty = useStore((s) => s.launchClaudeRustPty);

  return (
    <aside className="flex w-80 shrink-0 flex-col border-r border-ink-600 bg-ink-800/40">
      {/* Zone 1 — context + primary actions (pinned) */}
      <div className="flex shrink-0 flex-col gap-3 p-4 pb-3">
        <WorkspacePicker />
        <div className="flex flex-col gap-2">
          <button
            onClick={onLaunch}
            disabled={!connected}
            className="no-drag flex items-center justify-center gap-2 rounded-lg bg-primary px-3 py-2 text-sm font-medium text-ink-900 transition-colors hover:bg-primary-hover disabled:cursor-not-allowed disabled:opacity-50"
          >
            <Plus size={16} />
            Launch agent
          </button>
          {import.meta.env.DEV && (
            // Temporary spike entry: Claude via the Rust-owned PTY transport.
            // CAO/tmux remains the default for all providers.
            <button
              onClick={() => launchClaudeRustPty()}
              className="no-drag flex items-center justify-center gap-2 rounded-lg border border-ink-500 px-3 py-1.5 text-xs text-zinc-400 hover:bg-ink-700"
            >
              Claude · Rust PTY (dev)
            </button>
          )}
        </div>
      </div>

      {/* Zone 2 — sessions (the only scrolling zone) */}
      <div className="min-h-0 flex-1 overflow-y-auto border-t border-ink-700 px-3 py-3">
        <div className="flex flex-col gap-5">
          <SessionsSection sessions={sessions} />
          <DetachedAgentsSection />
        </div>
      </div>

      {/* Zone 3 — changes / review (pinned bottom, capped) */}
      <div className="max-h-[38%] shrink-0 overflow-y-auto border-t border-ink-700 p-3">
        <FileInventory />
      </div>
    </aside>
  );
}

/**
 * Rust-PTY agents whose frame was closed but are still RUNNING (close ≠ kill).
 * Lists them with reopen (reattach to the live process) + kill (terminate).
 */
function DetachedAgentsSection() {
  const rustPtySessions = useStore((s) => s.rustPtySessions);
  const frames = useStore((s) => s.frames);
  const reopenRustPty = useStore((s) => s.reopenRustPty);
  const forgetRustPty = useStore((s) => s.forgetRustPty);

  const framed = new Set(frames.map((f) => f.ptySessionId).filter(Boolean));
  const detached = Object.values(rustPtySessions).filter(
    (m) => !framed.has(m.ptySessionId),
  );
  if (detached.length === 0) return null;

  return (
    <Section title={`Detached agents · ${detached.length}`}>
      <p className="-mt-1 mb-1 text-[11px] text-zinc-500">
        Frame closed, process kept alive. Reopen reattaches; kill terminates.
      </p>
      <div className="flex flex-col gap-1">
        {detached.map((m) => {
          const exited = m.status === "exited";
          return (
            <div
              key={m.ptySessionId}
              className={`flex items-center gap-2 rounded-lg border px-2.5 py-1.5 ${
                exited
                  ? "border-ink-600 bg-ink-800/40"
                  : "border-teal-600/30 bg-teal-600/5"
              }`}
            >
              <span className="flex min-w-0 flex-1 flex-col">
                <span className="flex items-center gap-1.5 truncate text-xs text-zinc-200">
                  Claude Code
                  <span
                    className={`rounded px-1 text-[10px] font-semibold uppercase tracking-wide ${
                      exited
                        ? "bg-ink-600 text-zinc-400"
                        : "bg-teal-600/20 text-teal-400"
                    }`}
                  >
                    {exited ? "exited" : "running"}
                  </span>
                </span>
                {m.branch && (
                  <span className="flex items-center gap-1 truncate font-mono text-[11px] text-zinc-500">
                    <GitBranch size={11} />
                    {m.branch}
                  </span>
                )}
              </span>
              {exited ? (
                <button
                  onClick={() => forgetRustPty(m.ptySessionId)}
                  className="shrink-0 rounded border border-ink-500 px-2 py-0.5 text-[11px] text-zinc-400 hover:bg-ink-600"
                >
                  Dismiss
                </button>
              ) : (
                <>
                  <button
                    onClick={() => reopenRustPty(m.ptySessionId)}
                    className="shrink-0 rounded border border-ink-500 px-2 py-0.5 text-[11px] text-zinc-200 hover:bg-ink-600"
                  >
                    Reopen
                  </button>
                  <button
                    onClick={() => forgetRustPty(m.ptySessionId)}
                    aria-label="Kill agent"
                    className="shrink-0 rounded p-0.5 text-red-400/80 hover:text-red-300"
                    title="Kill agent (terminate process)"
                  >
                    <Power size={13} />
                  </button>
                </>
              )}
            </div>
          );
        })}
      </div>
    </Section>
  );
}

interface SessionGrouping {
  label: string;
  mono: boolean;
  members: { name: string; tag: string | null }[];
}

/** Bucket sessions by their readable base name so near-identical sessions
 *  (e.g. 12× "kdx-investigate-XXXX") collapse into one group. */
function groupSessions(sessions: { name: string }[]): SessionGrouping[] {
  const map = new Map<string, SessionGrouping>();
  for (const s of sessions) {
    const { label, tag, mono } = prettySession(s.name);
    const g = map.get(label) ?? { label, mono, members: [] };
    g.members.push({ name: s.name, tag });
    map.set(label, g);
  }
  return [...map.values()];
}

function SessionsSection({
  sessions,
}: {
  sessions: { name: string; status: string }[];
}) {
  const groups = groupSessions(sessions);
  return (
    <Section title={`Sessions · ${sessions.length}`}>
      {sessions.length === 0 ? (
        <p className="text-[11px] text-zinc-600">
          No sessions yet. Launch an agent to begin.
        </p>
      ) : (
        <div className="flex flex-col gap-0.5">
          {groups.map((g) =>
            g.members.length === 1 ? (
              <SessionRow key={g.members[0].name} name={g.members[0].name} />
            ) : (
              <SessionGroup key={g.label} group={g} />
            ),
          )}
        </div>
      )}
    </Section>
  );
}

/** A collapsed cluster of sessions sharing a base name; expands to its members. */
function SessionGroup({ group }: { group: SessionGrouping }) {
  const [open, setOpen] = useState(false);
  return (
    <div>
      <button
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-1.5 rounded px-2 py-1.5 text-left hover:bg-ink-700/50"
      >
        {open ? (
          <ChevronDown size={13} className="shrink-0 text-zinc-600" />
        ) : (
          <ChevronRight size={13} className="shrink-0 text-zinc-600" />
        )}
        <span
          className={`truncate text-xs ${group.mono ? "font-mono text-zinc-400" : "text-zinc-200"}`}
        >
          {group.label}
        </span>
        <span className="ml-auto shrink-0 rounded-full bg-ink-600 px-1.5 text-[10px] text-zinc-500">
          ×{group.members.length}
        </span>
      </button>
      {open && (
        <div className="flex flex-col gap-0.5 pl-3">
          {group.members.map((m) => (
            <SessionRow key={m.name} name={m.name} label={m.tag ?? undefined} />
          ))}
        </div>
      )}
    </div>
  );
}

function SessionRow({ name, label: labelOverride }: { name: string; label?: string }) {
  const [open, setOpen] = useState(false);
  const [detail, setDetail] = useState<SessionDetail | null>(null);
  const openTerminalFrame = useStore((s) => s.openTerminalFrame);
  const statuses = useStore((s) => s.terminalStatuses);
  const frames = useStore((s) => s.frames);
  const killSession = useStore((s) => s.killSession);

  const pretty = prettySession(name);
  const label = labelOverride ?? pretty.label;
  // When shown as a group member the label is the disambiguating tag → mono.
  const mono = labelOverride !== undefined ? true : pretty.mono;
  const tag = labelOverride !== undefined ? null : pretty.tag;

  const onKill = (e: React.MouseEvent) => {
    e.stopPropagation();
    const shown = pretty.tag ? `${pretty.label}-${pretty.tag}` : pretty.label;
    if (
      window.confirm(
        `Delete session "${shown}"?\n\nThis terminates its agent(s) and removes the tmux session. This can't be undone.`,
      )
    ) {
      killSession(name);
    }
  };

  useEffect(() => {
    if (!open) return;
    let alive = true;
    const load = () =>
      api
        .getSession(name)
        .then((d) => alive && setDetail(d))
        .catch(() => {});
    load();
    const t = setInterval(load, 4000);
    return () => {
      alive = false;
      clearInterval(t);
    };
  }, [open, name]);

  return (
    <div>
      <div className="group/row flex items-center rounded hover:bg-ink-700/50">
        <button
          onClick={() => setOpen((v) => !v)}
          className="flex min-w-0 flex-1 items-center gap-1.5 px-2 py-1.5 text-left"
        >
          {open ? (
            <ChevronDown size={13} className="shrink-0 text-zinc-600" />
          ) : (
            <ChevronRight size={13} className="shrink-0 text-zinc-600" />
          )}
          <span
            className={`truncate text-xs ${mono ? "font-mono text-zinc-400" : "text-zinc-200"}`}
            title={name}
          >
            {label}
          </span>
          {tag && (
            <span className="shrink-0 font-mono text-[10px] text-zinc-600">
              {tag}
            </span>
          )}
        </button>
        <button
          onClick={onKill}
          aria-label="Delete session"
          title="Delete session (terminates its agents)"
          className="mr-1 shrink-0 rounded p-1 text-zinc-600 opacity-0 transition-opacity hover:text-red-300 group-hover/row:opacity-100"
        >
          <Trash2 size={12} />
        </button>
      </div>
      {open && detail && (
        <div className="flex flex-col gap-0.5 pb-1 pl-[26px] pr-1">
          {detail.terminals.length === 0 && (
            <span className="px-1 py-1 text-[11px] text-zinc-600">
              no terminals
            </span>
          )}
          {detail.terminals.map((t) => {
            const isOpen = frames.some((f) => f.terminalId === t.id);
            return (
              <button
                key={t.id}
                onClick={() =>
                  openTerminalFrame({
                    terminalId: t.id,
                    provider: t.provider,
                    agentProfile: t.agent_profile,
                    sessionName: t.tmux_session,
                  })
                }
                className={`flex items-center justify-between rounded px-2 py-1 text-left hover:bg-ink-700/50 ${
                  isOpen ? "bg-ink-700/40" : ""
                }`}
              >
                <span className="truncate text-[11px] text-zinc-300">
                  {t.provider.replace(/_/g, " ")}
                </span>
                <StatusBadge status={statuses[t.id]} />
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}

function Section({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex flex-col gap-2">
      <h2 className="text-[11px] font-semibold uppercase tracking-wider text-zinc-500">
        {title}
      </h2>
      {children}
    </div>
  );
}
