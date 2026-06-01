import type { Terminal } from "@xterm/xterm";
import { invoke } from "@tauri-apps/api/core";
import { inTauri } from "../backend";
import { escapeTerminalPath } from "./terminalInput";

/** Persist a pasted image to a temp file (native only) and return its path. */
async function savePastedImage(file: File): Promise<string | null> {
  if (!inTauri()) return null;
  try {
    const buf = new Uint8Array(await file.arrayBuffer());
    let bin = "";
    for (let i = 0; i < buf.length; i++) bin += String.fromCharCode(buf[i]);
    const b64 = btoa(bin);
    const ext = (file.type.split("/")[1] || "png").replace("jpeg", "jpg");
    return await invoke<string>("save_paste_image", { data: b64, ext });
  } catch (e) {
    console.warn("[taime] save_paste_image failed", e);
    return null;
  }
}

/**
 * Cross-platform copy/paste for an xterm terminal, shared by EVERY transport
 * (CAO WebSocket + Rust PTY) so behavior never diverges. Returns a cleanup fn.
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
    // Screenshot/image paste (e.g. macOS Cmd+Ctrl+Shift+4 → clipboard): save it
    // to a temp file and insert the PATH, which CLIs like Claude Code read.
    const items = e.clipboardData?.items;
    const imageItem = items
      ? Array.from(items).find((it) => it.kind === "file" && it.type.startsWith("image/"))
      : undefined;
    if (imageItem) {
      const file = imageItem.getAsFile();
      if (file) {
        e.preventDefault();
        savePastedImage(file).then((p) => {
          if (p) term.paste(escapeTerminalPath(p) + " ");
        });
        return;
      }
    }
    const text = e.clipboardData?.getData("text");
    if (text) {
      term.paste(text);
      e.preventDefault();
    }
  };
  el.addEventListener("paste", onPaste);

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

  return () => el.removeEventListener("paste", onPaste);
}
