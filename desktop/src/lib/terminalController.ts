/**
 * Per-frame imperative command registry for terminals — the read-side sibling
 * of `terminalInput.ts`'s writer registry. Global shortcuts (Cmd+F / Cmd+G)
 * look up the ACTIVE frame's controller and drive it, without reaching into the
 * terminal's internals.
 *
 * Keyed on `frame.key` (the view), NOT terminal/session id: search is a view
 * command over the mounted frame, so it must survive a PTY detach/reopen that
 * keeps the same frame but swaps the underlying session.
 */

/** xterm search-match highlight colors (decorations are also what makes the
 *  resultCount in `onDidChangeResults` accurate, so always pass them). */
export const SEARCH_DECORATIONS = {
  matchBackground: "#4493f855",
  matchBorder: "#4493f800",
  matchOverviewRuler: "#4493f8",
  activeMatchBackground: "#d29922aa",
  activeMatchBorder: "#d29922",
  activeMatchColorOverviewRuler: "#d29922",
} as const;

export interface TerminalController {
  /** Open the frame's find bar (focuses its input). */
  openFind: () => void;
  /** Close the frame's find bar. */
  closeFind: () => void;
  /** Repeat the current search forward (no-op if the bar is closed/empty). */
  findNext: () => void;
  /** Repeat the current search backward. */
  findPrev: () => void;
}

const controllers = new Map<string, TerminalController>();

/** Register a frame's controller; returns an unregister fn. */
export function registerTerminalController(
  key: string,
  controller: TerminalController,
): () => void {
  controllers.set(key, controller);
  return () => {
    if (controllers.get(key) === controller) controllers.delete(key);
  };
}

/** Look up a frame's controller (null if none mounted). */
export function getTerminalController(
  key: string | null | undefined,
): TerminalController | null {
  if (!key) return null;
  return controllers.get(key) ?? null;
}
