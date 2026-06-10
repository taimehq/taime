import { inTauri, type BackendState } from "../backend";
import { useStore } from "../store";
import { storeHealthBanner } from "../lib/storeHealthBanner";

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
  const storeHealth = useStore((s) => s.storeHealth);
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

  // Degraded persistence (the corrupt-store finding): the daemon is up, but the
  // durable store is gone or was recreated after corruption — attribution is
  // not being recorded the way the user thinks. The daemon can't fix this
  // mid-run (the store opens once at boot), so this is honest signage, not an
  // action button.
  const health = inTauri() && connected ? storeHealthBanner(storeHealth) : null;
  if (health) {
    return (
      <div
        title={health.title}
        className={`no-drag flex h-[26px] min-w-0 items-center gap-2 rounded-full border px-2.5 text-[11px] ${
          health.off
            ? "border-red-500/60 bg-red-950/40 text-red-200"
            : "border-amber/60 bg-ink-700 text-zinc-200"
        }`}
      >
        <span
          className={`h-2 w-2 shrink-0 rounded-full ${health.off ? "bg-red-500" : "bg-amber"}`}
        />
        <span className="min-w-0 truncate whitespace-nowrap">{health.label}</span>
      </div>
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
