# Taime comprehensive review — June 2026

A 124-agent adversarial review of the app at `f2abb7f` (post daemon-hardening), plus external
research and a synthesized design for the adversarial multi-model feature ("Council").

> **⚠ Model correction (2026-06-10).** This review was written against an earlier framing where review/approval was treated as **mandatory** ("nothing merges without Review", "the Review gate"). That framing was an error. Taime's actual model: **attribution/audit is the always-on invariant; complete autonomy is the primary/default mode; review before merge/push/PR is OPT-IN, never mandatory.** The findings below still stand — but read every "nothing merges without Review" / "bypasses Review" claim as "the *opt-in* review/merge surface can't be trusted *when the user enables it*" and every attribution hole as a defect in the always-on substrate, not as a broken mandatory gate. Canonical model now: `architecture-lexicon.md` (Review row).

**How it was produced:** 9 review dimensions read the code in parallel; every finding was then
handed to an independent verifier instructed to refute it (high-severity findings got two —
a correctness-trace lens and a reproduction lens). 4 web researchers covered design
philosophies, the competitor landscape, the academic evidence on multi-model debate/ensembles,
and how real products implement multi-model loops. 3 designers independently designed the
Council feature under different lenses; a 3-judge panel scored them; a completeness critic
audited the whole run. Two verdicts were overturned on post-hoc adjudication (see
[findings.md](findings.md)).

**Verdict counts:** 76 confirmed / 7 refuted (+1 refuted→confirmed adjudication included in the 76 count below as the gate finding).

Contents:

- [findings.md](findings.md) — all 83 findings with evidence, fixes, and verifier notes
- [research/](research/) — the four research digests
- [council/](council/) — three design proposals + judge panel
- [completeness-critic.md](completeness-critic.md) — what this review did *not* cover

---

## 1. Executive summary

The architecture is genuinely good. The daemon-owned substrate (detached process, exact
repaint reattach, durable cleanup ledger, process-group kill, claim-first schedules) is
harder-won infrastructure than most competitors have, and the lexicon discipline shows
everywhere. The competitor research confirms the strategic bet: **attribution is a real white
space** — nobody in the category ships line/session-level provenance as a core feature, and
compliance pressure (EU AI Act, the Copilot co-author backlash) is making it purchasable.

**But the flagship thesis is not yet true in code.** The review's central result is a cluster
of confirmed-high findings showing that Attribution + Safe Context Switching + "nothing merges
without Review" currently fail in exactly the load-bearing places:

1. **The Review gate is UI placement, not enforcement** — `apply_selection`
   (`manager.rs:690-696`) merges with zero review-state check, and raw `Query` RPCs are
   reachable with the attach token.
2. **The merge actions themselves are broken** — the UI sends symbolic targets
   (`"main"`/`"self"`/agent-id) that the daemon treats as literal directories; every Merge and
   Revert from both review surfaces fails (or worse, applies to the wrong directory).
3. **Untracked files — the most common agent output — can never be merged or reverted**
   through Review (`git diff <base>` excludes them; zero hunks, never in the patch).
4. **Attribution silently lies in shared mode and on every schedule fire** — the fs watcher
   attributes the user's and other agents' edits to itself; schedule fires write directly
   into the user's real tree with nothing to merge, so the gate never engages.
5. **The frontend trust surfaces fail closed-looking-open** — daemon-down renders as an
   authoritative "No changes to review" with a working *Mark reviewed* button; review acks are
   one-shot per agent (later changes never re-raise the guard); closing a frame erases the
   daemon's dirty signal; detaching a view falsely marks running agents exited; Console input
   is silently dropped while echoing "sent to stdin".

