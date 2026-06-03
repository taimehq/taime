import { invoke, Channel } from "@tauri-apps/api/core";
import { inTauri } from "./backend";

/**
 * Bridge to the detached **session daemon** — the one Rust PTY path for Claude.
 * The daemon owns the PTY + an authoritative wezterm-term grid and survives app
 * crashes; output streams as RAW BYTES over a per-session `Channel` (ArrayBuffer,
 * no base64), with exit/turn as small JSON control objects on the same channel.
 * (CAO/tmux remains for the other CLIs and as the launch fallback for Claude.)
 */

/** A message on the per-session data channel: raw output bytes, or a control
 * object (process exit, attribution turn, error). */
type PtyChannelMessage =
  | ArrayBuffer
  | { type: "exit"; code: number | null }
  | { type: "turn"; [k: string]: unknown }
  | { type: string; [k: string]: unknown };

/** An attribution turn boundary pushed by the daemon. */
export interface TurnEvent {
  epoch: number;
  startOffset: number;
  endOffset: number;
  startedCause: string;
  endedCause: string;
  commandExit: number | null;
}

/** A session enumerated from the daemon's own registry (for discovery/adoption
 * after an app crash, and for liveness). Matches the daemon's `SessionSummary`. */
export interface DaemonSessionSummary {
  id: string;
  cwd: string;
  program: string;
  alive: boolean;
  attached: boolean;
  rows: number;
  cols: number;
  created_at_unix: number;
  attribution_key: string | null;
  /** Provider id (`claude_code`/…) — daemon-reported (Phase 4), else null. */
  provider: string | null;
  /** Inferred status in CAO vocabulary (IDLE/PROCESSING/WAITING_USER_ANSWER/
   *  COMPLETED/ERROR) — daemon-reported (Phase 4), null when not yet known. */
  status: string | null;
  protocol_version: number;
}

/**
 * Launch any supported CLI (`claude_code`/`codex`/`gemini_cli`/`grok_cli`) on the
 * detached session daemon via its provider registry (the daemon owns the launch
 * recipe + MCP injection). The default (unrestricted) profile is used; richer
 * profiles still route through CAO until the daemon learns them.
 */
export async function daemonSpawnAgent(
  provider: string,
  cwd: string | null,
  rows: number,
  cols: number,
  attributionKey: string | null,
  model: string | null = null,
): Promise<string> {
  return invoke<string>("daemon_spawn_agent", {
    provider,
    cwd: cwd ?? null,
    rows,
    cols,
    model: model ?? null,
    permissionMode: null,
    attributionKey: attributionKey ?? null,
  });
}

/** Daemon-owned worktree provisioning result (Phase 3). snake_case to match the
 *  Rust `WorktreeInfo`; `terminal_key` is the attribution id. */
export interface DaemonWorktreeInfo {
  terminal_key: string;
  project_root: string;
  repo_root: string | null;
  worktree_path: string;
  branch: string | null;
  base_sha: string | null;
  mode: string; // "worktree" | "shared"
  error: string | null;
}

/** Provision (or resolve) an isolated git worktree for a daemon agent — the
 *  daemon-owned replacement for CAO's /worktrees/provision. */
export async function daemonProvisionWorktree(
  projectRoot: string,
  provider: string,
  isolate: boolean,
): Promise<DaemonWorktreeInfo> {
  return invoke<DaemonWorktreeInfo>("daemon_provision_worktree", {
    projectRoot,
    provider,
    isolate,
  });
}

/** The daemon-side activity graph (Phase 6): agents + inter-agent edges
 *  (assign/handoff/message), read from the durable store — complete even with the
 *  UI closed. The frontend route switch to this lands with the diff move. */
export interface DaemonActivityGraph {
  agents: { id: string; provider: string | null; status: string | null }[];
  edges: { kind: string; source: string; target: string }[];
}

export async function daemonActivityGraph(): Promise<DaemonActivityGraph> {
  return invoke<DaemonActivityGraph>("daemon_activity_graph");
}

/** Enqueue an inbox message for a live daemon agent (Phase 5 message bus). The
 *  daemon delivers it into the receiver's stdin when it next goes idle. `receiver`
 *  is the agent's attribution id; returns the monotonic inbox id. */
