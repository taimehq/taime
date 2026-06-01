import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { inTauri } from "./backend";

/** Payload of the `terminal://{id}/fs-dirty` event (Rust serde camelCase). */
export interface DirtyPayload {
  terminalId: string;
  count: number;
  paths: string[];
}

/** One attributed file change (Rust serde camelCase). */
export interface FileEvent {
  path: string;
  kind: "create" | "modify" | "delete" | string;
  ts: number;
}

/** Payload of the `terminal://{id}/fs-event` channel: a debounce-window batch. */
export interface FsEventBatch {
  terminalId: string;
  events: FileEvent[];
}

/** Begin watching a terminal's working dir. No-op outside the Tauri webview. */
export async function watchTerminal(
  terminalId: string,
  dir: string,
): Promise<void> {
  if (!inTauri()) return;
  try {
    await invoke("watch_terminal", { terminalId, dir });
  } catch (e) {
    console.warn("[taime] watch_terminal failed", e);
  }
}

export async function unwatchTerminal(terminalId: string): Promise<void> {
  if (!inTauri()) return;
  try {
    await invoke("unwatch_terminal", { terminalId });
  } catch {
    /* ignore */
  }
}

export async function clearDirty(terminalId: string): Promise<void> {
  if (!inTauri()) return;
  try {
    await invoke("clear_dirty", { terminalId });
  } catch {
    /* ignore */
  }
}

/** Subscribe to dirty events for one terminal. Returns an unlisten fn. */
export async function onDirty(
  terminalId: string,
  cb: (p: DirtyPayload) => void,
): Promise<UnlistenFn> {
  if (!inTauri()) return () => {};
  try {
    return await listen<DirtyPayload>(
      `terminal://${terminalId}/fs-dirty`,
      (e) => cb(e.payload),
    );
  } catch {
    return () => {};
  }
}

/** Subscribe to the per-file attributed event stream. Returns an unlisten fn. */
export async function onFsEvent(
  terminalId: string,
  cb: (b: FsEventBatch) => void,
): Promise<UnlistenFn> {
  if (!inTauri()) return () => {};
  try {
    return await listen<FsEventBatch>(
      `terminal://${terminalId}/fs-event`,
      (e) => cb(e.payload),
    );
  } catch {
    return () => {};
  }
}
