import { useEffect } from "react";
import { inTauri } from "../backend";
import { useStore } from "../store";
import {
  sendToTerminal,
  formatDroppedPaths,
  isImagePath,
  setClipboardImageFromPath,
  PASTE_TRIGGER,
} from "../lib/terminalInput";

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

    // Find the frame under a drop point. Tauri's position is physical pixels and
    // can be ambiguous vs CSS pixels across displays, so test each frame's rect
    // with BOTH the raw and DPR-scaled point — whichever lands inside wins.
    const termKeyAtPoint = (px: number, py: number): string | null => {
      const dpr = window.devicePixelRatio || 1;
      const points: [number, number][] = [
        [px, py],
        [px / dpr, py / dpr],
      ];
      const els = Array.from(
        document.querySelectorAll<HTMLElement>("[data-term-key]"),
      );
      for (const [x, y] of points) {
        for (const el of els) {
          const r = el.getBoundingClientRect();
          if (x >= r.left && x <= r.right && y >= r.top && y <= r.bottom) {
            return el.dataset.termKey ?? null;
          }
        }
      }
      return null;
    };

    (async () => {
      try {
        const { getCurrentWebview } = await import("@tauri-apps/api/webview");
        const un = await getCurrentWebview().onDragDropEvent((event) => {
          if (event.payload.type !== "drop") return;
          const paths = (event.payload.paths ?? []) as string[];
          if (!paths.length) return;

          // Route to the frame under the cursor; fall back to the active frame.
          const pos = event.payload.position;
          const key = termKeyAtPoint(pos.x, pos.y) ?? termKeyForActiveFrame();
          if (!key) return;

          const images = paths.filter(isImagePath);
          const others = paths.filter((p) => !isImagePath(p));

          // Non-image files → insert their (escaped) paths as text.
          if (others.length) sendToTerminal(key, formatDroppedPaths(others));

          // Images → load each onto the system clipboard, then send the paste
          // trigger so the agent (e.g. Claude Code) reads it → [Image #N].
          (async () => {
            for (const img of images) {
              const ok = await setClipboardImageFromPath(img);
              if (ok) sendToTerminal(key, PASTE_TRIGGER);
            }
          })();
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
