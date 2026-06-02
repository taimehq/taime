# Taime Terminal Architecture — Plan of Record

**Status:** agreed direction; not yet started.
**Revision (2026-06-02):** incorporated architecture review — handoff protocol,
backpressure, daemon lifecycle/security, scoped reattach claim, OSC-as-one-signal.
**Revision 2 (2026-06-02):** operational hardening — protocol versioning + daemon
upgrade policy, measurable backpressure ack, attach-state normalization, precise
sequence `N`, scrollback placeholder protocol, peer-validated socket, GC that
never reaps a live agent.
**Revision 3 (2026-06-02):** implementation precision — byte-offset frame slicing,
xterm-only tested reattach prelude, explicit `AckBytes` upstream + batched acks,
tunable watermarks, attach-token lifecycle, macOS socket-path limits, legacy-daemon
control subset + auto-readvertise, corrected bracketed-paste invariant, `SCM_RIGHTS`
promoted to a named Step 2.5.
Companion to [`rust-pty-parity-audit.md`](./rust-pty-parity-audit.md), which is
the Step 0 verification gate this plan executes against.

## North star

A standalone **Rust session daemon** owns the PTYs and an authoritative headless
terminal emulator (`wezterm-term`). The Tauri app is a **thin client** that
attaches/detaches over a Unix socket. This single architecture delivers:

- **Fastest** — binary `Channel`/IPC transport, no base64, frame-coalesced.
- **Most reliable** — agents survive an app crash/reload; **exact visible-screen
  reattach** via a defined handoff protocol.
- **Most innovative / thesis-critical** — the authoritative grid + command
  boundaries live in Rust, making terminal output an **attribution substrate**
  (the flagship), not state trapped in a webview.

Keep `portable-pty`. Add an authoritative emulator. Move the PTY masters out of
the app process. Stream binary.

### Why a separate process is mandatory (not a signal trick)

In the current design the agent is a child of the Tauri app and the **PTY master
fd lives inside the Tauri process** (`pty.rs`, `main.rs:78`). When the app dies,
the master closes. Closing the master end produces terminal **hangup / EOF on
read / EIO on write** on the slave side, which CLIs commonly treat as session
death (and, for a controlling terminal, a hangup signal to the foreground
process group). The exact mechanics vary by process and signal handling, but the
conclusion is robust: **the master must outlive the app.** No `setsid`/`disown`/
process-group trick changes this — the trigger is the master closing, not group
membership. The only fix is to hold the master in a process that outlives the
app. That is what tmux does, and what the daemon does.

### Corrections & scope (read before the steps)

1. **xterm.js stays the byte renderer through Step 2.** `wezterm-term` is
   authoritative for *state/attribution*, **not** for display, until Step 3 is
   explicitly chosen. There is no "thin render-delta xterm" intermediate — xterm
   consumes bytes and *is* an emulator today (`TerminalViewRustPty.tsx`,
   `term.write(...)`). A true cell-grid renderer is a separate frontend project.

2. **Reattach is "exact visible-screen," not "exact."** The daemon serializes its
   authoritative grid into a minimal full-screen repaint (one screenful, bounded)
   and feeds *that* to xterm. This restores the **visible viewport** exactly and
   makes display (xterm) and record (`wezterm-term`) converge at each handoff. It
   does **not**, on its own, restore: scrollback history, selection, search state,
   OSC-8 hyperlinks, prompt/semantic zones, or xterm's internal parser state.
   - **Scrollback:** Step 2 repaints the **visible screen only**. Shipping a full
     10k-line history to xterm on every reattach risks a UI hang. History is a
     **lazy sync** — send chunks on scroll-up — and is explicitly deferred.

3. **Reattach needs a handoff protocol, not "stream bytes + repaint."** Across a
   process boundary you can't share the mutex the current path relies on
   (`pty.rs` reattach snapshots the buffer + flips `attached` under one lock). The
   daemon replaces that with an **epoch/sequence protocol** (see Step 2) so there
   are no duplicate or missing bytes around the handoff. Preserve the existing
   no-gap/no-dup rigor — don't regress it.

---

## Step 0 — Transport & stream resilience

Split because the two halves have different blast radius: 0a is transport-shaped
(no UX behavior change); 0b deliberately changes failure modes.

### Step 0a — Binary, coalesced transport (no behavior change)

- Replace base64 + global `emit` (`main.rs:78`) with a **per-session Tauri
  `Channel<T>` carrying raw bytes**.
- Add **frame coalescing** (~16 ms) in the reader thread.
- xterm unchanged (still the emulator). Visible output identical — with one honest
  caveat: **coalescing changes sub-frame *chunk timing*** (many small reads become
  one write). Imperceptible in practice and good for throughput, but it's not
  literally "zero change," so verify nothing timing-sensitive (e.g. a TUI that
  measures inter-byte gaps) regresses.
