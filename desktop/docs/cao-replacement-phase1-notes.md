# CAO replacement — Phase 1 implementation notes

**Status:** built. Companion to [`cao-replacement-plan.md`](./cao-replacement-plan.md)
(the plan of record) and [`terminal-daemon-implementation-notes.md`](./terminal-daemon-implementation-notes.md).

Phase 1 = **the provider adapter trait + TOML-backed registry + a generalized,
all-CLI daemon spawn**. Before this, the daemon could only launch Claude with a
hardcoded binary + args (`commands.rs` `claude_binary`/`claude_args` →
`SpawnSpec`). Now the daemon owns a provider **registry** that builds the launch
command + MCP injection for any of the four CLIs — the recipe lives daemon-side
so Phase-5 headless `assign` can spawn workers without the app.

## What landed

### Wire protocol (`taime-protocol`)
- `McpServerConfig` — one MCP server entry (`name`/`command`/`args`/`env`); the
  daemon stamps `CAO_TERMINAL_ID = attribution_key` into each before injecting.
- `AgentProfile` — the CAO `agent_profile` decomposed into launch data
  (`system_prompt`, `model`, `permission_mode`, `allowed_tools`, `native_agent`,
  `codex_profile`, `mcp_servers`). All-`None` = the default unrestricted launch.
- `AgentSpawnSpec` — high-level "launch provider X" request (provider id +
  profile + cwd + rows/cols + attribution_key + seed_prompt + env).
- `ClientMsg::SpawnAgent { req_id, spec }` — the new spawn path; the daemon's
  registry turns it into the concrete command. `Spawn`/`SpawnSpec` stay for the
  low-level path (daemon integration tests).
- `AgentStatus` (Idle/Processing/WaitingUserAnswer/Completed/Error) — defined now
  (adapters compute it); wired onto `SessionSummary` + a push event in Phase 4.

### Daemon provider registry (`taime-session-daemon/src/providers/`)
- `mod.rs` — the `Provider` trait (`build` = command + MCP injection + cleanup;
  plus read-only `status`/`approval_prompt`/`extract_response`/`idle_pattern`/
  `paste_enter_count` over a `GridView`), the `Registry` (TOML defaults + the
  built-in adapters), `DaemonSessionSpec` (generalizes `SpawnSpec` with
  `env_remove` + `paste_enter_count`), and the data-driven `Cleanup`
  (`RemoveFile`/`RemoveJsonMcpServers`/`RemoveDir`).
- `config.rs` — `~/.taime/providers.toml` over a built-in baseline (binary,
  base_args, model_flag, env), plus the gemini `settings.json` merge/restore
  helpers (preserve unrelated keys; drop an emptied `mcpServers`).
- `claude.rs` / `codex.rs` / `gemini.rs` / `grok.rs` — the four adapters, ported
  from CAO's `providers/*` recipes (command construction, MCP injection
  mechanism + cleanup, status/approval heuristics, `paste_enter_count`).

Per-provider MCP injection mechanism (parity with CAO):

| provider | command MCP injection | cleanup |
|---|---|---|
| `claude_code` | inline `--mcp-config <json>` (each server + stamped `CAO_TERMINAL_ID`) | none |
| `codex` | per-field `-c mcp_servers.<n>.{command,args,env,env_vars,tool_timeout_sec}` + `CAO_TERMINAL_ID` via `env_vars` inheritance (set in child env) | none |
| `gemini_cli` | merge into `~/.gemini/settings.json` `mcpServers` | remove the added keys |
| `grok_cli` | none (CAO v1 injects none) | none |

Other parity: Claude unsets inherited `CLAUDE*` (except the bedrock/vertex/
foundry/effort allowlist) via `DaemonSessionSpec.env_remove`; codex drops
`CODEX_*`; `paste_enter_count` is 2 for Claude, 1 for the rest. Unlike CAO (which
types the command into a shell in a fresh tmux pane), the daemon spawns the agent
binary **directly** as the PTY child — so codex's `echo ready` warm-up and the
gemini `cd <ws> &&` wrapper become "set the child cwd," and there is no shell
baseline to detect.

### Session lifecycle (`session.rs`/`manager.rs`/`conn.rs`)
- `Session::spawn_prepared` spawns a registry-built `Prepared` (command + cleanup
  + provider id); `spawn` keeps the low-level `SpawnSpec` path. The shared
  `spawn_inner` applies `env_remove` before overrides and runs the `Cleanup` on
  the reader's EOF/reap path (covers explicit kill, which routes through EOF).
- `Manager::spawn_agent` builds via the `Registry` and spawns. `conn.rs`
  dispatches `ClientMsg::SpawnAgent`.

### App + frontend
- `commands.rs`: `daemon_spawn_agent(provider, …)`; `daemon_spawn_claude` now
  delegates to the registry (no app-side claude binary/args). `daemon.rs`
  `spawn_agent` sends `SpawnAgent`.
- `pty.ts` `daemonSpawnAgent`; `store.ts` `launchAgentDaemon(provider)` (was
  `launchClaudeDaemon`) routes ALL four providers to the daemon when the profile
  is `default` and there's no target session; non-default profiles / sessions
  still go through CAO. `adoptDaemonSession` infers the provider from the session
  program (correct labels for adopted non-Claude survivors).

## Deferred (marked `NOTE(phase1)` in code)
- Restricted-tool `--disallowedTools` derivation (needs CAO's `tool_mapping`
  table) and codex's full `SECURITY_PROMPT` — restricted profiles still route
  through CAO until ported.
- Gemini workspace pre-trust + policy-deny TOML for restricted tools.
- Richer profiles (system prompt / MCP) over the daemon: the app currently routes
  only the **default** profile to the daemon; passing a fully-resolved
  `AgentProfile` from the app (via CAO's `get_profile_details`) is a follow-up.
- MCP endpoint is the **profile's** servers (CAO's `cao-mcp-server`) "wired to
  CAO's MCP for now"; Phase 5 swaps in the daemon's own endpoint — same
  `McpServerConfig` shape, no command-construction change.

## Verification
- `cargo test -p taime-protocol -p taime-session-daemon`: protocol round-trips
  (incl. `SpawnAgent`), 42 daemon unit tests (registry + each adapter's command
  construction, MCP injection, status transitions, gemini settings.json
  round-trip), 2 integration tests — all green.
- `cargo build -p taime -p taime-session-daemon`: zero warnings.
- `pnpm typecheck`: clean.
