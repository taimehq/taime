# Packaging the daemon with the app

The app is a thin client; the real backend is the detached `taime-session-daemon`
binary. `resolve_daemon_bin()` (desktop/src-tauri/src/daemon.rs) finds it, in
order: a **sibling** of the app exe (`target/<profile>/` in dev;
`Taime.app/Contents/MacOS/` in a bundle), then the bundle **Resources** dir
(`…/Resources/taime-session-daemon` or `…/Resources/binaries/taime-session-daemon`).

## Dev (`pnpm tauri dev`) — automatic

`beforeDevCommand` builds the daemon first (`cargo build -p taime-session-daemon`),
so the sibling `target/debug/taime-session-daemon` always exists.

## Bundle (`pnpm tauri build`) — automatic

`beforeBuildCommand` runs `scripts/stage-daemon.sh`, which builds the **release**
daemon and copies it into `src-tauri/binaries/`. `bundle.resources` ships the
`binaries/` directory, so the daemon lands at
`…/Contents/Resources/binaries/taime-session-daemon`, where `resolve_daemon_bin()`
finds it. No manual step.

### Why a `binaries/` dir + `.gitkeep`

`tauri-build` validates `bundle.resources` at **compile time** (in `build.rs`), so
the resource path must exist even for a bare `cargo build`. Pointing the resource
at the `binaries/` **directory** (with a committed `binaries/.gitkeep`) satisfies
that check without committing a 32 MB binary: bare `cargo build` sees an
empty-but-present dir; `tauri build` populates it via `stage-daemon.sh` before
bundling. `binaries/*` is gitignored except `.gitkeep`.