- **Exit:** no base64 on the hot path; CPU/latency drop under a flooding TUI /
  build log; parity rows for render/latency unaffected.

### Step 0b — Backpressure + interim replay mitigation (changes failure modes)

- **Backpressure / high-water mark:** if the outbound bytes-in-flight for a
  session exceed a **high watermark**, **pause reading the PTY master**; resume
  below a **low watermark**. The kernel PTY buffer then fills and the agent blocks
  on write — natural flow control, no unbounded buffering, no memory blowup.
  - **Watermarks are tunable.** 64 KB is illustrative, likely **too low** as a
    default: TUI bursts + webview scheduling jitter could stall agents
    artificially. Pick high/low watermarks empirically against a flooding TUI;
    expose them as config, don't hardcode.
  - **Ack contract (define precisely):** "in-flight" means *sent but not yet
    accepted by xterm* — measured by **xterm's `write(data, cb)` callback firing**
    (the chunk parsed/processed), **not** "socket write completed" or "Channel
    delivered." Acking on "sent to frontend" lets xterm's own write buffer grow
    unbounded behind a smooth-looking wire.
  - **Return path = explicit `AckBytes(offset)` control message.** The ack must
    travel back to the Rust reader or "pause the PTY" has no measurable trigger.
    The client acks the **highest processed byte offset**, **batched per
    frame/drain interval** (one ack per ~16 ms, monotonic) — *not* one ack per
    chunk, or the ack path becomes its own latency/chatter source. The daemon
    compares `sent_offset − last_acked_offset` against the watermarks.
- **Interim replay fix:** snapshot at a **clear/alt-screen boundary** instead of
  the raw 1 MB byte offset (`pty.rs` `MAX_BUFFER` drain) to avoid mid-escape
  truncation desync — while **preserving** the atomic subscribe-then-reattach
  no-gap/no-dup invariant the current code guarantees.
- Run the parity matrix in [`rust-pty-parity-audit.md`](./rust-pty-parity-audit.md)
  in the native app; **document where xterm fidelity breaks** — evidence for Step 2.
- **Exit:** flooding process can't grow memory unbounded; reattach no longer
  garbles on long sessions; parity findings recorded.

## Step 1 — Crash-resilience spike (optional, non-blocking)

- Wrap agents in **`dtach`** so the PTY master leaves the app process. Prove the
  loop: **kill the app → agents survive → relaunch → reattach.**
- **Scope:** validates *master-outside-app survival* only. It does **not**
  validate the daemon protocol, grid snapshots, attribution, or exact
  visible-screen repaint. It is **not a prerequisite for Step 2** — run it only if
  a fast survival demo is useful. `dtach` does not become a product dependency.

## Step 2 — The real target: `taime-session-daemon`

Separate long-lived Rust binary owning `portable-pty` + headless `wezterm-term`.

### Process model & lifecycle

