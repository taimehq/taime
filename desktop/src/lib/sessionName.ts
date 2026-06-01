/**
 * Session naming helpers.
 *
 * CAO sessions are tmux sessions prefixed with "cao-". Auto-generated ones look
 * like "cao-cea61400" (a hash) which reads poorly in the pipeline. We (a) name
 * NEW Taime-launched sessions after the project folder, and (b) strip the
 * "cao-" prefix for display.
 */

/** Display label for a session: drop the "cao-" prefix. */
export function prettySession(name: string): string {
  return name.replace(/^cao-/, "");
}

/** Filesystem-/tmux-safe base derived from the project folder name. */
export function sessionBaseFromDir(dir: string | null | undefined): string {
  const base = (dir ?? "").replace(/\/+$/, "").split("/").pop() || "project";
  return (
    base
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "")
      .slice(0, 32) || "project"
  );
}

/** A readable, unique-ish session name: "<project>-<rand4>" (CAO adds "cao-"). */
export function makeSessionName(dir: string | null | undefined): string {
  const rand = Math.random().toString(36).slice(2, 6);
  return `${sessionBaseFromDir(dir)}-${rand}`;
}
