/**
 * A friendly, memorable display label for an agent, deterministically derived
 * from its daemon-minted `agent_id` (an 8-hex string). The id is the durable
 * attribution anchor and stays the source of truth everywhere; this is purely a
 * human-facing handle so the UI shows "brave-otter" instead of "1a2b3c4d".
 *
 * Deterministic ⇒ the same agent always reads the same label, with no extra
 * state to persist or sync — the single source is the id itself. Collisions are
 * possible (256 combos) but harmless: the full id is always shown on hover and
 * remains the key for every operation.
 */

// Two-word "adjective-noun" handles — the established friendly-id convention
// (Docker/Heroku/Codespaces). Lowercase + hyphenated to sit in the mono type.
const ADJECTIVES = [
  "brave", "calm", "clever", "bold", "swift", "keen", "bright", "quiet",
  "lucky", "noble", "eager", "merry", "warm", "wise", "spry", "true",
];
const NOUNS = [
  "otter", "falcon", "maple", "river", "ember", "comet", "willow", "harbor",
  "cedar", "lark", "fjord", "delta", "quartz", "meadow", "pike", "cove",
];

/** Stable label for `agentId`. Falls back to a generic label for an empty id. */
export function agentLabel(agentId: string | null | undefined): string {
  if (!agentId) return "agent";
  // Two independent 16-way picks from the leading hex digits (deterministic).
  let h = 0;
  for (let i = 0; i < agentId.length; i++) {
    h = (h * 31 + agentId.charCodeAt(i)) >>> 0;
  }
  const adj = ADJECTIVES[h & 0xf];
  const noun = NOUNS[(h >>> 4) & 0xf];
  return `${adj}-${noun}`;
}
