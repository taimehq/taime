# Taime — Daemon / MCP / PTY Hardening Plan

> **Date:** 2026-06-09
> **Source:** Deep multi-agent review of the daemon / MCP / agent-interaction / Rust / PTY layers (88 agents, 78 candidate findings, 67 confirmed/partial after adversarial verification) + a code-verified peer-feedback pass.

## Implementation status (branch `daemon-hardening`) — ✅ COMPLETE

**Every item in this plan is implemented and verified.** Full suite green: 9 protocol + 141 daemon unit + 3 daemon integration tests, 91 vitest, `pnpm typecheck` clean, prod `pnpm build` OK, `cargo clippy --all-targets` **0 warnings** across all three crates.

### Tier 1–7 + L12 (the priority items):

| # | Item | What landed |
|---|------|-------------|
| 1 | Termination & reaper redesign (H1 + M14 + M15) | Capture pid at spawn; `kill()` SIGTERMs the process **group** (`session.rs signal_group`/`killpg`), gc reaper escalates to group SIGKILL after `KILL_GRACE`; independent `reap_if_exited()` `try_wait` reaper in `gc_tick`; poison-tolerant `lock_state`/`lock_child`; single-shot `finalize_exit`. **Regression test** `kill_terminates_the_whole_process_group_not_just_the_leader` (verified it fails under leader-only kill). |
| 2 | Durable provider cleanup (H2) | `kill_all` now SIGTERM-group → grace → SIGKILL + synchronous `finalize_exit` (runs cleanup) before exit; durable `taime_cleanups` ledger persisted at spawn, dropped on finalize, **replayed on boot** (`reconcile_cleanups_on_boot`); env-marker (`TAIME_SESSION_ID`) + boot `reap::sweep_orphan_agents` (ps -E → killpg orphan groups); atomic config writes (`config.rs atomic_write`, write-temp+fsync+rename). |
| 3 | Schedule claiming (H3 + H4) | `fire_schedule` advances `next_run` BEFORE gate/spawn (at-most-once); `check_schedules` single-flight CAS guard; `run_script_gate` gets `stdin=null` + `GATE_TIMEOUT` (30s) wall-clock kill. |
| 4 | IPC timeouts (H5 + M7) | App `read_server`/handshake/connect bounded (`HANDSHAKE_TIMEOUT` 5s / `RPC_TIMEOUT` 60s / `CONNECT_TIMEOUT` 5s); daemon `conn::handle` Hello deadline (`HELLO_DEADLINE` 5s) + counts the conn as a client only AFTER handshake. |
| 5 | Polls don't kill agents (M2) | `try_connect_handshake` no longer calls `restart_daemon` on reject/timeout — falls back to "no daemon" (agents preserved); only mutating `connect_handshake` auto-heals. |
| 6 | Blocking off the hot tick (M6) | `gc_tick` runs on `spawn_blocking` with a single-flight `gc_running` latch (`try_begin_gc`/`end_gc`) so a wedged tick can't pile up jobs. |
| 7 | Attach failure + disconnect (M5 + M13) | `attach()` reads the first reply synchronously (AttachOk→proceed, Error→fail fast); pump pushes `{type:"disconnected"}` on a non-clean break; frontend `pty.ts` ends the view on `disconnected`. |
| — | L12 | `Store::tune`: `busy_timeout(5s)` + `synchronous=NORMAL`. |

### #8 — Bound the queues (all done):
- **M3** — dead-letter sweep: `prune_history` expires `pending` inbox rows older than 7d (then they age out); gc fan-in unchanged but the leak is bounded.
- **M16** — per-connection response backlog is byte-accounted on a separate `Responder` channel (data path stays watermark-paced); over `MAX_OUTBOUND_RESPONSE_BYTES` (128 MiB) the connection closes.
- **L18** — MCP tool `body`/`value`/`message`/`summary` capped at 64 KiB (`MAX_TOOL_BODY`).
- **L19** — blackboard entries pruned at 30d in `prune_history`.

