/**
 * Session naming helpers.
 *
 * CAO sessions are tmux sessions prefixed with "cao-". Auto-generated ones look
 * like "cao-cea61400" (a hash) which reads poorly in the pipeline. We (a) name
 * NEW Taime-launched sessions after the project folder, and (b) strip the
 * "cao-" prefix for display.
 */

export interface SessionLabel {
  /** The meaningful part to show prominently. */
  label: string;
  /** A short disambiguating suffix (random tag), shown muted — or null. */
  tag: string | null;
  /** True when the whole name is an opaque hash (auto-generated, no real name). */
  mono: boolean;
}

/**
 * Turn a raw CAO session name into a readable label.
 *   "cao-kdx-investigate-38ri" → { label: "kdx-investigate", tag: "38ri" }
 *   "cao-81488ca1"             → { label: "81488ca", tag: null, mono: true }
 * The "cao-" prefix is dropped; a trailing random 4-char suffix is split off so
 * near-identical sessions read as one name + a quiet tag instead of noise.
 */
export function prettySession(name: string): SessionLabel {
  const stripped = name.replace(/^cao-/, "");
  // Pure hex hash → opaque auto-session; render short + mono, no fake "name".
  if (/^[0-9a-f]{6,12}$/i.test(stripped)) {
    return { label: stripped.slice(0, 7), tag: null, mono: true };
  }
  // "<base>-<rand4>" → base prominent, suffix muted.
  const m = stripped.match(/^(.+)-([a-z0-9]{4})$/i);
  if (m) return { label: m[1], tag: m[2], mono: false };
  return { label: stripped, tag: null, mono: false };
}

/** One-line string form for plain contexts (e.g. <option>): "label (tag)". */
export function prettySessionText(name: string): string {
  const { label, tag } = prettySession(name);
  return tag ? `${label} (${tag})` : label;
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
