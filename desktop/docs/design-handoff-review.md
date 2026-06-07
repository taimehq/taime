# Design handoff review — Claude Design bundle vs lexicon, daemon, and frontend

**Date:** 2026-06-07 · **Bundle:** claude.ai/design handoff `taime-tauri-app` (extracted at `/tmp/taime-design/taime-tauri-app`)
**Method:** 29-agent review — 10 parallel extractions (design model/chrome/surfaces/visual, current frontend, daemon protocol/capabilities, macOS+Tauri research, problem-space research), 3 gap analyses, 16 blocker/major findings adversarially verified against code (16 confirmed, 0 refuted). Full agent reports: `/tmp/taime-review/*.md`.

---

## Verdict

**The design is the right IA, the right lexicon, and the right bets — and the artifact is a storyboard, not a spec.** The conceptual skeleton matches the daemon ~95%: hierarchy (Workspace → nullable Task → Agent → Worktree), the taskId partition with first-class Uncategorized, task statuses, provider IDs, profile set, Definition/Run workflow split, task-scoped runs with node inheritance, explicit-never-default per-run schedule behavior, and the delegation MCP tools are all exact or near-exact matches with `desktop/docs/architecture-lexicon.md` and the shipped protocol-v9 daemon. Independent field research validates all four core bets. What fails production scrutiny is the *artifact layer*: the CSS, the prototype state model, the missing failure surfaces, and a set of fictions about the runtime (typed terminal streams, semantic agent IDs, in-repo worktrees, isolated scheduled agents).

Treat the bundle as **spec for layout, copy, and lexicon — never as code**. The current frontend is the engine: terminal stack, diff/attribution, guard semantics, reconcile lifecycle are production-grade and tested; the shell around them gets rewritten.

---

## 1. Design ↔ daemon: term-by-term scorecard

(verified against `taime-protocol/src/lib.rs`, `manager.rs`, `store.rs`, `schedules.rs`, `workflow.rs`, `workflow_engine.rs`, `mcp.rs`, `worktree.rs`, `profiles.rs`, `providers/mod.rs`)

