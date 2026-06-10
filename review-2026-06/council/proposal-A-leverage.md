# Council Workflows — adversarial plan/critique loops on the existing Workflow engine

## 1. Feature name candidates

1. **Council** (recommended) — the user-facing name for the pattern and its built-in templates (`council-plan`, `council-build`). A "council" is just a Workflow with a fan-out node; no new object enters the hierarchy.
2. **Plan Review** — thesis-aligned framing: "nothing merges without Review; nothing big gets built without Plan Review."
3. **Panel** — the primitive's name if we want something drier ("panel node").

The lexicon's no-new-nouns rule holds: Council names a *Workflow template family* plus one node capability — Agent/Task/Workflow/Run/Profile/Review are untouched.

## 2. Conceptual model

- **Workflow (Library)**: the entire plan→critique→revise→judge loop is one ordinary `WorkflowDefinition`. Branches are edges; iteration is a back-edge bounded by `max_iterations`. Nothing new conceptually.
- **Run (Workspace)**: one council execution; inherits `task_id` exactly like every run today, so the whole council lands in one Task — safe context switching intact.
- **Agent**: planner, each critic, and the judge are *real Agents* — minted Agent ID, own worktree, turns, diffs, attribution. The Run view answers "which model said what" — this is Attribution applied to deliberation, the flagship thesis extended upstream of code.
- **Profile**: critics/judges are Profiles (`plan-critic`, `plan-judge` — read-only, like `researcher`). Stance steering is profile + prompt, not new machinery.
- **Review**: untouched and load-bearing. A council's `implement` node produces a diff that goes through Review like any agent's. The council never merges anything.

**Three genuinely missing primitives** (everything else exists):
1. **Artifact passing** — `[[output:NODE_ID]]` / `[[param:NAME]]` substitution into node prompts at spawn. Today node prompts are static; outputs sit on the blackboard but downstream prompts can't reference them.
2. **Run parameters** — `run_workflow(name, params)` so one Library template serves every feature/bug ("the request" enters via `[[param:request]]`). Reuses `schedules.rs::substitute_vars` mechanics verbatim.
3. **Fan-out node** — one node that the engine expands into N parallel branch workers and joins. (Phase 2 only — see §9: the loop ships *without* it first.)

**How text physically moves between CLI agents** (no new transport): engine substitutes prior blackboard values into the node prompt → `enqueue_message("workflow", agent, prompt)` → idle-gated bracketed paste into the worker's PTY. Worker posts results back via the existing `share` MCP tool to a run-namespaced key. The engine is the only courier; agents never need to discover each other.

## 3. Data model + wire changes

**`workflow.rs`** (JSON-definition changes, serde-additive — old files parse forever):

```rust
pub struct FanBranch {
    pub provider: Option<String>,  // default: node/run provider
    pub profile:  Option<String>,  // default: node profile
    pub stance:   Option<String>,  // appended to the node prompt
}
pub struct WorkflowNode {
    // ...existing fields...
    #[serde(default)] pub fan: Vec<FanBranch>,      // empty = normal node
    #[serde(default)] pub max_visits: Option<u32>,  // per-node cap override (≤ PER_NODE_CAP)
}
pub struct WorkflowDefinition {
    // ...existing...
    #[serde(default)] pub params: Vec<String>,      // declared run parameters
}
```
Validation additions: `fan.len() <= 4`; declared params must each appear as `[[param:X]]` somewhere; `[[output:Y]]` references must name real node output_keys.

**Store** (one migration): `ALTER TABLE node_runs ADD COLUMN branch INTEGER NOT NULL DEFAULT 0;` `node_iteration_count` becomes `COUNT(DISTINCT iteration)`. Run params persisted as JSON on the run row (`ALTER TABLE ... ADD COLUMN params TEXT`).

**Wire protocol: zero postcard changes.** Workflow definitions and run summaries already cross as JSON inside the generic `Query`/`QueryResult` (lib.rs v7), so no protocol bump, no daemon-replacement upgrade. JSON shape additions:
- `WorkflowNodeState` gains `branches?: { agent_id, provider, stance, status }[]`.
- `runWorkflow` query args gain `params?: Record<string,string>`.

**MCP** (`mcp.rs`): `run_workflow` tool gains optional `params` object; `create_workflow`'s description documents `fan`, `[[output:]]`, `[[param:]]`, `max_visits`. `workflowGenPrompt.ts` `SCHEMA_BLOCK` updated to match (one source, as today).

