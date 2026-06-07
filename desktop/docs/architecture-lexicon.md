# Taime architecture & canonical lexicon

This is the **single canonical model** of Taime's architecture. Future docs, UI
copy, code comments, and architecture reviews should reconcile against this
file — not against each other. It was produced by reconciling four independent
architecture reviews against the post-migration code (the all-Rust daemon
architecture; CAO and tmux deleted), and it deliberately retires legacy
terminology where the legacy word no longer names a real object.

> The goal is not to preserve historical terminology. The goal is the clearest
> possible model for users, developers, and future documentation.

---

## The mental model (one paragraph)

You open a **Workspace** and create or select a **Task** (or stay
Uncategorized). You launch **Agents** into it — each a **Profile** running on
a **Provider**. At launch an agent is minted its **Agent ID** and its own
**Worktree**; everything it does — **Turns**, files touched, **Diffs** — is
recorded against that ID and persists even after its process exits. The
**Runtime** is replaceable: agents survive app restarts as detached runtimes
you can reattach. An **Orchestrator** is an Agent whose Profile grants
delegation; its delegations form a **Team**. The **Task** groups the work,
tracks lifecycle and review state, and shows rollups — it is the unit of safe
context switching. **Workflows** are reusable graphs (branches, loops) that
**Run** inside a workspace; **Schedules** fire agents on cron. The **Daemon**
keeps all of it alive independently of the app, and nothing merges without
**Review**.

---

## The canonical hierarchy

```
DAEMON — the persistent substrate (agents and schedules outlive the app;
│         the daemon is why)
│
├── LIBRARY (global, reusable definitions — workspace-independent)
│     ├── Providers   claude_code · codex · gemini_cli · grok_cli
│     ├── Profiles    default · orchestrator · bug-fixer · …  (orchestrator =
│     │               a Profile granting delegation — a capability, not a thing)
│     ├── Workflows   definitions: Nodes · Edges (when) · entry · max_iterations
│     └── Schedules   cron → fire an Agent, unattended
│
└── WORKSPACE (the user-selected project root)
      ├── Tasks — stored user intent (a partition over agents, NOT a container)
      │     ├── Task ID · Title / Description
      │     ├── Status   open · in_review · done · archived
      │     ├── member Agents + Workflow Runs   (via nullable task_id)
      │     ├── Task Review State   (aggregate + task-level decisions)
      │     └── Attribution Rollups (derived by join, never stored raw)
      │
      ├── Agent — the primary unit;  launch = Profile × Provider; task_id?
      │     ├── AGENT ID  ← the stable identity, minted at provision; the pivot
      │     │     ├── Worktree   (isolated git checkout | shared fallback)
      │     │     ├── Turns      (bounded spans of work + files touched)
      │     │     ├── Activity & Diffs  (provenance — persists after exit)
      │     │     └── Edges      (assign · handoff · message · request/reply)
      │     └── RUNTIME  ← ephemeral process/PTY; Status (IDLE…ERROR);
      │                    live (viewed or detached) | exited; replaceable
      │
      ├── Workflow RUN — a Library workflow executed here; each Node spawns
      │                  an Agent in this workspace; per-node run states; task_id?
      │
      └── DERIVED VIEWS (computed, never containers; workspace-wide AND
            task-filtered)
            Team graph · Attribution rollups · Contention ·
            Review (Merge · Revert · Mark reviewed) · Task Review
```

Four structural rules fall out of this:

