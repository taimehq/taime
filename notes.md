# Taime — codebase review

_Review date: 2026-05-31_

## Overall

This is genuinely strong work — well above typical prototype quality. The
architecture has clean ownership boundaries (Rust = process/config/watcher,
Python = orchestration source of truth, React = view state), the docstrings
explain *why* not just *what*, and the hard problems (PTY streaming reuse,
non-invasive git snapshots, optimistic-launch rollback, graceful sidecar
shutdown) are solved thoughtfully. The "real binaries only, never raw APIs"
constraint is respected consistently. The README's own "Open questions & risks"
section is honest and accurate.

The signature feature — per-agent worktree isolation + forensic activity graph —
is the right moat, and the `GIT_INDEX_FILE` dangling-commit snapshot trick
(`worktree_service.py:268`) is a legitimately elegant way to capture state
without touching the agent's branch or index.

Findings are ordered by what I'd fix first.

---

## Correctness & operational risks

### 1. Worktrees and branches leak forever (highest-impact)
`remove_worktree` is called in exactly one place — the terminal-*creation-failure*
cleanup path (`terminal_service.py:348`). By design, normal `delete_terminal`
deliberately *keeps* the worktree for review-after-stop. But nothing ever reaps
them afterward. Every isolated agent ever launched leaves a worktree under
`CAO_HOME_DIR/taime-worktrees/<slug>/` and a `taime/<provider>-<tid>` branch
behind permanently. Over weeks this is hundreds of worktrees + branches, each
symlinking `node_modules` into `git worktree list`. No GC endpoint, no TTL, no
"discard reviewed agent" action that calls `remove_worktree`. **Must-fix before
real use** — at minimum a `DELETE /worktrees/{tid}` + a startup sweep of
worktrees whose terminals no longer exist.

### 2. SQLite concurrency under the polling fan-out
`create_engine(DATABASE_URL, connect_args={"check_same_thread": False})`
(`database.py:173`) with default pooling, plus: the per-terminal 3s status
poller, the activity plugin writing on every lifecycle event,
`/activity/fs-events` batches, and turn checkpoints — all writing to one SQLite
file. The risk isn't multiple servers (the README covers that), it's
*concurrent writers inside one server*. Under a few active agents you'll likely
hit intermittent `database is locked`. Enable WAL mode
(`PRAGMA journal_mode=WAL`) and a `busy_timeout` — cheap insurance for exactly
this bursty load.

### 3. `BackendState.pid` never clears, and `publish()` has dead intent
In `backend.rs`, when the child dies the status flips to `down`/`restarting` but
`pid` lingers (the `if status == "down"` block at line 166 is a comment-only
no-op). The closure is named `publish` but doesn't publish — it only
mutates+returns; every caller re-checks `last_emitted` and emits separately. Not
a bug, but the naming misleads and the stale pid will confuse diagnostics.
Minor, but it's in the supervisor, which you want crystal-clear.

### 4. `git apply --3way` onto `main` is silent about partial application
`apply_selection` (`diff_service.py:439`) applies selected hunks to the main
checkout's *working tree* (unstaged). Two edge cases: (a) re-applying the same
hunk twice fails and surfaces as a generic "conflict," confusing users who
clicked Merge twice; (b) a `--3way` partial success can leave conflict markers
in `main` while still returning `applied: false` — the user sees failure but the
file is now dirtied with `<<<<<<<` markers. Consider checking
`git status`/stash-guarding the target before apply, and reporting "already
applied" distinctly from "conflict."

### 5. `accept_path` calls `path.is_dir()` on delete events
In `fs_watch.rs:346`, gitignore matching passes `path.is_dir()`, but on a
`Remove` event the path is already gone, so `is_dir()` returns false and
directory-scoped gitignore rules won't match on deletions. Edge case, low
severity — deletes inside an ignored dir could occasionally slip into the dirty
set.

---

## Quality & maintainability

### 6. Zero committed tests for the new backend services
The Rust side has 9 unit tests (config + fs_watch), but `worktree_service`,
`diff_service` (especially `_parse_unified` / `_build_patch` /
`apply_selection`), and `activity_service` have *no* committed Python tests —
verified via live E2E only. The hunk parser and patch reassembler are exactly
the kind of fiddly string logic that regresses silently. Table-driven tests over
`_parse_unified` (renames, `/dev/null` sides, multi-hunk, no-trailing-newline)
and a real-git fixture for `apply_selection` would pay for themselves the first
time you touch them. **Biggest gap relative to the code's overall maturity.**

### 7. `get_status` glyph-scraping is the structural fragility
Already flagged in the README, and `grok_cli.py` handles it about as well as
possible (chrome exclusion, PROCESSING-before-COMPLETED ordering, pinned to
0.2.14). But four providers each pinned to TUI strings of CLIs that ship weekly
is a standing maintenance tax. Two `TODO(grok)` markers (Plan Mode banner,
approval strings) are still unconfirmed-live. Worth a single "marker version"
registry so an upgrade break is one obvious place to re-pin.

### 8. `_parse_unified` path-prefix idiom
`p[:2] in ("a/",)` (`diff_service.py:309`) is a correct-but-cryptic
single-element-tuple membership test where `p.startswith("a/")` reads plainly.
Harmless, but it'll make the next reader pause.

### 9. `jsonEqual` via `JSON.stringify` for re-render guards
(`store.ts:17`) Fine at current data sizes, but it's key-order-sensitive and
O(n) on every session/status poll. If session payloads grow it'll quietly become
the hot path. Worth a comment noting the assumption.

### 10. Unauthenticated arbitrary-path probe
`GET /workspace/info?path=` runs `git` in any path the caller names. Loopback-only
so acceptable per the threat model, but it's the one route that acts on an
*arbitrary filesystem path from the request* rather than a known terminal id —
flag it explicitly in the auth note as the thing that must gain a guard before
any non-loopback exposure.

---

## Suggested order of work

1. Worktree/branch GC (endpoint + startup sweep) — the only thing that degrades
   with normal use.
2. WAL + `busy_timeout` on the SQLite engine.
3. Commit unit tests for `_parse_unified` / `apply_selection` and
   `ensure_worktree` fallback paths.
4. Distinguish "already applied" from "conflict" in `apply_selection`, and guard
   the `main` target against being left with conflict markers.

Nothing here is a teardown — the foundation is solid and the design decisions
are well-reasoned. These are the gaps between "verified working prototype" and
"leave it running for a month."
