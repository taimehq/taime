import { useEffect } from "react";
import { inTauri } from "../backend";
import { useStore } from "../store";
import { sendToTerminal, formatDroppedPaths } from "../lib/terminalInput";

/**
 * Drag a file or screenshot from Finder onto a terminal → its absolute path is
 * typed at that terminal's cursor (works for both transports). Claude Code (and
 * other CLIs) read images/files by path, so this covers "drop a screenshot in".
 *
 * Tauri captures the OS drop at the webview level and gives us REAL absolute
 * paths + a drop position (the HTML `drop` event does not fire / has no paths).
 * We hit-test the position to find which terminal frame received the drop, and
 * fall back to the active frame. Native-only (no real paths in a dev browser).
 */
export function useTerminalFileDrop() {
  useEffect(() => {
    if (!inTauri()) return;
    let cancelled = false;
    let unlisten: (() => void) | undefined;

    const termKeyForActiveFrame = (): string | null => {
      const s = useStore.getState();
      const f = s.frames.find((fr) => fr.key === s.activeFrameKey);
      if (!f) return null;
      return (f.transport === "rust_pty" ? f.ptySessionId : f.terminalId) ?? null;
    };

    (async () => {
      try {
        const { getCurrentWebview } = await import("@tauri-apps/api/webview");
        const un = await getCurrentWebview().onDragDropEvent((event) => {
          if (event.payload.type !== "drop") return;
          const paths = (event.payload.paths ?? []) as string[];
          const text = formatDroppedPaths(paths);
          if (!text) return;

          // Find the terminal under the drop point (position is physical px).
          const dpr = window.devicePixelRatio || 1;
          const x = event.payload.position.x / dpr;
          const y = event.payload.position.y / dpr;
          const el = document.elementFromPoint(x, y) as HTMLElement | null;
          const frameEl = el?.closest("[data-term-key]") as HTMLElement | null;
          const key = frameEl?.dataset.termKey ?? termKeyForActiveFrame();

          sendToTerminal(key, text);
        });
        if (cancelled) un();
        else unlisten = un;
      } catch (e) {
        console.warn("[taime] file-drop wiring failed", e);
      }
    })();

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
}
