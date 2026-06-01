# Taime — desktop app

Native Tauri v2 shell that orchestrates the official AI coding CLIs (Claude Code,
Codex, Gemini, Grok Build) by driving the **real** CLI binaries through the CAO
Python engine. This package is the desktop product; the orchestration engine
lives at `../backend/cao`.

## Prerequisites

- Node 20+ and `pnpm`
- Rust (stable) + platform toolchain (Xcode CLT on macOS)
- The CAO backend on PATH as `cao-server` (the dev install: `uv tool install`
  from `../backend/cao`, exposing `cao-server`, `cao`, `cao-mcp-server`)
- `tmux` (used by the backend for process isolation)

## Run (development)

```bash
cd desktop
pnpm install
pnpm tauri dev
```

On launch, the Rust layer:

1. resolves the backend address (see **Configuration**),
2. spawns and supervises `cao-server` (managed mode) — or attaches to an
   already-running one (external mode),
3. exposes the resolved URL to the React UI via the `get_api_url` command,
4. emits live `backend://status` events that drive the status pill.

## Configuration

Resolution order (highest priority first):

1. `TAIME_API_URL` env var (e.g. `http://127.0.0.1:9889`)
2. project-local `./.taimerc` (JSON) — see `../.taimerc.example`
3. user `~/.taime/config.json` (JSON)
4. built-in default `http://127.0.0.1:9889`

Useful env vars:

| Var | Effect |
| --- | --- |
| `TAIME_API_URL` | Set backend host+port in one shot |
| `TAIME_EXTERNAL_BACKEND=1` | Don't spawn `cao-server`; attach to a running one (dev escape hatch) |
| `TAIME_BACKEND_CMD` | Override the launch command (default `cao-server`) |

### Dev escape hatch (external backend)

Run the backend yourself, then start the app against it:

```bash
# terminal 1
cao-server --host 127.0.0.1 --port 9889

# terminal 2
TAIME_EXTERNAL_BACKEND=1 pnpm tauri dev
```

## Layout

```
desktop/
├── src/                 React 19 + TS + Tailwind frontend
│   ├── config.ts        get_api_url bridge → resolved backend URL
│   ├── api.ts           REST client (BASE = resolved apiUrl)
│   ├── backend.ts       backend status types + event subscription
│   └── components/      BackendStatusPill, ProviderCard, ...
└── src-tauri/           Rust shell
    └── src/
        ├── main.rs      Builder, state, graceful-shutdown hook
        ├── config.rs    layered config resolver (unit-tested)
        ├── backend.rs   cao-server supervision (spawn/health/restart/shutdown)
        └── commands.rs  get_api_url, get_backend_status
```

## Tests

```bash
cd src-tauri && cargo test     # config resolver (6 tests)
cd ..        && pnpm typecheck  # TS strict typecheck
```
