import { useEffect } from "react";
import { AlertCircle, Eye, Hand, Inbox, Power, X } from "lucide-react";
import { useStore, unreadCount, type AppNotification } from "../store";
import { agentLabel } from "../lib/agentLabel";

const FOCUS_RING =
  "focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-accent";

/** Notification kind → icon + color (the attention grammar: blocked is the
 *  state everything routes on; review is the dirty-work signal). */
const KIND_UI = {
  blocked: { Icon: Hand, cls: "text-amber" },
  review: { Icon: Eye, cls: "text-accent" },
  error: { Icon: AlertCircle, cls: "text-red-400" },
  exited: { Icon: Power, cls: "text-zinc-500" },
} as const;

/** Compact relative time from epoch ms ("5m ago"). */
function ago(at: number): string {
  const s = Math.max(0, Math.round((Date.now() - at) / 1000));
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.round(s / 60)}m ago`;
  if (s < 86400) return `${Math.round(s / 3600)}h ago`;
  return `${Math.round(s / 86400)}d ago`;
}

function GroupLabel({ text, count }: { text: string; count?: number }) {
  return (
    <div className="flex items-center gap-1.5 px-2 pb-1 pt-2">
      <span className="text-[10px] font-semibold uppercase tracking-wider text-zinc-600">
        {text}
      </span>
      {count !== undefined && (
        <span className="tnum rounded-full bg-ink-600 px-1.5 text-[10px] text-zinc-500">
          {count}
        </span>
      )}
    </div>
  );
}

/**
 * The bell's drawer: attention items (blocked / review / error / exited)
 * pushed by the daemon, grouped Unread / Earlier, newest first. Clicking an
 * item marks it read and navigates — review items deep-link to the task's
 * review tab, everything else jumps to the agent. Navigation IS the action
 * (no inline approve/deny).
 */
export function NotificationsDrawer({ onClose }: { onClose: () => void }) {
  const notifications = useStore((s) => s.notifications);
  const unread = useStore((s) => unreadCount(s));
  const markRead = useStore((s) => s.markRead);
  const markAllRead = useStore((s) => s.markAllRead);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);

  const jump = (n: AppNotification) => {
    markRead(n.id);
    onClose();
    const s = useStore.getState();
    // Review items aggregate at the task — deep-link to its review tab.
    if (n.kind === "review" && n.taskId) {
      s.selectTask(n.taskId, "review");
      return;
    }
    const meta = Object.values(s.rustPtySessions).find(
      (m) => m.terminalId === n.agentId,
    );
    if (meta) {
      s.setSection("agents");
      const frame = s.frames.find((f) => f.ptySessionId === meta.ptySessionId);
      if (frame) s.setActiveFrameGuarded(frame.key);
      else if (meta.status === "running") s.reopenRustPty(meta.ptySessionId);
      return;
    }
    // Agent gone from the registry (forgotten) — fall back to its task.
    if (n.taskId) s.selectTask(n.taskId);
  };

  const newestFirst = [...notifications].reverse();
  const unreadItems = newestFirst.filter((n) => !n.read);
  const earlierItems = newestFirst.filter((n) => n.read);

  const row = (n: AppNotification) => {
    const { Icon, cls } = KIND_UI[n.kind];
    return (
      <button
        key={n.id}
        onClick={() => jump(n)}
        title={n.agentId}
        className={`flex w-full items-start gap-2.5 rounded-md px-2 py-2 text-left hover:bg-ink-600/60 ${
          n.read ? "opacity-60" : ""
        } ${FOCUS_RING}`}
      >
        <Icon size={14} className={`mt-0.5 shrink-0 ${cls}`} />
        <span className="flex min-w-0 flex-1 flex-col gap-0.5">
          <span className="flex items-center gap-1.5">
            <span
              className="min-w-0 flex-1 truncate text-xs text-zinc-200"
              title={n.text}
            >
              {n.text}
            </span>
            {!n.read && (
              <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-amber" />
            )}
          </span>
          <span className="flex items-center gap-1.5 text-[10px] text-zinc-600">
            <span className="truncate font-mono" title={n.agentId}>
              {agentLabel(n.agentId)}
            </span>
            <span className="tnum shrink-0">{ago(n.at)}</span>
          </span>
        </span>
      </button>
    );
  };

  return (
    <>
      {/* Click-catcher under the drawer (above the app, below the drawer).
          Both carry no-drag: the drawer mounts inside the titlebar-drag header
          and -webkit-app-region IS inherited — without it, clicks drag the
          window. */}
      <div className="no-drag fixed inset-0 z-30" onMouseDown={onClose} />
      <aside className="no-drag fixed bottom-0 right-0 top-10 z-40 flex w-[340px] flex-col border-l border-hairline bg-ink-700 shadow-2xl">
        <div className="flex h-9 shrink-0 items-center gap-2 border-b border-hairline px-3">
          <span className="text-[10px] font-semibold uppercase tracking-wider text-zinc-500">
            Notifications
          </span>
          {unread > 0 && (
            <span className="tnum rounded-full bg-amber/20 px-1.5 text-[10px] text-amber">
              {unread} new
            </span>
          )}
          <span className="flex-1" />
          <button
            onClick={markAllRead}
            disabled={unread === 0}
            className={`rounded px-1.5 py-0.5 text-[10px] text-zinc-500 hover:text-zinc-300 disabled:cursor-default disabled:opacity-40 ${FOCUS_RING}`}
          >
            Mark all read
          </button>
          <button
            onClick={onClose}
            aria-label="Close notifications"
            className={`rounded p-1 text-zinc-500 hover:bg-ink-600 hover:text-zinc-200 ${FOCUS_RING}`}
          >
            <X size={13} />
          </button>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto p-1.5">
          {newestFirst.length === 0 ? (
            <div className="flex flex-col items-center gap-2 py-10 text-center">
              <Inbox size={20} className="text-ink-400" />
              <p className="text-[11px] text-zinc-600">No notifications.</p>
            </div>
          ) : (
            <>
              {unreadItems.length > 0 && (
                <>
                  <GroupLabel text="Unread" count={unreadItems.length} />
                  {unreadItems.map(row)}
                </>
              )}
              {earlierItems.length > 0 && (
                <>
                  <GroupLabel text="Earlier" />
                  {earlierItems.map(row)}
                </>
              )}
            </>
          )}
        </div>
      </aside>
    </>
  );
}
