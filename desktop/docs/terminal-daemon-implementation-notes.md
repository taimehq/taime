# Taime Terminal Daemon — Implementation Decisions (research-backed)

Companion to [`terminal-architecture-plan.md`](./terminal-architecture-plan.md). This
file records the **locked technical decisions** that the implementation follows, each
grounded in research (docs.rs / crates.io / official Tauri docs / source), per the
directive "do all the research for best practices, don't just guess." Where the plan
said one thing and research found a better answer, the deviation is called out.

## A. Tauri binary channel (Step 0a) — kills base64

- Stream raw PTY bytes with **`tauri::ipc::Channel<tauri::ipc::InvokeResponseBody>`** and
  send **`InvokeResponseBody::Raw(Vec<u8>)`**. JS receives an **`ArrayBuffer`**.
- **Do NOT** use `Channel<&[u8]>` / `Channel<Vec<u8>>` — those hit the blanket
  `impl<T: Serialize> IpcResponse` and serialize to a **JSON number-array** (worse than
  base64). This is the #1 trap; the official `load_image` docs example is misleading.
- JS: `new Channel<ArrayBuffer>()`; `onmessage = (m) => term.write(new Uint8Array(m))`.
  No `atob`/`TextDecoder`. **Retain the Channel ref** (GC kills `onmessage`). Arg key is
  camelCase (`on_data` → `onData`).
- **Coalesce** in the reader thread: flush every ~8–16 ms or at ~32–64 KB.
- Reattach replay = the **first `Raw` message** on the same channel → replay + live share
  one ordered pipe (no gap/dup).
- No capability/permission additions: channels ride the command's existing invoke access
  (`core:default` already present). No `core:channel` permission exists.

## B. wezterm-term authoritative emulator (Step 2)

- **Git dep, pinned rev `577474d89ee61aef4a48145cdec82a638d874751`** (NOT crates.io —
  `wezterm-term` is unpublished). Pull **`termwiz` from the SAME rev** (in-tree 0.24.0),
  not crates.io 0.23.3, or `Line`/`wezterm-surface` types won't match.
- One `wezterm_term::Terminal` per session. `Terminal::new(TerminalSize, Arc<dyn
  TerminalConfiguration>, term_program, term_version, Box<dyn Write+Send>)`.
- `TerminalConfiguration`: only `color_palette()` is required (→ `ColorPalette::default()`);
  override `scrollback_size()` (→ 10_000). Writer = no-op sink.
- Feed: `term.advance_bytes(&buf)` (chunks need not be complete escape sequences).
- Reads (via `Deref<TerminalState>`): `screen()`, `cursor_pos()`,
  `is_alt_screen_active()`, `palette()`, `get_semantic_zones()`.
- Cell walk: `line.visible_cells()` → `CellRef { str(), cell_index(), width(), attrs() }`;
  `attrs()` → `foreground()/background()` (`ColorAttribute`), `intensity()`, `underline()`,
  `italic()`, `reverse()`. `line.last_cell_was_wrapped()` for wrap detection.
- **Repaint:** there is NO built-in Terminal→escapes serializer. We **hand-roll an SGR
  serializer** from the authoritative grid (verified cell accessors) — clear+home, per-row
  SGR runs, cursor restore + visibility, re-enter alt-screen if `is_alt_screen_active()`.
  This makes display (xterm) and record (wezterm) **converge by construction** at each
  handoff, which is the plan's stated goal. (termwiz `Surface::diff_lines` +
  `TerminfoRenderer` is the alternative; hand-rolling avoids the `RenderTty`/`Capabilities`
  fiddliness and uses only verified APIs.)
- Snapshots: `visible_lines()`/`all_lines()` return owned `Vec<Line>` — clone **only at
  turn boundaries**, never per chunk. Scrollback exists only on the primary screen.

## C. Detached daemon spawn on macOS (Step 2)

- Plain `std::process::Command` + `CommandExt::pre_exec` calling **`libc::setsid()`** +
  `dup2` `/dev/null` to stdio; `spawn()` then **drop the `Child` without wait/kill** (std
  `Child` does not kill on drop → orphaned to launchd → survives app exit/crash).
- Single `setsid`, **no double-fork** (launchd reaps the orphan; no zombie). **Not** a
  Tauri sidecar/`externalBin` (the shell plugin kills those on teardown). Bundle the
  daemon via `bundle.resources` (lands in `Contents/Resources/`).
- `pre_exec` is async-signal-safe-only: `setsid`/`dup2`/`close` — no alloc/Mutex/env/log.
- Liveness = `flock(LOCK_EX|LOCK_NB)` held for daemon lifetime + successful connect
  (beats `kill(pid,0)`: PID reuse). `connect_or_respawn` on relaunch adopts a live daemon.

## D. Unix socket security (Step 2)

- **Placement:** socket in `std::env::temp_dir()/taime/` (Darwin per-user 0700 temp;
  honors `$TMPDIR`) with a **short hashed name** + a 0700 app subdir. macOS has no
  `$XDG_RUNTIME_DIR`. **Length-check the full path < 104 (macOS `sun_path`) before bind**,
  fail loudly. Runtime state (pid/lock/endpoint) lives separately under app-support.
