/** Shared display-formatting helpers (the truncation contract lives here). */

/** Middle-truncate a path-like string: keep the head and the tail (where the
 *  discriminating segments live), ellipsis in the middle. CSS only
 *  end-truncates, so paths route through this. Pair with a title attr
 *  carrying the full value. THE single implementation — import this; never
 *  re-declare a local copy. */
export function middleTruncate(s: string, max = 48): string {
  if (s.length <= max) return s;
  const head = Math.ceil((max - 1) / 2);
  const tail = max - 1 - head;
  return `${s.slice(0, head)}…${s.slice(s.length - tail)}`;
}