None of these are architectural — they are finishable bugs sitting on a sound substrate. The
recommended posture: **stop adding surface area until the thesis cluster is fixed**, then ship
the merge/commit/provenance path (the #1 product gap *and* the attribution differentiator),
then build Council on top — its output lands in Review, so Review must work first.

---

## 2. P0 — make the thesis true

The cluster, in suggested fix order (full detail in [findings.md](findings.md)):

| # | Finding | Area |
|---|---------|------|
| 1 | Merge/Revert symbolic targets (`"main"`/`"self"`) used as literal `git -C` dirs — review actions cannot work | `DiffView.tsx:294` → `manager.rs:690` → `diff.rs:380` |
| 2 | Daemon enforces no review gate on `apply_selection` (adjudicated) | `manager.rs:690-696` |
| 3 | Untracked (agent-created) files unmergeable/unrevertable through Review | `diff.rs:205-213,314-324` |
| 4 | Shared-mode + schedule-fire misattribution (user's edits recorded as the agent's) | `session.rs:550-587`, `worktree.rs:64-75` |
| 5 | Schedule fires use shared worktrees — unattended writes land in the user's tree, bypassing Review | `manager.rs:1500-1509` |
| 6 | Review ack is one-shot — guard never fires again after first ack; "Proceed without review" durably recorded as a review | `store.ts:506-515,1162-1212` |
| 7 | Daemon-down fallback renders "No changes to review" + enabled Mark reviewed | `DiffView.tsx:85-90`, `api.ts:559-567` |
| 8 | `closeFrame` resets the daemon's authoritative dirty set without review | `store.ts:1113-1143`, `session.rs:318-328` |
| 9 | Clean detach reported as exit — running detached agents falsely exited, unreattachable | `daemon.rs:477-485`, `pty.ts:323-329` |
| 10 | Console tab input silently dropped (writes need an attachment the Console never has) | `ConsolePanel.tsx:76-93`, `daemon.rs:495-503` |
| 11 | Edits outside the agent worktree invisible to attribution/diff/review | watcher scope |
| 12 | PTY ids restart at 0 each daemon generation — durable session/graph rows corrupted on every restart (30s idle-exit makes this routine) | `manager.rs:278,315,351`, `store.rs:271` |
| 13 | Agent IDs are 32 random bits, no uniqueness check — collision silently merges two agents | id minting |
| 14 | Disabled schedules silently re-enable (and lose `last_run`) on every daemon restart | `manager.rs:1364-1408` |
| 15 | Task Activity tab passes a task id as `workspace_root` — turns never render | frontend |
| 16 | Corrupt/unopenable store → daemon runs forever with attribution silently off, stderr → /dev/null | `manager.rs:269-275` |
| 17 | `SessionSummary.role` added without a protocol version bump — same-version builds, incompatible wire layouts | `taime-protocol` |
| 18 | "Delete workspace" default path force-deletes isolated worktrees (unmerged work) behind a single un-typed confirm | UX |

Items 1–3 + 6–8 are, together, the statement "the Review pipeline does not currently work
end-to-end." They are also blocking for Council (§6), whose entire value lands in Review.

---

## 3. Reliability & security (confirmed, below the thesis cluster)

Selected; all in [findings.md](findings.md):

- **Daemon:** gc single-flight latch not panic-safe (one panic permanently disables all
  maintenance); untimed blocking PTY writes can wedge the maintenance tick; SQLite writes
  under the session state lock beneath the global sessions lock; accept-loop hot-spin on
  EMFILE; boot orphan sweep can SIGKILL agents owned by an alternate-socket daemon.
- **Orchestration:** workflow runs orphaned by restart stay `running` forever; keyword edges
  match anywhere in output (misrouting PASS/FAIL gates); engine can't detect a dead node
  worker (20-min wedge); cron evaluates UTC while prompt vars are local; orchestrator
  "lock-down" is advisory-only on codex (and pairs with `--yolo`); any tool-injected agent can
  overwrite user-authored workflow definitions.
- **Security:** raw control protocol bypasses MCP from-stamping (attribution edges forgeable);
  attach token is not an isolation boundary — destructive Query RPCs reachable off the curated
  tool surface; team tools are daemon-wide, not workspace-scoped (adjudicated); data dir and
  worktrees not permission-hardened like the runtime dir.
- **Persistence:** no downgrade gate (old binary re-stamps `user_version` downward); retention
  prune erases attribution for still-open work; unbounded growth in several tables;
  `synchronous=NORMAL` on the at-most-once schedule ledger.

**Refuted (don't fix):** MCP token argv leak (passes via env), the condvar
backpressure-park leak (detach/ack paths clear `paused` under lock), inbox 'pending' guard,
`assign` lockdown-bypass, unvalidated `working_directory` scope escape. See findings.md for
the refutation reasoning — several were plausible enough that the verification pass paid for
itself.

---

## 4. Design & UX assessment

The craft bones are right (Geist-mono identity, luminance-layer hierarchy, real keyboard
surface). Confirmed issues concentrate on **trust and destructive-action consistency**, which
matter disproportionately for this product: one-click Kill in grid frames vs two-step confirm
in AgentDetail; the context-switch guard modal has no keyboard support, no focus trap, and
visually emphasizes "Proceed without review"; `AddScheduleDialog` breaks dialog conventions
(no Escape/autofocus/daemon gating); informative micro-copy at 9–10px in failing-contrast
grays; zero-state panes that render a lone unlabeled button.

From the design-philosophy research ([research/design-philosophies.md](research/design-philosophies.md)),
the principles most worth internalizing, in priority order for Taime:

1. **Verification is the product** — make the diff-review surface the hero; per-agent
   attribution visible; track accept/dismiss rates.
2. **Calm periphery, loud center** — the agent grid is ambient glanceable state; only
   "needs attention" earns saturation or motion.
3. **Block/ledger model for agent output** — discrete units with status, duration, exit code;
   a live progress ledger per agent (Warp's pattern).
4. **Keyboard identity = muscle memory** — every mouse path needs a key path (the guard modal
   violates this today).
5. **Approval gates as first-class UI** — plan-before-code approval, per-agent permission
   scopes visible at a glance.
6. **Lean into "Technical Mono"** — schematic, data-forward, no gradients/glass; the
   attribution/ledger view is the signature element; differentiate from the Geist-clone wave
   with warmth + density, not novel chrome.
7. **3–5 visible parallel agents is the design sweet spot** — default layout for that count,
   overflow demoted to a list.

---

## 5. Missing features (verified against what exists, ranked)

All 12 confirmed; competitor grounding in
[research/competitor-landscape.md](research/competitor-landscape.md).

**High — activation/churn risk:**
1. **Merge dead-ends at `git apply`** — no commit, no push, no PR, no provenance in git. The
   moment a user drops to the terminal to commit, all attribution evaporates. Fix doubles as
   the differentiator: commit in the agent worktree with `Co-authored-by` +
   `Taime-Agent-Id` trailers, merge/cherry-pick to main, optional `gh pr create`.
2. **Attribution recorded but never exportable** — no agent-trace / git-notes / JSON artifact.
   Adopting the emerging `agent-trace`/`git-ai` formats would make Taime first-to-market in
   GUI form (nobody ships this).
3. **No OS-level notifications** — blocked/finished agents go unnoticed unless frontmost.
   (Notify on completion/needs-attention only — the Warp/Conductor pattern.)
4. **No conversation resume / relaunch-into-worktree** for exited agents — table stakes
   (every CLI supports `--resume`/`--continue`).

**Medium — weekly friction:** run/test scripts against an agent's worktree from Review;
Schedules/Workflows run history (unattended work is unauditable); in-app profile editor +
per-launch model choice; searchable attribution (file → agents query); best-of-N launch +
side-by-side compare (the category is converging on this; also the natural seed of Council);
open-in-editor / reveal-worktree escape hatch; exited-agent cleanup UI (the `forget` action
is currently dead code).

**Low:** task ↔ GitHub Issues/Linear linkage.

---

## 6. Council — the adversarial multi-model feature (synthesized design)

Your ask: *one model/provider drafts a plan → fed to 1–3 other models in parallel → responses
come back → planner validates and incorporates what it agrees with → loop N times.*

Three independent designs were produced and judged
([council/](council/)): **A** (leverage the existing workflow engine), **B** (first-class
primitive), **C** (UX-first). Judge consensus: **A's substrate, C's experience, B's hardening
grafts.** A scored highest on feasibility (9/10 — "its diagnosis of the three missing
primitives matches workflow_engine.rs exactly"); C on evidence-completeness and UX (9, 8).
All three independently converged on the same name (**Council**), the same loop shape, and
the same lexicon discipline (no new nouns — a Council compiles to an ordinary Workflow whose
seats are real Agents with real worktrees and full attribution).

### What the evidence says (research/adversarial-ai-evidence.md)

The 2025–26 literature both supports and constrains your idea:

- **Cross-model critique of a plan measurably helps** on code, math, and constraint-checkable
  planning (+7–15 pts in consensus-ensemble studies) — *when critiques are structured and
  grounded*. It is weakest/negative on open-ended judgment, where critique induces conformity.
- **Free-form debate is the wrong shape**: correct→incorrect flips outnumber the reverse;
  degradation worsens over rounds. Structured, independent critique beats discussion.
- **2–3 critics, 1–2 rounds** is the plateau; more rounds *decline* (problem drift, error
  propagation). Adaptive early-stop beats fixed schedules.
- **Cross-provider diversity wins among near-parity frontier models** (2 diverse ≈ 16
  homogeneous) — your multi-CLI substrate is exactly the right vehicle — but if one model is
  clearly strongest, sampling it repeatedly wins (so allow same-provider seats, with a note).
- **The planner should revise, but never judge acceptance**: models fix *located* errors well
  yet self-select badly (+10% self-preference bias). A separate, different-provider judge
  decides; anonymize artifacts before judging.
- **Executable feedback outranks every model-only signal** (Reflexion/LLM-Modulo): an
  optional `verify_command` seat (e.g. `cargo check` against the plan's claims) is the
  strongest single addition.
- **Honest framing**: under matched compute, ensembles often lose to best-single-model +
  tests. Market Council as *structured plan review in front of the Review gate*, not
  auto-improvement.

### The design

**Identity.** A Council is a Workflow template family + one engine capability. `CouncilConfig`
(planner seat, 1–3 critic seats with stances, judge seat, `max_rounds`, optional
`verify_command`) compiles to an ordinary `WorkflowDefinition`; runs are ordinary Runs with
`task_id`; every seat is a real Agent (minted ID, own worktree, turns, diffs). Attribution
extends upstream of code: *who proposed, who objected, what changed, why* becomes queryable
provenance. No wire-protocol bump — everything rides the existing JSON `Query` channel.

**The loop (round r):**
1. **Plan** — persistent planner agent (keeps its explored-codebase context across rounds)
   writes `PLAN.md`, reports via `share`/`council_submit`.
2. **Fan-out** — N critic agents spawn *in parallel, fresh each round*, worktrees forked from
   the same base commit. Each sees only the plan — never each other (independent first
   drafts; the documented sycophancy mitigation, physically enforced by isolated PTYs).
   Stance-steered prompts ("find what breaks" / "find what's overengineered") with a
   structured contract: `VERDICT: APPROVE|REVISE` + numbered objections, each requiring
   file:line evidence; *unjustified agreement is a failed review*.
3. **Join** — quorum: proceed on ≥1 critique, mark stragglers `timed out`, killpg the rest.
   Unanimous APPROVE in round 1 → skip straight to judge (adaptive early-stop).
4. **Revise** — the planner receives the critiques and must emit a **per-objection
   disposition ledger**: `ACCEPTED` (citing the changed section) or `REBUTTED` (with reason).
   The daemon hashes plan v(r) vs v(r+1) to compute a `responsive` flag — the
   planner-ignored-critiques detector.
5. **Judge** — fresh agent, **provider ≠ planner (enforced at validation)**, receives the
   full dossier with provider names stripped. Emits four sections — Consensus /
   Disagreements / Unique findings / Decision — and `APPROVED | REVISE | ESCALATE`.
   `ESCALATE` → run status `awaiting_user`: the human is the verifier of last resort.
6. **Iterate scoped** — round-2 critics receive only unresolved objections + the plan-version
   diff (flagged for new regressions), never a fresh full critique. A **no-progress
   detector** (unresolved count not decreasing) halts with "no consensus" + the ledger.

**Artifact transport** (the CLIs are PTY agents, not APIs): prompts carry role contract +
file paths, never full artifacts (PTY paste is fragile). Artifacts live in a **dossier
outside every worktree** — `data_dir()/taime/councils/<run_id>/round-<n>/` — wired to the
just-hardened durable-cleanup ledger, so council artifacts never pollute attribution, diffs,
contention, or Review. Critics read absolute dossier paths natively (grep/cat). Dossier
access is seat-scoped (a critic cannot read sibling critiques).

**UX** (C's design, the judges' favorite): a **Council tab** in the New Workflow dialog —
objective textarea, planner provider defaulted to the strongest installed CLI, two critic
chips auto-filled cross-provider with stances pre-assigned, judge row auto-locked to a third
provider, rounds stepper (1–3, default 2), optional verify command, cost line ("≈7 agent
turns, ~6–12 min"). Live progress is a **round timeline** (Round 1 — Plan ✓ 1m42s · Critiques
2/3 · Revise — · Judge —); each seat is a clickable block through to the real agent frame.
The centerpiece is the **Objection Ledger**: one row per objection — text · critic ·
disposition (Accepted→links the changed plan section / Rebutted→reason / Unaddressed, amber)
· judge ruling — plus a side-by-side plan v1→v2 diff annotated by the objection IDs that
drove each change. Disagreements are shown, never auto-resolved. Intervention: pause at phase
boundaries, edit the plan before critics see it, per-objection override, "stop and adopt
current plan". Effort presets: **Quick** (1 critic, 1 round, no judge) / **Standard**
(defaults) / **Deep** (3 critics, 3 rounds).

**Evidence-grounded defaults:** 2 cross-provider critics (hard cap 4) · `max_rounds: 2` ·
early-stop on unanimous APPROVE · independent parallel critiques · planner revises, judge
(≠ provider) accepts · anonymized judging · no-progress halt · verify_command when present
dominates model verdicts.

**Build phases** (A's sequencing — prove the loop in days, then invest):
1. **Days 1–2:** `[[output:NODE]]` / `[[param:NAME]]` prompt substitution + run params
   (reuses `schedules.rs::substitute_vars`) + built-in `planner`/`plan-critic`/`plan-judge`
   profiles + a `council-plan` template with critics as *sequential* nodes. The full loop
   works here with zero engine concurrency.
2. **Days 3–5:** fan-out node + parallel join + quorum timeout + `branch` column +
   kill-all-on-cancel + graph ×N badge.
3. **Week 2:** Council config tab compiling to workflow JSON; round timeline; Objection
   Ledger + plan-version diff; dossier + seat scoping; responsive flag; presets;
   anonymization.
4. **Fast-follow:** **"Convene council" on an existing agent's diff** from the Task Review
   header (`subject_kind: diff`) — the strongest thesis tie: Review gains "reviewed by N
   models" as evidence attached to the human gate (never replacing it). Plus
   verdict-vs-human-merge telemetry (does APPROVED predict the human merging? — the future
   noise filter).

**Top risks:** verdict-grammar fragility through TUIs (mitigate: last-line grammar, regex
edges, judge-only routing, parse failure degrades to "no consensus", never a wedge); critique
noise → trust collapse (mitigate: stances, citation-required contracts, Quick default,
objection accept-rate telemetry); value risk under matched compute (mitigate: plan-stage-only
scope — cheap tokens, high leverage — and the verify seat).

---

## 7. Suggested sequencing

1. **Thesis sprint (P0 §2):** review-gate enforcement + merge-path fixes + attribution holes
   + the frontend trust cluster. Roughly 18 findings, mostly small; several share root causes.
2. **Commit & merge with provenance** (gap #1) + agent-trace export (gap #2) — turns the
   biggest hole into the differentiator.
3. **Council phases 1–2** — cheap, fast, and the loop's output lands in a Review pipeline
   that now works.
4. **Gaps cluster:** OS notifications, resume, run history, model picker, cleanup UI.
5. **Council phase 3–4** (config tab, ledger, convene-on-diff, telemetry).
6. **Hygiene (from the critic):** CI for `cargo test --workspace` + vitest (nothing runs
   them today), LICENSE, daemon log file (stderr currently → /dev/null), corrupt-store
   move-aside recovery, updater/signing decision, supply-chain audit (`wezterm-term` git dep).

## 8. What this review did not cover

See [completeness-critic.md](completeness-critic.md): packaging/updates/signing, CI, the
app-host crate (`src-tauri/src/daemon.rs`) as a unit, `emulator.rs`/`repaint.rs` correctness,
cross-platform stance, observability, supply chain, performance/capacity quantification,
maintainability of the two god files (`manager.rs` 2,959 lines, `store.rs` 2,155), and
screen-reader accessibility. The lexicon's "nothing merges without Review" claim should be
annotated until §2 items land — docs currently assert a guarantee the code doesn't make.