### #9 — Deferred items (all done):
- **M8** — `cancel_workflow_run` + `Store::cancel_run` + the `workflow_cancel` query arm; `finish_run` guarded to not clobber `cancelled`; engine's existing `status != "running"` bail now reachable.
- **M9** — `Session::infer_status` returns `None` until `out_offset > 0` (no spurious cold-start ERROR, all providers).
- **L4/L5** — codex/grok/gemini status regexes scoped to the chrome-filtered tail + checked after idle/complete; grok bare `failed to` dropped.
- **L13** — `add_column` ignores only "duplicate column" (propagates other errors); `PRAGMA user_version` stamped.
- **L6** — `apply_selection` detects 3-way conflict markers, reports `conflicts` + `conflicted`.
- **L8** — `Store::record_fs_events` batches an fs-change burst into one transaction.
- **L14** — partial `taime/*` branch deleted on fallback-to-shared.
- **L15** — workflow `keyword:` edges match on a word boundary (`(?i)\bWORD\b`).
- **L20** — `initialize` echoes a supported client `protocolVersion`.
- **L22** — `DaemonClient::list()` maps a daemon `Error` to its clean message.
- **L2/L3** — `repaint::serialize` prepends a self-sufficient reset prelude (exit-alt + DECSTR + autowrap/cursor-keys/scroll-region/charset).
- **L23** — `MAX_FRAME_LEN` doc corrected; rows/cols clamped to `MAX_TERM_DIM` in `Session::resize`.

### Tests + UI follow-ups (done):
- **H3 gate test** — `run_script_gate_with_timeout` made injectable; `gate_honors_exit_status` + `gate_times_out_and_fails_closed` regression tests.
- **M2 UI prompt** — `daemon_incompatible`/`daemon_restart` commands + store `daemonIncompatible`/`restartDaemon` + a consent-gated "Backend incompatible — Restart" pill (`BackendStatusPill`).

> Changes are on branch `daemon-hardening`.

---

## Verdict

The architecture is coherent and the security-sensitive parts are correct (MCP anti-spoof on the stdio shim, postcard + strict-version handshake, tagged length-delimited framing, ack-watermark backpressure — all verified). The defects below are where it **won't work perfectly**.

