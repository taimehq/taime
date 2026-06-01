/**
 * Transport-agnostic terminal input injection.
 *
 * Drag-and-drop (and any future "send text to this terminal" feature) needs to
 * write into a terminal without knowing whether it's the CAO WebSocket or the
 * Rust PTY. Each terminal view registers a writer keyed by its terminal/session
 * id; the drop handler looks up the writer for whichever frame the file landed
 * on and writes the (shell-escaped) path(s) at the cursor — no submit, so the
 * user can keep typing.
 */

type Writer = (text: string) => void;

const writers = new Map<string, Writer>();

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

/** POSIX single-quote escaping so paths with spaces/quotes paste safely. */
export function shellQuotePath(p: string): string {
  return `'${p.replace(/'/g, `'\\''`)}'`;
}

/** Format dropped file paths for insertion: quoted, space-separated, trailing space. */
export function formatDroppedPaths(paths: string[]): string {
  const cleaned = paths.filter((p) => p && p.trim());
  if (cleaned.length === 0) return "";
  return cleaned.map(shellQuotePath).join(" ") + " ";
}
