import type { RustPtyMeta } from "../store";

/**
 * The **daemon-owned terminal registry adapter** (CAO-replacement Phase 2).
 *
 * A `terminalId` is dual-purpose: for a CAO terminal it points at a real tmux
 * pane; for a daemon agent it's the provisioned worktree id used purely as the
 * attribution key (no tmux binding). The app keeps daemon agents in
 * `rustPtySessions` keyed by the daemon session id, each carrying the worktree
 * `terminalId` + its `cwd`/`provider`.
 *
 * This module is the single place that answers "is this terminal daemon-owned?"
 * and resolves daemon-backed metadata locally, so terminal-level inspection
 * (working directory now; status in Phase 4; diff/graph/attribution in Phase 6)
 * routes by ownership and a daemon terminal never falls through to CAO/tmux.
 */

/** The daemon session that owns `terminalId` as its attribution key, or null. */
export function daemonOwner(
  terminalId: string | null | undefined,
  rustPtySessions: Record<string, RustPtyMeta>,
): RustPtyMeta | null {
  if (!terminalId) return null;
  for (const meta of Object.values(rustPtySessions)) {
    if (meta.terminalId === terminalId) return meta;
  }
  return null;
}

/** Whether `terminalId` is backed by a daemon session (not a CAO tmux pane). */
export function isDaemonOwned(
  terminalId: string | null | undefined,
  rustPtySessions: Record<string, RustPtyMeta>,
): boolean {
  return daemonOwner(terminalId, rustPtySessions) !== null;
}

/**
 * The working directory for a terminal, routed by ownership: a daemon-owned
 * terminal resolves to its locally-known `cwd` (set at spawn — no CAO round-trip,
 * and survives worktrees moving daemon-side in Phase 3); everything else falls
 * back to `caoLookup` (CAO `/terminals/{id}/working-directory`).
 */
export async function resolveWorkingDirectory(
  terminalId: string,
  rustPtySessions: Record<string, RustPtyMeta>,
  caoLookup: (id: string) => Promise<string | null>,
): Promise<string | null> {
  const owner = daemonOwner(terminalId, rustPtySessions);
  if (owner) return owner.cwd ?? null;
  return caoLookup(terminalId);
}
