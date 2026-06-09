import { describe, it, expect } from "vitest";
import { agentLabel } from "./agentLabel";

describe("agentLabel", () => {
  it("is deterministic for the same id", () => {
    expect(agentLabel("1a2b3c4d")).toBe(agentLabel("1a2b3c4d"));
  });

  it("renders as a lowercase adjective-noun slug", () => {
    expect(agentLabel("deadbeef")).toMatch(/^[a-z]+-[a-z]+$/);
  });

  it("falls back to a generic label for an empty/nullish id", () => {
    expect(agentLabel("")).toBe("agent");
    expect(agentLabel(null)).toBe("agent");
    expect(agentLabel(undefined)).toBe("agent");
  });

  it("varies across different ids", () => {
    const ids = [
      "00000000", "11111111", "22222222", "abcdef01",
      "12345678", "87654321", "cafebabe", "0badf00d",
    ];
    const labels = new Set(ids.map(agentLabel));
    expect(labels.size).toBeGreaterThan(1);
  });
});
