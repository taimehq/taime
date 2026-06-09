import { providerTitle } from "./providerLabel";

/**
 * Per-agent attribution colors — stable color per team member so authorship
 * reads at a glance. THE single palette, shared by the full-screen DiffView
 * and the Task Review tab's per-agent sections (one grammar everywhere).
 * Index by the agent's team order, modulo the palette length.
 */

export interface AuthorColor {
  dot: string;
  text: string;
  chip: string;
}

export const AUTHOR_COLORS: AuthorColor[] = [
  { dot: "bg-sky-400", text: "text-sky-300", chip: "bg-sky-500/15 text-sky-300" },
  { dot: "bg-violet-400", text: "text-violet-300", chip: "bg-violet-500/15 text-violet-300" },
  { dot: "bg-emerald-400", text: "text-emerald-300", chip: "bg-emerald-500/15 text-emerald-300" },
  { dot: "bg-amber-400", text: "text-amber-300", chip: "bg-amber-500/15 text-amber-300" },
  { dot: "bg-rose-400", text: "text-rose-300", chip: "bg-rose-500/15 text-rose-300" },
  { dot: "bg-cyan-400", text: "text-cyan-300", chip: "bg-cyan-500/15 text-cyan-300" },
];

/** Display name for a contributor: provider title, else the agent-id stub. */
export function authorName(
  c: { provider: string | null; agent_id: string } | null | undefined,
): string {
  if (!c) return "unattributed";
  return c.provider ? providerTitle(c.provider) : c.agent_id.slice(0, 6);
}
