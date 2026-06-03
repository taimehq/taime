import { invoke, Channel } from "@tauri-apps/api/core";
import { inTauri } from "./backend";

/**
 * Bridge to the Rust-owned PTY (the Claude transport).
 *
 * Transport (Step 0a): output streams as RAW BYTES over a per-session
 * `Channel<InvokeResponseBody>`. Data chunks arrive as `ArrayBuffer` (no base64);
 * a process exit arrives as a small JSON control object on the same ordered
 * channel. Mirrors the operations the Rust PtyManager exposes, including the
 * close-view ≠ kill distinction and the backpressure ack (Step 0b).
 */

export interface PtySessionInfo {
  id: string;
  cwd: string;
  attached: boolean;
  alive: boolean;
}

/** A message on the per-session data channel: raw output bytes, or a control
 * object (currently only the exit notice). */
type PtyChannelMessage = ArrayBuffer | { type: "exit"; code: number | null };

export async function ptySpawnClaude(
  cwd: string | null,
  rows: number,
  cols: number,
): Promise<string> {
  return invoke<string>("pty_spawn_claude", { cwd: cwd ?? null, rows, cols });
}

export async function ptyWrite(sessionId: string, data: string): Promise<void> {
  if (!inTauri()) return;
  try {
    await invoke("pty_write", { sessionId, data });
  } catch (e) {
    console.warn("[taime] pty_write failed", e);
  }
}

export async function ptyResize(
  sessionId: string,
  rows: number,
  cols: number,
): Promise<void> {
  if (!inTauri()) return;
  try {
    await invoke("pty_resize", { sessionId, rows, cols });
  } catch {
    /* ignore */
  }
}

/** Detach the view — does NOT kill the agent (it keeps running). */
export async function ptyCloseView(sessionId: string): Promise<void> {
  if (!inTauri()) return;
  try {
    await invoke("pty_close_view", { sessionId });
  } catch {
    /* ignore */
  }
}

/**
 * Attach a view to a session over a binary channel. The retained scrollback
 * replays as the first `onBytes` call; live output follows; `onExit` fires when
 * the process exits. Returns the `Channel` — the **caller must keep the returned
 * reference alive** for the lifetime of the view, or its `onmessage` is GC'd and
 * output silently stops.
 */
export async function ptyAttach(
  sessionId: string,
  onBytes: (bytes: Uint8Array) => void,
  onExit: (code: number | null) => void,
): Promise<Channel<PtyChannelMessage> | null> {
  if (!inTauri()) return null;
  const onData = new Channel<PtyChannelMessage>();
  onData.onmessage = (msg) => {
    if (msg instanceof ArrayBuffer) {
      onBytes(new Uint8Array(msg));
    } else if (ArrayBuffer.isView(msg)) {
      // Defensive: some runtimes hand back a typed-array view rather than a bare
      // ArrayBuffer for raw sends.
      const view = msg as ArrayBufferView;
      onBytes(new Uint8Array(view.buffer, view.byteOffset, view.byteLength));
    } else if (msg && typeof msg === "object" && (msg as { type?: string }).type === "exit") {
      onExit((msg as { code: number | null }).code ?? null);
    }
  };
  try {
    await invoke("pty_attach", { sessionId, onData });
    return onData;
  } catch (e) {
    console.warn("[taime] pty_attach failed", e);
    return null;
  }
}

/**
 * Backpressure ack (Step 0b): report the highest byte offset the client has
 * processed (xterm's write-callback fired). Batched ~once per frame by the
 * caller; monotonic. The manager pauses reading the PTY when sent−acked exceeds
 * the high watermark.
 */
export async function ptyAck(sessionId: string, offset: number): Promise<void> {
  if (!inTauri()) return;
  try {
    await invoke("pty_ack", { sessionId, offset });
  } catch {
    /* ignore */
  }
}

/** Explicitly terminate the agent process. */
export async function ptyKill(sessionId: string): Promise<void> {
  if (!inTauri()) return;
  try {
    await invoke("pty_kill", { sessionId });
  } catch {
    /* ignore */
  }
}

export async function ptyList(): Promise<PtySessionInfo[]> {
  if (!inTauri()) return [];
  try {
    return await invoke<PtySessionInfo[]>("pty_list");
  } catch {
    return [];
  }
}

// ---------------------------------------------------------------------------
// Session daemon transport (Step 2). Same binary-channel shape as the in-app
// path; the bytes originate in the detached daemon that survives app crashes.
// ---------------------------------------------------------------------------

/** An attribution turn boundary pushed by the daemon. */
export interface TurnEvent {
  epoch: number;
  startOffset: number;
  endOffset: number;
  startedCause: string;
  endedCause: string;
  commandExit: number | null;
}

export async function daemonSpawnClaude(
  cwd: string | null,
  rows: number,
  cols: number,
  attributionKey: string | null,
): Promise<string> {
  return invoke<string>("daemon_spawn_claude", {
    cwd: cwd ?? null,
    rows,
    cols,
    attributionKey: attributionKey ?? null,
  });
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

/** Detach the view — the daemon keeps the agent running. */
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

/** Liveness list from the detached daemon (its own session registry). */
export async function daemonList(): Promise<{ id: string; alive: boolean }[]> {
  if (!inTauri()) return [];
  try {
    return await invoke<{ id: string; alive: boolean }[]>("daemon_list");
  } catch {
    return [];
  }
}

/**
 * Attach to a daemon session. Output (repaint + live) arrives as `ArrayBuffer`
 * via `onBytes`; control objects arrive as JSON: `exit` → `onExit`, `turn` →
 * `onTurn`. Returns the `Channel` — the caller must keep it alive.
 */
export async function daemonAttach(
  sessionId: string,
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
    await invoke("daemon_attach", { sessionId, onData });
    return onData;
  } catch (e) {
    console.warn("[taime] daemon_attach failed", e);
    return null;
  }
}

/** Transport binding so a terminal view can be backed by either the in-app PTY
 * manager or the detached daemon without knowing which. */
export type PtyBackend = "inapp" | "daemon";

export interface PtyTransport {
  attach: (
    sessionId: string,
    onBytes: (bytes: Uint8Array) => void,
    onExit: (code: number | null) => void,
    onTurn?: (turn: TurnEvent) => void,
  ) => Promise<Channel<PtyChannelMessage> | null>;
  write: (sessionId: string, data: string) => Promise<void>;
  resize: (sessionId: string, rows: number, cols: number) => Promise<void>;
  ack: (sessionId: string, offset: number) => Promise<void>;
  closeView: (sessionId: string) => Promise<void>;
}

const inAppTransport: PtyTransport = {
  // The in-app PtyManager emits no turn events (attribution lives in the daemon);
  // ignore onTurn.
  attach: (sessionId, onBytes, onExit) => ptyAttach(sessionId, onBytes, onExit),
  write: ptyWrite,
  resize: ptyResize,
  ack: ptyAck,
  closeView: ptyCloseView,
};

const daemonTransportImpl: PtyTransport = {
  attach: (sessionId, onBytes, onExit, onTurn) =>
    daemonAttach(sessionId, onBytes, onExit, onTurn),
  write: daemonWrite,
  resize: daemonResize,
  ack: daemonAck,
  closeView: daemonCloseView,
};

export function transportFor(backend: PtyBackend): PtyTransport {
  return backend === "daemon" ? daemonTransportImpl : inAppTransport;
}
