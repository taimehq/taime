import { inTauri, type BackendState } from "../backend";
import { useStore } from "../store";

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
  const connected = useStore((s) => s.connected);
  const daemonIncompatible = useStore((s) => s.daemonIncompatible);
  const restartDaemon = useStore((s) => s.restartDaemon);

  // Review M2: a poll found an incompatible/unresponsive daemon and DID NOT
  // replace it (that would kill live agents). Offer an explicit, consent-gated
  // restart instead of silently degrading.
  if (inTauri() && daemonIncompatible) {
    return (
      <button
        type="button"
        onClick={() => {
          if (
            window.confirm(
              "The backend is incompatible (likely an app upgrade). Restart it? This stops any running agents.",
            )
          ) {
            void restartDaemon();
          }
        }}
        title="An incompatible daemon is running. Restart it (stops running agents)."
        className="no-drag flex h-[26px] min-w-0 items-center gap-2 rounded-full border border-red-500/60 bg-red-950/40 px-2.5 text-[11px] text-red-200 hover:bg-red-900/50"
      >
        <span className="h-2 w-2 shrink-0 rounded-full bg-red-500" />
        <span className="min-w-0 truncate whitespace-nowrap">Backend incompatible — Restart</span>
      </button>
    );
  }

  // In Tauri the supervisor descriptor is static ("healthy" by construction —
  // backend.ts) and proves nothing about the daemon. `connected` is the live
  // daemon_ping result, so the pill reports the daemon honestly. Outside Tauri
  // App already synthesizes external/external_down from the same fact.
  const status: BackendState["status"] =
    inTauri() && state.status === "healthy" && !connected ? "down" : state.status;
  return (
    <div
      className="no-drag flex h-[26px] min-w-0 items-center gap-2 rounded-full border border-ink-500 bg-ink-700 px-2.5 text-[11px]"
      title={state.detail}
    >
      <span className={`h-2 w-2 shrink-0 rounded-full ${DOT[status]}`} />
      {/* Truncation contract: status labels never wrap — truncate at min width. */}
      <span className="min-w-0 truncate whitespace-nowrap text-zinc-200">
        {LABEL[status]}
      </span>
      {state.external && (
        <span className="shrink-0 rounded bg-ink-500 px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-zinc-400">
          ext
        </span>
      )}
    </div>
  );
}
