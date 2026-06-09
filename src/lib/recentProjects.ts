/**
 * localStorage-backed persistence for the workspace picker: the last-used
 * project directory + a most-recent-first history. Works identically in the
 * Tauri webview and a plain dev browser (both have localStorage).
 */
const RECENTS_KEY = "taime.recentProjects";
const WORKSPACE_KEY = "taime.workspaceDir";
const CAP = 8;

export function loadRecentProjects(): string[] {
  try {
    const raw = localStorage.getItem(RECENTS_KEY);
    if (!raw) return [];
    const arr = JSON.parse(raw);
    return Array.isArray(arr) ? arr.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

export function saveRecentProjects(list: string[]): void {
  try {
    localStorage.setItem(RECENTS_KEY, JSON.stringify(list.slice(0, CAP)));
  } catch {
    /* storage unavailable — non-fatal */
  }
}

export function loadWorkspaceDir(): string | null {
  try {
    return localStorage.getItem(WORKSPACE_KEY) || null;
  } catch {
    return null;
  }
}

export function saveWorkspaceDir(dir: string | null): void {
  try {
    if (dir) localStorage.setItem(WORKSPACE_KEY, dir);
    else localStorage.removeItem(WORKSPACE_KEY);
  } catch {
    /* storage unavailable — non-fatal */
  }
}

/** Move `path` to the front of the history (dedup, capped). */
export function addRecent(list: string[], path: string): string[] {
  const normalized = path.replace(/\/+$/, "");
  return [normalized, ...list.filter((p) => p.replace(/\/+$/, "") !== normalized)].slice(0, CAP);
}

/** Last path segment, for a friendly display name. */
export function basename(path: string): string {
  const parts = path.replace(/\/+$/, "").split("/");
  return parts[parts.length - 1] || path;
}

/** Parent directory, shown muted under the basename. */
export function dirname(path: string): string {
  const clean = path.replace(/\/+$/, "");
  const idx = clean.lastIndexOf("/");
  return idx > 0 ? clean.slice(0, idx) : "/";
}
