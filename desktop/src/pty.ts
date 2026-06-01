import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { inTauri } from "./backend";

/**
 * Bridge to the Rust-owned PTY (the Claude transport). Output streams as
 * `pty://{id}/data` (base64) and `pty://{id}/exit`. Mirrors the operations the
 * Rust PtyManager exposes, including the close-view ≠ kill distinction.
 */

export interface PtySessionInfo {
  id: string;
  cwd: string;
  attached: boolean;
  alive: boolean;
}

function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

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

/** Reattach — returns the scrollback to replay into a fresh terminal. */
export async function ptyReattachView(sessionId: string): Promise<Uint8Array> {
  if (!inTauri()) return new Uint8Array();
  try {
    const b64 = await invoke<string>("pty_reattach_view", { sessionId });
    return b64ToBytes(b64);
  } catch (e) {
    console.warn("[taime] pty_reattach_view failed", e);
    return new Uint8Array();
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

/** Subscribe to a session's output. Returns an unlisten fn. */
export async function onPtyData(
  sessionId: string,
  cb: (bytes: Uint8Array) => void,
): Promise<UnlistenFn> {
  if (!inTauri()) return () => {};
  try {
    return await listen<string>(`pty://${sessionId}/data`, (e) =>
      cb(b64ToBytes(e.payload)),
    );
  } catch {
    return () => {};
  }
}

/** Subscribe to a session's exit. Returns an unlisten fn. */
export async function onPtyExit(
  sessionId: string,
  cb: (code: number | null) => void,
): Promise<UnlistenFn> {
  if (!inTauri()) return () => {};
  try {
    return await listen<number | null>(`pty://${sessionId}/exit`, (e) =>
      cb(e.payload),
    );
  } catch {
    return () => {};
  }
}
