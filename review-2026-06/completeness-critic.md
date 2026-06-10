# Completeness review — what the multi-agent review missed

## 1. Verification integrity problems (highest priority — adjudicate before shipping the report)

- **Direct cross-dimension contradiction:** providers-mcp CONFIRMED "Team tools are daemon-wide, not workspace-scoped" while security REFUTED the same claim. I verified the code: `mcp.rs:224-239` `list_agents` returns `manager.list()` with **no workspace filter** — the CONFIRMED verdict is right; the security refutation needs re-adjudication.
- **Summary/verdict mismatches inside providers-mcp:** its summary asserts the argv token leak and the `assign` lockdown-escape as fact, yet both findings are labeled refuted. Verified: the token is injected via **env** (`manager.rs:343`, `TAIME_MCP_TOKEN`), never argv — the refutation is correct and the summary prose is wrong. Do not quote that summary. Its report also ends with stray `</summary></invoke>` markup — possible truncation/corruption.
- **attribution [refuted/high] "review gate is UI-only" looks wrongly refuted:** `manager.rs:690-696` `apply_selection` applies with zero review-state check, exactly as the dimension's own summary asserts. Thesis-level — deserves a second opinion. (I independently re-confirmed the companion high: `DiffView.tsx:295` sends literal `"main"`/`"self"` which `diff.rs:380` uses as `git -C <target_dir>` — real.)
- **daemon-lifecycle [refuted/medium] lost-condvar wakeup** is asserted as real in that dimension's own summary. Same internal contradiction pattern.

## 2. Subsystems/files no dimension covered

- **Packaging/distribution:** `src-tauri/tauri.conf.json` has no updater plugin, no signing/notarization; the daemon ships via `bundle.resources:["binaries"]` + `scripts/stage-daemon.sh`. Combined with the documented "protocol bump replaces daemon and kills agents" policy (`DEVELOPING.md`), there is **no upgrade story at all** — unreviewed.
- **No CI:** no `.github/`, nothing runs the 141 daemon + 3 integration + 91 vitest tests automatically; DEVELOPING.md itself warns bare `cargo test` silently skips the daemon crate. Also **no LICENSE file**.
- **App-host crate as a unit:** `src-tauri/src/daemon.rs` (658 lines — spawn-detached, `restart_daemon`, attach pump) was only grazed from the daemon and frontend sides.
- **Docs accuracy:** `architecture-lexicon.md` canonically claims "nothing merges without Review" — contradicted by code; nobody flagged lexicon-vs-code drift, or audited README/DEVELOPING.
- **Hardening-plan follow-through:** `daemon-hardening-plan.md` claims all items complete; no one verified item-by-item (note persistence's refuted inbox-'pending' finding overlaps plan item M3 — likely the same disputed code).
- Minor: `seedPrompt.ts`/`workflowGenPrompt.ts` (the LLM prompts that generate workflows — product-relevant prompt quality), `emulator.rs`/`repaint.rs` correctness, Tauri capability surface (`capabilities/default.json` is minimal — fine, but unexamined).

## 3. Review angles skipped

- **Cross-platform scope:** macOS-centric (`ps -E`, AppleScript clipboard); Windows never addressed even as an explicit non-goal; Linux only via one /tmp perms finding.
- **Observability:** detached daemon logs `eprintln!` → `/dev/null`; no log file, no crash reporting — diagnosability never reviewed as an angle.
- **Supply chain:** `wezterm-term` is a git dependency; no cargo/pnpm audit pass.
- **Performance/capacity:** no quantified pass (dual terminal grids per agent, single SQLite connection under N agents, poll multiplication, full worktree checkout disk cost).
- **Accessibility** beyond contrast/focus (screen reader/ARIA).

## 4. User questions left unanswered

- **The adversarial multi-model feature stops at scoring.** Three proposals + judge panel ≠ "design a feature": no winning design synthesized into a spec (data model, daemon/workflow-engine integration, review-surface UX, provider mapping). This was an explicit deliverable.
- **"Quality" lacks a maintainability dimension:** `manager.rs` 2,959 lines / `store.rs` 2,155 — god-object risk, module boundaries, error-handling consistency never assessed.
- **Research→product linkage:** philosophies/competitor research completed, but no synthesis tying it to a prioritized recommendation list (verify the parent has one).