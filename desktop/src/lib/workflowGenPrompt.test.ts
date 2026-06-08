/**
 * Tests for the AI workflow-generator prompt composer (workflowGenPrompt.ts).
 * Pure string assembly — assert that every scope carries the shared contract
 * block, that each scope's distinguishing directives appear (and don't bleed
 * into the others), and that the user description is appended last.
 */
import { describe, it, expect } from "vitest";
import { composePrompt, type WorkflowGenScope } from "./workflowGenPrompt";

const SCOPES: WorkflowGenScope[] = [
  "workflow_only",
  "workflow_and_run",
  "context_aware",
];

describe("composePrompt — shared contract block", () => {
  it.each(SCOPES)("%s carries the schema + grammar + vocabulary", (scope) => {
    const p = composePrompt(scope, "do a thing");
    // Tool + schema fields.
    expect(p).toContain("create_workflow");
    expect(p).toContain('"definition"');
    expect(p).toContain('"entry"');
    expect(p).toContain('"max_iterations"');
    expect(p).toContain('"output_key"');
    // when-grammar, verbatim.
    expect(p).toContain('"always" | "keyword:WORD" | "/regex/"');
    expect(p).toContain("first matching edge wins");
    expect(p).toContain("the node is terminal");
    // Output routing via share; loop bounds.
    expect(p).toContain("share tool");
    expect(p).toContain("default 20");
    // Profiles + providers, verbatim from the contract.
    expect(p).toContain(
      "orchestrator, feature-builder, bug-fixer, security-reviewer, product-builder, default",
    );
    expect(p).toContain("claude_code, codex, gemini_cli, grok_cli");
    // Standing rules.
    expect(p).toContain("fully self-contained");
    expect(p).toContain("fix the definition and retry");
    expect(p).toContain("one-line graph summary");
  });
});

describe("composePrompt — scope directives", () => {
  it("workflow_only: create one, never run, never launch", () => {
    const p = composePrompt("workflow_only", "x");
    expect(p).toContain("SCOPE: WORKFLOW ONLY");
    expect(p).toContain("Create exactly one workflow definition");
    expect(p).toContain("Never run it and never launch agents");
    // No bleed from the other scopes.
    expect(p).not.toContain("run_workflow once");
    expect(p).not.toContain("list_agents");
  });

  it("workflow_and_run: create, then run_workflow once + report the run id", () => {
    const p = composePrompt("workflow_and_run", "x");
    expect(p).toContain("SCOPE: WORKFLOW + FIRST RUN");
    expect(p).toContain("call run_workflow once");
    expect(p).toContain("report the run id");
    expect(p).not.toContain("list_agents");
  });

  it("context_aware: list_agents first, complement, do not run", () => {
    const p = composePrompt("context_aware", "x");
    expect(p).toContain("SCOPE: CONTEXT-AWARE");
    expect(p).toContain("First call list_agents");
    expect(p).toContain("complement");
    expect(p).toContain("do not duplicate work already running");
    expect(p).toContain("do not run it and do not launch agents");
    expect(p).not.toContain("run_workflow once");
  });
});

describe("composePrompt — description", () => {
  it.each(SCOPES)("%s ends with USER REQUEST + the description", (scope) => {
    const description = "Nightly: triage open issues, loop until clean.";
    const p = composePrompt(scope, description);
    expect(p.endsWith(`USER REQUEST:\n${description}`)).toBe(true);
  });
});
