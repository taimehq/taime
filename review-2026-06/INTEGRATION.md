# P0 lanes — integration review & result (2026-06-10)

Three P0 fix lanes were built in parallel (each in its own worktree) and then
integrated and verified. This file records the outcome so whoever lands them
knows exactly what was resolved and why.

## Lanes

| Branch | Items | Verdict |
| --- | --- | --- |
| `fix/p0-review-gate` | P0 1–3: symbolic merge/revert target resolution, daemon-enforced merge gate (ack + digest binding), untracked-file merge/revert | **Complete & correct** |
| `fix/p0-frontend-trust` | P0 7–10, 15 (+ ~10 hardening commits): strict review reads, frame-close keeps daemon dirty, detach≠exit, Console→inbox, Task Activity by workspace_root | **Complete & correct** |
| `chore/hygiene` | CI, LICENSE (AGPL-3.0-only), daemon.log, P0 16 (corrupt-store recovery + StoreHealth pill) | **Complete & correct** |

P0 done after this lands: **1, 2, 3, 6, 7, 8, 9, 10, 15, 16** (item 6 = the
earlier guard fix on main). Remaining P0: 4, 5, 11 (attribution/watcher), 12,
13 (identity), 14 (schedule re-enable), 17 (protocol bump), 18 (delete-workspace
UX) — all daemon-side, unblocked now that the gate works.

## Integration: `integ/p0-all` (off main, merge order gate → hygiene → trust)

The textual merge was **not** trivial. Merging each lane against bare main hid
the interactions that only appear once `gate` is in the base. Resolved:

**4 textual conflicts** — `api.ts`, `lib.ts`, `DiffView.tsx` (gate's diff
fallbacks-with-`digest` vs trust's strict no-fallback reads → took trust's
strict version; gate's digest/`ApplyResult`/`expectedDigest` contract rides
through the types intact), and `store.test.ts` (unioned both lanes' mocks).
*Caution for the real merge: resolve these hunk-by-hunk. A whole-file
`--theirs` silently drops gate's digest contract and main's `clearReviewed` —
caught only by typecheck.*

**2 semantic interactions** a clean textual merge would have shipped broken:
- gate changed `api.markReviewed` to honest-`false` (a non-persisted ack must
  not claim success); trust's new test still asserted the old `true`. →
  reconciled the test to gate's contract.
- trust added a `#[cfg(test)] mod tests` in `daemon.rs` *before*
  `resolve_daemon_bin`; hygiene's new CI runs `clippy -D warnings`. Integrated
  = red CI on first push. → moved the test module to end of file.

## Completeness review (11-agent adversarial pass, 7 confirmed / 0 refuted)

All assigned items verified complete and correct end-to-end; **the thesis holds
— no path merges unreviewed work** (symbolic targets fail closed; the gate
refuses on missing store/ack/digest or stale digest; "Proceed without review"
is a session-local skip, never a durable ack). Findings:

- **[medium, cross-lane — FIXED in `integ/p0-all`]** "Mark reviewed" fired
  `clearDirty` then `markReviewed` as two unordered RPCs; `clear_dirty`'s daemon
  arm DELETEs the durable ack while `mark_reviewed` INSERTs it, so a lost race
  wiped the freshly-persisted ack (guard re-prompts on reviewed work after
  restart). Not a merge-without-review. Fixed by a single ordered
  `markFrameReviewed` action (awaits the dirty-clear before the ack INSERT) +
  a test pinning the order.
- **[low, open]** `>50` dirty files: daemon clears the ack on growth but the
  app's path-set-equality early-return doesn't re-arm the local guard
  (self-heals on restart; needs >50 dirty files).
- **[low, open]** A *tracked* file whose name matches a transient pattern shows
  as mergeable in `hunked_diff` but is hidden from `file_diffs` (surfaces
  disagree; temps are ~never committed).
- **[low, open ×3, hygiene]** WAL/SHM + DatabaseCorrupt recovery paths untested;
  runtime (post-boot) store corruption isn't re-surfaced (known, accepted);
  daemon.log rotation is boot-only.

## Verification (full CI-equivalent on `integ/p0-all`)

`pnpm typecheck` clean · **154 vitest** · `cargo test --workspace --locked`
(162 daemon + 9 protocol + 6 app + 3 integration) · `cargo clippy --workspace
--all-targets --locked -- -D warnings` clean.

## To land

`integ/p0-all` is the verified result (29 commits ahead of main). Either
fast-forward main to it, or re-merge the three lanes into main in the order
above and re-apply the 6 resolutions (this file is the guide) + the
`markFrameReviewed` fix. The four low findings are tracked in
`review-2026-06/findings.md` and the backlog; none blocks landing.
