/**
 * Best-effort extraction of the running model from a CLI's startup banner.
 *
 * The official CLIs print their model on launch (e.g. Claude Code's
 * "Opus 4.8 (1M context)"). We scan the early terminal output for a known
 * shape and return a short label. Reliable for fresh launches and Rust-PTY
 * scrollback replay; a CAO reattach to an already-scrolled session may miss it
 * (the banner is gone), in which case the model just stays unknown.
 */
// Version-agnostic shape-matchers (they track new releases without edits).
// Current flagships at time of writing (June 2026): Claude Opus 4.8 /
// Sonnet 4.6, GPT-5.5, Gemini 3.5, Grok 4.3 + the grok-build-0.1 coding model.
const PATTERNS: RegExp[] = [
  // Claude (display): "Opus 4.8", "Sonnet 4.6", "Haiku 4.5"
  /\b(?:Opus|Sonnet|Haiku)\s+\d+(?:\.\d+)?\b/i,
  // Claude (model id): "claude-opus-4-8"
  /\bclaude-(?:opus|sonnet|haiku)-[\d.-]+\b/i,
  // OpenAI / Codex: "GPT-5.5", "GPT 5.5", "gpt-5.5-codex"
  /\bgpt[-\s]?\d+(?:\.\d+)?(?:-[a-z]+)?\b/i,
  // Gemini: "Gemini 3.5 Flash", "Gemini 3.1 Pro", "gemini-3.5-flash"
  /\bgemini[-\s]?\d+(?:\.\d+)?(?:[-\s](?:pro|flash(?:-lite)?|lite))?\b/i,
  // Grok: "Grok 4.3", "Grok 4.1 Fast", "Grok Build", "grok-build-0.1"
  /\bgrok[-\s](?:build(?:-[\d.]+)?|\d+(?:\.\d+)?(?:[-\s](?:fast|heavy))?)\b/i,
];

export function parseModel(text: string): string | null {
  for (const re of PATTERNS) {
    const m = text.match(re);
    if (m) return m[0].replace(/\s+/g, " ").trim();
  }
  return null;
}

/**
 * Accumulates terminal output and reports the model once, via `onModel`.
 * Returns a `feed(bytes)` to call on each output chunk; it self-disables after
 * a hit or once it has seen enough text (the banner is always near the top).
 */
export function makeModelSniffer(onModel: (model: string) => void) {
  const decoder = new TextDecoder("utf-8", { fatal: false });
  let buf = "";
  let done = false;
  const CAP = 16_384; // banner is within the first output; cap the scan
  return (bytes: Uint8Array) => {
    if (done) return;
    buf += decoder.decode(bytes, { stream: true });
    const hit = parseModel(buf);
    if (hit) {
      done = true;
      onModel(hit);
      return;
    }
    if (buf.length > CAP) {
      done = true; // give up — keep the last window in case of split tokens
    }
  };
}
