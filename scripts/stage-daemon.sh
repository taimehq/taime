#!/usr/bin/env bash
# Build the session daemon (release) and stage it for bundling so the packaged
# app can spawn it (resolved from Resources/ by resolve_daemon_bin). Run
# automatically by `tauri build` (beforeBuildCommand). `tauri dev` uses the
# sibling target/debug binary directly (built by beforeDevCommand) and does NOT
# need this.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"   # repo root
cargo build --release --manifest-path "$here/src-tauri/Cargo.toml" -p taime-session-daemon
mkdir -p "$here/src-tauri/binaries"
cp -f "$here/src-tauri/target/release/taime-session-daemon" \
      "$here/src-tauri/binaries/taime-session-daemon"
echo "[stage-daemon] staged src-tauri/binaries/taime-session-daemon"
