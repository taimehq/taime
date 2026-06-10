# Council — adversarial multi-model validation as a Taime primitive

## 1. Name candidates

1. **Council** (recommended) — a Library definition whose executions are **Council Runs**, reusing the lexicon's existing Run noun rather than minting "Deliberation". Seats, Rounds, and Verdicts are the only new sub-nouns.
2. **Panel** — reads well next to Review ("panel-reviewed"), weaker as a verb ("convene a council" beats "run a panel").
3. **Roundtable** — friendly but hides the adversarial intent; rejected per the lexicon's anti-coinage rule unless marketing demands softness.

## 2. Conceptual model

A **Council** is a Library definition (global, daemon-owned, `~/.taime/councils/*.json`), sibling of Workflow — not a Workflow special-case, because the existing engine is strictly sequential with one scalar output per node, and a council needs parallel fan-out, structured multi-artifact rounds, and typed verdicts.

- **Seats, not Profiles.** A council has one **Planner** seat, 1–3 **Critic** seats, one **Judge** seat. A seat = Profile × Provider (× model) plus a **stance**. Profiles stay behavior definitions; the seat is the role-in-this-council. Two new built-ins: `council-critic` (read-only, researcher-grade lockdown like `orchestrator`'s `fs_read`/`fs_list`) and `council-judge`.
- **Agents stay the pivot.** Every seat occupant in every round is a real Agent: minted Agent ID, own Worktree, Turns, Status — fully attributed. The Planner is **one persistent agent across rounds** (full thread context — the Zen/PAL threading lesson); Critics are **fresh agents each round** (independent drafts, no cross-exposure); the Judge is fresh, fed the full dossier.
- **Artifacts are attribution records.** Position → Critiques → Revision → Verdict are durable rows keyed by the authoring Agent ID — "who proposed, who objected, what changed, why" becomes queryable provenance, same substrate as Turns/Diffs.
- **Task/Run scoping** mirrors Workflow Runs: a Council Run carries a nullable `task_id`; seat agents inherit it (spawn-path rule 4 extends to a fifth path).
- **Review.** A verdict never merges anything. It attaches machine-review evidence to the Planner's Agent ID and rolls up into Task Review as "Council: APPROVED · 3 models · 2 rounds". The invariant strengthens: nothing merges without Review, and Review can now *contain* cross-model validation. Humans remain the only merge authority (Cursor/worktree lesson 7: never auto-merge).
- **Workflows compose it.** A `WorkflowNode` may delegate to a council (§3); the verdict keyword becomes the node's routed output, so existing `keyword:APPROVED` edges work unchanged.

## 3. Data model + wire protocol

**`council.rs` (new, sibling of `workflow.rs`):**

```rust
pub struct CouncilDefinition {
    pub name: String,
    pub planner: Seat,
    pub critics: Vec<CriticSeat>,        // validate 1..=3
    pub judge: Seat,                     // validate: judge.provider != planner.provider
    #[serde(default = "default_rounds")] // 2
    pub max_rounds: u32,
    #[serde(default)]
    pub gate_rounds_on_user: bool,       // pause for human between rounds
    pub brief: Option<String>,           // template; [[task]], [[subject]] substituted
}
pub struct Seat { pub profile: String, pub provider: String, pub model: Option<String> }
pub struct CriticSeat { #[serde(flatten)] pub seat: Seat, pub stance: Stance }
pub enum Stance { Breakage, Overengineering, Security, Generic } // PAL stance-steering
```

**Store (append to `SCHEMA`):**

```sql
CREATE TABLE taime_councils (name TEXT PRIMARY KEY, file_path TEXT,
    definition TEXT NOT NULL, source TEXT, created_at INTEGER);
CREATE TABLE taime_council_runs (
    id TEXT PRIMARY KEY, council_name TEXT NOT NULL, workspace_root TEXT,
    task_id TEXT, status TEXT NOT NULL,      -- running|completed|failed|cancelled|awaiting_user
    round INTEGER NOT NULL DEFAULT 1,
    planner_agent TEXT,                       -- Agent ID, persistent across rounds
    verdict TEXT, verdict_source TEXT,        -- judge|unanimous|user_override
    subject_kind TEXT,                        -- plan|diff  (what is being validated)
    workflow_node_run_id TEXT,                -- non-null when embedded in a workflow run
    started_at INTEGER, ended_at INTEGER, error TEXT);
CREATE TABLE taime_council_artifacts (
    id TEXT PRIMARY KEY, run_id TEXT NOT NULL, round INTEGER NOT NULL,
    kind TEXT NOT NULL,                       -- position|critique|revision|verdict|user_note
    author_agent TEXT NOT NULL,               -- attribution anchor
    seat TEXT, stance TEXT,
    verdict TEXT,                             -- APPROVE|REVISE|BLOCKER (critique) / APPROVED|REJECTED|ESCALATE
    confidence INTEGER,                       -- 0-100, self-reported (DebUnc finding)
    responsive INTEGER,                       -- daemon-computed: did revision actually change?
    body_path TEXT, created_at INTEGER NOT NULL);
CREATE INDEX idx_council_artifacts_run ON taime_council_artifacts(run_id, round);
```

**Wire protocol: zero new `ClientMsg` variants.** Workflows already ride `Query { kind, args }`; councils add kinds `councils`, `council_create`, `council_run` (args: name, workspace_root, task_id?, subject_agent? for diff-validation), `council_run_status`, `council_artifact_body`, `council_cancel`, `council_intervene` (inject `user_note` / resume a gated round / override verdict). No protocol bump; the UI polls like `WorkflowGraph` does.

**MCP tools (mcp.rs):**

- `council_submit { kind, verdict?, confidence?, body?, body_file? }` — the seat's report-back. Author stamped from the authenticated caller (the existing spoof-proof pattern); the engine validates the caller is the seat it is currently awaiting. `body_file` is a path **validated to be inside the caller's worktree**; the daemon reads it, copying into the dossier — this is the unbounded-size channel, and it keeps the artifact visible in the agent's own diff (attributed authorship for free). Inline `body` stays under the existing 64 KiB cap.
- `convene_council { name, subject? }` + `create_council { definition }` for orchestrators; guarded like `run_workflow` (a new `is_council_seat(caller)` check prevents recursive convening).

**`WorkflowNode`** gains `#[serde(default)] pub council: Option<String>` — when set, the engine runs that council as the step; node output = `"<VERDICT> <one-line summary>"`. Old definitions parse unchanged.

## 4. Engine mechanics — how text physically moves

New `council_engine.rs`, same shape as `workflow_engine.rs` (background thread, store-mediated, poll loops), plus the one new capability: **parallel spawn + concurrent collection**.

The physical transport is a **Dossier**: `data_dir()/taime/councils/<run_id>/round-<n>/` containing `position.md`, `position.diff`, `critique-<agent_id>.md`, `verdict.md`. It lives **outside every worktree** so Review/diff/contention stay unpolluted (the same reason atomic-write temps were filtered). Prompts carry absolute dossier paths; the CLIs run unrestricted (`--dangerously-skip-permissions`/`--yolo`) and read them natively. Dossier dirs get durable cleanup rows (the just-hardened H2 machinery).

Round *r*:

1. **Position.** Round 1: provision the Planner's worktree (task brief seeded via the inbox, exactly like `spawn_workflow_node`). The Planner writes `PLAN.md` in its worktree (attributed file activity), then `council_submit(kind="position", body_file="PLAN.md")`. The daemon copies it plus the Planner's current diff (existing diff machinery) into the dossier.
2. **Fan-out.** Spawn all critics **in parallel**, each a fresh agent in a worktree forked from the same base commit, prompt = stance contract + dossier paths for *this round's position only* — critics never see each other or prior approvals (independent-first-drafts evidence). Stance prompts demand: every objection cites `file:line` or quotes the plan; unjustified agreement is non-compliant ("structured critique penalizing unjustified agreement").
3. **Collect.** Poll `taime_council_artifacts` for each critic's `council_submit(kind="critique", verdict, confidence)` with a per-seat budget; `kill()` (process-group, H1) each critic on submit or timeout.
4. **Converge or revise.** Mechanical check, no LLM: all collected critiques `APPROVE` → step 6. Else if `round < max_rounds`: deliver critiques to the *live* Planner via the inbox (idle-gated stdin) with dossier paths; Planner updates `PLAN.md`, submits `kind="revision"`. The daemon hashes position vs revision to compute `responsive` (planner-ignored-critiques detector). Loop.
5. **Gate (optional).** `gate_rounds_on_user` → status `awaiting_user`; resume/inject note via `council_intervene`.
6. **Verdict.** Spawn the Judge (provider ≠ planner, enforced at parse): fresh agent, prompt = the *entire* dossier in order, who said what at which round (lesson 6: merger sees the thread, not a flat summary). It must emit four sections — Consensus / Disagreements / Unique findings / Decision (Council Mode structure) — and `council_submit(kind="verdict", verdict=APPROVED|REJECTED|ESCALATE)`. Persist; if workflow-embedded, write the node's blackboard key so edges route.

## 5. UX

- **IA**: "Councils" joins Workflows in the Library sidebar. `CouncilsScreen` mirrors `WorkflowsScreen` (header · run-scope Task selector · Definition | Runs tabs). `NewCouncilDialog` mirrors `NewWorkflowDialog`: JSON tab with template + client pre-validation, and "Generate with AI" via a `councilGenPrompt.ts` contract block.
- **Convene points**: (1) Councils screen Run; (2) **Task Review header — "Convene council"** with `subject_kind="diff"`, validating an existing agent's diff (the thesis tie-in); (3) a workflow node.
- **Live progress**: a run drawer (sibling of `WorkflowGraph`) showing a **round timeline**: rows = rounds, columns = Planner | Critics | Judge. Seat cards reuse the Status badge; clicking opens the live terminal frame (they're real agents) or the diff.
- **Disagreement display**: per-critic verdict chips (APPROVE emerald / REVISE amber / BLOCKER rose) with confidence; the Judge's verdict rendered as its four sections, disagreements **shown, never auto-resolved** (llm-council lesson: judge style-preferences diverge from humans).
- **Intervention**: pause-per-round toggle, inject note, cancel (reuses `cancel_workflow_run` semantics), override verdict (`verdict_source="user_override"`, recorded as a `user_note` artifact).
- **Review surface**: Task Review and the agent diff drawer gain a "Council" badge → run drawer. Merge buttons unchanged.

## 6. Termination — evidence-grounded defaults

- **Critics: 2 (range 1–3).** Convergent finding: 2–4 agents, plateau ~3–5 (72→87% from 1→3, flat beyond).
- **Rounds: `max_rounds=2`.** Accuracy plateaus then *declines* with repetition/context-overload (arXiv 2506.00066); "1–2 critique-revise rounds" is the digest's bottom line.
- **Early stop**: unanimous critic APPROVE in round 1 short-circuits to the Judge immediately (iMAD adaptive-break: spend debate only on uncertain cases).
- **Per-round behavior**: critiques independent and parallel (correct→incorrect flips dominate when agents see each other, 2509.05396); the Planner revises (models *can* fix located errors, 2311.08516 — the bottleneck is detection, which critics provide); acceptance decided by a **separate, different-provider Judge** (self-preference +10% inflation, 2410.21819; "When Agents Disagree": dedicated external selection beats self-selection).
- **No consensus after `max_rounds`** → Judge must pick REJECTED or ESCALATE; ESCALATE surfaces as `awaiting_user` in Task Review — the human is the verifier of last resort (LLM-Modulo: model-only signals never outrank external decision).

## 7. Cost / latency controls

Effort presets at convene time (Copilot pattern): **Quick** = 1 critic, 1 round; **Standard** = defaults; **Deep** = 3 critics + 3 rounds. Per-seat `model` lets critics run cheap models (Aider: cheap models review/edit well). Critics killed the moment they submit; the Planner runtime is reused across rounds (no respawn). Parallel critics make round latency = slowest critic, not the sum. Stance contracts cap critique length. Councils are opt-in per task — the triage gate is the human (skip the council for trivial work).

## 8. Failure modes

- **Critic CLI hangs**: per-seat budget (default 10 min, vs the engine's 20); proceed on quorum ≥1 collected critique, record a `timed_out` artifact, killpg the straggler. Zero critiques → round fails like a timed-out workflow node.
- **Planner ignores critiques**: daemon-computed `responsive` flag (revision hash unchanged ⇒ unresponsive, surfaced in UI); the Judge's contract explicitly grades responsiveness and may REJECT.
- **Sycophantic convergence**: structural, not prompt-hope — critics never see other critiques; stances engineer disagreement (PAL); judge ≠ planner provider enforced at validation; confidence required on every critique.
- **Daemon restart mid-run**: rows are durable; startup marks `running` council runs `failed` (matches orphaned-session handling), dossier cleaned by the durable-cleanup pass. Resumability is Phase 5.
- **Recursive convening**: `is_council_seat` guard mirrors `is_workflow_worker`.

## 9. Phased plan (~5 weeks)

1. **Substrate (1.5–2 wk)**: `council.rs`, schema, dossier + cleanup wiring, `council_submit`, `council_engine.rs` (parallel collect), Query kinds, cancel.
2. **UX (1–1.5 wk)**: CouncilsScreen, run-timeline drawer, NewCouncilDialog + gen prompt.
3. **Review integration (1 wk)**: Task Review badge + "Convene council" on a diff (`subject_kind="diff"`).
4. **Composition (0.5–1 wk)**: workflow-node `council` field; orchestrator MCP tools.
5. **Hardening (1 wk)**: round gating, restart resumability, and **verdict-vs-human-merge telemetry** (Greptile lesson: learn whether APPROVED predicts the human actually merging — the future noise filter).

## 10. Top 3 risks

1. **Critique noise → feature abandonment.** If critics bikeshed, users stop convening. Mitigation: stance steering, citation-required contracts, Quick preset default, and Phase-5 telemetry to measure verdict/human agreement before deepening investment.
2. **Engine surface area.** Parallel PTY agents multiply timeout/leak/zombie paths. Mitigation: reuse the workflow engine's store-mediated pattern plus the just-landed killpg/durable-cleanup/IPC-timeout hardening; no new IPC primitives.
3. **Latency/cost perception.** 4–8 CLI launches per run with cold starts; a Standard run is minutes, not seconds. Mitigation: persistent planner, parallel critics, early-stop, and honest live progress UI so waiting feels observable rather than stuck.