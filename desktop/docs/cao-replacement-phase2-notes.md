# CAO replacement — Phase 2 implementation notes

**Status:** built. Companion to [`cao-replacement-plan.md`](./cao-replacement-plan.md).

Phase 2 = **daemon-owned session/terminal registry + an API-compatibility shim
that routes each terminal call by daemon-ownership** so tmux/CAO is off the
session/status/inspect path for daemon sessions.

## What was already in place (no work needed)

The Step-2 daemon work had already separated daemon agents from CAO more than the
plan assumed:
- **List** — daemon agents are enumerated via `daemon_list` (`useRustPtyReconcile`,
  boot adoption + 4s liveness), never through CAO.
- **Status** — `refreshStatuses` already filters out daemon frames
  (`!f.ptySessionId`); `refreshSessionRollups` only walks CAO `sessions`;
  `useTurnCheckpoints` keys off CAO `terminalStatuses`, which daemon frames never
  populate (their turns arrive as daemon turn events → `recordTurn`). So no CAO
  `/terminals/{id}` poll happens for a daemon agent.
- **Reconcile** — `useTerminalReconcile` excludes daemon frames
  (`!isDaemonTransport`), so CAO never auto-opens a second frame for a daemon
  attribution id.

## What Phase 2 adds

The one remaining place a daemon-owned terminal still reached into CAO was
`useFsWatch` resolving the watch directory via CAO `/terminals/{id}/working-
directory`. That also would have **broken in Phase 3** once worktrees move
daemon-side (CAO would no longer hold the worktree record).

- **`lib/terminalRouting.ts`** — the explicit *daemon-owned terminal registry
  adapter*: `daemonOwner(terminalId)` / `isDaemonOwned(terminalId)` answer "is
  this attribution id backed by a daemon session?" from the local
  `rustPtySessions` registry, and `resolveWorkingDirectory(...)` routes by
  ownership (daemon-owned → its locally-known `cwd`; else CAO).
- **`useFsWatch`** now resolves the working directory through that adapter,
  reading `rustPtySessions` via `getState()` so the watch still keys only off
  `terminalId` (no re-mount churn). A daemon agent's fs-watch no longer touches
  CAO and survives worktrees moving daemon-side.

The adapter is the single seam the later phases plug into: **Phase 4** routes
daemon status through it (the daemon's `AgentStatus` instead of CAO
`/terminals/{id}`), and **Phase 6** routes diff/hunks/attribution/graph the same
way.

## Verification
- `pnpm typecheck`: clean.
- No protocol change (avoids a `PROTOCOL_VERSION` bump; provider on the daemon
  registry is inferred from the session program via `providerFromProgram` until
  Phase 4 bumps the wire for `status`).
