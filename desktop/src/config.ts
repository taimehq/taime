import { invoke } from "@tauri-apps/api/core";

/**
 * The resolved backend location, owned by the Rust layer.
 * Field names are camelCase because the Rust struct uses
 * `#[serde(rename_all = "camelCase")]`.
 */
export interface ResolvedConfig {
  apiUrl: string; // http://host:port
  wsUrl: string; // ws://host:port
  host: string;
  port: number;
  externalBackend: boolean;
  source: string; // where the value came from (diagnostics)
}

// Browser-dev override: when running the frontend in a plain browser (no Tauri
// IPC), VITE_TAIME_API_URL lets you point at a specific backend instead of the
// built-in default. Has no effect inside the Tauri webview (IPC wins).
function fallbackFromEnv(): ResolvedConfig {
  const raw = (import.meta as { env?: Record<string, string> }).env
    ?.VITE_TAIME_API_URL;
  let host = "127.0.0.1";
  let port = 9889;
  if (raw) {
    const m = raw.replace(/^\w+:\/\//, "").match(/^([^:/]+):(\d+)/);
    if (m) {
      host = m[1];
      port = parseInt(m[2], 10);
    }
  }
  return {
    apiUrl: `http://${host}:${port}`,
    wsUrl: `ws://${host}:${port}`,
    host,
    port,
    externalBackend: false,
    source: raw
      ? `fallback (VITE_TAIME_API_URL=${raw})`
      : "fallback (Tauri IPC unavailable)",
  };
}

const FALLBACK: ResolvedConfig = fallbackFromEnv();

let cached: ResolvedConfig | null = null;

/**
 * Discover the backend address via the Rust `get_api_url` command.
 * Falls back to the built-in default if the Tauri bridge is unavailable
 * (e.g. running the frontend in a plain browser during development).
 */
export async function getConfig(): Promise<ResolvedConfig> {
  if (cached) return cached;
  try {
    cached = await invoke<ResolvedConfig>("get_backend_routing");
  } catch (e) {
    console.warn("[taime] get_api_url failed; using fallback", e);
    cached = FALLBACK;
  }
  return cached;
}