- **Bind:** `umask(0o077)` around `bind` (born 0600) + explicit `set_permissions(0o600)`;
  the **0700 parent dir is the portable hard guarantee** (don't trust socket-file mode
  alone — default bind mode is `0777 & ~umask`).
- **Stale socket:** connect-probe → on `ECONNREFUSED`/`ENOENT` `remove_file` then bind.
  Unlink only after a failed connect confirms no live daemon.
- **Peer validation:** `UnixStream::peer_cred()?.uid()` vs `libc::getuid()` on every accept
  (abstracts `getpeereid` macOS / `SO_PEERCRED` Linux — never hardcode `SO_PEERCRED`).
- **Attach token:** 32 random bytes (OsRng) → 0600 sibling file, rotated each start,
  unlinked on exit, **constant-time compare** (`subtle`). Defense-in-depth; uid gate is
  primary. Read by client only after the uid gate passes.
- SIGTERM/SIGINT handler unlinks socket + token; client stale-cleanup is the SIGKILL
  backstop.

## E. SCM_RIGHTS fd passing (Step 2.5 — reserved, not built)

- Reserve a **`FdFollows { session_id, kind }`** control variant now ("next message
  carries exactly one fd in SCM_RIGHTS ancillary"). Keep the data socket a real
  `AF_UNIX SOCK_STREAM`; never hide `as_raw_fd`.
- When built: `sendfd = "0.4.4"` (`features=["tokio"]`); wrap `send_with_fd`/`recv_with_fd`
  in `writable()`/`readable()` retry loops (its tokio impl uses `try_io`, returns
  `WouldBlock`, doesn't loop). `MasterPty::as_raw_fd() -> Option<RawFd>` exists in 0.8.1+.

## F. Protocol framing + serialization (Step 2)

- **`tokio-util` `LengthDelimitedCodec`** (`features=["codec"]`): default u32 BE 4-byte
  prefix; yielded `BytesMut` is **payload-only** (prefix stripped). `.send(Bytes)` (call
  `.freeze()`); needs `futures::{SinkExt, StreamExt}`. Set `max_frame_length` explicitly.
- Type byte lives **inside** the payload as `frame[0]`.
- **Control messages: postcard `1.1.3`** (`features=["use-std"]`), **NOT bincode** —
  bincode 3.0.0 is a deliberate non-compiling stub and all versions trip RUSTSEC-2025-0141
  (unmaintained). This **overrides** the plan's "bincode" note. postcard is serde-compatible
  so the message shapes are unchanged.
- **Hot path (PTY data): hand-rolled `[type:u8][offset:u64 BE][raw bytes]`** — no serde
  (avoids redundant inner length + copy).
- **portable-pty: bump daemon to `0.9`** (macOS unaffected by 0.9.0's Windows-only
  regression #6783). The app's `pty.rs` can stay on 0.8 until the daemon supersedes it.

## G. Attribution boundary detection (Step 2) — the flagship

- Two consumers per PTY byte slice: (1) `wezterm_term::Terminal` (authoritative grid),
  (2) a **parallel `termwiz::escape::parser::Parser` tap** to recover OSC 133
  `CommandStatus`/A/B/C + `CurrentWorkingDirectory` (wezterm-term parses then discards the
  exit code).
- **Plural boundary signals**, priority: (a) **app checkpoints** (strongest, spoof-proof —
  app knows when it pressed Enter / launched an agent; the primary signal for alt-screen
  TUIs like `claude` that emit no OSC 133), (b) OSC 133, (c) output **quiet-window**
  (~600–800 ms, adaptive), (d) **fs-dirty correlation** from the existing `fs_watch.rs`,
  (e) process lifecycle (coarse fence).
- **Turn model:** monotonic `start_offset`/`end_offset` (+ `StableRowIndex` range that
  survives scrollback eviction), `grid_snapshot` (text), `fs_dirty_paths`,
  `started_at_cause`/`ended_at_cause`. Don't cell-diff (meaningless under alt-screen redraw).

## Offset / `N` convention (handoff)

Half-open: **`seq_n` = total bytes ingested by `wezterm-term`** so far; the grid reflects
bytes `[0, seq_n)`. Live data frames carry absolute `start_offset >= seq_n`. The client,
after the prelude + repaint, **keeps frame bytes with absolute offset >= seq_n**, slicing a
straddling frame: `bytes[(seq_n - start)..]`. This is the half-open form of the plan's
"discard <= N, keep > N" rule (adjusted for clarity; behavior identical).

## Build, run & packaging

- **Workspace:** `src-tauri/Cargo.toml` is the workspace root; members
  `crates/taime-protocol` + `crates/taime-session-daemon`. Build the app alone
  with `cargo build -p taime` (skips the heavy wezterm git dep, which is
  daemon-only). `cargo build` / `cargo test` (no `-p`) build everything.
- **Dev:** the app resolves the daemon binary as a **sibling of the app exe**
  (`target/debug/taime-session-daemon`), so run `cargo build -p taime-session-daemon`
  once before using the "Claude · Daemon (dev)" launch button under
  `pnpm tauri dev`. If the daemon binary is absent, `daemon_spawn_claude` returns
  a clean error and the in-app Rust-PTY path is unaffected.
- **Packaging (deferred — needs a real bundle run to validate):** `tauri-build`
  validates `bundle.resources` paths at **compile time**, so we do NOT declare the
  release daemon as a resource in `tauri.conf.json` (it would break `cargo build`
  before the release binary exists). To bundle: (1) `cargo build -p
  taime-session-daemon --release`, (2) add to `tauri.conf.json` →
  `bundle.resources`: `{ "target/release/taime-session-daemon": "taime-session-daemon" }`
  (lands in `Contents/Resources/`, which `resolve_daemon_bin` already checks),
  (3) under Hardened Runtime, sign the helper bottom-up + notarize. A
  `beforeBuildCommand` that builds the daemon release binary first makes
  `tauri build` self-contained.

## Verification status (this branch)

- **Step 0a/0b:** in-app `PtyManager` — 17 Rust tests (binary transport, attach
  replay, boundary-trim, backpressure) + `tsc` + frontend build. ✅
- **Step 1:** `docs/spikes/step1-dtach-survival.sh` proves master-outside-app
  survival. ✅ (superseded by Step 2's daemon).
- **Step 2:** protocol (6 tests) + daemon (17 unit) + a **headless end-to-end
  integration test over a real Unix socket** (2 tests: full lifecycle
  handshake→spawn→**resize-on-attach**→I/O echo→list→kill→exit, and bad-token
  reject). ✅ The app↔daemon bridge is the same protocol the integration test
  exercises; the only unverified seam is the GUI (needs interactive `tauri dev`).

## Post-review closing actions (addressed)

- **Resize-first handoff:** `Attach` now carries the client's `rows`/`cols`; the
  daemon resizes the PTY + emulator **before** snapshotting, so the grid repaint
  reflects the real viewport, not the spawn-time 24×80 (handoff step 1). The
  frontend fits xterm one layout frame before attaching. Integration test attaches
  at 30×100 over a 24×80 spawn and asserts `AttachOk` reflects 30×100.
- **Crash-relaunch auto-adopt:** `daemonList()` is **connect-only** (never spawns
  a daemon) and now returns full `SessionSummary`. `useRustPtyReconcile` enumerates
  on boot and calls `adoptDaemonSession` for each survivor, so agents that outlived
  an app crash appear in the detached-agents panel and reopen on the daemon
  transport — no user action. `kill` is likewise connect-only (won't boot a daemon
  to kill an already-gone session).
- **App-driven attribution checkpoint:** the frontend fires `daemon_checkpoint`
  ("submit") on Enter for daemon frames — the plan's strongest, spoof-proof
  boundary signal (alt-screen TUIs like `claude` emit no OSC 133). The daemon
  guards empty turns, so spurious Enters are harmless.

## Transport consolidation (one Rust PTY path)

The interim **in-app `PtyManager`** (`src-tauri/src/pty.rs`, Step 0a/0b) has been
**removed**. It was a stepping stone — own the PTY in-process, kill base64 — that
the daemon strictly supersedes (same binary `Channel` transport, plus crash
survival + the authoritative grid). There is now exactly one Rust PTY path: the
detached daemon. `portable-pty` is no longer an app-crate dependency (only the
daemon uses it).

Launch routing: the single "Launch agent" dialog routes **Claude → daemon** when
`daemon_available()` (binary resolvable or already running), **falling back to
CAO** on failure / unbundled builds; the other CLIs always use CAO. The two
dev-only "Rust PTY"/"Daemon" buttons are gone. So Claude defaults to the Rust
daemon with no separate entry point, and never hard-fails.

## Remaining follow-ons (substrate complete, loop not yet closed)

- **fs_watch ↔ turn correlation:** the daemon emits `TurnInfo` with empty
  `fs_dirty_paths` by design (the app owns `fs_watch`); correlating dirty events to
  turns by time window and surfacing them in the activity graph is the remaining
  attribution-UI work. Turn events already land in the store (`frameTurns`).
- **Production rollout / packaging:** the daemon launch is intentionally
  `import.meta.env.DEV`-gated while it matures toward CAO parity (per the plan's
  per-CLI retirement), and bundling is deferred (see "Build, run & packaging" —
  declaring the release daemon as a `bundle.resource` breaks `cargo build` until
  it's built, and a real `tauri build` + signing run is needed to validate).

## Compile-time-verify items (flagged low-confidence in research)

1. Hand-rolled repaint SGR correctness vs the plan's acceptance set (alt-screen↔main,
   wrapped lines, full SGR/color, cursor shape/visibility, bracketed-paste) — can only be
   fully proven in an interactive xterm; built against the verified cell model.
2. macOS received-fd CLOEXEC for Step 2.5 (no `MSG_CMSG_CLOEXEC` on darwin) — set
   `FD_CLOEXEC` explicitly if/when the daemon execs. N/A until Step 2.5.
