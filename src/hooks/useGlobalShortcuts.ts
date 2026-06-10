import { useEffect } from "react";
import { useStore } from "../store";
import { getTerminalController } from "../lib/terminalController";

/**
 * The single app-level keyboard dispatcher, mounted once at the root. New
 * navigation/UX surfaces (frame switching, search, command palette) register
 * their shortcuts here rather than scattering window listeners across the tree.
 *
 * We read/write the store via `getState()` so this listener never needs to be
 * re-subscribed when state changes — it mounts once and lives for the app.
 */
export function useGlobalShortcuts() {
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      // Cmd on macOS (metaKey); accept Ctrl too for the dev-in-browser path.
      const mod = e.metaKey || e.ctrlKey;
      if (!mod) return;

      // While the command palette is open it owns the keyboard — only Cmd+K
      // (to close) is honored; don't switch frames or zoom behind the overlay.
      if (useStore.getState().commandPaletteOpen) {
        if (e.key.toLowerCase() === "k") {
          e.preventDefault();
          useStore.getState().setCommandPaletteOpen(false);
        }
        return;
      }

      // --- Terminal font zoom (terminal-only; Monaco/diff is separate) ---
      // "=" and "+" share a physical key; "-"/"_" likewise. Cmd+0 resets.
      switch (e.key) {
        case "=":
        case "+":
          e.preventDefault();
          useStore.getState().adjustTerminalFontSize(1);
          return;
        case "-":
        case "_":
          e.preventDefault();
          useStore.getState().adjustTerminalFontSize(-1);
          return;
        case "0":
          e.preventDefault();
          useStore.getState().resetTerminalFontSize();
          return;
      }

      // --- Layout: Cmd+Shift+Enter toggles grid <-> focus ---
      if (e.key === "Enter" && e.shiftKey) {
        e.preventDefault();
        useStore.getState().toggleLayoutMode();
        return;
      }

      // --- Sidebar: Cmd+\ collapses/expands the left column ---
      if (e.key === "\\") {
        e.preventDefault();
        useStore.getState().toggleSidebar();
        return;
      }

      // --- Agent team / activity graph: Cmd+Shift+A toggles the visual graph ---
      if (e.shiftKey && e.key.toLowerCase() === "a") {
        e.preventDefault();
        const s = useStore.getState();
        s.setGraphOpen(!s.graphOpen);
        return;
      }

      // --- Frame switching by number: Cmd+1..8 = Nth frame, Cmd+9 = last ---
      // Routed through setActiveFrameGuarded — the single store action every
      // navigation surface uses to change the active frame.
      // Use e.code: Option/Shift remap the digit in e.key, so it's unreliable.
      //   Cmd+#         → select that window's tab and fullscreen it.
      //   Cmd+Option+#  → make that window active in place (stay in grid).
      // (Option, not Shift — Cmd+Shift+3/4/5 are macOS screenshot shortcuts.)
      const digit = e.code.match(/^Digit([1-9])$/);
      if (digit) {
        const { frames, setActiveFrameGuarded, setLayoutMode } =
          useStore.getState();
        if (frames.length === 0) return;
        e.preventDefault();
        const n = Number(digit[1]);
        const idx = n === 9 ? frames.length - 1 : n - 1;
        const target = frames[idx];
        if (target) {
          setActiveFrameGuarded(target.key);
          if (!e.altKey) setLayoutMode("focus");
        }
        return;
      }

      // --- Command palette (Cmd+K) toggles the navigation/action surface ---
      // ⌘K is the palette ONLY — the workspace switcher is ⌘O.
      if (e.key.toLowerCase() === "k") {
        e.preventDefault();
        const s = useStore.getState();
        s.setCommandPaletteOpen(!s.commandPaletteOpen);
        return;
      }

      // --- Workspace switcher (Cmd+O) toggles the title-bar dropdown ---
      if (e.key.toLowerCase() === "o") {
        e.preventDefault();
        const s = useStore.getState();
        s.setWsSwitcherOpen(!s.wsSwitcherOpen);
        return;
      }

      // --- Find-in-terminal: Cmd+F opens the active frame's find bar; ---
      // Cmd+G / Shift+Cmd+G repeat the search. Driven through the per-frame
      // controller so search stays scoped to the mounted frame.
      const k = e.key.toLowerCase();
      if (k === "f" || k === "g") {
        const ctrl = getTerminalController(useStore.getState().activeFrameKey);
        if (!ctrl) return; // no active terminal — let the event pass through
        e.preventDefault();
        if (k === "f") ctrl.openFind();
        else if (e.shiftKey) ctrl.findPrev();
        else ctrl.findNext();
        return;
      }
    };

    // Capture phase: run before a focused xterm can consume the key, so app
    // shortcuts fire reliably no matter where focus is.
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, []);
}
