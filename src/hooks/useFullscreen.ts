import { useEffect, useState } from "react";
import { inTauri } from "../backend";

/**
 * Native-fullscreen tracker for the title bar: macOS auto-hides the traffic
 * lights in fullscreen, so the 88px reserve must collapse with them (not
 * collapsing is the classic web-app tell). Probes `isFullscreen()` on every
 * window resize event — the fullscreen transition always emits one.
 */
export function useFullscreen(): boolean {
  const [fullscreen, setFullscreen] = useState(false);

  useEffect(() => {
    if (!inTauri()) return;
    let alive = true;
    let unlisten: (() => void) | undefined;
    (async () => {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      const win = getCurrentWindow();
      const probe = async () => {
        try {
          const f = await win.isFullscreen();
          if (alive) setFullscreen(f);
        } catch {
          /* window handle unavailable — keep the last known value */
        }
      };
      void probe();
      const stop = await win.onResized(() => void probe());
      if (alive) unlisten = stop;
      else stop();
    })();
    return () => {
      alive = false;
      unlisten?.();
    };
  }, []);

  return fullscreen;
}