- **Detached, not a managed sidecar.** A Tauri-*managed* sidecar is killed on app
  teardown — which would defeat crash survival. The daemon is spawned **detached**
  (double-fork / `setsid`, outside the app's process group) so it holds the
  masters independently. The app **discovers and adopts** it on relaunch via the
  advertised socket; it does not "own" it as a child that dies with it.
- **Orphan GC — "do not surprise me":** the daemon GCs **dead/exited sessions**
  and shuts itself down once it has **no remaining sessions**. It must **never
  auto-reap a live agent** by default — closing the app overnight must not kill a
  running agent. Any *time-based* reaping of live sessions is **opt-in,
  user-configurable, and visible in the UI** (e.g. a "detached agents" panel
  showing what's still running and its idle time). A lightweight client
  **heartbeat** distinguishes "app closed" from "app crashed," but neither, on its
  own, kills a live session. On a missed heartbeat (crash), the daemon keeps
  running and **re-advertises its live sessions** so the next app launch
  auto-adopts them with no user action (see Transport / IPC).

### Transport / IPC

- **Binary framing, no JSON on the hot path.** Length-prefixed frames
  (`[len:u32][type:u8][payload]`); **bincode** for control/metadata, **raw byte
  payloads (no base64)** for the data stream. (Not "zero-copy" — bytes still cross
  the socket and the Tauri/webview boundary; the win is no base64 + no JSON, not
  literal zero-copy.)
- **Versioned handshake (first control exchange):** `magic`, `protocol_version`,
  `capabilities` (feature flags), `session_id`, and an `attach_token`. This is
  what makes app-update-while-old-daemon-running safe.
- **Control message set (reserve now):** `Attach`, `Resize`, `AckBytes(offset)`
  (backpressure return path), `Kill`, `List`, `GetHistory(before_seq, max_lines)`
  (deferred), plus the data frames. Keep it small and versioned.
- **Attach-token lifecycle:** the daemon **generates the token at startup** and
  writes it next to the socket in the **0600 protected dir** (a sibling
  `…/<id>.token` or a small state file). The app **rediscovers it by reading that
  file** on relaunch — so a detached daemon is re-attachable without the token
  being broadcast anywhere. **Rotated** on each daemon start; **invalidated** by
  unlinking on exit. It's **defense-in-depth** — the primary gate is the
  peer-uid check; the token stops another *same-user* process from hijacking.
- **Daemon upgrade policy (detached daemons make this real):** on relaunch the app
  may find a daemon of a different version. Default policy:
  - **Compatible** (`protocol_version` + required `capabilities` match) → **adopt**
    it and continue its sessions.
  - **Incompatible** → **do not kill its live sessions.** The legacy daemon keeps
    serving its existing agents until they exit; the new app spawns a v2 daemon for
    **new** sessions and the UI surfaces "N sessions running under a previous
    version." The app must retain a **minimal backward-compatible control subset
    (at least `List` + `Kill`)** so the user can *see and terminate* legacy
    sessions even when it can't `Attach` to them.
  - **Step 2.5 — zero-downtime upgrade via `SCM_RIGHTS`:** hand the PTY master fd
    across the socket (daemon→app, or old daemon→new daemon) to migrate a live
    session without dropping it. Deferred, but the protocol should not preclude it.
- **Auto-readvertise + auto-adopt:** the daemon **always advertises its live
  sessions** on the socket. On app relaunch (crash or clean), the app **enumerates
  and adopts** them immediately — no user action needed to recover after a crash.
- **Socket security & path limits:** keep the socket in a user-protected dir with
  **0600** perms **plus peer validation** (`SO_PEERCRED` on Linux / `getpeereid`
  on macOS) and the `attach_token`. **Mind the path-length limit** — `sun_path` is
  **~104 bytes on macOS** / 108 on Linux, and the app container/data dir can blow
  past that. Use a **short, hashed socket name** under a protected runtime/cache
  dir (e.g. the Darwin per-user temp dir, or `$XDG_RUNTIME_DIR`), not the full
  container path. Don't rely on directory perms alone.

### Attach handoff protocol (the rigor that replaces the shared mutex)

On attach, strictly ordered:
0. **Reattach prelude — a virtual reset, xterm-only.** A reused xterm may be in
   alt-screen, carry stale SGR/modes/cursor state, wrapped lines, or a half-parsed
   escape; a grid repaint won't converge from an unknown baseline. The client
   writes a reset-to-baseline sequence **into xterm only — never to the PTY**
   (writing reset bytes to the PTY would land in the agent's stdin). Crucially
   this is an **explicit, tested prelude, not a guessed escape string**: the exact
   sequence is whatever the acceptance test proves converges. **Acceptance set
   must cover:** alt-screen TUI ↔ main screen, wrapped lines, full color/SGR,
   cursor shape/visibility, and bracketed-paste mode. The repaint (step 3) then
   sets the real target state (re-entering alt-screen if the grid is alt).
1. **Resize** — client sends `Resize(cols, rows)`; daemon calls `pty.resize()` →
   kernel delivers SIGWINCH to the agent. *(Resize is the #1 failure point in
   client/server terminals; it is a first-class protocol message sent whenever the
   xterm container changes size — audit row #8.)*
2. Daemon **snapshots the grid at sequence `N`** and sends the **grid repaint**.
3. Daemon **resumes the byte stream from offset `> N`**; the client reconciles at
   **byte granularity** (see frame model). No duplicate, no gap — the cross-process
   generalization of today's subscribe-then-reattach lock.

**Frame model + overlap handling.** A data frame is `{start_offset, end_offset,
bytes}`. "Drop frames `≤ N`" is ambiguous because **a frame can straddle `N`**. The
client rule is byte-level: **discard bytes with offset `≤ N`; for a frame where
`start ≤ N < end`, slice it and apply only `bytes[(N − start + 1)..]`; apply whole
frames with `start > N` as-is.** This guarantees exactly-once application across
the cut.

**Definition of `N` (be precise):** `N` is a **monotonically increasing byte
offset of the single PTY stream, counted after ingestion by `wezterm-term`.** The
daemon feeds each byte to the emulator *and* forwards it to clients from the same
ordered stream; `N` is the cut point. The grid reflects bytes `[0..=N]`; resumed
data carries bytes `(N..]`. This makes `prelude → repaint(grid@N) → apply bytes>N`
provably equivalent to "xterm was fed the entire stream."

### Authoritative state & attribution

- `wezterm-term` holds the authoritative grid; bytes still stream to xterm for
  display, **plus** structured metadata to attribution.
- **Boundary signals are plural — OSC 133 is one input, not the contract.** Today's
  attribution already uses checkpoint + fs-event concepts, not OSC. Combine:
  OSC 133 command zones *when present*, explicit agent launch / send-message
  checkpoints, prompt/input boundaries, process lifecycle, output **quiet
  windows**, and **filesystem-event correlation** (tie a grid epoch to the
  `fs_watch` dirty events it produced).
- **Per-turn grid snapshots** at boundaries become structured attribution
  artifacts.

### Scrollback & history (placeholder protocol — deferred, but reserved now)

Even though full scrollback fidelity is deferred (Step 2 repaints the visible
screen only), the **data model is reserved now** so Step 3 doesn't inherit an
underspecified history:

- The daemon **retains structured scrollback** (lines with cell attributes)
  **separately from the visible grid**, using/around `wezterm-term`'s history
  model. *(Verify before relying on it: confirm the crate exposes accessible
  structured-history APIs and what its memory/line bounds are — don't assume it's
  free.)*
- Reserve a **`GetHistory(before_seq, max_lines)` → ranged line response** request
  in the protocol. It is unimplemented in Step 2 but its shape is fixed, so the
  lazy-on-scroll sync (and any future grid renderer) has a contract to build on.
- Visible repaint and history are **distinct channels**: repaint = grid@N;
  history = ranged backfill served on demand.

### Environment sync

- The daemon captures env at spawn. If the user changes env in app settings while
  the daemon runs, **new sessions** pick up the change; **existing sessions** keep
  their spawn-time env unless an explicit "refresh env" is sent. Document this so
  it isn't a surprise.

### Exit criteria

- Agents survive app crash/reload (closes audit rows #22–#24 + the open SIGTERM
  finding).
- Reattach is **exact at the visible screen**; resize is robust across attach.
- Attribution receives boundary + per-turn snapshot events; a grid epoch can be
  correlated to its `fs_watch` dirty events.

## Step 3 — Frontend grid renderer (decide later, not now)

- Only after attribution + crash survival + exact visible-screen reattach are
  proven, decide whether a true cell-grid (Warp-style) renderer is worth it. The
  Step 2 architecture leaves it a pure frontend swap. Default: **don't build it**
  unless a concrete need appears. A grid renderer is also the natural home for
  full-fidelity scrollback/selection/search, currently deferred in Step 2.

---

## Implementation notes (for when Step 2 starts)

- **Framing:** use `tokio_util::codec` (`LengthDelimitedCodec`) for the
  length-prefixed protocol — gives the framing essentially for free and keeps
  allocations down, matching the "fastest" goal.
- **Socket cleanup:** the daemon **unlinks its own socket on clean exit**; the
  client must also handle a **stale socket file** (left by a crash) — try to
  connect, and if it's dead, unlink + respawn.
- **Bracketed paste (audit row #4):** the invariant is **byte order preservation —
  no bytes inserted, dropped, or reordered**, *not* "never split a sequence."
  Splitting `\x1b[200~` / `\x1b[201~` across frames is fine; xterm's parser is a
  state machine that reassembles across `write()` calls. So framing/coalescing just
  must not mangle bytes. Still add an explicit bracketed-paste parity check after
  the transport change so a large multi-line paste arrives as paste, not auto-run
  input.

## What retires (conditional)

The CAO/tmux/WebSocket terminal path retires **per CLI, as the daemon reaches
parity for that CLI**. Rust PTY is Claude-scoped today (`commands.rs`,
`pty_spawn_claude`), so this is not an unqualified retirement: Codex/Gemini/Grok
stay on CAO/tmux until the daemon supports them at parity. CAO remains for
orchestration regardless — just out of the terminal hot path.

## Sequencing logic

Speed win (0a) → stream safety (0b) → optional survival demo (1) → resilience +
attribution substrate fused (2) → optional renderer (3). Each step ships
standalone value and de-risks the next. **Attribution substrate, not raw speed,
is the strategic payoff** — terminal state cannot stay trapped in a webview
emulator if the product thesis is agent activity attribution.

## Reference points

Convergent design across tools that compete on terminal fidelity: **zellij**
(Rust client/server split), **WezTerm** (`portable-pty` + `wezterm-term` +
native renderer), **Zed** (`alacritty_terminal` headless core), **VS Code**
(separate pty-host so window reload doesn't kill terminals), **Warp** (custom
Rust emulator + block-structured semantic output).