| Axis | Design | Daemon | Status |
|---|---|---|---|
| Provider IDs | `claude_code/codex/gemini_cli/grok_cli` | identical (providers/mod.rs:246-249) | **MATCH** |
| Profile set | 6 built-ins incl. product-builder | identical (profiles.rs:168-218) | **MATCH** |
| Task entity + statuses | `open/in_review/done/archived` | identical, validated (manager.rs:758) | **MATCH** |
| taskId partition / Uncategorized | nullable, first-class | identical (lib.rs:366-368) | **MATCH** |
| Workflow Def/Run + node inheritance | yes | identical semantics (workflow_engine.rs:103-108) | **MATCH** |
| MCP delegation tools | assign/handoff/message/request/reply/share/get | exactly those + list_agents/broadcast/create_workflow/run_workflow (mcp.rs:29-159) | **MATCH** |
| Schedule task behavior **values** | `uncategorized/attach_to_task/create_task_per_run` | `absent/fixed/per_run` — same semantics + rollback (schedules.rs:36-40, manager.rs:1420-1427) | **NAMING — decide now** (user-authored file format) |
| Schedule frontmatter keys | `workspace_root`, `health_check`, Variables block | `workspace`, `script`, fixed `[[date]]/[[time]]/[[flow_name]]` allowlist (schedules.rs:104-110,160-167) | **NAMING + var-system gap** |
| Workflow node noun | `profile` | `role` (required JSON key, workflow.rs:41; mcp.rs:141 even glosses "role is a profile name") | **NAMING — decide now** (user-authored ~/.taime/workflows/*.json) |
| worktreeMode | `isolated\|shared` | `"worktree"\|"shared"` (lib.rs:201-203) | **NAMING** (canon doc already says isolated) |
| Agent status | lowercase; lexicon `running/idle/blocked`; STATUS_META defines 8 | `IDLE/PROCESSING/WAITING_USER_ANSWER/COMPLETED/ERROR` (lib.rs:380-393) + frontend-synthesized PENDING | **NAMING**; design's `queued/paused/review` agent statuses are unbacked — delete |
| Agent ID spelling | `agentId` uniformly | **five names**: attribution_key, terminal_key, terminal_id, agent_key, id (lib.rs:115,195; manager.rs:651,737,883) — all on the retired list | **NAMING — highest leverage rename** |
| Agent display name | semantic IDs (`coder-1`) + label | random 8-hex (manager.rs:323); **no label field anywhere** | **BACKEND GAP** — daemon must mint/store labels; forensic ID stays hex underneath |
| Task review rollups | `dirtyAgents/reviewedAgents/filesChanged/hunksPending` stored on Task | not stored; dirty derived live; **reviewed state is frontend-only** (store.ts reviewedFrames). store.rs:134-135 comments *promise* review aggregation | **BACKEND GAP — thesis-level**: park-and-resume is violated if review state dies with the UI |
| Schedule → Workflow target | `targetType: workflow\|agent` | agent-only; both fire paths (fire_schedule + run_schedule) hardcode fire_headless (manager.rs:1396-1431, 1442-1468) | **BACKEND GAP** (lexicon itself promises Agent\|Workflow) |
| Schedule run history | `runs:[{at,status,dur}]` | last_run/next_run scalars; last_run set even on **failed** fire and gate-skip — can't encode failure (manager.rs:1428-1430) | **BACKEND GAP** |
| Per-agent assignment | first-class field | inbox seed message, pruned at 30d; seed_prompt plumbed but unread by providers (lib.rs:176-178, store.rs:613-616) | **BACKEND GAP** (small: column + query) |
| Notifications / attention | persisted entity, approve/deny | live pushes only (StatusChanged/FsDirty/Exited); turns/fs/exits ARE persisted but no notification join; unattached agents discoverable only by polling | **DECISION**: daemon-side notification log (argues daemon per thesis) |
| Per-agent plan steps | plan + subtasks | nothing plan-shaped anywhere; only workflow node_states | **CUT or derive later** — do not build |
| PTY stream | typed line objects `{t:'prompt'\|'ok'\|...}` | raw ANSI bytes: T_DATA [offset][bytes], T_REPAINT [seq][escape] (lib.rs:87-97) | **DESIGN FICTION (blocker)** — Console mode must be a *projection* (turns-as-blocks), not a second stream; prototype even breaks its own one-stream rule (agents.jsx:335 vs :387-390) |
| Worktree path/branch | in-repo `.worktrees/<id>`, feat/* branches | app-data dir, `taime/<provider>-<key>` branches (worktree.rs:44-62,170) | **DESIGN FICTION** — display real paths (middle-truncated) |
| Scheduled-agent isolation | shown isolated | deliberately **shared** (manager.rs:1320-1327) | **CONFLICT with attribution thesis** — decide: keep shared (weaker attribution) or isolate unattended fires |
| Workspace | entity w/ ws-ids, active flag, counts | emergent project_root grouping (manager.rs:1009-1031) | design projection — frontend-owned collection is fine |
| Workflow entity | `wf-*` id, `WF-1042` code, description, def-level status | name-keyed, no id/code/description; status on runs only: `completed\|failed`; `queued` doesn't exist (runs start or are rejected at cap 8) | **NAMING + display fiction** |
| Retired-term leakage | clean (one `role` alias in ProfileChip) | attribution_key/terminal_key/tmux_session/Session grouping/[[flow_name]]/`role` throughout app-facing JSON | **BACKEND DEBT** — rename wave |

### Sequencing the reconciliation
1. **Freeze user-authored file formats now** (every day adds migration debt): schedule frontmatter keys + task_behavior values; workflow node `profile` (keep `role` as accepted alias one release).
2. **One rename wave on app-facing JSON**: `agent_id` everywhere (dual-emit one release), workspace_root, mode `isolated`, kill tmux_*/Session leakage. Postcard wire is positional — only serde_json key names matter.
3. **Small backend additions** (each <1 day): label + assignment columns on the worktree row; workflow-runs list query; schedule run-history table (with real success/failure); schedule `workflow:` target.
4. **Thesis-level decisions**: daemon-persisted review state + notification log (both argue daemon-side — "park and resume without losing state" is violated by frontend-only state); shared-vs-isolated scheduled fires.
5. **Design cuts**: typed PTY lines → raw xterm + turn-block projection; plan panel → workflow node_states only; queued/paused/review agent statuses → delete; `taime` CLI fixture text → MCP traces.

---

## 2. Design ↔ current frontend: rewrite scope

**Verdict: shell rewrite + store refactor + data-layer no-op.** Zero shared shell topology; ~70% of the hard code ports. `api.ts` already speaks the design's nouns (tasks, schedules with task_mode, workflows, attribution/hunked_diff/apply_selection) — no transport work beyond the daemon asks above.

**Migration order (each phase shippable):**
0. Tokens + fonts (bundle Geist woff2; resolve #c8c7c2 role — see §4)
1. Dead-code + lexicon sweep (killSession no-op, getTerminalStatus stub + 3s timer, sessionStatusRollup, lib/sessionName.ts, useTurnCheckpoints, cao_ws; Role→Profile labels; consolidate 8 provider-name maps)
2. Store refactor: agents (durable) + views (attached) replacing Frame/RustPtyMeta/terminalId triangle; workspaces collection; single status-map module; re-key reviewedFrames by Agent ID. **store.test.ts's 26 tests are the safety net — update in the same change.**
3. New shell: rail + sidebar + 3-col titlebar + section state; mount existing surfaces unchanged inside sections.
4. Tasks + Agents as screens (Task Review hosts DiffView internals re-homed; AgentDetail = focus mode + new chrome).
5. Net-new: Dashboard, Settings, Notifications slice, Onboarding, Console mode (projection of the PTY/turn stream).
6. Palette re-target + keyboard extension.

**Reverse gaps — the design must NOT regress these (it omits all of them):**
- **ContextSwitchGuard** (store.ts:871-907) — the flagship invariant; port as universal navigation gate. The design's biggest omission.
- Multi-terminal **ShellGrid** (keep as Agents-section view mode), 15-shortcut capture-phase keymap, daemon reconcile/detached/exited/crash-adoption, terminal find/file-drop/clipboard/model-chip, real overlay titlebar (tauri.conf.json already does what the design fakes), working schedule **creation** (design's Schedules view is read-only), foreign-workspace agent handling, loud-failure launch paths, ErrorBoundary/Snackbar, drag-resizable persisted sidebar (design hardcodes 240px), per-frame dirty chips.
- Component verdicts: current **CommandPalette skeleton wins** (design's has an invisible-cursor bug); current **WorkflowGraph wins** (real back-edge rendering; design's cyclic graphs render nothing); DiffView internals + design's chrome; design's NewAgentModal step structure + current's task-resolution logic. **Drop design's client-side ID minting (overlays.jsx:223) — fatal for a forensic-identity product.**
- Design state model is an antipattern catalog (global mutable arrays, remount key app.jsx:294 that **nukes view state on navigation — a direct contradiction of safe context switching**). Keep zustand; copy only `termModes` per Agent ID + the `taskInitialTab` deep-link trick.

---

## 3. Production-readiness critique of the design itself

**Meta-finding: the bundle disagrees with itself.** Screenshots = retired lexicon iteration (unusable as pixel reference); `_ds` kit.css = drifted source-of-truth (but holds the *better* scrollbar/selection/pulse specs); app.css silently rewrites foundation tokens (including destroying the "console is darkest" rule). Production needs one tokens.css + regenerated canon screenshots at 1280×820 **and 920×600**.

### macOS-nativeness (currently a web demo in a Mac costume)
- No `data-tauri-drag-region` anywhere; fake traffic lights with two different fake palettes. Production already ships native overlay chrome — the 40px design bar needs trafficLightPosition retuned y:24→~18-20, and the 88px reserve must **collapse in native fullscreen** (lights auto-hide; not collapsing is the classic web tell).
- **No menu bar designed, none in production Rust.** Without an Edit menu wired to standard selectors, **⌘C/⌘V/⌘X/⌘A don't reliably work in WKWebView text fields.** App menu needs ⌘, Settings; every advertised shortcut should exist as a menu item.
- **⌘K double-booked**: palette (app.jsx:159) AND "⌘K to jump between workspaces" (chrome.jsx:45). Reserve ⌘K for the palette; workspace switcher gets ⌘O/⌃⌘K.
- Breakpoints ≤820/≤680 are **dead code under minWidth 920** — and ≤680 hides the only navigation. Replace with container queries on the main column (for the good 1150/980 column-shedding) + user-toggled persisted panels. Never auto-hide the rail.
- `::-webkit-scrollbar` width styling converts macOS overlay scrollbars to always-visible classic — adopt kit.css's transparent-at-rest ghost thumb.
- No native context menus; WKWebView default right-click menu will leak through. No `::selection`; chrome text is drag-selectable.
- 1.5px border family renders unevenly on retina; 0.5px hairlines are free on always-WKWebView.
- Rail spec conflict: README 52px vs CSS 48px — pick 48 (what was visually approved).

### Pixel/alignment discipline (token system is decorative, not enforced)
- `var(--space-*)` appears **zero times** in app.css; 14px (off-ladder) is the de-facto padding. Re-declare the grid honestly, then enforce.
- Verified defects: **broken focus ring** (`box-shadow: 0 0 0 2px var(--ring-focus)` invalid → declaration dropped → settings inputs have NO ring, app.css:728); **Material Design diff colors** `#c8e6c9/#ffcdd2` on the flagship attribution surface (app.css:666-669); z-index inversion (ws-menu 200 > modal 110); `.t-mono` redefined losing tnum+slashed-zero; two clock styles, one without tabular-nums; three count-pill shapes, three active-bar widths, four card paddings, five icon-button sizes; pane widths 240/220/280 across sibling master-details.
- **Truncation is the sharpest failure — visible in the bundle's own screenshots at ~924px** (4px above production minWidth): wrapped "2 active" headline, mid-token ID wraps ("orch-"/"1", "WF-"/"1042"), clipped badges, overlapping bottom strip. Only `.titlebar-crumb` has an ellipsis rule. Required: a truncation contract (IDs nowrap+ellipsis+tooltip, paths middle-truncate, metrics in tnum fixed slots) + a hard 920×600 QA gate.

### Information architecture at scale and in failure
- Sidebar renders every task, no virtualization/search/cap; Running rows labeled by `t.title.split(' ')[0]`; member counts read the global array not the ws-scoped prop; **no Archived group → archived tasks unreachable**.
- **Failure surfaces entirely missing — gravest IA gap for a daemon-backed app**: no daemon-down/reconnecting state, no agent-error surface (STATUS_META defines `error`; nothing renders it), no merge-conflict/contention view (a top mined failure mode in the field), no loading skeletons, no zero-workspace/first-run-empty states.
- Attention routing underpowered: binary amber dot vs the field-validated **Notify / Review / Question** grammar + blocked-duration urgency; `blocked` is the state every surveyed tool routes on, and the sidebar doesn't sort by it.

### Interaction completeness
No `:disabled` rules, no in-flight states, no confirms/undo for Stop/Merge/Deny (ApprovalModal's Deny is a dismiss). Review/merge conflation in both diff surfaces; TaskReview merge state is tab-local and evaporates. No pane drag-resize, no multi-select (batch stop/merge/archive are obvious fleet ops). Demand before build: a state-machine sheet per action and a "what survives a context switch" contract per surface.

### Terminal surface (the hidden hard joint)
The styled-div terminal sells the aesthetic but dodges every real constraint: WebGL context caps (attach WebglAddon only to visible terminals), context loss after sleep (verify on focus/visibility), watermark flow control against xterm's 50MB silent-discard buffer (pause ~128KB/resume ~16KB to the daemon), never fit() in display:none. **Parked agents show the daemon's exact-repaint last screen as a cheap snapshot — that's the differentiator, render it as such.** Console mode = Warp-blocks-style projection of turns (already the attribution atom). Lock the status copy: "PTY attached · live" / "re-attached · last screen". Adopt Zellij's press-ENTER-to-run gate on re-attached stdin.

### Copy/voice
Anchor copy is excellent ("The task aggregates — it does not own the diff."). Leaks: 'workstream' in Guardrails/onboarding, "Always approve" meaning its opposite, "an shared Worktree", '{n} worktrees' wrong under shared mode, three divergent profile-description lists, onboarding logo block (violates wordmark-only). Define the **error voice** now (terse, diagnostic, no apology: "daemon unreachable · retrying (3)").

### Must-not-ship prototype artifacts
unpkg React-dev/Babel-standalone/Lucide UMD (already blocked by production CSP `script-src 'self'` — the prototype literally cannot render in the shell), window-global registry, array-index keys, localeCompare on 'HH:MM' strings, unguarded `agent.plan`, duplicate @font-face dropping font-display, TTF→woff2.

---

## 4. Visual system decisions needed
- **#c8c7c2 role conflict**: design uses it as `--fg-1` body text (matches the locked palette memory); current app uses it as CTA fill. Make the call explicitly (recommend: text, per the locked palette).
- Accent shift #4493f8 → #5b8def; flat ramp → 5-step ladder + translucent hairlines; rename the `teal`-means-blue Tailwind alias.
- Brand: rail logo block + onboarding logo violate wordmark-only; current titlebar wordmark is the reference.
- Consider Linear's 3-variable LCH theme generation to make theming trivial later.

---

## 5. Field research: the bets, validated (full citations in /tmp/taime-review/extract-research-problem-space.md)
1. **Task as nullable partition — VALIDATED & differentiated.** The field's two poles both fail: no-grouping tools retrofitted grouping (Conductor Todos→queue, Devin Spaces, Jun 2026); ceremony-first kanban got pushback (Vibe Kanban HN). Nimbalyst's lesson: grouping works when **columns are derived state, not user ceremony**. No competitor has Taime's exact middle position.
2. **Agent-first launch — VALIDATED.** Fixes documented spawn hesitation; "task creation becomes so cheap and seamless" is the win condition (Terragon founder, ~30 tasks/day). Field doctrine: 3-5 concurrent agents — make attention cost visible, don't cap spawning.
3. **Terminal|Console dual mode — VALIDATED by convergent evolution** (Warp's two modes; Codex's thread+terminal; Conductor's year of terminal retrofits). The hard part is cross-mode state-sharing rules — one stream, console as projection.
4. **Aggregate Task Review — DIRECTIONALLY VALIDATED, highest-risk surface.** Review is the universal bottleneck, but >400-line aggregates get rubber-stamped (documented credential-leak case). No competitor ships cross-agent aggregate review — no pattern to copy. Mitigate: per-agent segmentation, risk-ranking, <200-line units, review-assist (Sculptor Suggestions / Codex inline-comments-feed-next-turn).

**Market structure:** both loved standalone orchestrators (Terragon, Bloop) died early 2026 while platforms absorbed the category. Defensible ground = exactly the thesis: **forensic per-agent attribution (no shipping orchestrator surfaces line-level agent blame), exact-repaint PTY ownership, local-first multi-provider neutrality.** Weigh every feature against "does a platform vendor already ship this?"

**Patterns to absorb:** Notify/Review/Question inbox + blocked-duration urgency (LangChain Agent Inbox, Prigent); auto-archive on PR-merge hygiene (Claude desktop) to prevent the Uncategorized junk drawer; single-action merge+teardown (workmux); turns as navigable blocks (Warp); worktree env bootstrap (.env/node_modules propagation) — the #1 reported onboarding failure in worktree tools, unaddressed in the design.

---

## 6. The seven gates (fix in the design before implementation tickets)
1. **Chrome integration spec** — native lights + 40px bar + drag regions + fullscreen + menu bar + full shortcut map.
2. **920×600 truncation/min-width contract** — the design currently fails at min size.
3. **State-survival contract per surface** + daemon-owned Agent ID minting + single workspace FK.
4. **Terminal surface spec** — xterm lifecycle, parked-agent snapshot, console-as-projection, flow control.
5. **System-failure surfaces** — daemon-down, agent error, contention, first-run.
6. **Token consolidation + interaction-state grammar** — then the pixel sweep is mechanical.
7. **Workflow/Schedule authoring flows** — two of five rail sections are currently read-only stubs.

**What must NOT change:** the lexicon, the nullable-Task partition, agent-first launch, dual-mode, per-agent merge under an aggregate review lens, #c8c7c2 as brand primary, the surface ladder, the instrument-panel voice.
