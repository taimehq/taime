/** Provider id → human title — the ONE provider display map (import this;
 *  never re-declare a local copy). */
export const PROVIDER_TITLE: Record<string, string> = {
  claude_code: "Claude Code",
  codex: "Codex CLI",
  gemini_cli: "Gemini CLI",
  grok_cli: "Grok Build CLI",
};

/** Provider id → vendor (the launch dialog's secondary label). */
export const PROVIDER_VENDOR: Record<string, string> = {
  claude_code: "Anthropic",
  codex: "OpenAI",
  gemini_cli: "Google",
  grok_cli: "xAI",
};

/** Canonical display order for provider lists (also the daemon's registry). */
export const PROVIDER_ORDER = ["claude_code", "codex", "gemini_cli", "grok_cli"];

export function providerTitle(provider: string): string {
  return PROVIDER_TITLE[provider] ?? provider.replace(/_/g, " ");
}
