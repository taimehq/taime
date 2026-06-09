/**
 * Pure tests for the new-workspace seed-prompt composition (lib/seedPrompt.ts).
 * Hermetic — the module imports only lib/workflowGenPrompt (also pure).
 */
import { describe, it, expect } from "vitest";
import { composeSeedPrompt, seedTaskTitle } from "./seedPrompt";
import { SCHEMA_BLOCK } from "./workflowGenPrompt";

describe("composeSeedPrompt", () => {
  const intent = "A CLI that converts Markdown files to styled PDFs.";
  const out = composeSeedPrompt(intent);

  it("ends with the user's intent verbatim", () => {
    expect(out).toContain(`WHAT THE USER WANTS TO BUILD:\n${intent}`);
    expect(out.trimEnd().endsWith(intent)).toBe(true);
  });

  it("carries the scaffold directive: own the files + commit early", () => {
    expect(out).toContain("git init");
    expect(out).toContain("current working directory");
    expect(out.toLowerCase()).toContain("readme");
  });

  it("carries the emergence budget (small team, optional workflow, no schedules)", () => {
    expect(out).toContain("at most TWO workers");
    expect(out).toContain("Do not create schedules");
  });

  it("folds in the exact create_workflow schema, framed as optional", () => {
    expect(out).toContain("OPTIONAL");
    // The contract is the SAME source the workflow generator uses (DRY).
    expect(out).toContain(SCHEMA_BLOCK);
  });

  it("trims surrounding whitespace from the intent", () => {
    const padded = composeSeedPrompt("   build a thing   ");
    expect(padded).toContain("WHAT THE USER WANTS TO BUILD:\nbuild a thing");
  });
});

describe("seedTaskTitle", () => {
  it("uses the first non-empty line", () => {
    expect(seedTaskTitle("Build a thing\nwith details")).toBe("Build a thing");
    expect(seedTaskTitle("\n\n  Real title  \nmore")).toBe("Real title");
  });

  it("caps long single lines with an ellipsis", () => {
    const long = "x".repeat(100);
    const t = seedTaskTitle(long);
    expect(t.length).toBeLessThanOrEqual(72);
    expect(t.endsWith("…")).toBe(true);
  });

  it("falls back to 'New project' for empty intent", () => {
    expect(seedTaskTitle("")).toBe("New project");
    expect(seedTaskTitle("   \n  ")).toBe("New project");
  });
});