**Two fixes change the product thesis** (agents outlive the app; never corrupt the user's real CLI config):

- **H1** — termination orphans process groups.
- **H2** — shutdown leaks injected MCP config into `~/.gemini` / `~/.grok`.

Everything else is reliability / UX hardening around an already-sound design.

---

## Priority-ordered fix plan

Regrouped by shared mechanism so one change closes several findings.

### 1. Termination & reaper redesign — *(H1 + M14 + M15)*

**Problem.** `Session::kill` (`src-tauri/crates/taime-session-daemon/src/session.rs:415-426`, line 422) calls only `self.inner.child.lock().unwrap().kill()`. portable-pty's `ChildKiller::kill` signals the **single leader PID** (`SIGHUP` then `SIGKILL`). Every CLI is a `setsid()` session/group leader (this is **portable-pty 0.9.0** internal `spawn_command` behavior — `setsid()` + `ioctl(TIOCSCTTY)` in `pre_exec`; there is **zero** `killpg`/`setpgid`/`pre_exec` in our own source, confirmed by grep), and the CLIs fork helpers (node workers, ripgrep, git, language servers, MCP stdio shims). SIGKILL to the leader never reaches them → they reparent to launchd and survive, holding worktree files/locks open and blocking the conservative worktree GC.

**Fix.**
- Capture `child.process_id()` at spawn; store pid/pgid on `SessionInner`.
- On `kill` / `kill_all` (`manager.rs:2085`): `libc::killpg(pgid, SIGTERM)` → grace → `libc::killpg(pgid, SIGKILL)`; keep `child.wait()` as the final leader reap (collects the zombie).
- **`killpg` is necessary but not sufficient** — a grandchild that itself calls `setsid()`/`setpgid()` escapes the group, and there is no cheap reap on macOS (no `/proc`). Backstop with an **env marker**: set `TAIME_SESSION_ID=<id>` in the spawn env (inherited across `setsid`), and on **daemon boot** sweep the process table (`libproc` / `ps -E`) for processes still carrying a marker whose owning session is gone, and `kill` them. (This sweep also serves #2.)
- **Independent reaper (folds in M14 + M15):** the PTY reader thread is today the *sole* reaper (`child.wait()` on EOF) and uses `.unwrap()` on the state mutex everywhere — a panic under that lock (e.g. wezterm parsing escape-heavy output) poisons it and the child is never reaped (`dead` stuck false → GC + idle-shutdown break); and a **backpressure-parked reader** (`session.rs:634`, parked on the Condvar above the 2 MiB watermark) can't observe a *natural* child exit until the client acks/disconnects. Fix both by adding a `child.try_wait()` reaper to `gc_tick` (set `dead`, run cleanup, send `Exited`, `notify_all` to unpark the reader) and making the reader/EOF locks **poison-tolerant** (`lock().unwrap_or_else(|e| e.into_inner())`).

**Tests.** Spawn `sh -c 'sleep 60 & exec sleep 60'` (and a node-worker variant), `kill()`, assert **no surviving descendants**. Assert a session whose child exits while the reader is parked is reaped by `gc_tick`.

---

### 2. Durable provider cleanup — *(H2)*

**Problem.** The signal handler (`main.rs:242-244`) runs `mgr.kill_all()` → socket/token/lock `cleanup()` → `std::process::exit(0)`. The real config teardown `inner.cleanup.run()` lives on the **PTY reader thread, only after EOF** (`session.rs` reader-exit path), and `exit(0)` terminates all threads without joining. So every gemini/grok agent leaves dead MCP-server entries (carrying a stamped `CAO_TERMINAL_ID` and a dead `TAIME_MCP_TOKEN`) plus policy files / workspace dirs in the user's **real** `~/.gemini/settings.json`, `~/.grok/config.toml`, `~/.gemini/policies/`. The user's own `gemini`/`grok` then tries to launch a `taime` MCP server with a dead token.

**Fix.**
- Add `mgr.run_all_cleanups()` (snapshots live sessions, calls `inner.cleanup.run()` directly — the RemoveFile/RemoveDir/Remove*McpServers actions are idempotent) and call it **synchronously** after `kill_all` in both the signal path and the idle-shutdown path, before `process::exit`.
- **Boot reconciliation is load-bearing, not the synchronous path** — SIGKILL / crash / OOM is the common case and bypasses any shutdown handler. Persist a **cleanup ledger** to the store at spawn; on boot, replay/reconcile orphaned cleanup actions (sweep stale `taime` MCP entries from `~/.gemini`/`~/.grok`) alongside the existing orphaned-`running`-row sweep. Reuses #1's env marker to find leaked helpers.
- Make provider config writes **atomic** (write-temp + fsync + rename) in `providers/config.rs` — today `std::fs::write` can truncate the user's real config on a crash mid-write.

---

### 3. Schedule claiming (at-most-once + gate timeout) — *(H3 + H4)*

**Problem.** `fire_schedule` (`manager.rs:1498-1530`) runs the gate (`run_script_gate`, line 1502) and `fire_headless` (line 1512) **first**, and only advances `next_run` via `set_schedule_run` at line 1529 — *after*. A slow gate or slow spawn keeps the row `next_run <= now`, so the next `check_schedules` (~30s tick) **re-fires the same schedule**. Separately, `run_script_gate` (`manager.rs:132`) uses `std::process::Command::...output()` with **no timeout and no `stdin(Stdio::null())`** — a hanging gate leaks a stuck thread + child every tick. (Note: the gate already runs off the hot 250ms loop via `spawn_blocking` at `main.rs:273`, so H4 is about leaked gate children, not a tick stall.)

**Fix.**
- Atomically **claim** the schedule before firing: add an in-flight / lease column (or advance `next_run` on claim under a per-row guard), so an overlapping tick no-ops. **Per-row, not a global lock** — *different* schedules should be allowed to overlap; only same-schedule re-entry is the bug.
- Run the gate with `stdin(Stdio::null())` and a wall-clock timeout (treat timeout as gate-fail); kill the gate child on timeout.

**Tests.** Mock a slow gate; assert a single fire across overlapping ticks. Assert a hanging gate is killed at the timeout and leaks no child.

---

### 4. IPC read timeouts (both directions) — *(H5 + M7)*

**Problem.** The app's `handshake` / `read_server` (`src-tauri/src/daemon.rs`) have **no `tokio::time::timeout`**; `ensure_running` only proves the socket is *connectable*. A wedged daemon, or any same-user process squatting the deterministic socket path, makes every spawn/list/attach await forever with no error and no recovery (the `HelloRejected` self-heal never triggers on silence). Daemon-side, `conn::handle` calls `manager.conn_opened()` as its **first line** (`conn.rs:30-31`), *before* reading Hello — so a silent connection bumps `active_conns`, pinning the daemon awake (idle-shutdown predicate uses `no_clients = active_conns == 0`).

**Fix.** Wrap the client handshake/read in `tokio::time::timeout(~5s)` (route a timeout through `restart_daemon`/fallback as appropriate). Add a daemon-side first-frame deadline before the Hello loop. Count toward `no_clients` only after Hello succeeds (separate authed-conn counter).

---

### 5. Stop read-only polls from killing live agents — *(M2)*

**Problem.** `daemon.list()` / query / activity-graph reads use `try_connect_handshake`, which calls `restart_daemon` on `HelloRejected` → SIGTERM → `kill_all` (every agent dies). The frontend reconcile poll runs every 4s (`useRustPtyReconcile.ts`, app-wide). On an app upgrade that bumps `PROTOCOL_VERSION` while an old daemon still drives live agents, the next background poll **silently destroys them** — no gesture, no warning.

**Fix.** Auto-replace only on **mutating** paths (`spawn`/`kill`/`attach`). Read/poll paths return a distinct "incompatible daemon" status the UI surfaces with a confirm-to-restart ("will stop agents") prompt, or at minimum emit a user-visible event before SIGTERM.

---

### 6. Get blocking work off the hot tick & event loop — *(M6 + M12)*

**Problem.** `gc_tick` (`manager.rs:2164`) — which includes `deliver_pending` (`manager.rs:429`, doing SQLite on the single `Mutex<Connection>` + blocking PTY `write_all`) — is awaited **inline** on the 250ms interval (`main.rs:262`), unlike its siblings `check_schedules`/`sweep_worktrees`/`prune_history` which the same author deliberately `spawn_blocking`s (`main.rs:273/278/283`). A child that stops draining stdin makes `write_all` block the tick worker, stalling delivery/attribution/status for **all** agents. Separately, non-async Tauri commands (`commands.rs` `delete_directory`, `workspace_info`, `set_clipboard_image_from_path`) run blocking I/O on the webview event-loop thread.

**Fix.**
- ⚠️ **Do not naively `spawn_blocking` the whole `gc_tick` per tick** — a wedged tick would pile up blocked jobs on the blocking pool every 250ms. Use a **single-flight guard**: an `AtomicBool` (`compare_exchange` on entry, skip the tick if maintenance is already running, clear on completion). Or split it — keep the cheap non-blocking parts on the 250ms cadence and move only the blocking SQLite/PTY pieces behind the single-flight `spawn_blocking`.
- Make `delete_directory` / `workspace_info` `async` + `tauri::async_runtime::spawn_blocking`.

---

### 7. Surface attach failure & connection loss — *(M5 + M13)*

**Problem.** `attach()` (`src/daemon.rs`) sends `Attach`, spawns the pump, inserts the `AttachHandle`, and returns `Ok(())` **without awaiting `AttachOk`/`Error`**. On a daemon **crash** the pump task just ends — the handle isn't removed and no `{type:exit}`/`{type:disconnected}` is emitted, so the framed terminal freezes silently. A session reaped between `list` and click yields a stuck blank "open" terminal (frontend only `console.warn`s the `Error` frame). **Compounded by a blind spot:** the reconcile poll deliberately **skips framed (actively-viewed) sessions** when marking exited, so a dead agent in an open frame can stay `"running"` forever with no exit event.

**Fix.** Read the first control reply in `attach()` (`Error` → `Err(message)`); treat `Error` as terminal on the pump; on loop-break remove the handle and push `{type:disconnected}`; add the frontend `onDisconnected`/`onError` paths. Let the reconcile poll demote a framed session once `daemonList` reports it gone.

---

### 8. Bound the queues — *(M3 + M16 + L18 + L19)*

- **M3:** validate `to` at enqueue for direct sends (broadcast already does); add a dead-letter sweep to `prune_history` for old `pending` rows with no live/recoverable receiver (today `prune_history` never deletes pending — `store.rs`); confirm the parent resolves before enqueuing the "Worker finished" notice.
- **M16:** the per-connection outbound channel is `unbounded` and only the *data* path is watermarked — add per-connection outbound byte accounting (close above a cap, e.g. 64–128 MiB) or move control responses to a bounded channel.
- **L18/L19:** cap MCP tool body sizes (`body`/`value`/`message`); add blackboard/pending retention.

---

### 9. Deferred (correct, lower leverage)

| ID | Issue | Fix |
|----|-------|-----|
| M8 | No workflow-run cancellation; `status != running` bail is dead code (`workflow_engine.rs`) | Add `CancelRun{run_id}` → `status='cancelled'`; kill the in-flight node session |
| M9 | Empty/cold-start grid → ERROR (claude `claude.rs:155-157` + gemini → `Error`; **grok `grok.rs:117` → `Processing`**); claude self-contradicts its `:188-191` "reserve ERROR for a dead grid" comment | Empty/young grid → unknown; gate ERROR on `out_offset > 0` / age |
| L4/L5 | Codex PROGRESS / codex+grok+gemini ERROR regexes match assistant-quoted text | Anchor to the chrome-filtered tail; check after the completed/idle check; drop grok's bare `failed to` |
| L12/L13 | No `busy_timeout` / `synchronous` / `user_version` (`store.rs`) | `busy_timeout(5s)` + `synchronous=NORMAL`; match the ALTER rc to ignore only duplicate-column |
| L6 | Selective-merge 3-way conflicts reported as `conflicts:[]` (`diff.rs`) | Detect markers / `U` status, populate `conflicts` |
| L8 | Per-file autocommit FS-event inserts (`store.rs`) | One transaction per batch; `synchronous=NORMAL` |
| L14 | Orphaned `taime/*` branch leak on a double git failure (`worktree.rs`) | Best-effort `git branch -D` on fallback-to-shared |
| L15 | `keyword:` workflow edges use substring `contains()` ("PASS"⊂"compass") (`workflow.rs`) | Word-boundary or "starts-with" match per the prompt contract |
| L20 | `initialize` ignores client `protocolVersion` (`mcp.rs`) | Echo a supported version |
| L22 | `list()` returns `Err(Debug-dump)` — only RPC missing the `Error=>Err(message)` arm (`daemon.rs:330`) | Add the `Error => Err(message)` arm |
| L2/L3 | Repaint not self-sufficient (works only because the frontend builds a fresh xterm each mount) (`repaint.rs`) | Prepend a full reset (RIS/DECSTR + autowrap/cursor-keys/scroll-region/charset); emit DECSCUSR |
| L23 | `MAX_FRAME_LEN` doc says repaint can exceed the cap; both ends cap at 16 MiB (`lib.rs`) | Correct the comment; clamp rows/cols |

---

## Corrections to the original review (code-verified)

- **L22 is not "swallowed."** `list()` (`daemon.rs:321`) returns `Err(format!("unexpected list reply: {other:?}"))` at line 330 — the poll *fails the tick* (not a silent `[]`); the message is merely a Debug dump instead of the clean daemon `message`. It is the only RPC missing the `ServerMsg::Error => Err(message)` arm the other five have.
- **Anti-spoof is MCP-only.** The MCP `send_message` stamps `from` = authenticated token holder. The **control-RPC** path `ClientMsg::SendMessage { sender, … }` (`conn.rs:127`) passes the **client-supplied** `sender` straight to `enqueue_message(...)` and `record_edge("message", &sender, …)` with no validation. Acceptable under the same-uid socket + attach-token model, but a *different* threat model — don't generalize "anti-spoof everywhere."
- **M9 is provider-specific** (claude/gemini → `Error`, grok → `Processing`), not "every provider."

---

## Verified-good (do **not** "fix" — these were investigated and are correct by design)

- No locks held across `.await` (verified repeatedly).
- `spawn_blocking` correctly used for git/MCP/query/cron paths.
- MCP anti-spoof caller identity (`send_message_stamps_from_caller_not_a_client_field` test).
- Single-instance `flock` + token-before-bind + stale-socket connect-probe + 0600/0700 perms + peer-uid gate + constant-time attach-token compare.
- Diff/3-way apply (newline round-trip is byte-exact; linked worktrees share `.git/objects` so `--3way` finds base blobs).
- postcard positional encoding + strict-equality version handshake; data-path ack-watermark backpressure.
- `import_cao` idempotency (`INSERT OR IGNORE` on copied PKs).

---

## Reference

- Full per-subsystem walkthrough + all 78 findings with verifier evidence: see the review transcript (synthesis report).
- Architecture canon: `architecture-lexicon.md`.
