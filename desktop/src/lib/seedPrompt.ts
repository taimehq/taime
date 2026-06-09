/**
 * Prompt composition for the "Start something new" experience (SeedDialog).
 * Pure — no store, no IO — so the dialog's launch path and tests share one
 * source, exactly like lib/workflowGenPrompt.ts.
 *
 * The composed string is delivered as the FOUNDING agent's assignment (its
 * first prompt), NOT its system prompt. The founding agent launches on the
 * built-in `orchestrator` profile (profiles.rs): that profile already carries a
 * delegation system prompt + the MCP orchestration tools (assign / handoff /
 * list_agents / share / create_workflow). What the orchestrator's system prompt
 * does NOT contain is the exact create_workflow JSON schema — so we fold the
 * shared SCHEMA_BLOCK in here, framed as optional, behind an explicit budget.
 *
 * Honesty notes (these match what the daemon can actually do):
 *  - We do NOT scaffold the folder daemon-side; the agent runs `git init` and
 *    writes files itself, so genesis stays attributed to its agent_id.
 *  - A brand-new / empty folder provisions in SHARED mode (no HEAD to fork);
 *    the early-commit directive is what later makes isolation meaningful.
 *  - The emergence budget is advisory (prompt-level), not a daemon guarantee.
 */

import { SCHEMA_BLOCK } from "./workflowGenPrompt";

const ROLE = `You are the founding agent for a BRAND-NEW project in Taime. The user has described what they want to build (at the end of this message). Begin IMMEDIATELY — do not wait for further instructions and do not ask what to do next. Your very first action is to create the project structure and an initial git commit (see below), then keep going until there is a working MVP. Do the work yourself by default; only bring in specialist help if a subtask is genuinely worth parallelizing.`;

/** The scaffold contract: own the file-writes + git, and commit EARLY so the
 *  attribution/safe-context-switching thesis has something to attribute. */
const SCAFFOLD_DIRECTIVE = `GET TO A WORKING START
- Work in your current working directory — it is the project root.
- Scaffold a clean, conventional structure for the right stack, implement a minimal but working version (an MVP), and add a README with run instructions.
- Initialize version control EARLY: run \`git init\` if this isn't already a repo, make an initial commit as soon as a skeleton exists, then commit in small steps. Early commits are how Taime attributes your work and how per-agent isolation sharpens — do not wait until the end.
- Favor simple, idiomatic choices and working software over breadth. Verify it builds/runs before calling it done.`;

/** Keep the team small and the structure emergent — the antidote to a
 *  burst of unreviewed work all at once. */
const EMERGENCE_BUDGET = `GROW THE TEAM ONLY AS THE WORK DEMANDS
- Do the work yourself by default. Delegate only when a subtask is genuinely independent and worth parallelizing.
- If you delegate, use \`assign\` with the right specialist role ("product-builder", "feature-builder", "bug-fixer", "security-reviewer"). Keep it to at most TWO workers.
- Author a Taime workflow only if you identify a repeatable multi-step loop worth saving for later runs — at most one, and only once its shape is clear. Do not create schedules.`;

/** The create_workflow contract, framed as optional and gated by the budget
 *  above — reuses the exact same SCHEMA_BLOCK the workflow generator uses. */
const WORKFLOW_OPTION = `OPTIONAL — SAVING A REPEATABLE LOOP AS A WORKFLOW
Only if you decide (per the budget above) that a repeatable loop is worth saving, you can author ONE Taime workflow with the create_workflow tool. Otherwise ignore this section.

${SCHEMA_BLOCK}`;

/** Compose the founding agent's first prompt from the user's project intent. */
export function composeSeedPrompt(intent: string): string {
  return [
    ROLE,
    SCAFFOLD_DIRECTIVE,
    EMERGENCE_BUDGET,
    WORKFLOW_OPTION,
    `WHAT THE USER WANTS TO BUILD:\n${intent.trim()}`,
  ].join("\n\n");
}

/** A task title derived from a free-form intent: the first non-empty line,
 *  trimmed and capped so the Seed Task reads cleanly in the sidebar/breadcrumb.
 *  (The full intent is kept verbatim as the task description.) */
export function seedTaskTitle(intent: string): string {
  const firstLine =
    intent
      .split("\n")
      .map((l) => l.trim())
      .find((l) => l.length > 0) ?? "";
  const capped = firstLine.length > 72 ? `${firstLine.slice(0, 71).trimEnd()}…` : firstLine;
  return capped || "New project";
}
