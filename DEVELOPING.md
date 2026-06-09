# Taime — developer guide

The Tauri v2 product. The app is a thin client; the real engine is the
detached `taime-session-daemon`, which the app spawns and talks to over a
per-user Unix socket. Concepts and vocabulary (Workspace, Task, Agent, Agent
ID, Worktree, Runtime, Workflow/Run, Schedule) are defined in
[`architecture-lexicon.md`](architecture-lexicon.md) — reconcile
against that file, not against other docs.

## Prerequisites

- Node 20+ and `pnpm`
- Rust (stable) + platform toolchain (Xcode CLT on macOS)
- The agent CLIs you want to drive (`claude`, `codex`, `gemini`, `grok`) on
  `PATH` and logged in

## Dev loop

```bash
pnpm install
pnpm tauri dev
```

`beforeDevCommand` builds the daemon (`cargo build -p taime-session-daemon`),
so the app always finds it as a sibling of the app exe
(`src-tauri/target/debug/taime-session-daemon`). Vite hot-reloads the
frontend; Rust changes need a rerun.

## Build

```bash
pnpm tauri build
```

`beforeBuildCommand` runs `scripts/stage-daemon.sh`, which builds the release
daemon and stages it into `src-tauri/binaries/` so the bundle ships it under
`Contents/Resources/binaries/`.

## The daemon

Lives at `src-tauri/crates/taime-session-daemon`. It is spawned **detached**
(`setsid`, stdio to `/dev/null`, never waited on), so it survives app exit and
crash — agents keep running, and on the next boot the app discovers and adopts
them (`daemon_list` is connect-only and never spawns a daemon just to look).

**Protocol-version policy:** the handshake requires an exact
`PROTOCOL_VERSION` match (`crates/taime-protocol/src/lib.rs` — postcard is
positional, so any message change is a wire-layout change). When the app
reaches a daemon speaking an older protocol (i.e. after an app upgrade), it
**replaces** it: SIGTERM the stale daemon — whose shutdown handler kills its
agents and unlinks its runtime files — then spawn the current binary. A
protocol bump therefore terminates running agents; that is the explicit,
intended trade-off (self-healing upgrades over cross-version compatibility).
See `restart_daemon` in `src-tauri/src/daemon.rs`.

## Tests

```bash
cd src-tauri
cargo test --workspace               # app + protocol + daemon
cargo clippy --workspace --all-targets
```

**`--workspace` is required**: bare `cargo test` / `cargo clippy` only cover
the app crate (`taime`) and silently skip the daemon and protocol crates.
Conversely, `cargo build -p taime` builds just the app, skipping the daemon's
heavy `wezterm-term` git dependency.

```bash
pnpm typecheck   # TypeScript strict
pnpm build       # vite production build
```

The daemon's integration tests exercise the full lifecycle over a real Unix
socket (handshake → spawn → attach/resize → I/O → list → kill).

## Source map

```
src/
├── store.ts             Zustand store — all app state (frames, tasks, status)
├── api.ts               typed invoke() wrappers over the Tauri commands
├── pty.ts               binary terminal transport (Channel<ArrayBuffer> → xterm)
├── components/          LaunchAgentDialog, DiffView, TaskReviewDrawer, …
src-tauri/
├── src/commands.rs      every #[tauri::command] the frontend calls
├── src/daemon.rs        daemon client: spawn-detached, connect/handshake,
│                        stale-daemon replacement, attach plumbing
├── crates/taime-protocol/        wire types, PROTOCOL_VERSION, frame tags
└── crates/taime-session-daemon/
    ├── main.rs · listener.rs · conn.rs   socket setup, accept loop, per-conn RPC
    ├── manager.rs · session.rs · runtime.rs   agent runtimes (PTY + emulator)
    ├── emulator.rs · repaint.rs          wezterm-term grid + reattach repaint
    ├── attribution.rs · store.rs         turn boundaries + SQLite persistence
    ├── worktree.rs · diff.rs · fswatch.rs   isolation, review diffs, dirty state
    ├── mcp.rs · providers/ · profiles.rs    orchestration tools, CLI adapters
    └── workflow.rs · workflow_engine.rs · schedules.rs   Workflows, Runs, cron
```
