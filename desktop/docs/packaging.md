# Packaging the daemon with the app

The app is a thin client; the real backend is the detached `taime-session-daemon`
binary. `resolve_daemon_bin()` (desktop/src-tauri/src/daemon.rs) looks for it,
in order:

1. a **sibling** of the app exe (`target/<profile>/taime-session-daemon` in dev;
   `Taime.app/Contents/MacOS/taime-session-daemon` in a bundle), then
2. the bundle **Resources** dir (`…/Contents/Resources/taime-session-daemon` or
   `…/Resources/binaries/taime-session-daemon`).

## Dev — works out of the box

`pnpm tauri dev`'s `beforeDevCommand` builds the daemon first
(`cargo build -p taime-session-daemon`), so the sibling
`target/debug/taime-session-daemon` always exists. No manual step.

## Bundle (`pnpm tauri build`) — one config toggle

`bundle.resources` is validated by `tauri-build` at **compile time**, so it can't
be enabled unconditionally (it would break a bare `cargo build` on a fresh
checkout where the binary isn't staged yet). To produce a distributable bundle
that ships the daemon:

1. Stage the release daemon into `src-tauri/binaries/`:
   ```
   bash desktop/scripts/stage-daemon.sh
   ```
2. Enable the resource in `desktop/src-tauri/tauri.conf.json`:
   ```json
   "bundle": { "active": true, "targets": "all",
     "resources": ["binaries/taime-session-daemon"], … }
   ```
3. `pnpm tauri build`.

The daemon is copied into the bundle's Resources, where `resolve_daemon_bin()`
finds it. (Alternatively, wire steps 1–2 into CI's build job so a release build
always stages + bundles the daemon.)

> Known gap: this packaging step is currently manual (the resource toggle is left
> off so `cargo build` works standalone). Automating it cleanly needs a committed
> placeholder at `binaries/taime-session-daemon` or a CI staging job — tracked as
> a follow-up.