**Profiles** (`profiles.rs` builtins): `planner` (read-only; produce a structured plan), `plan-critic` (read-only; structured critique ending `VERDICT: APPROVE|REVISE` with numbered, justified issues; "unjustified agreement is a defect"), `plan-judge` (read-only; emit `APPROVE` or `REVISE` as the last line, plus a Consensus / Disagreements / Unresolved summary).

## 4. Engine mechanics

`workflow_engine::drive` changes, step by step:

1. **Substitute** before spawn: `[[param:X]]` from run params; `[[output:Y]]` from blackboard key `{run_id}::Y` (missing → `"(not yet produced)"`). Same fixed-allowlist style as schedules — never env, never arbitrary keys.
2. **Normal node**: unchanged (spawn, instruct `share(key=run_id::output_key)`, poll, kill, route).
3. **Fan node**: spawn all branches concurrently — each `spawn_workflow_node` (own worktree, Agent ID, inherited task_id), prompt = substituted base + its `stance` + share-instruction to `{run_id}::{output_key}::b{i}`; one `node_runs` row per branch. **Branches never see each other** — independent first drafts by construction (the engine holds the only copy of sibling keys).
4. **Join**: poll all branch keys under the existing WAIT_TICKS budget (parallel, so wall time = slowest critic, not the sum). On deadline, missing branches are marked failed; the engine composes a labeled join — `## Critique 1 — codex (stance: feasibility) ... ## Critique 3 — grok_cli: NO RESPONSE (timed out)` — writes it to `{run_id}::{output_key}` itself, kills all branch workers, and proceeds if **≥1** branch reported.
5. **Route** on the joined text with the existing first-match-wins edges. Crucially, `/VERDICT:\s*REVISE/` → revise-loop, `always` → proceed is an **adaptive early-stop**: unanimous APPROVE skips the revise round entirely.
6. **Loop plumbing trick**: the `revise` node sets `output_key: "plan"` — last-writer-wins on the blackboard means `[[output:plan]]` is always the *latest* plan, so round 2 critics critique the revision with no extra machinery.

The `council-plan` template (ships built-in, like built-in profiles):

```
plan(entry, planner, provider A)
  → critique [fan: codex "find what breaks", gemini_cli "find what's overengineered"]
      reads [[param:request]] + [[output:plan]]
  → revise (planner, provider A; output_key "plan")     when /VERDICT:\s*REVISE/
      reads plan + joined critiques; must emit RESOLVED:/REJECTED: ledger per issue
  → judge (plan-judge, provider B ≠ A)
      APPROVE → terminal (final plan on blackboard, notify) | REVISE → critique
  critique → judge  when always            (early-stop path)
max_iterations: 8   (= 2 full critique rounds)
```
`council-build` appends `judge --APPROVE--> implement(feature-builder) → verify`, ending in Review.

## 5. UX

- **Lives in the existing Workflows section** — definitions, Runs tab, graph drawer. No new IA surface.
- **Configuration**: NewWorkflowDialog gains a "Templates" row above the JSON editor (council-plan / council-build pre-fill the editor — user tweaks critics/providers inline). Schema-reference disclosure documents `fan`/`[[output:]]`.
- **Run**: when a definition declares `params`, the Run button opens a small popover with one textarea per param ("Request: …"). Scope selector unchanged — councils join Tasks like any run.
- **Live progress**: WorkflowGraph renders a fan node as a stacked card with a `×3` badge and per-branch status dots; clicking opens a branch list → each agent's diff/terminal (agents are real PTY frames — the user can watch any critic live today).
- **Disagreement display**: RunCard gains an "Outputs" disclosure built from the already-persisted `node_runs.output`: the plan, each critique with its VERDICT parsed into an APPROVE/REVISE chip, the revise ledger, the judge's Consensus/Disagreements/Unresolved block. Disagreements are the headline, not buried — show the structure, never a single auto-picked "winner" (LLM-judge style preferences diverge from humans).
- **Intervention points**: existing `cancel_workflow_run` (extended to kill all in-flight branches); open any live agent's terminal mid-run; v1 plan editing = cancel + rerun with an edited param (honest scope).

## 6. Termination — defaults grounded in the evidence

