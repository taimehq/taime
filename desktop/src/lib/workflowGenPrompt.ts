/**
 * Prompt composition for the AI workflow generator (NewWorkflowDialog's
 * "Generate with AI" tab). Pure — no store, no IO — so the dialog's live
 * preview and the launch path share one source and tests stay hermetic.
 *
 * The composed prompt is delivered as an orchestrator agent's assignment
 * (first prompt); the orchestrator profile injects the MCP tools it cites
 * (create_workflow, run_workflow, list_agents, share).
 */

/** Generation scope — mirrors the dialog's three radio cards exactly. */
export type WorkflowGenScope =
  | "workflow_only"
  | "workflow_and_run"
  | "context_aware";

const ROLE = `You are a workflow author for Taime. Your job: turn the user request below into one Taime workflow — a graph of agent steps with conditional branches and bounded loops — using the create_workflow MCP tool available to you.`;

/** The contract block: tool, exact JSON schema, when-grammar, output routing,
 *  loop bounds, and the profile/provider vocabulary — verbatim from the
 *  daemon's workflow contract (workflow.rs + the create_workflow tool). */
const SCHEMA_BLOCK = `THE create_workflow TOOL
Call create_workflow with one argument, "definition": a JSON string of this exact shape:

{
  "name": "<unique workflow name>",
  "entry": "<node id the run starts at>",
  "max_iterations": <optional loop bound, default 20>,
  "nodes": [
    { "id": "<node id>", "profile": "<profile>", "prompt": "<worker prompt>", "output_key": "<optional blackboard key>", "provider": "<optional provider>" }
  ],
  "edges": [
    { "from": "<node id>", "to": "<node id>", "when": "<condition>" }
  ]
}

EDGE CONDITIONS (when)
"always" | "keyword:WORD" | "/regex/" — matched against the source node's shared output. The first matching edge wins; if no edge matches, the node is terminal.

OUTPUT ROUTING
Each node's worker posts its result with the share tool to the node's output_key (default: the node id). Edges route on that value, so any node with branching edges must say in its prompt exactly what to reply (e.g. "Reply PASS or FAIL").

LOOPS
A loop is a back-edge to an earlier node, bounded by max_iterations (default 20). Set max_iterations explicitly whenever you add a back-edge.

PROFILES
orchestrator, feature-builder, bug-fixer, security-reviewer, product-builder, default (plus any custom ~/.taime/agents/*.toml profile).

PROVIDERS
claude_code, codex, gemini_cli, grok_cli. Omit a node's provider to use the run default.

RULES
- Node prompts must be fully self-contained — workers see nothing else: not this conversation, not the other nodes' prompts.
- If create_workflow returns an error, fix the definition and retry.
- Reply with the workflow name and a one-line graph summary when done.`;

const SCOPE_BLOCKS: Record<WorkflowGenScope, string> = {
  workflow_only: `SCOPE: WORKFLOW ONLY
Create exactly one workflow definition. Never run it and never launch agents — your only side effect is the single create_workflow call.`,
  workflow_and_run: `SCOPE: WORKFLOW + FIRST RUN
Create exactly one workflow definition, then call run_workflow once with its name and report the run id. Node agents will spawn and do real work.`,
  context_aware: `SCOPE: CONTEXT-AWARE
First call list_agents and read the existing agents. Design the workflow to complement them — do not duplicate work already running. Create exactly one workflow definition; do not run it and do not launch agents.`,
};

/** Compose the full generator assignment: role + contract + scope directives,
 *  ending with the user's request. */
export function composePrompt(
  scope: WorkflowGenScope,
  description: string,
): string {
  return [
    ROLE,
    SCHEMA_BLOCK,
    SCOPE_BLOCKS[scope],
    `USER REQUEST:\n${description}`,
  ].join("\n\n");
}