1. **The Library/Workspace split.** Profiles, Workflow *definitions*, and
   Schedules are global, reusable, daemon-owned objects
   (`~/.taime/{agents,workflows,schedules}` + the daemon's SQLite store). Only
   their *executions* — agent launches and Workflow Runs — are workspace-
   scoped. A Schedule fires with zero workspaces open. Nesting definitions
   under Workspace would predict per-project schedules that don't exist.
2. **The identity pivot.** The Agent ID is minted at worktree provision,
   *before* the process exists. The Worktree and the Runtime both attach to
   it. Creation order is workspace → Agent ID → worktree → process; ownership
   is "an Agent has a Worktree." Both are true; the ID is the pivot.
3. **Record vs view.** The attribution *record* (turns, files, diffs, edges,
   history) hangs off each Agent ID. Cross-agent surfaces (Team graph,
   contention, rollups, review) are workspace-level **derived views** —
   computed at query time, never stored as containers.
4. **Membership, not ownership (Tasks).** Task membership is a nullable
   `task_id` on the agent's durable record (the worktree row). An Agent
   belongs to at most one Task; Tasks partition workspace agents into
   task-scoped groups plus **Uncategorized**. All four spawn paths propagate
   `task_id`: manual launch (selected), `assign` (child inherits parent's),
   workflow node spawn (inherits the Run's), schedule fire (per the schedule's
   explicit task behavior). Archiving a Task preserves membership as a
   read-only view; deleting a Task demotes members to Uncategorized. Neither
   ever kills runtimes, deletes worktrees, or erases attribution. Task does
   not own raw attribution — Agent ID remains the sole anchor.

---

## The canonical lexicon

### Top-level concepts (user-facing)

| Term | Definition |
|---|---|
| **Workspace** | The user-selected project root that scopes work. The top-level user object. |
| **Task** | A named, workspace-scoped unit of user intent (bug, feature, issue, cleanup, review, investigation). Groups Agents and Workflow Runs via membership; owns lifecycle (`open · in_review · done · archived`), review state, and derived rollups. Never owns raw attribution. The unit of safe context switching. |
| **Task ID** | Stable identity of a Task. |
| **Task Review** | The aggregate review surface for a Task: combined diff, per-agent diffs, dirty agents, reviewed/unreviewed state, rollups, and task-level decisions. Grounded per Agent/Worktree; merge executes per-worktree underneath. |
| **Uncategorized** | Agents (or runs) with `task_id = null`. Always a legal state — the zero-click launch default; Task is never a toll booth. |
| **Agent** | The primary unit: one AI worker — a Profile running on a Provider, working against its own Worktree, identified by its Agent ID. Optionally a member of one Task. |
| **Agent ID** | The stable, durable identity of an agent. Minted at provision; keys the worktree, turns, fs events, diffs, messages, MCP identity, and graph nodes. Persists after the process exits. *(Implementation alias: `attribution_key` / `terminal_key` / frontend `terminalId` — see migration table.)* |
| **Provider** | The CLI engine backing an agent: `claude_code`, `codex`, `gemini_cli`, `grok_cli`. Resolved by the daemon's provider registry. |
| **Profile** | A named behavior definition applied at launch: system prompt, model, tools, orchestration capability. Built-ins plus `~/.taime/agents/*.toml`. *(The single canonical noun — "role" is allowed only as informal UI copy, never as a distinct concept.)* |
| **Orchestrator** | An Agent whose Profile grants delegation (the injected MCP tools: assign, handoff, message, request/reply, share/get). A capability, not a separate service. |
| **Worker** | An Agent assigned by an Orchestrator. A relationship label, not a distinct type. |
| **Worktree** | The agent's working copy: an isolated git worktree forked from the workspace repo (lives in the app data dir, not inside the workspace), 1:1 with the Agent ID. Mode: `isolated` \| `shared` (shared = deliberate fallback when isolation isn't possible). |
| **Runtime** | The daemon-owned process/PTY behind a live agent. Ephemeral and replaceable: survives app restarts (detached), can be reattached, dies independently of the agent's record. State: live (viewed or detached) \| exited. |
| **Status** | The inferred runtime state driving every badge: `IDLE` · `PROCESSING` · `WAITING_USER_ANSWER` · `COMPLETED` · `ERROR`. |
| **Turn** | A bounded span of agent activity (start/end causes, output offsets, grid snapshots, files touched). The atomic unit of attribution. |
| **Attribution** | The recorded provenance of who did what: turns, file activity, diffs — persisted by the daemon even with the UI closed. The flagship substrate. |
| **Contention** | Two or more agents touching the same path. A derived view over attribution. |
| **Team** | Agents connected by orchestration edges (assign · handoff · message · request/reply). Visualized by the Team graph (the activity drawer). |
| **Workflow** | A reusable, declarative graph of agent steps: Nodes (profile + prompt), Edges with `when` conditions (`always` \| `keyword:WORD` \| `/regex/`), an entry node, and `max_iterations` for loops. Authored by users or generated by an Orchestrator; executed by the engine. A **definition** — global, not workspace-bound. |
| **Run** | One execution of a Workflow inside a workspace: a `run_id`, per-node states, and the Agents the engine spawns for each node. The workspace-scoped counterpart of the Workflow definition. |
| **Schedule** | A cron-triggered, unattended launch: cron expression + Profile + prompt (markdown with YAML front-matter in `~/.taime/schedules`). Fires in the daemon even when the app is closed. |
| **Daemon** | The persistent runtime substrate that owns everything above: agent runtimes, attribution recording, the workflow engine, and schedule firing. It outlives the app — which is why agents survive app restarts and schedules fire unattended. **Upgrade caveat (explicit policy):** survival holds across same-version restarts/crashes only; a protocol-bumping app upgrade *replaces* the daemon and terminates its running agents (the strict-equality handshake + positional wire format make read-only adoption impossible). |
| **Review** | The gate on agent output: per-file/hunk diffs attributed to an Agent ID, with Merge · Revert · Mark reviewed. Nothing merges without it. |

### Internal-only terms (never in user-facing docs/UI)

| Term | What it actually is |
|---|---|
| `Session` (daemon struct) | One agent's runtime wrapper (PTY + emulator + attribution tap), 1:1 with an agent. In docs, say "agent runtime" or "agent process." |
| PTY session id (`pty-N`) | The live process handle for attach/write/kill/reattach. Runtime identity only — never the product identity. |
| `terminalId` / `terminal_key` / `terminal_id` | Legacy aliases of the **Agent ID** (tmux/CAO heritage). Wire/DB names stay for compatibility; concepts don't. |
| Frame | A UI view in the shell grid attached to a runtime (0..1 attached view at a time; closing a frame detaches, it never kills). |
| Inbox / message bus | The delivery mechanism behind message/handoff/assign (idle-gated stdin delivery). Users see the verbs, not the bus. |
| Blackboard | The shared key/value store behind `share`/`get` and workflow node fan-in. |
| `flows` table | The SQLite table backing **Schedules** (CAO heritage name). Rename opportunistically. |
| wezterm-term grid | The daemon's authoritative terminal state; surfaces only as "exact reattach repaint." |

---

## Terminology migration table

| Legacy / overloaded | Canonical | Notes |
|---|---|---|
| Session (workspace grouping) | **delete** — it was 1:1 with the Workspace root; say "the workspace's agents" | The grouping was a derived compatibility view, never a stored object |
| Session (daemon runtime) | **Runtime** (docs); code may keep the `Session` struct internally | Internal-only |
| "Add to session" | "Add to Task" (launch task picker) | Follows the deletion; Task is the grouping that Session pretended to be |
| `session_name` / `member_of` grouping | `task_id` membership | Proto-Task technical debt; migrate then retire |
| Terminal (as identity) | **Agent ID** | "Terminal" survives only as the colloquial on-screen terminal view |
| `terminalId` / Terminal ID / Attribution Key (docs) | **Agent ID** | `attribution_key` remains the implementation-layer name during migration |
| Role (UI) | **Profile** | One UI string change; code/store/API already say profile |
| Flow | **Schedule** | Retired everywhere except migration notes; `flows` table = heritage |
| Routine | **Workflow** | Pre-finalization name; never shipped |
| tmux session / window | — | Deleted with CAO |
| Worktree `mode: "worktree"` | mode **isolated** | Fixes the mode named after its own object (`shared` unchanged) |

---

## Appendix: reconciliation notes

Four independent architecture reviews were reconciled against the code. Final
adjudication: **zero genuine architectural disagreements** — every apparent
conflict was terminology drift. The disputes and their resolutions:

- **"Session contains Agents" vs "Agent launch creates a Session"** — both
  were true of *different referents* (the derived workspace grouping vs the
  daemon runtime struct). Neither survives as a user concept.
- **Canonical identity (Session ID vs PTY ID vs Terminal ID vs Attribution
  Key)** — unanimous on the concept: the attribution key is the durable
  identity; the PTY id is the runtime handle. Adjudicated name: **Agent ID**.
- **Worktree ordering (Workspace→Agent→Worktree vs Workspace→Worktree→Agent)**
  — dissolved by the identity-pivot model: the Agent ID is minted first, and
  the Worktree and Runtime both attach to it.
- **Where Workflows/Schedules live** — the one dispute settled by code rather
  than naming: definitions are global (Library); only Runs/launches are
  workspace-scoped.
- **Attribution placement (sibling of Agents vs under the agent)** — both,
  once *record* (per Agent ID) and *view* (workspace-level, derived) are
  separated.
- **New coinages rejected** ("Working Context", "Persona", "Space", "History",
  "Agent Console") — minting new nouns is how drift starts. The underlying
  point of "Working Context" (isolation isn't universal) is kept as the
  Worktree's `isolated | shared` mode.

Factual anchors (verified in code at the time of writing):

- Worktrees live under the app data dir (`dirs::data_dir()/taime/worktrees/`),
  forked *from* the workspace repo — not inside the workspace.
- The branch name (e.g. `taime/claude_code-8ccec507`) *embeds* the Agent ID;
  it is not the ID itself.
- The daemon enforces one attached client per runtime; a frame is 0..1 per
  agent, and closing it detaches (`daemon_close_view`), never kills.
- Activity-graph queries key on `COALESCE(attribution_key, pty_session_id)` —
  the Agent ID is primary, the PTY id is the fallback for pre-provision spans.
- `daemon_list` is connect-only (never spawns a daemon), which is what makes
  detached-agent discovery and crash adoption safe on every boot.
