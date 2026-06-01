# Rust PTY ↔ CAO/tmux Parity & Divergence Audit (Phase 5)

**Gate:** Claude does **not** default to the Rust PTY transport until this audit
passes. Run it in the **native app** (`pnpm tauri dev`) — the Rust PTY path and
the file watcher only work inside the Tauri webview.

## Method (equivalent sessions, not the same process)

Do **not** attach two transports to one process. Instead launch **two equivalent
Claude sessions in the same repo** and run the same prompts/workflow through each:

- **A — CAO/tmux/WebSocket:** Launch agent → Claude Code (the normal launch).
- **B — Rust PTY/Tauri:** the dev "Claude · Rust PTY" launch button.

Use the same workspace dir, same model, same prompts. Record each row as
**Pass / Fail / Note** for A and B, then judge divergence.

## Comparison matrix

| # | Dimension | A: CAO/tmux/WS | B: Rust PTY | Divergence notes |
|---|-----------|----------------|-------------|------------------|
| 1 | Initial render (TUI paints fully, no garbling) | | | |
| 2 | Input latency (typing feels immediate) | | | |
| 3 | Paste — small text | | | |
| 4 | Paste — large/multi-line (bracketed paste, no auto-run) | | | |
| 5 | Ctrl-C interrupts (SIGINT reaches Claude) | | | |
| 6 | Escape handling | | | |
| 7 | Enter / submit | | | |
| 8 | Resize / redraw on pane resize | | | |
| 9 | Full-screen Claude UI redraws (alt-screen) | | | |
| 10 | Approval prompts render + accept correctly | | | |
| 11 | Plan Mode UI | | | |
| 12 | Subagent / status indicators | | | |
| 13 | Scrollback (history scrolls, depth) | | | |
| 14 | Selection + copy (⌘C / Ctrl+Shift+C) | | | |
| 15 | Image: drag a screenshot → `[Image #N]` | | | |
| 16 | Image: paste a screenshot → `[Image #N]` | | | |
| 17 | File drag → escaped path inserted | | | |
| 18 | Bypass-permissions mode ("bypass permissions on" footer) | | | |
| 19 | **Background:** process when frame CLOSED (must keep running) | n/a (tmux) | close_view ≠ kill | |
| 20 | **Background:** process when WINDOW hidden/minimized | | | |
| 21 | Reattach after close (B: Detached → Reopen replays scrollback) | n/a | | |
| 22 | Explicit kill terminates the process tree | | | |
| 23 | Cleanup after app quit (no orphaned child) | | | |
| 24 | Cleanup after force-quit (SIGKILL of app) | | | |
| 25 | **Attribution:** edits show dirty badge + in File Inventory | | | |
| 26 | **Attribution:** DiffView shows provenance + per-hunk | | | |
| 27 | **Attribution:** change appears in the activity graph | | | |

Rows 25-27 are the flagship check: **the Rust PTY agent must get the same
attribution surface as the CAO agent** (it does in backend E2E; confirm visually).

## Known-expected differences (not failures)

- **Latency (#2):** Rust PTY data is base64+JSON over a Tauri event; CAO is raw
  binary WS frames. A sub-ms difference is expected. Flag only if *perceptible*.
- **Background (#19):** tmux gives the CAO path persistence for free; the Rust
  path gets it from `close_view ≠ kill_session` + the Detached agents panel.
- **Selection/copy (#14):** tmux mouse-mode can capture selection on the CAO
  path (Option/Shift-drag forces local selection); the Rust path is unobstructed.
  This asymmetry is *the point* of the migration — note it, don't "fix" CAO.

## Exit criteria (to make Claude default to Rust PTY)

1. Rows 1-18 + 25-27: **B is at parity or better** vs A (no perceptible regression).
2. Rows 19-24 (lifecycle): all **Pass** for B — close keeps it alive, reopen
   replays, kill terminates the tree, no orphan on quit/force-quit.
3. Any divergence is either an accepted known-difference above or has a tracked
   fix. Then: Phase 7 (Claude → Rust PTY by default, CAO/tmux fallback on launch
   failure). Codex/Gemini/Grok stay on CAO/tmux.

## Phase 6 hardening items to confirm during the audit

- No blocking PTY read while holding a manager lock. (Reader holds only Arc
  clones — verified by design; confirm no UI stall under heavy output.)
- Reader exits cleanly + the session is removed on EOF (verified by
  `exited_session_is_removed_from_list`).
- ⚠ **Open finding:** on SIGTERM the managed backend exits but the `taime`
  process itself lingered past the 3s grace in dev once — re-check rows 23/24.
- Process-tree kill is currently baseline `child.kill()`; confirm no orphaned
  Claude subprocess after kill (row 22) / quit (row 23).
