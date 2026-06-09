/**
 * Prompt composition for the "Start something new" experience (SeedDialog).
 * Pure — no store, no IO — so the dialog's launch path and tests share one
 * source, exactly like lib/workflowGenPrompt.ts.
 *
 * The composed string is delivered as the FOUNDING agent's assignment (its
 * first prompt). It launches on the built-in `orchestrator` profile
 * (profiles.rs): a delegation system prompt + the MCP orchestration tools
 * (assign / handoff / list_agents / share / create_workflow).
 *
 * The founding agent is a TRUE orchestrator — it never writes code itself. It
 * (1) runs a short discovery conversation with the user to nail down scope +
 * tech stack, then (2) delegates the build to specialist workers (assign) and
 * integrates their results. The actual scaffolding/coding is the workers' job
 * (e.g. the "product-builder" profile). The create_workflow JSON schema
 * (SCHEMA_BLOCK) is folded in, optional, for when a repeatable loop is worth
 * saving.
 *
 * Delegation depends on the orchestrator's MCP tools (assign) being live — the
 * daemon injects them for the `orchestrator` profile.
 */

import { SCHEMA_BLOCK } from "./workflowGenPrompt";

const ROLE = `You are the founding ORCHESTRATOR for a BRAND-NEW project in Taime (the user's idea is at the end of this message). You COORDINATE a team — you do NOT write code, scaffold files, or run build commands yourself. Your job: turn the idea into a plan the user agrees with, then delegate the building to specialist worker agents and integrate their results. Start now.`;

/** Phase 1: an interactive discovery conversation — the orchestrator helps the
 *  user pin down scope + tech stack BEFORE any building happens. */
const DISCOVERY = `STEP 1 — TALK TO THE USER FIRST (do this now; do NOT start building)
Open a short, focused discovery conversation in this terminal to turn the idea into a concrete plan:
- Clarify the MVP goal and scope — what it must do first vs. later.
- Surface key constraints — platform/runtime, the accounts or APIs involved (e.g. Slack / Gmail / Granola access + auth), data, deadlines.
- Decide the TECH STACK: propose a sensible default stack for this kind of project, justify it in a sentence, and ask the user to confirm or adjust.
Ask only the few questions that actually change the plan — propose defaults instead of interrogating, and iterate. When the user is happy, write a short plan summary (scope + stack + first milestones) and confirm it before delegating.`;

/** Phase 2: delegate the build to specialist workers — the orchestrator never
 *  implements; it assigns, tracks, and integrates. */
const DELEGATE = `STEP 2 — DELEGATE THE BUILD (only after the user agrees the plan)
You implement NOTHING yourself. Use the assign tool to spawn specialist workers (each runs in its own git worktree) and integrate what they produce:
- "product-builder" — scaffold the project for the agreed stack and build the working MVP.
- "feature-builder" — add features; "bug-fixer" — fix failures with a regression test; "security-reviewer" — audit.
Give each worker a fully self-contained brief (it sees only what you send — restate the relevant plan + stack). Run independent work in parallel, use list_agents to track progress, and own the final integration plus a short status summary back to the user. If direction becomes unclear mid-build, come back to the user — never guess.`;

/** The create_workflow contract, framed as optional — reuses the exact same
 *  SCHEMA_BLOCK the workflow generator uses. */
const WORKFLOW_OPTION = `OPTIONAL — SAVING A REPEATABLE LOOP AS A WORKFLOW
If a repeatable multi-step loop emerges that's worth re-running later (e.g. build → test → fix), you MAY save ONE Taime workflow with the create_workflow tool. Otherwise ignore this section. Do not create schedules.

${SCHEMA_BLOCK}`;

/** Compose the founding orchestrator's first prompt from the user's intent:
 *  discover (scope + stack) → delegate to workers → integrate; never implement. */
export function composeSeedPrompt(intent: string): string {
  return [
    ROLE,
    DISCOVERY,
    DELEGATE,
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
