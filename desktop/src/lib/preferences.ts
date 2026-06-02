/**
 * localStorage-backed UI preferences. Same plain-helper pattern as
 * `lib/recentProjects.ts` — works identically in the Tauri webview and a dev
 * browser (both have localStorage). We deliberately avoid Zustand persist
 * middleware for a couple of scalar settings; the store seeds from these
 * loaders and writes back through the savers.
 */

const TERMINAL_FONT_SIZE_KEY = "taime.terminalFontSize";

export const TERMINAL_FONT_SIZE_DEFAULT = 13;
export const TERMINAL_FONT_SIZE_MIN = 9;
export const TERMINAL_FONT_SIZE_MAX = 24;

/** Clamp to the supported range and snap to whole pixels. */
export function clampTerminalFontSize(n: number): number {
  if (!Number.isFinite(n)) return TERMINAL_FONT_SIZE_DEFAULT;
  return Math.min(
    TERMINAL_FONT_SIZE_MAX,
    Math.max(TERMINAL_FONT_SIZE_MIN, Math.round(n)),
  );
}

export function loadTerminalFontSize(): number {
  try {
    const raw = localStorage.getItem(TERMINAL_FONT_SIZE_KEY);
    if (!raw) return TERMINAL_FONT_SIZE_DEFAULT;
    return clampTerminalFontSize(Number(raw));
  } catch {
    return TERMINAL_FONT_SIZE_DEFAULT;
  }
}

export function saveTerminalFontSize(size: number): void {
  try {
    localStorage.setItem(
      TERMINAL_FONT_SIZE_KEY,
      String(clampTerminalFontSize(size)),
    );
  } catch {
    /* storage unavailable — non-fatal */
  }
}

// --- Sidebar (collapse + resizable width) ---

const SIDEBAR_WIDTH_KEY = "taime.sidebarWidth";
const SIDEBAR_COLLAPSED_KEY = "taime.sidebarCollapsed";

export const SIDEBAR_WIDTH_DEFAULT = 320; // matches the old w-80
export const SIDEBAR_WIDTH_MIN = 220;
export const SIDEBAR_WIDTH_MAX = 480;
/** Always leave at least this much room for the shell grid. */
const SIDEBAR_GRID_RESERVE = 320;

export function clampSidebarWidth(n: number): number {
  if (!Number.isFinite(n)) return SIDEBAR_WIDTH_DEFAULT;
  let w = Math.min(SIDEBAR_WIDTH_MAX, Math.max(SIDEBAR_WIDTH_MIN, Math.round(n)));
  // Never let the sidebar crowd out the grid on a small window — clamp against
  // the live viewport too, not just the static max.
  if (typeof window !== "undefined") {
    const maxForViewport = Math.max(
      SIDEBAR_WIDTH_MIN,
      window.innerWidth - SIDEBAR_GRID_RESERVE,
    );
    w = Math.min(w, maxForViewport);
  }
  return w;
}

export function loadSidebarWidth(): number {
  try {
    const raw = localStorage.getItem(SIDEBAR_WIDTH_KEY);
    return clampSidebarWidth(raw ? Number(raw) : SIDEBAR_WIDTH_DEFAULT);
  } catch {
    return SIDEBAR_WIDTH_DEFAULT;
  }
}

export function saveSidebarWidth(px: number): void {
  try {
    localStorage.setItem(SIDEBAR_WIDTH_KEY, String(clampSidebarWidth(px)));
  } catch {
    /* non-fatal */
  }
}

export function loadSidebarCollapsed(): boolean {
  try {
    return localStorage.getItem(SIDEBAR_COLLAPSED_KEY) === "1";
  } catch {
    return false;
  }
}

export function saveSidebarCollapsed(collapsed: boolean): void {
  try {
    localStorage.setItem(SIDEBAR_COLLAPSED_KEY, collapsed ? "1" : "0");
  } catch {
    /* non-fatal */
  }
}
