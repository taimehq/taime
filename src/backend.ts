import { type UnlistenFn } from "@tauri-apps/api/event";

/** Backend descriptor for the status pill. The backend is now the local session
 *  daemon (no managed sidecar), so this is effectively static. */
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

/** The session daemon is the backend; report healthy inside Tauri. */
export async function getBackendStatus(): Promise<BackendState> {
  return {
    status: inTauri() ? "healthy" : "external",
    detail: "taime-session-daemon",
    external: false,
    pid: null,
    apiUrl: "",
  };
}

/** No live transitions to subscribe to (the daemon isn't a supervised sidecar);
 *  emit the current state once and return a no-op unlisten. */
export async function onBackendStatus(
  cb: (s: BackendState) => void,
): Promise<UnlistenFn> {
  cb(await getBackendStatus());
  return () => {};
}
