/**
 * Transport-agnostic terminal input injection.
 *
 * Drag-and-drop (and any future "send text to this terminal" feature) needs to
 * write into a terminal without coupling to the transport (the daemon Rust
 * PTY). Each terminal view registers a writer keyed by its terminal/session
 * id; the drop handler looks up the writer for whichever frame the file landed
 * on and writes the (shell-escaped) path(s) at the cursor — no submit, so the
 * user can keep typing.
 */

import { invoke } from "@tauri-apps/api/core";
import { inTauri } from "../backend";

type Writer = (text: string) => void;

const writers = new Map<string, Writer>();

/** Ctrl+V control byte — the paste trigger CLIs like Claude Code read the
 * system clipboard on (its image paste → `[Image #N]`). */
export const PASTE_TRIGGER = "\x16";

const IMAGE_EXTS = new Set([
  "png", "jpg", "jpeg", "gif", "webp", "bmp", "tiff", "tif", "heic", "svg",
]);

export function isImagePath(p: string): boolean {
  const ext = p.split(".").pop()?.toLowerCase() ?? "";
  return IMAGE_EXTS.has(ext);
}

/** Put an image FILE on the macOS clipboard so the agent can read it on paste. */
export async function setClipboardImageFromPath(path: string): Promise<boolean> {
  if (!inTauri()) return false;
  try {
    await invoke("set_clipboard_image_from_path", { path });
    return true;
  } catch (e) {
    console.warn("[taime] set_clipboard_image_from_path failed", e);
    return false;
  }
}

/** Register a terminal's input writer; returns an unregister fn. */
export function registerTerminalInput(key: string, writer: Writer): () => void {
  writers.set(key, writer);
  return () => {
    if (writers.get(key) === writer) writers.delete(key);
  };
}

/** Write text to a registered terminal. Returns false if none is registered. */
export function sendToTerminal(key: string | null | undefined, text: string): boolean {
  if (!key) return false;
  const w = writers.get(key);
  if (!w) return false;
  w(text);
  return true;
}

/**
 * Backslash-escape a path the way terminals do on file drag (escape whitespace
 * + shell metacharacters, leave the path otherwise bare). This matters: Claude
 * Code recognizes a *bare, escaped* image path and renders it as `[Image #N]`;
 * a single-quoted path is treated as literal text ("just the link"). Bare +
 * escaped also pastes correctly into a normal shell.
 */
export function escapeTerminalPath(p: string): string {
  return p.replace(/(["'\\$`!&|;<>*?(){}\[\]\s#])/g, "\\$1");
}

/** Format dropped file paths for insertion: escaped, space-separated, trailing space. */
export function formatDroppedPaths(paths: string[]): string {
  const cleaned = paths.filter((p) => p && p.trim());
  if (cleaned.length === 0) return "";
  return cleaned.map(escapeTerminalPath).join(" ") + " ";
}
