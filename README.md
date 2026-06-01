# Taime

A premium native desktop app that orchestrates the four official AI coding CLIs —
**Claude Code** (Anthropic), **Codex CLI** (OpenAI), **Gemini CLI** (Google), and
**Grok Build CLI** (xAI) — through one calm, high-fidelity interface.

Taime drives the **real CLI binaries** (never raw provider APIs), so your
subscriptions and every native agent capability — Claude Pro/Max, ChatGPT
Plus/Pro, Gemini tiers, SuperGrok/X Premium+, Grok Plan Mode & subagents — stay
fully intact.

```
┌──────────────────────────────┬─────────────────────────────────────────┐
│  Control & Workspace          │  High-Performance Shell Grid              │
│  • Pipeline / session status  │  ┌───────────────┐ ┌───────────────┐     │
│  • Workspace (single project) │  │  Claude Code  │ │   Codex CLI   │     │
│  • Launch agent               │  │  (live PTY)   │ │  (live PTY)   │     │
│  • File inventory (dirty)     │  └───────────────┘ └───────────────┘     │
│  • Diff / approval matrix     │  ┌───────────────┐ ┌───────────────┐     │
│                               │  │  Gemini CLI   │ │ Grok Build CLI│     │
│                               │  └───────────────┘ └───────────────┘     │
└──────────────────────────────┴─────────────────────────────────────────┘
```

## Architecture

Taime is built on a fork of AWS Labs' `cli-agent-orchestrator` (CAO). Three
layers with clean ownership boundaries:

| Layer | Owns | Tech |
| --- | --- | --- |
| **Python** (`backend/cao`, run as a sidecar) | Orchestration source of truth: tmux-isolated CLI processes, terminal/session registry (SQLite), PTY-over-WebSocket streaming, MCP handoff/assign/send_message, diff | FastAPI, libtmux, FastMCP |
| **Rust** (`desktop/src-tauri`) | Process lifecycle (spawn/health/restart/shutdown of the backend), layered config, the file watcher + dirty-state events | Tauri v2, tokio, notify, reqwest |
| **React** (`desktop/src`) | UI/view state, optimistic launch, the two-column layout, terminal grid, diff viewer | React 19, TypeScript, Zustand, xterm.js (webgl), Monaco |

**Data bridges:** REST + binary-frame WebSocket (React ↔ Python), Tauri IPC +
events (Rust ↔ React), env vars at spawn (Rust → Python). The terminal stream
reuses CAO's PTY-attach endpoint (`pty.openpty()` → `tmux attach` →
`loop.add_reader` → batched `send_bytes`); xterm renders it on the GPU for 60fps.

## Key design decisions

- **Real binaries only.** No provider API keys are ever injected; each CLI
  inherits the user's on-machine login. The env allow/block list in
  `clients/tmux.py` preserves subscription auth.
- **Streaming** = the existing low-latency PTY-over-WebSocket path (Rust PTY
  ownership is a planned Phase B), not `capture_pane` polling.
- **Single active project** (v1): all agents share one workspace dir, so the
  watcher, dirty-state, and diff matrix have one clear scope.
- **Context-switch safety:** switching away from an agent that left uncommitted
  changes raises a guard — review the diff or knowingly proceed.

## Run it

Prereqs: Node 20+ & pnpm, Rust (stable) + platform toolchain, `tmux`, and the
CAO backend installed editable as `cao-server`
(`uv tool install --editable backend/cao`). The target CLIs
(`claude`, `codex`, `gemini`, `grok`) should be on `PATH` and logged in.

```bash
cd desktop
pnpm install
pnpm tauri dev          # Rust spawns + supervises cao-server automatically
```

Dev escape hatch — run the backend yourself and attach:

```bash
cao-server --host 127.0.0.1 --port 9889          # terminal 1
TAIME_EXTERNAL_BACKEND=1 pnpm tauri dev          # terminal 2
```

Config resolution (highest first): `TAIME_API_URL` env → `./.taimerc` →
`~/.taime/config.json` → built-in `http://127.0.0.1:9889`. See
`.taimerc.example`.

## Tests

```bash
cd desktop/src-tauri && cargo test     # Rust: config resolver + fs_watch (9 tests)
cd desktop          && pnpm typecheck  # TypeScript strict
```

## Status

Core product complete and verified end-to-end: native shell + supervised
backend, live terminal streaming (Claude & Grok verified bidirectional), the
two-column layout, file watcher + dirty state + context-switch guard, the Monaco
diff matrix, and the Grok Build provider (interactive TUI + `grok -p` fast path).

Planned enhancements (non-blocking): startup reconcile of stale terminal rows,
an orchestration-edge pipeline graph (from `emit_send_message` events), batched
status polling, and Rust-owned PTY streaming (Phase B).

## Open questions & risks

- **Auth posture:** the backend is loopback-only and unauthenticated by design
  (fine for a local Tauri app). Any remote/proxied use needs a real token on
  REST + WS first.
- **`get_status` is glyph-scraping** per provider and is version-fragile across
  CLI releases — Grok's markers are pinned to v0.2.14 and should be re-checked on
  upgrades.
- **One tmux-attach subprocess per open terminal** — lazily attach only focused
  frames at scale.
- **Single-writer SQLite** — exactly one `cao-server` must run (Rust enforces a
  singleton; it also adopts an already-healthy backend rather than duplicating).
