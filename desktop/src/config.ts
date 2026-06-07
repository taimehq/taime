/**
 * Backend descriptor. After the CAO/tmux removal there is no HTTP backend — the
 * `taime-session-daemon` is the backend (a Unix socket the Rust side bridges).
 * This is kept only so the status pill has something to show.
 */
export interface ResolvedConfig {
  apiUrl: string;
  wsUrl: string;
  host: string;
  port: number;
  externalBackend: boolean;
  source: string;
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

/** The backend is now the local session daemon — a static descriptor. */
export async function getConfig(): Promise<ResolvedConfig> {
  return { ...FALLBACK, source: "taime-session-daemon (local)" };
}
