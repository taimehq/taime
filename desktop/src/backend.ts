import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** Mirrors the Rust `BackendState` (serde camelCase). */
export interface BackendState {
  status:
    | "starting"
    | "healthy"
    | "down"
    | "restarting"
    | "external"
    | "external_down";
  detail: string;
  external: boolean;
  pid: number | null;
  apiUrl: string;
}

export const UNKNOWN_BACKEND: BackendState = {
  status: "starting",
  detail: "Connecting to the Taime backend…",
  external: false,
  pid: null,
  apiUrl: "",
};

/** True when running inside the Tauri webview (vs. a plain browser). */
export function inTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** One-shot pull of the current supervisor state. Never throws. */
export async function getBackendStatus(): Promise<BackendState> {
  try {
    return await invoke<BackendState>("get_backend_status");
  } catch {
    return UNKNOWN_BACKEND;
  }
}

/**
 * Subscribe to live backend-status transitions emitted by Rust.
 * Never throws: Tauri's `listen` calls `transformCallback` synchronously and
 * throws outside the webview — so an unguarded call here would crash React's
 * effect commit and blank the screen. Returns a no-op unlisten on failure.
 */
export async function onBackendStatus(
  cb: (s: BackendState) => void,
): Promise<UnlistenFn> {
  try {
    return await listen<BackendState>("backend://status", (e) => cb(e.payload));
  } catch (e) {
    console.warn("[taime] backend status events unavailable", e);
    return () => {};
  }
}
