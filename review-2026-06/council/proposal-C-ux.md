# Council — plan-critique-revise for Taime

## 1. Name candidates

1. **Council** (recommended) — a Workflow template whose Run convenes a planner, independent critics, and a judge. Short, maps to the research lineage (council/consensus), no collision with the lexicon (Review, Team, Orchestrator stay untouched).
2. **Roundtable** — warmer, but implies free discussion, which is exactly what the evidence says not to build.
3. **Quorum** — accurate for the judge/termination semantics, but obscure.

"Planner / Critic / Judge" are Profile names; "seat" is informal UI copy only (per the lexicon's role-vs-Profile rule).

## 2. Conceptual model — no conflation

- A Council is **not a new execution object**. It compiles to a **Workflow definition** (Library, global) from a small `CouncilConfig`. Running it creates an ordinary **Run** (workspace-scoped, optional `task_id` — the existing run-scope picker).
- Every seat is a real **Agent**: Profile × Provider, own Agent ID, own Worktree, full Turns/Diffs attribution. Nothing is invisible — the flagship thesis holds.
- New built-in **Profiles**: `council-planner`, `council-critic`, `council-judge` (critic/judge are read-only like `researcher`/`security-reviewer` — critics critique the plan, they never edit; aider's asymmetric-roles lesson).
- The Council is **engine-driven, not Orchestrator-driven**. A deterministic driver enforces independence, anonymization, and structured templates; an LLM orchestrator would reintroduce self-preference bias (+10% own-output inflation) and sycophancy. An Orchestrator may *convene* a council via a new MCP tool, but never moderates it.
- Output is a **plan artifact**, not merged code. "Send to implementation" launches a builder agent whose diff goes through **Review** — nothing merges without Review, unchanged.

## 3. Data model + wire protocol changes

**Workflow grammar** (`workflow.rs`, serde-compatible additions):

```rust
pub struct WorkflowNode {
    // existing: id, profile, prompt, output_key, provider
    #[serde(default)] pub model: Option<String>,      // per-seat model knob
    #[serde(default)] pub persist: bool,              // reuse the same agent across visits
    #[serde(default)] pub inputs: Vec<String>,        // blackboard keys materialized as files
}
pub struct WorkflowEdge {
    // existing: from, to, when
    #[serde(default)] pub mode: EdgeMode,             // Route (default) | Fanout
}
```

`persist: true` → the engine skips `manager.kill()` after the node reports; revisits deliver the next prompt via the existing inbox (`enqueue_message`, idle-gated). Killed at run end. `Fanout` edges fire *all* matching targets concurrently; a node with multiple fired in-edges waits for all (join-all).

**Council config** (daemon-side compiler input, also the MCP tool schema):

```rust
pub struct CouncilConfig {
    pub name: String,
    pub objective: String,                 // the task
    pub planner: Seat,                     // { provider, model?, profile? }
    pub critics: Vec<CriticSeat>,          // 1..=3, each { provider, model?, stance }
    pub judge: Seat,                       // validated: provider != planner.provider
    pub max_rounds: u32,                   // default 2
    pub verify_command: Option<String>,    // e.g. "cargo check" — run by a verifier seat
}
pub enum Stance { Breaks, Overengineered, MissingRequirements, General }
```

**Store**: new table `run_artifacts (id, run_id, round, kind: plan|critique|disposition|verdict|final, key, author_agent, content, created_at)` — the blackboard is last-writer-wins; the Council needs *versioned* history to answer "why did the plan change".

**Wire protocol** (protocol bump): `Request::CreateCouncil(CouncilConfig) -> name`, `Request::GetRunArtifacts(run_id) -> Vec<ArtifactMeta + content>`; run-summary JSON gains `council: { round, phase, seats: [{node_id, agent_id, provider_anon, status, elapsed}], objections: [...] }`. New MCP tool `create_council` (gated from workflow node workers like `run_workflow`).

## 4. Engine mechanics — the loop

Compiled graph: `plan` (persist) → fanout → `critic_1..N` → join → `revise` (= the persistent planner node, back-edge) → `judge` → `keyword:ACCEPT` → `finalize` | `keyword:ITERATE` → critics again | fallback `/.*/` → `finalize-unresolved`.

How text physically moves between CLI agents — three existing channels used deliberately:

1. **Prompt injection via inbox** (exists): each node's composed prompt is enqueued by the engine and delivered when the PTY is idle. Prompts stay short — role contract + file manifest, never full artifacts (PTY paste is fragile).
2. **Blackboard `share` via MCP** (exists): the report-back channel, as today (`{run_id}::{output_key}`). The engine snapshots every report into `run_artifacts` (round-stamped). The 64 KiB MCP cap is ample for plans/critiques; for safety the contract is file-first (below) with `share` carrying the same text.
3. **File materialization** (new, the `inputs` mechanism): the daemon owns every worktree, so before seeding a node it writes each input artifact to `<worktree>/.taime/council/round-N/<key>.md` and appends `.taime/` to `.git/info/exclude` — agents read inputs natively (grep/cat), artifacts never pollute attribution, diffs, contention, or Review.

Round walkthrough (defaults: 2 critics, judge, 2 rounds):

1. **Plan.** Planner (persistent agent — it keeps its explored-codebase context for revision, the "merger with full thread context" lesson) writes `PLAN.md` with numbered sections, shares it. Engine snapshots round-1 plan.
2. **Critique (parallel, independent).** Engine spawns N *fresh* critic agents simultaneously, each in its own worktree forked from the same base commit, each receiving only the plan file — never each other's output (independent first drafts; prevents the conformity cascade where correct→incorrect flips dominate). Each prompt is stance-steered ("find what breaks" / "find what's overengineered" — PAL's engineered disagreement) and template-constrained: `VERDICT: APPROVE|REVISE` plus numbered objections, each requiring justification + file/line evidence; "unjustified agreement is a failed review." Latency = slowest critic.
3. **Adaptive break.** All critics `APPROVE` in round 1 → skip straight to finalize (iMAD: debate only when warranted).
4. **Revise.** The persistent planner receives critiques as files plus: "For each objection ID, output a disposition: ACCEPTED (cite the changed plan section) or REBUTTED (with reason). Then the revised plan." Models *can* fix located errors — the disposition mapping is both the repair mechanism and the UX gold.
5. **Verify (optional, dominant).** If `verify_command` is set, a verifier seat runs it against the plan's claims (e.g. the plan's "no API change" vs `cargo check` on a spike). Executable feedback outranks every model-only signal (Reflexion, LLM-Modulo).
6. **Judge.** A different-provider agent receives plan v2, critiques, dispositions — with provider/model names stripped from all artifacts (anonymize before cross-ranking; judges demonstrably play favorites). Emits `ACCEPT` or `ITERATE: <unresolved objection ids>`.
7. **Iterate scoped.** Round 2 critics receive *only* the unresolved objections plus the diff between plan versions — never a fresh full critique (loop only on disagreement items; prevents problem drift and round-over-round degradation).
8. **Finalize.** Engine assembles `CONSENSUS.md` in the planner worktree + a `final` artifact: **Consensus / Disagreements / Unique findings / Final plan** (Council Mode's four sections). Unresolved disagreements are preserved, never hidden.

## 5. UX

**Configuration (<30s).** Workflows section, "New workflow" dialog gains a third tab: **Council**. One screen: objective textarea; planner provider (default: strongest installed CLI); two critic chips auto-filled cross-provider with stances pre-assigned — opinionated defaults, every knob overridable but none required; judge row auto-locked to a third provider with "why different provider" microcopy; rounds stepper (1–3, default 2); optional verify command flagged "strongest signal"; Task scope picker (existing); footer cost line: "≈ 7 agent turns, ~6–12 min".

**Live progress.** Council-shaped runs replace the generic graph drawer with a **round timeline**: `Round 1 — Plan ✓ 1m42s · Critiques 2/3 · Revise — · Judge —`. Each seat is a Warp-style block: status dot, anonymized label (Critic A/B; provider revealed at run end or on demand), live elapsed counter, click-through to the real agent frame (every seat is an attachable PTY) or its artifact. Calm periphery: only the active phase animates.

**Disagreement display — the product.** The **Objection Ledger**: one row per objection — text · critic seat · planner disposition (Accepted → links to the changed plan section; Rebutted → reason; Unaddressed, amber) · judge ruling. Plan-version view: side-by-side markdown diff of v1→v2 with changed sections annotated by the objection ID that drove them. This answers "why did the plan change each round" structurally, not by reading transcripts.

**Intervention.** Pause at any phase boundary (engine checks a flag like cancel does); edit the plan artifact before critics see it (human as a seat); per-objection override (mark resolved / must-fix — injected into the next round); "Stop and adopt current plan"; Abort = existing `cancel_workflow_run`. Final screen: CONSENSUS.md + ledger + **Send to implementation** (launches a builder agent seeded with the plan file, inside the Task → its diff flows into Review).

**IA.** Library: the compiled definition listed under Workflows with a `council` badge. Runs: the workflow's Runs tab + Task detail rollup.

## 6. Termination — evidence-grounded defaults

- **2 critics, judge as third voice** (3 total): convergent 2–4-agent finding; gains plateau ~3–5, with diminishing returns beyond.
- **2 rounds max, hard cap 3**: accuracy plateaus then *declines* with rounds (error propagation, context overload); degradation worsens over rounds in debate studies.
- **Early stop on unanimous round-1 APPROVE** (iMAD adaptive break beats fixed schedules) and **on judge ACCEPT**.
- **No-progress detector**: if round N's unresolved-objection count ≥ round N−1's, stop with "no consensus" + ledger — looping past disagreement is where correct→incorrect flips dominate ("Talk Isn't Always Cheap").
- **Judge ≠ planner provider, enforced**: self/family-preference bias (~+10%) contaminates planner-judged loops; dedicated external selection consistently wins ("When Agents Disagree").
- **Cross-provider critics by default; homogeneous allowed with a caveat note**: among near-parity frontier CLIs heterogeneity wins decisively, but Self-MoA shows one clearly-strongest model sampled repeatedly can beat mixing — so the UI permits same-provider seats and says when it's sensible.
- Backstops: existing `max_iterations`, `PER_NODE_CAP`, 20-min node timeout, `cancel_workflow_run`.

## 7. Cost/latency controls

Parallel critics (latency = slowest, not sum); per-seat `model` knob — critics on cheaper models, planner on the strongest (aider: asymmetric quality allocation); **Quick mode** preset (1 critic, 1 round, no judge — planner self-finalizes with the critique attached); user-invoked only, no automatic councils (the user is the triage gate); scoped round-2 prompts (unresolved items only) cut token spend ~60%; one run slot of the existing 8-run cap, ≤5 concurrent agents per council (matches the 3–5 parallel-agent design sweet spot); cost line shown before launch.

## 8. Failure modes

- **Critic hangs**: existing node timeout; quorum policy — judge proceeds with ≥1 arrived critique, the seat shows "timed out" in the ledger; all critics time out → run fails, plan preserved as artifact.
- **Planner ignores critiques**: revise contract requires per-objection dispositions; the engine diff-checks IDs, sends *one* nudge for missing ones, then auto-marks them **Unaddressed** — forwarded to the judge and rendered amber. Never silently re-prompts in a loop.
- **Sycophantic convergence**: structurally prevented — critics never see each other, stances force disagreement, templates penalize unjustified agreement, judge is cross-provider, artifacts anonymized, no-progress detector halts conformity oscillation.
- **Unparseable judge verdict**: keyword edges + the compiled `/.*/ → finalize-unresolved` fallback — garbage ends the run as "no consensus," never wedges it.
- **Persistent planner dies / daemon restarts**: artifacts are durable (`run_artifacts` + worktree files); engine respawns a fresh planner seeded with the full artifact history via `inputs`; restart-orphaned runs fail cleanly per existing behavior, artifacts intact.

## 9. Phased plan

1. **Engine substrate (~1.5 wk)**: `persist`/`inputs`/`model`/`Fanout`+join in grammar; frontier-set executor (sequential path unchanged when no fanout); `run_artifacts` table; file materialization + git-exclude; protocol bump + `GetRunArtifacts`.
2. **Compiler + profiles (~4 d)**: `CouncilConfig` → workflow; three built-in Profiles with structured templates; verdict/objection parsing; `create_council` MCP tool; quorum + no-progress logic.
3. **UX (~1.5 wk)**: Council tab in NewWorkflowDialog; round timeline + seat blocks; Objection Ledger + plan-version diff; pause/steer/abort; Send to implementation.
4. **Hardening (~4 d)**: verifier seat, cost estimates, restart recovery, and accept/dismiss telemetry on objections (Greptile's lesson: learn the noise filter from user feedback).

## 10. Top 3 risks

1. **Structured-output fragility**: routing and the ledger depend on CLIs emitting parseable verdicts through PTY+blackboard. Mitigation: file-first artifacts, strict templates, fallback edges, parse failure degrades to "no consensus" — never a hang or silent misroute.
2. **Engine regression surface**: parallelism touches the daemon's most safety-critical path (agent spawning under caps). Mitigation: no-fanout graphs execute on the existing code path; per-run agent ceiling; reuse cancel/timeout machinery; heavy unit coverage like `workflow.rs` has today.
3. **Value risk**: the 2025–26 corrections show ensembles often lose to best-single-model + tests at matched budgets; a noisy Objection Ledger erodes trust exactly like noisy review comments (>30% dismissal = trust collapse). Mitigation: verify-command emphasis, plan-stage-only scope (cheap tokens, high leverage), objection accept-rate tracking surfaced to the user, Quick mode as the honest default for small tasks.