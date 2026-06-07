import type { BackendState } from "../backend";

const DOT: Record<BackendState["status"], string> = {
  healthy: "bg-emerald-400",
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
      className="no-drag flex h-[26px] min-w-0 items-center gap-2 rounded-full border border-ink-500 bg-ink-700 px-2.5 text-[11px]"
      title={state.detail}
    >
      <span className={`h-2 w-2 shrink-0 rounded-full ${DOT[state.status]}`} />
      {/* Truncation contract: status labels never wrap — truncate at min width. */}
      <span className="min-w-0 truncate whitespace-nowrap text-zinc-200">
        {LABEL[state.status]}
      </span>
      {state.external && (
        <span className="shrink-0 rounded bg-ink-500 px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-zinc-400">
          ext
        </span>
      )}
    </div>
  );
}
