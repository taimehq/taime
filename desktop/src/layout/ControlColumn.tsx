import { useEffect, useState } from "react";
import { Plus, ChevronRight, ChevronDown, GitBranch, Power } from "lucide-react";
import { useStore } from "../store";
import { api, type SessionDetail } from "../api";
import { StatusBadge } from "../components/StatusBadge";
import { FileInventory } from "../components/FileInventory";
import { WorkspacePicker } from "../components/WorkspacePicker";
import { prettySession } from "../lib/sessionName";

export function ControlColumn({
  onLaunch,
}: {
  onLaunch: () => void;
}) {
  const sessions = useStore((s) => s.sessions);
  const connected = useStore((s) => s.connected);
  const launchClaudeRustPty = useStore((s) => s.launchClaudeRustPty);

  return (
    <aside className="flex w-80 shrink-0 flex-col border-r border-ink-600 bg-ink-800/40">
      <div className="flex flex-col gap-5 overflow-y-auto p-4">
        <WorkspacePicker />

        <div className="flex flex-col gap-2">
          <button
            onClick={onLaunch}
            disabled={!connected}
            className="no-drag flex items-center justify-center gap-2 rounded-lg bg-teal px-3 py-2 text-sm font-medium text-ink-900 transition-colors hover:bg-teal-400 disabled:cursor-not-allowed disabled:opacity-50"
          >
            <Plus size={16} />
            Launch agent
          </button>
          {import.meta.env.DEV && (
            // Temporary spike entry: Claude via the Rust-owned PTY transport.
            // CAO/tmux remains the default for all providers.
            <button
              onClick={() => launchClaudeRustPty()}
              className="no-drag flex items-center justify-center gap-2 rounded-lg border border-violet-500/40 px-3 py-1.5 text-xs text-violet-300 hover:bg-violet-500/10"
            >
              Claude · Rust PTY (dev)
            </button>
          )}
        </div>

        <PipelineSection sessions={sessions} />

        <DetachedAgentsSection />

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
    <Section title={`Detached agents (${detached.length})`}>
      <p className="-mt-1 mb-1 text-[10px] text-zinc-600">
        Running, frame closed. Reopen reattaches; kill terminates.
      </p>
      <div className="flex flex-col gap-1">
        {detached.map((m) => (
          <div
            key={m.ptySessionId}
            className="flex items-center gap-2 rounded-lg border border-violet-500/30 bg-violet-500/5 px-2.5 py-1.5"
          >
            <span className="flex min-w-0 flex-1 flex-col">
              <span className="truncate text-[12px] text-zinc-200">Claude Code</span>
              {m.branch && (
                <span className="flex items-center gap-1 truncate font-mono text-[9px] text-sky-300/80">
                  <GitBranch size={9} />
                  {m.branch}
                </span>
              )}
            </span>
            <button
              onClick={() => reopenRustPty(m.ptySessionId)}
              className="shrink-0 rounded border border-ink-500 px-2 py-0.5 text-[11px] text-zinc-200 hover:bg-ink-600"
            >
              Reopen
            </button>
            <button
              onClick={() => forgetRustPty(m.ptySessionId)}
              aria-label="Kill agent"
              className="shrink-0 rounded p-0.5 text-rose-400/80 hover:text-rose-300"
              title="Kill agent (terminate process)"
            >
              <Power size={13} />
            </button>
          </div>
        ))}
      </div>
    </Section>
  );
}

function PipelineSection({
  sessions,
}: {
  sessions: { name: string; status: string }[];
}) {
  return (
    <Section title={`Pipeline (${sessions.length})`}>
      {sessions.length === 0 ? (
        <p className="text-[11px] text-zinc-600">
          No sessions yet. Launch an agent to begin.
        </p>
      ) : (
        <div className="flex flex-col gap-1">
          {sessions.map((s) => (
            <SessionRow key={s.name} name={s.name} />
          ))}
        </div>
      )}
    </Section>
  );
}

function SessionRow({ name }: { name: string }) {
  const [open, setOpen] = useState(false);
  const [detail, setDetail] = useState<SessionDetail | null>(null);
  const openTerminalFrame = useStore((s) => s.openTerminalFrame);
  const statuses = useStore((s) => s.terminalStatuses);
  const frames = useStore((s) => s.frames);

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
    <div className="rounded-lg border border-ink-600 bg-ink-700/30">
      <button
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-1.5 px-2.5 py-2 text-left"
      >
        {open ? (
          <ChevronDown size={13} className="shrink-0 text-zinc-500" />
        ) : (
          <ChevronRight size={13} className="shrink-0 text-zinc-500" />
        )}
        <span className="truncate text-xs text-zinc-200" title={name}>
          {prettySession(name)}
        </span>
        {detail && detail.terminals.length > 0 && (
          <span className="ml-auto shrink-0 rounded-full bg-ink-600 px-1.5 text-[10px] text-zinc-400">
            {detail.terminals.length}
          </span>
        )}
      </button>
      {open && detail && (
        <div className="flex flex-col gap-0.5 border-t border-ink-600 px-2 py-1.5">
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
                className={`flex items-center justify-between rounded px-2 py-1.5 text-left hover:bg-ink-600/50 ${
                  isOpen ? "bg-ink-600/30" : ""
                }`}
              >
                <span className="flex min-w-0 items-center gap-2">
                  <span className="truncate text-[11px] text-zinc-300">
                    {t.provider.replace(/_/g, " ")}
                  </span>
                  <span className="truncate font-mono text-[10px] text-zinc-600">
                    {t.id.slice(0, 6)}
                  </span>
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
