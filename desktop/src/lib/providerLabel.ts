/** Provider id → human title, shared by the shell grid and command palette. */
export const PROVIDER_TITLE: Record<string, string> = {
  claude_code: "Claude Code",
  codex: "Codex CLI",
  gemini_cli: "Gemini CLI",
  grok_cli: "Grok Build CLI",
};

export function providerTitle(provider: string): string {
  return PROVIDER_TITLE[provider] ?? provider.replace(/_/g, " ");
}
