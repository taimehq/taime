import { useEffect } from "react";
import { AlertCircle, Eye, Hand, Inbox, Power, X } from "lucide-react";
import { useStore, unreadCount, type AppNotification } from "../store";

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

/**
 * The bell's drawer: attention items (blocked / review / error / exited)
 * pushed by the daemon, newest first. Clicking an item marks it read and jumps
 * to the agent (guarded — the context-switch gate still applies).
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
    const s = useStore.getState();
    const meta = Object.values(s.rustPtySessions).find(
      (m) => m.terminalId === n.agentId,
    );
    if (!meta) return;
    s.setSection("agents");
    const frame = s.frames.find((f) => f.ptySessionId === meta.ptySessionId);
    if (frame) s.setActiveFrameGuarded(frame.key);
    else if (meta.status === "running") s.reopenRustPty(meta.ptySessionId);
    onClose();
  };

  const items = [...notifications].reverse();

  return (
    <>
      {/* Click-catcher under the drawer (above the app, below the drawer).
          Both carry no-drag: the drawer mounts inside the titlebar-drag header
          and -webkit-app-region IS inherited — without it, clicks drag the
          window. */}
      <div className="no-drag fixed inset-0 z-30" onMouseDown={onClose} />
      <aside className="no-drag fixed bottom-0 right-0 top-10 z-40 flex w-[340px] flex-col border-l border-ink-600 bg-ink-700 shadow-2xl">
        <div className="flex h-9 shrink-0 items-center gap-2 border-b border-ink-600 px-3">
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
            className="rounded px-1.5 py-0.5 text-[10px] text-zinc-500 hover:text-zinc-300 disabled:cursor-default disabled:opacity-40"
          >
            Mark all read
          </button>
          <button
            onClick={onClose}
            aria-label="Close notifications"
            className="rounded p-1 text-zinc-500 hover:bg-ink-600 hover:text-zinc-200"
          >
            <X size={13} />
          </button>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto p-1.5">
          {items.length === 0 ? (
            <div className="flex flex-col items-center gap-2 py-10 text-center">
              <Inbox size={20} className="text-ink-400" />
              <p className="text-[11px] text-zinc-600">No notifications.</p>
            </div>
          ) : (
            items.map((n) => {
              const { Icon, cls } = KIND_UI[n.kind];
              return (
                <button
                  key={n.id}
                  onClick={() => jump(n)}
                  title={n.agentId}
                  className={`flex w-full items-start gap-2.5 rounded-md px-2 py-2 text-left hover:bg-ink-600/60 ${
                    n.read ? "opacity-60" : ""
                  }`}
                >
                  <Icon size={14} className={`mt-0.5 shrink-0 ${cls}`} />
                  <span className="flex min-w-0 flex-1 flex-col gap-0.5">
                    <span className="flex items-center gap-1.5">
                      <span className="min-w-0 flex-1 truncate text-xs text-zinc-200">
                        {n.text}
                      </span>
                      {!n.read && (
                        <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-amber" />
                      )}
                    </span>
                    <span className="flex items-center gap-1.5 text-[10px] text-zinc-600">
                      <span className="truncate font-mono">{n.agentId}</span>
                      <span className="tnum shrink-0">{ago(n.at)}</span>
                    </span>
                  </span>
                </button>
              );
            })
          )}
        </div>
      </aside>
    </>
  );
}
