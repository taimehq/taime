# Taime

A native multi-agent coding workspace. Taime launches the official AI coding
CLIs — **Claude Code**, **Codex**, **Gemini CLI**, **Grok CLI** — as **Agents**
in your project (the **Workspace**), each in its own isolated git **Worktree**,
and records **Attribution** for everything they do: turns, files touched,
diffs, all keyed to a durable **Agent ID**. Nothing merges without **Review**.

Taime drives the real CLI binaries (never raw provider APIs), so your
subscriptions and every native agent capability stay intact.

On top of that substrate:

- **Tasks** group agents and runs into units of user intent — the unit of safe
  context switching, with aggregate review state and rollups.
- **Orchestrators** are agents whose Profile grants delegation (assign,
  handoff, message) — their delegations form a **Team**.
- **Workflows** are reusable agent graphs (branches, loops) executed as
  **Runs**; **Schedules** fire agents on cron, even with the app closed.

The canonical model and vocabulary live in
[`desktop/docs/architecture-lexicon.md`](desktop/docs/architecture-lexicon.md) —
that file is authoritative; everything else reconciles against it.

## Architecture

Two processes, all Rust + TypeScript:

| Piece | What it is |
| --- | --- |
| **App** (`desktop/`) | Tauri v2 shell: React 19 + TypeScript UI (xterm.js, Monaco) over a thin Rust client. Holds view state only. |
| **Session daemon** (`desktop/src-tauri/crates/taime-session-daemon`) | A detached process that owns the PTYs, the authoritative terminal grids, attribution recording, worktrees, Tasks, the workflow engine, and schedule firing. It outlives the app: agents keep running across app restarts and reattach with an exact repaint. |
| **Wire protocol** (`desktop/src-tauri/crates/taime-protocol`) | Shared crate: length-delimited, postcard-encoded messages over a per-user Unix socket. |

The app talks to the daemon over the Unix socket; there is no HTTP server and
no other runtime dependency.

## Repo layout

```
desktop/                 the product (Tauri app + workspace root for all crates)
├── src/                 React frontend
├── src-tauri/           Rust app crate + the two crates above
└── docs/                architecture-lexicon.md (canonical), packaging.md, plans
tests/                   headless end-to-end proof of the attribution loop
```

## Quickstart

Prereqs: Node 20+ & pnpm, stable Rust + platform toolchain. The target CLIs
(`claude`, `codex`, `gemini`, `grok`) should be on `PATH` and logged in.

```bash
cd desktop
pnpm install
pnpm tauri dev    # builds the daemon first, then launches the app
```

`pnpm tauri build` produces a self-contained bundle with the daemon inside
(see `desktop/docs/packaging.md`).

## Tests & lint

```bash
cd desktop/src-tauri
cargo test --workspace     # app + protocol + daemon (incl. socket integration tests)
cargo clippy --workspace --all-targets
```

**Note:** the bare commands (no `--workspace`) only cover the app crate and
skip the daemon and protocol crates — always pass `--workspace`.

```bash
cd desktop
pnpm typecheck             # TypeScript strict
```

See [`desktop/README.md`](desktop/README.md) for the developer guide.
