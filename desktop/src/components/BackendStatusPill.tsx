import type { BackendState } from "../backend";

const DOT: Record<BackendState["status"], string> = {
  healthy: "bg-teal-400",
  starting: "bg-amber animate-pulse",
  restarting: "bg-amber animate-pulse",
  external: "bg-amber animate-pulse",
  down: "bg-red-500",
  external_down: "bg-red-500",
};

const LABEL: Record<BackendState["status"], string> = {
  healthy: "Backend healthy",
  starting: "Backend starting",
  restarting: "Backend restarting",
  external: "External backend",
  down: "Backend down",
  external_down: "External backend down",
};

export function BackendStatusPill({ state }: { state: BackendState }) {
  return (
    <div
      className="no-drag flex items-center gap-2 rounded-full border border-ink-500 bg-ink-700 px-3 py-1.5 text-xs"
      title={state.detail}
    >
      <span className={`h-2 w-2 rounded-full ${DOT[state.status]}`} />
      <span className="text-zinc-200">{LABEL[state.status]}</span>
      {state.external && (
        <span className="rounded bg-ink-500 px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-zinc-400">
          ext
        </span>
      )}
    </div>
  );
}