- **2 critics default, hard cap 4** — convergent finding: 2–4 agents, plateau ~5; among near-parity frontier models 2 diverse agents match 16 homogeneous, and our critics are cross-provider by construction.
- **Max 2 critique→revise rounds** (`max_iterations: 8`, `max_visits: 2` on critique/revise) — accuracy plateaus then *declines* with rounds (problem drift, error propagation; "Talk Isn't Always Cheap" shows degradation worsens over rounds).
- **Adaptive early-stop**: unanimous `VERDICT: APPROVE` routes straight to judge/terminal — iMAD-style "debate only when the answer looks uncertain" beats fixed schedules; expressible today as edge ordering.
- **Independent critiques, no cross-talk** — independent first drafts before any cross-exposure is the documented mitigation for sycophantic convergence; physically enforced by parallel isolated PTYs.
- **Planner revises; a different-provider judge accepts** — models can fix *located* errors but self-select badly (+10% self-preference bias; "When Agents Disagree": dedicated external selection wins). Judge provider ≠ planner provider de-correlates family bias.
- **Convergence = judge `APPROVE`** (last-line grammar) **or** iteration bound hit, in which case the run completes with status `completed` + an "unconverged after 2 rounds" note and the latest plan — never an infinite loop (`max_iterations` + `PER_NODE_CAP` already guarantee this).

## 7. Cost / latency controls

- Critics are **read-only profiles** (no builds/tests) on **user-chosen providers** — put cheap models on critique, the strong model on plan/revise (aider's asymmetric-roles lesson).
- Parallel fan: wall time = slowest critic. Early-stop skips revise+judge on clean plans (triage gate).
- Workers are killed the moment they `share` (existing) — no idle PTYs.
- `fan ≤ 4` validation; `MAX_CONCURRENT_WORKFLOW_RUNS = 8` already caps engine threads; branch spawns stay inside one run's budget.
- Per-branch output capped at 16 KiB in the join (blackboard values already MCP-capped at 64 KiB) so templated prompts stay paste-safe.

## 8. Failure modes

- **Critic CLI hangs**: per-node deadline applies to all branches at once; the join marks it `NO RESPONSE`; run proceeds on ≥1 critique, fails only on zero. (Strictly softer than today, where one hung node kills the run.)
- **Planner ignores critiques**: revise prompt *requires* a RESOLVED/REJECTED ledger keyed to critique issue numbers; the judge is instructed to emit REVISE if any issue is unaddressed — the loop, not hope, enforces incorporation.
- **Sycophantic convergence**: prevented structurally — critics never see each other or the planner's reasoning, stances engineer disagreement (Zen/PAL evidence: engineered beats organic), judge ≠ planner provider. Residual: branch workers hold `get` and could guess sibling keys — mitigated by profile instruction; key-scoping is a fast follow.
- **Routing misfire on free text**: verdict grammar is "last line, exact word"; templates use `/VERDICT:\s*REVISE/` regex, not bare keywords, and route the loop on the *judge's* short output, never long prose.
- **Cancel mid-fan**: engine's existing cancel-observation bail extended to kill every in-flight branch (process-group kill from f2abb7f makes this durable).
- **Oversized substitution**: truncate-with-note at the cap; worktree-file spill ("plan written to .taime/plan.md in your worktree") is the phase-3 escape hatch.

## 9. Phased plan

- **Phase 1 — substitution + params (1–2 days).** `[[output:]]`/`[[param:]]` in the engine; params through api/MCP/Run popover; `planner`/`plan-critic`/`plan-judge` builtins; ship `council-plan` with critics as *sequential separate nodes* (each reads only `[[output:plan]]` — content-independent, just slower). **The full loop proves value here with zero engine concurrency.**
- **Phase 2 — fan-out node (2–3 days).** Parallel branches, join, quorum timeout, `branch` column, kill-all-on-cancel, graph ×k badge + branch list.
- **Phase 3 — polish (2–3 days).** Run "Outputs" transcript with VERDICT chips and the judge's Consensus/Disagreements block; template picker; stance presets; spill-to-file.

~1 week total; value demonstrable after day 2.

## 10. Top 3 risks

1. **The evidence cuts both ways**: under matched compute, critique loops may not beat one strong model (Self-MoA; MAD corrections), and correct→incorrect flips are real. Mitigations are baked into defaults (2 critics, 2 rounds, early-stop, structured critique), and the human Review gate means a bad council costs tokens, never correctness of merged code — but we should not oversell auto-improvement.
2. **Free-text routing fragility**: the whole loop hinges on CLI agents emitting verdict grammar through a TUI. Grammar-on-last-line + regex edges + judge-only loop routing reduce this, but a misbehaving CLI version can still misroute; the unconverged-terminal default makes the failure benign.
3. **PTY paste limits**: multi-plan, multi-critique prompts approach paste sizes that can wedge Ink-style TUIs. The 16 KiB per-branch cap controls it short-term; the worktree-file spill is the real fix and must not slip past phase 3.