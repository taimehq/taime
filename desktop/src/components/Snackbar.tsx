import { useEffect } from "react";
import { CheckCircle2, AlertCircle, Info, X } from "lucide-react";
import { useStore } from "../store";

const ICON = {
  success: CheckCircle2,
  error: AlertCircle,
  info: Info,
};
const TONE = {
  success: "border-emerald-600/50 text-emerald-300",
  error: "border-red-900/60 text-red-300",
  info: "border-ink-500 text-zinc-300",
};

export function Snackbar() {
  const snackbar = useStore((s) => s.snackbar);
  const hide = useStore((s) => s.hideSnackbar);

  useEffect(() => {
    if (!snackbar) return;
    const t = setTimeout(hide, 4000);
    return () => clearTimeout(t);
  }, [snackbar, hide]);

  if (!snackbar) return null;
  const Icon = ICON[snackbar.type];

  return (
    <div className="pointer-events-none fixed bottom-4 left-1/2 z-50 -translate-x-1/2">
      <div
        className={`pointer-events-auto flex items-center gap-2.5 rounded-lg border bg-ink-800 px-4 py-2.5 text-sm shadow-xl ${TONE[snackbar.type]}`}
      >
        <Icon size={16} />
        <span>{snackbar.message}</span>
        <button
          onClick={hide}
          className="ml-2 text-zinc-500 hover:text-zinc-200"
        >
          <X size={14} />
        </button>
      </div>
    </div>
  );
}
