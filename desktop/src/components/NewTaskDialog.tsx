import { useState } from "react";
import { X } from "lucide-react";
import { api } from "../api";
import { useStore } from "../store";
import { middleTruncate } from "../lib/format";

const FOCUS_RING =
  "focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-accent";

/**
 * Create a task in the active workspace (title required, description
 * optional), then navigate to it. Creation rides the existing daemon path
 * (api.createTask — the daemon mints the id); failure is loud and keeps the
 * dialog open.
 */
export function NewTaskDialog({ onClose }: { onClose: () => void }) {
  const workspaceDir = useStore((s) => s.workspaceDir);
  const connected = useStore((s) => s.connected);
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [busy, setBusy] = useState(false);

  const canCreate = !!workspaceDir && title.trim().length > 0 && !busy;

  const submit = async () => {
    const t = title.trim();
    if (!t || !workspaceDir || busy) return;
    setBusy(true);
    try {
      const task = await api.createTask(workspaceDir, t, description.trim());
      if (!task) {
        useStore.getState().showSnackbar({
          type: "error",
          message: "Task create failed — daemon unreachable",
        });
        return; // keep the dialog open; nothing was created
      }
      onClose();
      // Navigate to the new task (guard-routed — the context-switch gate
      // still applies if an agent has unreviewed work).
      useStore.getState().selectTask(task.id);
    } finally {
      setBusy(false);
    }
  };

  const handleKey = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onClose();
      return;
    }
    if (e.key !== "Enter") return;
    const tag = (e.target as HTMLElement).tagName;
    if (tag === "BUTTON") return; // native activation
    if (tag === "TEXTAREA" && !(e.metaKey || e.ctrlKey)) return; // newline
    e.preventDefault();
    void submit();
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
      onClick={onClose}
    >
      <div
        onKeyDown={handleKey}
        className="no-drag w-[28rem] rounded-xl border border-ink-500 bg-ink-800 shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center gap-3 border-b border-ink-600 px-5 py-3.5">
          <h2 className="text-sm font-semibold text-zinc-100">New task</h2>
          <span className="flex-1" />
          <button
            onClick={onClose}
            aria-label="Close"
            className={`rounded p-1 text-zinc-500 hover:text-zinc-200 ${FOCUS_RING}`}
          >
            <X size={16} />
          </button>
        </div>

        <div className="p-5">
          <p className="mb-4 text-[11px] leading-relaxed text-zinc-500">
            A task organizes agents and aggregates their review. It does not
            own worktrees or attribution — agents keep those.
          </p>

          <label className="mb-1.5 block text-[11px] font-medium uppercase tracking-wide text-zinc-500">
            Title
          </label>
          <input
            type="text"
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            placeholder="e.g. Rate-limit middleware"
            autoFocus
            disabled={busy}
            className={`mb-4 w-full rounded-lg border border-ink-500 bg-ink-700 px-3 py-2 text-sm text-zinc-200 placeholder:text-zinc-600 disabled:opacity-60 ${FOCUS_RING}`}
          />

          <label className="mb-1.5 block text-[11px] font-medium uppercase tracking-wide text-zinc-500">
            Description
          </label>
          <textarea
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            placeholder="What is the intent of this task?"
            rows={3}
            disabled={busy}
            className={`mb-4 w-full resize-y rounded-lg border border-ink-500 bg-ink-700 px-3 py-2 text-sm leading-relaxed text-zinc-200 placeholder:text-zinc-600 disabled:opacity-60 ${FOCUS_RING}`}
          />

          {workspaceDir ? (
            <p className="text-[11px] text-zinc-600">
              Workspace:{" "}
              <span className="font-mono text-zinc-500" title={workspaceDir}>
                {middleTruncate(workspaceDir, 44)}
              </span>
            </p>
          ) : (
            <p className="text-[11px] text-amber">
              Open a workspace to create tasks.
            </p>
          )}
          {workspaceDir && !connected && (
            <p className="mt-1 text-[11px] text-zinc-600">
              daemon unreachable · retrying
            </p>
          )}
        </div>

        <div className="flex items-center justify-end gap-2 border-t border-ink-600 px-5 py-3">
          <button
            onClick={onClose}
            className={`rounded-lg px-3 py-1.5 text-sm text-zinc-400 hover:text-zinc-200 ${FOCUS_RING}`}
          >
            Cancel
          </button>
          <button
            onClick={() => void submit()}
            disabled={!canCreate}
            className={`rounded-lg bg-primary px-3 py-1.5 text-sm font-medium text-white hover:bg-primary-hover disabled:opacity-50 ${FOCUS_RING}`}
          >
            {busy ? "Creating…" : "Create task"}
          </button>
        </div>
      </div>
    </div>
  );
}
