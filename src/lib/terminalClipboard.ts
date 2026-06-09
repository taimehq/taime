import type { Terminal } from "@xterm/xterm";
import { PASTE_TRIGGER } from "./terminalInput";

/**
 * Cross-platform copy/paste for an xterm terminal — ONE shared implementation
 * (used by the daemon Rust-PTY view) so behavior never diverges. Returns a
 * cleanup fn.
 *
 * Best practices applied:
 *  - COPY is explicit (⌘C on macOS, Ctrl+Shift+C on Win/Linux) and only when
 *    there is a selection — Ctrl+C is left untouched so it still reaches the PTY
 *    as SIGINT. (We deliberately do NOT copy-on-select, which would clobber the
 *    system clipboard every time you drag to read output.)
 *  - PASTE uses the browser `paste` event (fires for ⌘V / Ctrl+V and
 *    right-click paste on the focused terminal) and routes through `term.paste`,
 *    which applies bracketed-paste mode so multi-line pastes don't auto-execute.
 *    Paste keystrokes are swallowed from the PTY so Ctrl+V can't also send ^V.
 *  - Ctrl+Shift+V (no native browser paste) falls back to clipboard.readText.
 *  - ⌘A selects the whole buffer on macOS.
 */
export function wireClipboard(term: Terminal, el: HTMLElement): () => void {
  const isMac =
    typeof navigator !== "undefined" &&
    (navigator.platform.toLowerCase().includes("mac") || /Mac/.test(navigator.userAgent));

  const onPaste = (e: ClipboardEvent) => {
    // Screenshot/image paste (e.g. macOS Cmd+Ctrl+Shift+4 → clipboard): the
    // image is already on the SYSTEM clipboard, which is how a CLI like Claude
    // Code ingests it. We can't pipe image bytes through the PTY, so instead we
    // send the agent its paste trigger (Ctrl+V) and let it read the clipboard
    // → [Image #N]. (Text paste is forwarded normally below.)
    const items = e.clipboardData?.items;
    const hasImage = items
      ? Array.from(items).some((it) => it.kind === "file" && it.type.startsWith("image/"))
      : false;
    const text = e.clipboardData?.getData("text");
    if (hasImage && !text) {
      e.preventDefault();
      term.input(PASTE_TRIGGER);
      return;
    }
    if (text) {
      term.paste(text);
      e.preventDefault();
    }
  };
  el.addEventListener("paste", onPaste);

  // Right-click: own the gesture so the native webview/OS context menu never
  // appears. (It diverged across transports — the CAO/tmux view showed the
  // system menu AND a second app menu — and a terminal shouldn't surface
  // Reload/Inspect/Services anyway.) Terminal-native behavior instead: copy the
  // current selection if there is one, otherwise paste the clipboard. Shared
  // here so both transports are identical.
  const onContextMenu = (e: MouseEvent) => {
    e.preventDefault();
    const sel = term.getSelection();
    if (sel) {
      navigator.clipboard.writeText(sel).catch(() => {});
      term.clearSelection();
    } else {
      navigator.clipboard
        .readText()
        .then((t) => t && term.paste(t))
        .catch(() => {});
    }
  };
  el.addEventListener("contextmenu", onContextMenu);

  term.attachCustomKeyEventHandler((e) => {
    if (e.type !== "keydown") return true;
    const key = e.key.toLowerCase();

    // Copy (only with a selection) — never intercept a bare Ctrl+C.
    const copyCombo = isMac
      ? e.metaKey && !e.ctrlKey && !e.altKey && key === "c"
      : e.ctrlKey && e.shiftKey && key === "c";
    if (copyCombo) {
      const sel = term.getSelection();
      if (sel) navigator.clipboard.writeText(sel).catch(() => {});
      return false;
    }

    // Native paste shortcuts (⌘V / Ctrl+V): let the browser `paste` event do
    // the work; swallow the keystroke so xterm doesn't also emit ^V to the PTY.
    if ((isMac && e.metaKey && key === "v") || (!isMac && e.ctrlKey && !e.shiftKey && key === "v")) {
      return false;
    }

    // Ctrl+Shift+V (no browser paste event) — read the clipboard explicitly.
    if (!isMac && e.ctrlKey && e.shiftKey && key === "v") {
      navigator.clipboard
        .readText()
        .then((t) => t && term.paste(t))
        .catch(() => {});
      return false;
    }

    // Select-all the buffer on macOS ⌘A (Ctrl+A stays a PTY/readline control).
    if (isMac && e.metaKey && !e.ctrlKey && key === "a") {
      term.selectAll();
      return false;
    }

    return true;
  });

  return () => {
    el.removeEventListener("paste", onPaste);
    el.removeEventListener("contextmenu", onContextMenu);
  };
}