export async function daemonSendMessage(
  sender: string,
  receiver: string,
  message: string,
): Promise<number> {
  return invoke<number>("daemon_send_message", { sender, receiver, message });
}

export async function daemonWrite(sessionId: string, data: string): Promise<void> {
  if (!inTauri()) return;
  try {
    await invoke("daemon_write", { sessionId, data });
  } catch (e) {
    console.warn("[taime] daemon_write failed", e);
  }
}

export async function daemonResize(sessionId: string, rows: number, cols: number): Promise<void> {
  if (!inTauri()) return;
  try {
    await invoke("daemon_resize", { sessionId, rows, cols });
  } catch {
    /* ignore */
  }
}

export async function daemonAck(sessionId: string, offset: number): Promise<void> {
  if (!inTauri()) return;
  try {
    await invoke("daemon_ack", { sessionId, offset });
  } catch {
    /* ignore */
  }
}

/** App-driven attribution checkpoint (strongest boundary signal) — e.g. when the
 *  user submits a command. */
export async function daemonCheckpoint(sessionId: string, cause: string): Promise<void> {
  if (!inTauri()) return;
  try {
    await invoke("daemon_checkpoint", { sessionId, cause });
  } catch {
    /* ignore */
  }
}

/** Detach the view — the daemon keeps the agent running (close ≠ kill). */
export async function daemonCloseView(sessionId: string): Promise<void> {
  if (!inTauri()) return;
  try {
    await invoke("daemon_close_view", { sessionId });
  } catch {
    /* ignore */
  }
}

export async function daemonKill(sessionId: string): Promise<void> {
  if (!inTauri()) return;
  try {
    await invoke("daemon_kill", { sessionId });
  } catch {
    /* ignore */
  }
}

/** Enumerate the detached daemon's sessions. Returns [] (without spawning a
 *  daemon) when none is running — safe to call on every boot. */
export async function daemonList(): Promise<DaemonSessionSummary[]> {
  if (!inTauri()) return [];
  try {
    return await invoke<DaemonSessionSummary[]>("daemon_list");
  } catch {
    return [];
  }
}

/** Whether the daemon transport is usable (binary resolvable or already
 *  running). The launcher routes Claude → daemon when true, else CAO. */
export async function daemonAvailable(): Promise<boolean> {
  if (!inTauri()) return false;
  try {
    return await invoke<boolean>("daemon_available");
  } catch {
    return false;
  }
}

/**
 * Attach to a daemon session. The grid repaint (at the supplied viewport) +
 * live output arrive as `ArrayBuffer` via `onBytes`; control objects arrive as
 * JSON: `exit` → `onExit`, `turn` → `onTurn`. Returns the `Channel` — the caller
 * must keep it alive (GC of it silently stops output).
 */
export async function daemonAttach(
  sessionId: string,
  rows: number,
  cols: number,
  onBytes: (bytes: Uint8Array) => void,
  onExit: (code: number | null) => void,
  onTurn?: (turn: TurnEvent) => void,
): Promise<Channel<PtyChannelMessage> | null> {
  if (!inTauri()) return null;
  const onData = new Channel<PtyChannelMessage>();
  onData.onmessage = (msg) => {
    if (msg instanceof ArrayBuffer) {
      onBytes(new Uint8Array(msg));
    } else if (ArrayBuffer.isView(msg)) {
      const view = msg as ArrayBufferView;
      onBytes(new Uint8Array(view.buffer, view.byteOffset, view.byteLength));
    } else if (msg && typeof msg === "object") {
      const m = msg as { type?: string; code?: number | null };
      if (m.type === "exit") onExit(m.code ?? null);
      else if (m.type === "turn" && onTurn) onTurn(msg as unknown as TurnEvent);
      else if (m.type === "error") console.warn("[taime] daemon error", msg);
    }
  };
  try {
    // rows/cols let the daemon resize the PTY + emulator BEFORE the grid repaint,
    // so the repaint matches the real viewport (handoff step 1).
    await invoke("daemon_attach", { sessionId, rows, cols, onData });
    return onData;
  } catch (e) {
    console.warn("[taime] daemon_attach failed", e);
    return null;
  }
}
