//! Daemon-side diff (CAO-replacement Phase 6 — the `diff_service` port).
//!
//! `git diff` of an agent's worktree vs its base (`base_sha` for an isolated
//! worktree — the fork point — so the diff shows all the agent's changes),
//! structured for review: a combined unified diff + per-file both-sides
//! reconstruction (Monaco) + per-hunk blocks for selective merge/revert. Results
//! are returned as JSON matching the frontend's existing shapes, so the route
//! flip is fetch-layer-only (no component rework).

use std::path::Path;
use std::process::{Command, Output};

use serde_json::{json, Value};

/// Files larger than this are listed but not embedded (matches CAO).
const MAX_CONTENT_BYTES: usize = 512 * 1024;
/// The empty-tree object — the base for a repo with no commits.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

fn git_raw(cwd: &Path, args: &[&str]) -> std::io::Result<Output> {
    Command::new("git").current_dir(cwd).args(args).output()
}

/// Trimmed stdout on success, else `None`.
fn git_ok(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = git_raw(cwd, args).ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        None
    }
}

fn is_git(cwd: &Path) -> bool {
    git_ok(cwd, &["rev-parse", "--show-toplevel"]).is_some()
}

/// Resolve the diff base: the worktree's `base_sha` if present, else `HEAD`, else
/// the empty-tree (no commits).
fn resolve_base(cwd: &Path, base: Option<&str>) -> String {
    if let Some(b) = base.filter(|b| !b.is_empty()) {
        return b.to_string();
    }
    if git_ok(cwd, &["rev-parse", "--verify", "HEAD"]).is_some() {
        "HEAD".to_string()
    } else {
        EMPTY_TREE.to_string()
    }
}

fn read_worktree_file(cwd: &Path, path: &str) -> (String, bool) {
    match std::fs::read(cwd.join(path)) {
        Ok(bytes) => {
            if bytes.len() > MAX_CONTENT_BYTES {
                (String::new(), false)
            } else if let Ok(s) = String::from_utf8(bytes) {
                (s, false)
            } else {
                (String::new(), true) // binary
            }
        }
        Err(_) => (String::new(), false),
    }
}

fn show_file(cwd: &Path, base: &str, path: &str) -> String {
    git_ok(cwd, &["show", &format!("{base}:{path}")]).unwrap_or_default()
}

/// Probe a candidate project dir for the workspace picker (CAO `workspace_info`).
pub fn workspace_info(path: &str) -> Value {
    let p = Path::new(path);
    if !p.is_dir() {
        return json!({ "path": path, "exists": false, "is_git": false, "repo_root": null, "branch": null, "head_short": null });
    }
    let repo_root = git_ok(p, &["rev-parse", "--show-toplevel"]).map(|s| s.trim().to_string());
    let is_git = repo_root.is_some();
    let branch = git_ok(p, &["rev-parse", "--abbrev-ref", "HEAD"]).map(|s| s.trim().to_string());
    let head_short = git_ok(p, &["rev-parse", "--short", "HEAD"]).map(|s| s.trim().to_string());
    json!({ "path": path, "exists": true, "is_git": is_git, "repo_root": repo_root, "branch": branch, "head_short": head_short })
}

/// The combined working-tree diff + changed-file list (CAO `get_terminal_diff`).
/// Skip clearly-transient files (atomic-write temps, swap/backup, OS cruft) from
/// the review/diff/contention surfaces — the same rule the fs watcher uses, so
/// write-churn noise (e.g. `foo.md.tmp.<pid>.<hash>`) never shows up as a
/// reviewable change.
fn is_transient(rel_path: &str) -> bool {
    let name = rel_path.rsplit('/').next().unwrap_or(rel_path);
    crate::fswatch::is_transient_file(name)
}

/// FNV-1a over the assembled review patch — the content fingerprint that binds
/// "the diff the user reviewed" to "the diff being applied" (review L2/L3: the
/// gate must refuse when the worktree moved under a stale view). Not a security
/// boundary (same-uid clients are trusted by design); it only detects drift.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

pub fn patch_digest(raw: &str) -> String {
    format!("{:016x}", fnv1a64(raw.as_bytes()))
}

/// The repo toplevel of `dir` (the dir itself when not a repo) — `git apply`
/// and `git diff` paths are toplevel-relative, so every path we emit or probe
/// must anchor there, not at the cwd (which in shared mode can be a subdir).
fn repo_toplevel(dir: &Path) -> std::path::PathBuf {
    git_ok(dir, &["rev-parse", "--show-toplevel"])
        .map(|s| std::path::PathBuf::from(s.trim()))
        .unwrap_or_else(|| dir.to_path_buf())
}

/// Untracked (non-transient) files as `(toplevel_relative, cwd_relative)`
/// pairs. `ls-files` emits cwd-relative names, but every other diff surface
/// (tracked `git diff`, `git apply`) speaks toplevel-relative — emitting the
/// bare names from a subdir cwd makes `git apply` silently skip them (exit 0,
/// "Skipped patch", review L4).
fn untracked_files(path: &Path) -> Vec<(String, String)> {
    let prefix = git_ok(path, &["rev-parse", "--show-prefix"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    git_ok(path, &["ls-files", "--others", "--exclude-standard"])
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty() && !is_transient(l))
        .map(|f| (format!("{prefix}{f}"), f.to_string()))
        .collect()
}

/// Untracked files as synthetic new-file patches — the
/// `git diff --no-index /dev/null` form, `(toplevel_relative, patch)` per
/// file; `patch` is `None` for non-UTF8 content (embedding it through the
/// String pipeline would corrupt the bytes with U+FFFD on merge — review L5 —
/// so such files stay listed-but-unhunked, exactly the pre-untracked-support
/// behavior). Binary files get git's "Binary files differ" stub (no hunks),
/// which parses to an unselectable entry. Agents almost never commit, so
/// their new files exist only as untracked paths, which `git diff <base>`
/// omits; every patch-shaped surface appends these so agent-created files
/// are visible, hunked, mergeable, AND revertable — not just listed.
fn untracked_patches(path: &Path) -> Vec<(String, Option<String>)> {
    let top = repo_toplevel(path);
    untracked_files(path)
        .into_iter()
        .map(|(rel, _)| {
            // Run from the toplevel with the toplevel-relative path so the
            // patch headers carry it. `--no-index` exits 1 when the sides
            // differ — read stdout regardless.
            let patch = git_raw(&top, &["diff", "--no-index", "--", "/dev/null", &rel])
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok());
            (rel, patch)
        })
        .collect()
}

/// The full review patch: tracked changes vs `base` + synthetic new-file
/// patches for untracked files. This single assembly feeds hunked_diff (what
/// the UI selects from) AND apply_selection (what the merge reassembles), so
/// the two surfaces can never drift apart.
fn review_patch(path: &Path, base: &str) -> String {
    let mut raw = git_ok(path, &["diff", base]).unwrap_or_default();
    for (_, patch) in untracked_patches(path) {
        if let Some(p) = patch {
            raw.push_str(&p);
        }
    }
    raw
}

pub fn terminal_diff(agent_id: &str, cwd: &str, base: Option<&str>) -> Value {
    let path = Path::new(cwd);
    if !is_git(path) {
        return json!({
            "agent_id": agent_id, "working_directory": cwd, "is_git": false,
            "diff": "", "files_changed": 0, "files": [], "error": null
        });
    }
    let base = resolve_base(path, base);
    let mut diff = git_ok(path, &["diff", &base]).unwrap_or_default();
    let mut files: Vec<String> = git_ok(path, &["diff", &base, "--name-only"])
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty() && !is_transient(l))
        .map(String::from)
        .collect();
    for (f, patch) in untracked_patches(path) {
        if let Some(p) = patch {
            diff.push_str(&p);
        }
        files.push(f);
    }
    json!({
        "agent_id": agent_id, "working_directory": cwd, "is_git": true,
        "diff": diff, "files_changed": files.len(), "files": files, "error": null
    })
}

/// Per-file both-sides reconstruction for side-by-side review (CAO
/// `get_file_diffs`). Returns `{agent_id, files: [FileDiff]}`.
pub fn file_diffs(agent_id: &str, cwd: &str, base: Option<&str>) -> Value {
    let path = Path::new(cwd);
    if !is_git(path) {
        return json!({ "agent_id": agent_id, "files": [] });
    }
    let base = resolve_base(path, base);

    // additions/deletions per path (numstat); "-\t-" = binary.
    use std::collections::HashMap;
    let mut nums: HashMap<String, (i64, i64, bool)> = HashMap::new();
    for line in git_ok(path, &["diff", &base, "--numstat", "-M"]).unwrap_or_default().lines() {
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() == 3 {
            let binary = parts[0] == "-";
            let add = parts[0].parse::<i64>().unwrap_or(0);
            let del = parts[1].parse::<i64>().unwrap_or(0);
            nums.insert(parts[2].to_string(), (add, del, binary));
        }
    }

    let mut files: Vec<Value> = Vec::new();
    for line in git_ok(path, &["diff", &base, "--name-status", "-M"]).unwrap_or_default().lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.is_empty() {
            continue;
        }
        let code = parts[0];
        let (status, old_path, new_path) = if code.starts_with('R') && parts.len() >= 3 {
            ("renamed", Some(parts[1].to_string()), parts[2].to_string())
        } else if parts.len() >= 2 {
            let s = match code.chars().next() {
                Some('A') => "added",
                Some('D') => "deleted",
                _ => "modified",
            };
            (s, None, parts[1].to_string())
        } else {
            continue;
        };
        if is_transient(&new_path) {
            continue;
        }
        let (add, del, binary) = nums.get(&new_path).copied().unwrap_or((0, 0, false));
        let original = if status == "added" {
            String::new()
        } else {
            let from = old_path.as_deref().unwrap_or(&new_path);
            show_file(path, &base, from)
        };
        let (modified, bin2) = if status == "deleted" {
            (String::new(), false)
        } else {
            read_worktree_file(path, &new_path)
        };
        files.push(json!({
            "path": new_path, "status": status, "old_path": old_path,
            "original": original, "modified": modified,
            "additions": add, "deletions": del, "binary": binary || bin2
        }));
    }
    // Untracked → added (skipping transient temps). Keyed by the
    // toplevel-relative path so the entry matches its hunked_diff hunks (the
    // UI joins the two surfaces on `path`); content is read via the
    // cwd-relative name.
    for (rel, cwd_rel) in untracked_files(path) {
        let (modified, binary) = read_worktree_file(path, &cwd_rel);
        let adds = modified.lines().count() as i64;
        files.push(json!({
            "path": rel, "status": "added", "old_path": null,
            "original": "", "modified": modified, "additions": adds, "deletions": 0, "binary": binary
        }));
    }
    json!({ "agent_id": agent_id, "files": files })
}

/// Per-file, per-hunk diff for selective merge/revert (CAO `get_hunked_diff`).
/// `digest` fingerprints the served patch; the client echoes it back to
/// apply_selection as `expected_digest`, which refuses when the worktree has
/// moved since this view was fetched (stale hunk indices would otherwise
/// select content the reviewer never saw).
pub fn hunked_diff(agent_id: &str, cwd: &str, base: Option<&str>) -> Value {
    let path = Path::new(cwd);
    if !is_git(path) {
        return json!({ "agent_id": agent_id, "base": null, "digest": null, "files": [] });
    }
    let base = resolve_base(path, base);
    let raw = review_patch(path, &base);
    json!({
        "agent_id": agent_id, "base": base,
        "digest": patch_digest(&raw),
        "files": parse_unified(&raw)
    })
}

/// Parse unified-diff text into per-file blocks with indexed hunks (CAO
/// `_parse_unified`).
fn parse_unified(raw: &str) -> Vec<Value> {
    let mut files: Vec<Value> = Vec::new();
    let mut cur_path: Option<String> = None;
    let mut old_path: Option<String> = None;
    let mut header = String::new();
    let mut hunks: Vec<Value> = Vec::new();
    let mut hunk_header = String::new();
    let mut hunk_body = String::new();
    let mut hunk_idx = 0;
    let mut adds = 0i64;
    let mut dels = 0i64;
    let mut in_hunk = false;

    fn flush_hunk(
        hunks: &mut Vec<Value>,
        idx: &mut i64,
        hunk_header: &mut String,
        hunk_body: &mut String,
        adds: &mut i64,
        dels: &mut i64,
    ) {
        if !hunk_header.is_empty() {
            hunks.push(json!({
                "index": *idx, "header": hunk_header.clone(),
                "text": format!("{}\n{}", hunk_header, hunk_body.trim_end_matches('\n')),
                "additions": *adds, "deletions": *dels
            }));
            *idx += 1;
        }
        hunk_header.clear();
        hunk_body.clear();
        *adds = 0;
        *dels = 0;
    }

    fn flush_file(
        files: &mut Vec<Value>,
        cur_path: &mut Option<String>,
        old_path: &mut Option<String>,
        header: &mut String,
        hunks: &mut Vec<Value>,
    ) {
        if let Some(p) = cur_path.take() {
            files.push(json!({
                "path": p, "old_path": old_path.take(),
                "header": header.trim_end_matches('\n').to_string(),
                "hunks": std::mem::take(hunks)
            }));
        }
        header.clear();
    }

    for line in raw.lines() {
        if line.starts_with("diff --git ") {
            flush_hunk(&mut hunks, &mut hunk_idx, &mut hunk_header, &mut hunk_body, &mut adds, &mut dels);
            flush_file(&mut files, &mut cur_path, &mut old_path, &mut header, &mut hunks);
            in_hunk = false;
            hunk_idx = 0;
            // path from "diff --git a/X b/Y"
            if let Some(b) = line.split(" b/").nth(1) {
                cur_path = Some(b.to_string());
            }
            header.push_str(line);
            header.push('\n');
        } else if line.starts_with("@@ ") {
            flush_hunk(&mut hunks, &mut hunk_idx, &mut hunk_header, &mut hunk_body, &mut adds, &mut dels);
            hunk_header = line.to_string();
            in_hunk = true;
        } else if in_hunk {
            if line.starts_with('+') && !line.starts_with("+++") {
                adds += 1;
            } else if line.starts_with('-') && !line.starts_with("---") {
                dels += 1;
            }
            hunk_body.push_str(line);
            hunk_body.push('\n');
        } else {
            // git appends a TAB after paths containing spaces on the
            // `--- a/X\t` / `+++ b/X\t` lines — strip it, or the hunk key
            // would never match the file list / selection keys (review L11).
            if line.starts_with("--- ") {
                old_path = line
                    .strip_prefix("--- a/")
                    .map(|p| p.strip_suffix('\t').unwrap_or(p))
                    .map(String::from);
            } else if line.starts_with("+++ ") {
                if let Some(p) = line.strip_prefix("+++ b/") {
                    cur_path = Some(p.strip_suffix('\t').unwrap_or(p).to_string());
                }
            }
            header.push_str(line);
            header.push('\n');
        }
    }
    flush_hunk(&mut hunks, &mut hunk_idx, &mut hunk_header, &mut hunk_body, &mut adds, &mut dels);
    flush_file(&mut files, &mut cur_path, &mut old_path, &mut header, &mut hunks);
    files
}

/// Apply selected hunks to a target dir (CAO `apply_selection`): reassemble a
/// patch from the chosen hunks and `git apply` it (`--reverse` for revert).
/// `selections` is `{path: [hunk_index, …]}` (null/absent ⇒ all hunks of that
/// file). `expected_digest`, when given, must match the digest of the patch
/// assembled NOW — the binding between the diff the reviewer saw (hunked_diff
/// served it alongside the same digest) and the diff being applied; a worktree
/// that moved in between is a structured `stale` refusal, never a merge of
/// content nobody reviewed. Returns `{applied, target_dir, files, conflicts,
/// error}`.
pub fn apply_selection(
    cwd: &str,
    base: Option<&str>,
    target_dir: &str,
    mode: &str, // "merge" | "revert"
    selections: &Value,
    expected_digest: Option<&str>,
) -> Value {
    let src = Path::new(cwd);
    let base = resolve_base(src, base);
    // The same patch set hunked_diff serves, so the UI's hunk selections
    // (including agent-created untracked files) reassemble 1:1.
    let raw = review_patch(src, &base);
    if let Some(exp) = expected_digest {
        let now = patch_digest(&raw);
        if now != exp {
            return json!({
                "applied": false, "stale": true, "target_dir": target_dir,
                "files": [], "conflicts": [],
                "error": "the diff changed since it was reviewed — reload and review the new changes"
            });
        }
    }
    let parsed = parse_unified(&raw);
    // Source-side content reader for the new-file content compare (live path: the
    // worktree file on disk).
    let src_top = repo_toplevel(src);
    let read_src = move |path: &str| std::fs::read(src_top.join(path)).ok();
    assemble_and_apply(&parsed, &read_src, target_dir, mode, selections)
}

/// Reassemble a patch from the selected hunks of `parsed` and `git apply` it into
/// `target_dir` (`--reverse` for revert). The ONLY source-dependent step is the
/// new-file content compare (an agent-created file that already exists at the
/// target): `read_src` returns the source-side bytes for a path, so this body
/// serves BOTH a live worktree and a reclaimed agent's archive ref unchanged.
/// `parse_unified` + the digest staleness check happen in the caller (they depend
/// on which patch text the source provides). Returns `{applied, target_dir,
/// files, conflicts, error}`.
fn assemble_and_apply(
    parsed: &[Value],
    read_src: &dyn Fn(&str) -> Option<Vec<u8>>,
    target_dir: &str,
    mode: &str, // "merge" | "revert"
    selections: &Value,
) -> Value {
    // Reassemble a patch from the selected hunks.
    let target_top = repo_toplevel(Path::new(target_dir));
    let mut patch = String::new();
    let mut applied_files: Vec<String> = Vec::new();
    let mut pre_conflicts: Vec<String> = Vec::new();
    let sel_obj = selections.as_object();
    for file in parsed {
        let path = file["path"].as_str().unwrap_or("");
        let want: Option<Vec<i64>> = sel_obj
            .and_then(|o| o.get(path))
            .map(|v| {
                v.as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_i64()).collect())
                    .unwrap_or_default()
            });
        // Skip files not in the (present, non-empty) selection map.
        if sel_obj.map(|o| !o.is_empty()).unwrap_or(false) && want.is_none() {
            continue;
        }
        let header = file["header"].as_str().unwrap_or("");
        let empty = Vec::new();
        let hunks = file["hunks"].as_array().unwrap_or(&empty);
        let chosen: Vec<&Value> = hunks
            .iter()
            .filter(|h| match &want {
                Some(ids) if !ids.is_empty() => ids.contains(&h["index"].as_i64().unwrap_or(-1)),
                _ => true, // null/empty selection ⇒ all hunks
            })
            .collect();
        // New-file patches need target-aware handling `git apply` doesn't give:
        // it hard-fails the WHOLE patch when the file already exists at the
        // target (the normal state after the first merge of an agent-created
        // file, which stays untracked in the worktree and re-appears in every
        // diff — review L1). Identical content ⇒ no-op success; different ⇒ a
        // conflict naming the file; reverting an already-absent file ⇒ no-op.
        let is_new_file = header.contains("\nnew file mode");
        let is_binary_stub = header.contains("\nBinary files ");
        if is_new_file && !is_binary_stub {
            let at_target = target_top.join(path);
            if mode == "merge" && at_target.exists() {
                let same = read_src(path) == std::fs::read(&at_target).ok();
                if same {
                    applied_files.push(path.to_string()); // already there — no-op
                } else {
                    pre_conflicts.push(path.to_string());
                }
                continue;
            }
            if mode == "revert" && !at_target.exists() {
                applied_files.push(path.to_string()); // already gone — no-op
                continue;
            }
            // An EMPTY new file is a header-only patch with zero hunks; the
            // header alone applies (creates/deletes the empty file), so a
            // selected empty file must not fall through the no-hunks skip
            // (review L6 — it used to vanish from the merge while the UI
            // reported success).
            if chosen.is_empty() && hunks.is_empty() {
                patch.push_str(header);
                patch.push('\n');
                applied_files.push(path.to_string());
                continue;
            }
        }
        if chosen.is_empty() {
            continue;
        }
        patch.push_str(header);
        patch.push('\n');
        for h in chosen {
            patch.push_str(h["text"].as_str().unwrap_or(""));
            patch.push('\n');
        }
        applied_files.push(path.to_string());
    }

    if !pre_conflicts.is_empty() {
        // Refuse atomically BEFORE git apply: a mixed patch would hard-fail
        // wholesale anyway, and this names the real cause.
        return json!({
            "applied": false, "conflicted": true, "target_dir": target_dir,
            "files": applied_files, "conflicts": pre_conflicts,
            "error": "file(s) already exist at the target with different content"
        });
    }
    if patch.trim().is_empty() {
        if !applied_files.is_empty() {
            // Everything selected was already at the target — a no-op merge.
            return json!({ "applied": true, "target_dir": target_dir, "files": applied_files, "conflicts": [], "error": null });
        }
        return json!({ "applied": false, "target_dir": target_dir, "files": [], "conflicts": [], "error": "nothing selected" });
    }

    // Write the patch to a temp file and git-apply it in the target dir.
    let tmp = std::env::temp_dir().join(format!("taime-apply-{:08x}.patch", rand::random::<u32>()));
    if std::fs::write(&tmp, &patch).is_err() {
        return json!({ "applied": false, "target_dir": target_dir, "files": [], "conflicts": [], "error": "write patch failed" });
    }
    let tmp_str = tmp.to_string_lossy().to_string();
    // Direct working-tree apply first: `--3way` refuses outright when the
    // target's index doesn't match its working tree ("does not match index") —
    // which is the NORMAL state of the dir a revert targets (its uncommitted
    // changes are exactly what's being reversed). Fall back to `--3way` only
    // when the direct apply doesn't fit, for its conflict-marker output (a
    // failed direct apply is atomic, so the retry starts from an untouched
    // tree).
    let reverse: &[&str] = if mode == "revert" { &["--reverse"] } else { &[] };
    let direct = git_raw(Path::new(target_dir), &[&["apply"], reverse, &[tmp_str.as_str()]].concat());
    let out = match &direct {
        Ok(o) if o.status.success() => direct,
        _ => git_raw(Path::new(target_dir), &[&["apply", "--3way"], reverse, &[tmp_str.as_str()]].concat()),
    };
    let _ = std::fs::remove_file(&tmp);

    match out {
        Ok(o) if o.status.success() => {
            json!({ "applied": true, "target_dir": target_dir, "files": applied_files, "conflicts": [], "error": null })
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr).to_string();
            // `git apply --3way` on a real conflict writes `<<<<<<<`/`>>>>>>>`
            // markers into the working tree and exits non-zero (review L6).
            // Distinguish that from a hard apply failure by scanning the targeted
            // files for markers — a conflict is "applied, needs resolution", not a
            // silent no-op the old `conflicts:[]` implied.
            let conflicts: Vec<String> = applied_files
                .iter()
                .filter(|f| {
                    std::fs::read_to_string(Path::new(target_dir).join(f))
                        .map(|c| c.contains("<<<<<<<") && c.contains(">>>>>>>"))
                        .unwrap_or(false)
                })
                .cloned()
                .collect();
            if conflicts.is_empty() {
                json!({ "applied": false, "conflicted": false, "target_dir": target_dir, "files": applied_files, "conflicts": [], "error": err })
            } else {
                json!({ "applied": true, "conflicted": true, "target_dir": target_dir, "files": applied_files, "conflicts": conflicts, "error": err })
            }
        }
        Err(e) => {
            json!({ "applied": false, "conflicted": false, "target_dir": target_dir, "files": [], "conflicts": [], "error": e.to_string() })
        }
    }
}

// ---- Archive-side rendering (Phase 2): the SAME review/merge surfaces, served
// ---- from a reclaimed agent's cached patch + `refs/taime/archive/*` ref instead
// ---- of a live worktree checkout. The cached patch is `git diff base archive`
// ---- (so it parses identically to the live `review_patch`); both file sides come
// ---- from the base/archive refs. ----

/// `git show <ref>:<path>` as `(text, is_binary)` — the ref-backed twin of
/// [`read_worktree_file`] (size-capped, non-UTF8 ⇒ binary).
fn read_ref_file(repo: &Path, gitref: &str, path: &str) -> (String, bool) {
    match git_raw(repo, &["show", &format!("{gitref}:{path}")]) {
        Ok(o) if o.status.success() => {
            if o.stdout.len() > MAX_CONTENT_BYTES {
                (String::new(), false)
            } else if let Ok(s) = String::from_utf8(o.stdout) {
                (s, false)
            } else {
                (String::new(), true)
            }
        }
        _ => (String::new(), false),
    }
}

/// Raw bytes of `git show <ref>:<path>`, `None` if absent — the archive-side
/// `read_src` for [`assemble_and_apply`]'s new-file content compare.
fn show_ref_bytes(repo: &Path, gitref: &str, path: &str) -> Option<Vec<u8>> {
    let out = git_raw(repo, &["show", &format!("{gitref}:{path}")]).ok()?;
    out.status.success().then_some(out.stdout)
}

/// Combined diff + changed-file list for a reclaimed agent (the `terminal_diff`
/// twin). The cached patch IS the diff; the file list is its parsed paths.
pub fn terminal_diff_archived(agent_id: &str, cached_patch: &str) -> Value {
    let files: Vec<String> = parse_unified(cached_patch)
        .iter()
        .filter_map(|f| f["path"].as_str().map(String::from))
        .collect();
    json!({
        "agent_id": agent_id, "working_directory": null, "is_git": true,
        "diff": cached_patch, "files_changed": files.len(), "files": files, "error": null
    })
}

/// Per-hunk selectable diff for a reclaimed agent (the `hunked_diff` twin). The
/// digest is the cached patch's — now immutable, so the merge gate's staleness
/// check is stable across fetch→apply.
pub fn hunked_diff_archived(agent_id: &str, base: &str, cached_patch: &str) -> Value {
    json!({
        "agent_id": agent_id, "base": base,
        "digest": patch_digest(cached_patch),
        "files": parse_unified(cached_patch)
    })
}

/// Per-file both-sides reconstruction for a reclaimed agent (the `file_diffs`
/// twin), drawn from the base + archive refs. Untracked files were committed into
/// the snapshot, so they arrive as ordinary `added` entries — no separate
/// untracked pass needed.
pub fn file_diffs_archived(agent_id: &str, repo_root: &str, base: &str, archive_ref: &str) -> Value {
    let repo = Path::new(repo_root);
    if !is_git(repo) {
        return json!({ "agent_id": agent_id, "files": [] });
    }
    use std::collections::HashMap;
    let mut nums: HashMap<String, (i64, i64, bool)> = HashMap::new();
    for line in git_ok(repo, &["diff", base, archive_ref, "--numstat", "-M"]).unwrap_or_default().lines()
    {
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() == 3 {
            let binary = parts[0] == "-";
            let add = parts[0].parse::<i64>().unwrap_or(0);
            let del = parts[1].parse::<i64>().unwrap_or(0);
            nums.insert(parts[2].to_string(), (add, del, binary));
        }
    }
    let mut files: Vec<Value> = Vec::new();
    for line in git_ok(repo, &["diff", base, archive_ref, "--name-status", "-M"]).unwrap_or_default().lines()
    {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.is_empty() {
            continue;
        }
        let code = parts[0];
        let (status, old_path, new_path) = if code.starts_with('R') && parts.len() >= 3 {
            ("renamed", Some(parts[1].to_string()), parts[2].to_string())
        } else if parts.len() >= 2 {
            let s = match code.chars().next() {
                Some('A') => "added",
                Some('D') => "deleted",
                _ => "modified",
            };
            (s, None, parts[1].to_string())
        } else {
            continue;
        };
        if is_transient(&new_path) {
            continue;
        }
        let (add, del, binary) = nums.get(&new_path).copied().unwrap_or((0, 0, false));
        let original = if status == "added" {
            String::new()
        } else {
            let from = old_path.as_deref().unwrap_or(&new_path);
            read_ref_file(repo, base, from).0
        };
        let (modified, bin2) = if status == "deleted" {
            (String::new(), false)
        } else {
            read_ref_file(repo, archive_ref, &new_path)
        };
        files.push(json!({
            "path": new_path, "status": status, "old_path": old_path,
            "original": original, "modified": modified,
            "additions": add, "deletions": del, "binary": binary || bin2
        }));
    }
    json!({ "agent_id": agent_id, "files": files })
}

/// Apply selected hunks of a reclaimed agent's cached patch into `target_dir`
/// (the `apply_selection` twin). The new-file content compare reads the archive
/// ref instead of a worktree file; everything else is the shared apply core.
pub fn apply_selection_archived(
    repo_root: &str,
    archive_ref: &str,
    cached_patch: &str,
    target_dir: &str,
    mode: &str,
    selections: &Value,
    expected_digest: Option<&str>,
) -> Value {
    if let Some(exp) = expected_digest {
        if patch_digest(cached_patch) != exp {
            return json!({
                "applied": false, "stale": true, "target_dir": target_dir,
                "files": [], "conflicts": [],
                "error": "the diff changed since it was reviewed — reload and review the new changes"
            });
        }
    }
    let parsed = parse_unified(cached_patch);
    let repo = Path::new(repo_root).to_path_buf();
    let read_src = move |path: &str| show_ref_bytes(&repo, archive_ref, path);
    assemble_and_apply(&parsed, &read_src, target_dir, mode, selections)
}

// ---- Commit-with-provenance (Phase 2, gap #1): the merge no longer dead-ends at
// ---- `git apply`. After a clean apply of the selected hunks, stage exactly those
// ---- files and record them as ONE commit carrying the provenance trailer the
// ---- caller assembled (Co-authored-by + Taime-Agent-Id == refs/taime/archive/*).
// ---- Built ON the hardened apply core, so the digest/staleness binding, untracked
// ---- handling, and conflict detection all carry over unchanged. Merge-only —
// ---- revert never commits (it discards the agent's own work back to base). ----

/// Whether `target_dir` can produce a commit identity. `git commit` hard-fails
/// without `user.name`/`user.email` (config or env); pre-checking lets the merge
/// refuse BEFORE it applies anything, so a missing identity never leaves a
/// half-applied, uncommitted tree.
fn has_commit_identity(target_dir: &str) -> bool {
    git_ok(Path::new(target_dir), &["var", "GIT_COMMITTER_IDENT"]).is_some()
}

/// The first selected path that already has uncommitted changes (staged or
/// unstaged) at the target — the merge would otherwise fold the target's own
/// edits into the provenance commit (the partial-commit pathspec captures the
/// working-tree content of these paths). A `Some` is a refusal: the commit must
/// record ONLY the agent's applied hunks. One `git status` probe per selected
/// path keeps the match exact (no porcelain rename/quote parsing).
fn target_dirty_selected_path(target_dir: &str, selections: &Value) -> Option<String> {
    let obj = selections.as_object()?;
    let top = repo_toplevel(Path::new(target_dir));
    for key in obj.keys() {
        let dirty = git_ok(&top, &["status", "--porcelain", "--", key])
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        if dirty {
            return Some(key.clone());
        }
    }
    None
}

/// Refusals that must short-circuit BEFORE any apply touches the target tree:
/// a selected path that's already dirty, or a target with no commit identity.
/// `None` ⇒ clear to apply + commit.
fn commit_preflight(target_dir: &str, selections: &Value) -> Option<Value> {
    if let Some(path) = target_dirty_selected_path(target_dir, selections) {
        return Some(json!({
            "applied": false, "committed": false, "target_dir": target_dir,
            "files": [], "conflicts": [],
            "error": format!("target already has uncommitted changes to '{path}' — commit or stash it before merging")
        }));
    }
    if !has_commit_identity(target_dir) {
        return Some(json!({
            "applied": false, "committed": false, "target_dir": target_dir,
            "files": [], "conflicts": [],
            "error": "target repo has no git identity — set user.name and user.email to commit the merge"
        }));
    }
    None
}

/// Stage + commit exactly the files an apply landed in `target_dir`, with
/// `message` (subject + body + trailer block, assembled by the caller). Called
/// ONLY after a successful, conflict-free apply: a non-applied / stale /
/// conflicted `res` passes straight through with `committed:false` — a tree that
/// didn't cleanly receive the hunks is never committed. The commit is a partial
/// commit over just the applied pathspec, so unrelated changes in the target
/// (e.g. the user's real `main` tree) are left untouched. Returns the apply
/// result augmented with `{committed, commit}` (commit = short sha).
fn finish_commit(mut res: Value, target_dir: &str, message: &str) -> Value {
    let applied = res["applied"].as_bool().unwrap_or(false);
    let conflict_free = res["conflicts"].as_array().map(|a| a.is_empty()).unwrap_or(true);
    let files: Vec<String> = res["files"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    if !applied || !conflict_free || files.is_empty() {
        res["committed"] = json!(false);
        return res;
    }
    let top = repo_toplevel(Path::new(target_dir));
    // Stage exactly the applied files (the partial commit below re-reads the
    // worktree for these paths regardless, but staging surfaces an error early
    // and makes the intent explicit).
    let mut add_args: Vec<&str> = vec!["add", "--"];
    add_args.extend(files.iter().map(String::as_str));
    if git_ok(&top, &add_args).is_none() {
        res["committed"] = json!(false);
        res["error"] = json!("staging the merged files failed");
        return res;
    }
    // Nothing actually staged ⇒ the selected change is already present at the
    // target (e.g. re-merging an agent-created file that was merged before and
    // still shows as untracked in the agent's worktree). `git commit` would fail
    // "nothing to commit"; report a clean no-op instead of a misleading error —
    // the desired state already holds. (apply already reported applied:true.)
    let mut pathspec: Vec<&str> = vec!["diff", "--cached", "--quiet", "--"];
    pathspec.extend(files.iter().map(String::as_str));
    if git_raw(&top, &pathspec).map(|o| o.status.success()).unwrap_or(false) {
        res["committed"] = json!(false);
        res["noop"] = json!(true);
        res["error"] = Value::Null;
        return res;
    }
    // Commit ONLY those paths — a partial commit, so other staged/unstaged
    // changes in the target are not swept in.
    let mut commit_args: Vec<&str> = vec!["commit", "-m", message, "--"];
    commit_args.extend(files.iter().map(String::as_str));
    match git_raw(&top, &commit_args) {
        Ok(o) if o.status.success() => {
            let sha = git_ok(&top, &["rev-parse", "--short", "HEAD"])
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            res["committed"] = json!(true);
            res["commit"] = json!(sha);
            res
        }
        Ok(o) => {
            res["committed"] = json!(false);
            res["error"] = json!(String::from_utf8_lossy(&o.stderr).trim().to_string());
            res
        }
        Err(e) => {
            res["committed"] = json!(false);
            res["error"] = json!(e.to_string());
            res
        }
    }
}

/// Merge selected hunks of a LIVE agent's worktree into `target_dir` AND record
/// them as one provenance commit (the `apply_selection` twin that doesn't stop at
/// `git apply`). Same digest binding, untracked handling, and `--3way` conflict
/// path; the caller supplies the trailer-bearing `message`.
pub fn commit_selection(
    cwd: &str,
    base: Option<&str>,
    target_dir: &str,
    selections: &Value,
    expected_digest: Option<&str>,
    message: &str,
) -> Value {
    if let Some(refusal) = commit_preflight(target_dir, selections) {
        return refusal;
    }
    let res = apply_selection(cwd, base, target_dir, "merge", selections, expected_digest);
    finish_commit(res, target_dir, message)
}

/// Merge selected hunks of a RECLAIMED agent's archived patch into `target_dir`
/// AND record them as one provenance commit (the `apply_selection_archived`
/// twin). Content comes from the archive ref; everything else matches
/// [`commit_selection`].
#[allow(clippy::too_many_arguments)]
pub fn commit_selection_archived(
    repo_root: &str,
    archive_ref: &str,
    cached_patch: &str,
    target_dir: &str,
    selections: &Value,
    expected_digest: Option<&str>,
    message: &str,
) -> Value {
    if let Some(refusal) = commit_preflight(target_dir, selections) {
        return refusal;
    }
    let res = apply_selection_archived(
        repo_root, archive_ref, cached_patch, target_dir, "merge", selections, expected_digest,
    );
    finish_commit(res, target_dir, message)
}

/// `(selected, total)` hunk counts for a selection — the provenance trailer's
/// `Taime-Hunks`/`Taime-Merge-Scope`. Each changed file is ≥1 unit (an empty
/// new file has no hunks but is one unit of work); a present selection key with a
/// null/empty array means "the whole file". `selected == total` ⇒ a full merge.
fn count_selected(raw: &str, selections: &Value) -> (usize, usize) {
    let parsed = parse_unified(raw);
    let sel_obj = selections.as_object();
    let mut total = 0usize;
    let mut selected = 0usize;
    for file in &parsed {
        let path = file["path"].as_str().unwrap_or("");
        let units = file["hunks"].as_array().map(|a| a.len()).unwrap_or(0).max(1);
        total += units;
        match sel_obj.and_then(|o| o.get(path)) {
            None => {}
            Some(v) => match v.as_array() {
                Some(ids) if !ids.is_empty() => selected += ids.len().min(units),
                _ => selected += units, // null / empty array ⇒ whole file
            },
        }
    }
    (selected, total)
}

/// `(selected, total)` hunk counts for a LIVE agent's worktree selection.
pub fn selection_counts_live(cwd: &str, base: Option<&str>, selections: &Value) -> (usize, usize) {
    let p = Path::new(cwd);
    let base = resolve_base(p, base);
    count_selected(&review_patch(p, &base), selections)
}

/// `(selected, total)` hunk counts for a RECLAIMED agent's archived patch.
pub fn selection_counts_archived(cached_patch: &str, selections: &Value) -> (usize, usize) {
    count_selected(cached_patch, selections)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(cwd: &Path, args: &[&str]) {
        assert!(Command::new("git").current_dir(cwd).args(args).output().unwrap().status.success(), "git {args:?}");
    }

    fn repo() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("taime-diff-{:08x}", rand::random::<u32>()));
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        git(&dir, &["config", "user.email", "t@t"]);
        git(&dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-qm", "init"]);
        dir
    }

    #[test]
    fn terminal_diff_reports_modified_and_untracked() {
        let dir = repo();
        std::fs::write(dir.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        std::fs::write(dir.join("b.txt"), "new file\n").unwrap();
        let v = terminal_diff("t1", dir.to_str().unwrap(), Some("HEAD"));
        assert_eq!(v["is_git"], true);
        let files = v["files"].as_array().unwrap();
        assert!(files.iter().any(|f| f == "a.txt"));
        assert!(files.iter().any(|f| f == "b.txt"));
        assert!(v["diff"].as_str().unwrap().contains("TWO"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn diffs_exclude_atomic_write_temps() {
        let dir = repo();
        // A real edit plus the atomic-write temp an editor/agent leaves behind.
        std::fs::write(dir.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        std::fs::write(dir.join("a.txt.tmp.26298.0a12ff87c3d1"), "scratch\n").unwrap();
        std::fs::write(dir.join("notes.md~"), "backup\n").unwrap();

        let t = terminal_diff("t1", dir.to_str().unwrap(), Some("HEAD"));
        let tfiles = t["files"].as_array().unwrap();
        assert!(tfiles.iter().any(|f| f == "a.txt"), "real edit shows");
        assert!(
            !tfiles.iter().any(|f| f.as_str().unwrap().contains(".tmp.") || f.as_str().unwrap().ends_with('~')),
            "transient temps filtered from terminal_diff: {tfiles:?}"
        );

        let f = file_diffs("t1", dir.to_str().unwrap(), Some("HEAD"));
        let ffiles = f["files"].as_array().unwrap();
        assert!(ffiles.iter().any(|f| f["path"] == "a.txt"));
        assert!(
            !ffiles.iter().any(|f| {
                let p = f["path"].as_str().unwrap();
                p.contains(".tmp.") || p.ends_with('~')
            }),
            "transient temps filtered from file_diffs review list"
        );

        let h = hunked_diff("t1", dir.to_str().unwrap(), Some("HEAD"));
        assert!(
            !h["files"].as_array().unwrap().iter().any(|f| {
                let p = f["path"].as_str().unwrap();
                p.contains(".tmp.") || p.ends_with('~')
            }),
            "transient temps filtered from the hunked (mergeable) surface too"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_diffs_reconstructs_both_sides() {
        let dir = repo();
        std::fs::write(dir.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        let v = file_diffs("t1", dir.to_str().unwrap(), Some("HEAD"));
        let files = v["files"].as_array().unwrap();
        let a = files.iter().find(|f| f["path"] == "a.txt").unwrap();
        assert_eq!(a["status"], "modified");
        assert!(a["original"].as_str().unwrap().contains("two"));
        assert!(a["modified"].as_str().unwrap().contains("TWO"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hunked_diff_parses_hunks() {
        let dir = repo();
        std::fs::write(dir.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        let v = hunked_diff("t1", dir.to_str().unwrap(), Some("HEAD"));
        let files = v["files"].as_array().unwrap();
        let a = files.iter().find(|f| f["path"] == "a.txt").unwrap();
        let hunks = a["hunks"].as_array().unwrap();
        assert!(!hunks.is_empty());
        assert!(hunks[0]["text"].as_str().unwrap().contains("@@"));
        assert_eq!(hunks[0]["index"], 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hunked_diff_includes_untracked_files_as_selectable_hunks() {
        let dir = repo();
        std::fs::write(dir.join("b.txt"), "new file\n").unwrap();
        let v = hunked_diff("t1", dir.to_str().unwrap(), Some("HEAD"));
        let files = v["files"].as_array().unwrap();
        let b = files
            .iter()
            .find(|f| f["path"] == "b.txt")
            .expect("untracked file present on the hunked surface");
        let hunks = b["hunks"].as_array().unwrap();
        assert_eq!(hunks.len(), 1, "one synthetic new-file hunk");
        assert_eq!(hunks[0]["index"], 0);
        assert!(hunks[0]["text"].as_str().unwrap().contains("+new file"));
        assert_eq!(hunks[0]["additions"], 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_selection_merges_and_reverts_an_untracked_file() {
        let src = repo();
        std::fs::write(src.join("b.txt"), "agent-created\n").unwrap();
        let target = std::env::temp_dir().join(format!("taime-target-{:08x}", rand::random::<u32>()));
        std::fs::create_dir_all(&target).unwrap();
        git(Path::new(&target), &["init", "-q"]);

        // Merge: the new file lands in the target.
        let v = apply_selection(
            src.to_str().unwrap(),
            Some("HEAD"),
            target.to_str().unwrap(),
            "merge",
            &json!({ "b.txt": [0] }),
            None,
        );
        assert_eq!(v["applied"], true, "error: {:?}", v["error"]);
        assert_eq!(std::fs::read_to_string(target.join("b.txt")).unwrap(), "agent-created\n");

        // Re-merging the same (still-untracked) file is a no-op success, not
        // the wholesale `git apply` failure "already exists in working
        // directory" (review L1 — iterative review of agent files).
        let v = apply_selection(
            src.to_str().unwrap(),
            Some("HEAD"),
            target.to_str().unwrap(),
            "merge",
            &json!({ "b.txt": [0] }),
            None,
        );
        assert_eq!(v["applied"], true, "idempotent re-merge: {:?}", v["error"]);

        // Same file but DIFFERENT content at the target: an explicit conflict
        // naming the file — never a silent skip or a whole-patch failure.
        std::fs::write(target.join("b.txt"), "edited at target\n").unwrap();
        let v = apply_selection(
            src.to_str().unwrap(),
            Some("HEAD"),
            target.to_str().unwrap(),
            "merge",
            &json!({ "b.txt": [0] }),
            None,
        );
        assert_eq!(v["applied"], false);
        assert_eq!(v["conflicts"][0], "b.txt", "conflict names the file: {v}");

        // Revert (reverse-apply to self): the file is removed from the source.
        let v = apply_selection(
            src.to_str().unwrap(),
            Some("HEAD"),
            src.to_str().unwrap(),
            "revert",
            &json!({ "b.txt": [0] }),
            None,
        );
        assert_eq!(v["applied"], true, "error: {:?}", v["error"]);
        assert!(!src.join("b.txt").exists(), "reverted untracked file is gone");
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn apply_selection_merges_a_hunk() {
        let src = repo();
        // make a change in src
        std::fs::write(src.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        // target = a separate clone at the same base
        let target = std::env::temp_dir().join(format!("taime-target-{:08x}", rand::random::<u32>()));
        std::fs::create_dir_all(&target).unwrap();
        git(Path::new(&target), &["init", "-q"]);
        git(Path::new(&target), &["config", "user.email", "t@t"]);
        git(Path::new(&target), &["config", "user.name", "t"]);
        std::fs::write(target.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        git(Path::new(&target), &["add", "-A"]);
        git(Path::new(&target), &["commit", "-qm", "init"]);

        let v = apply_selection(
            src.to_str().unwrap(),
            Some("HEAD"),
            target.to_str().unwrap(),
            "merge",
            &json!({ "a.txt": [0] }),
            None,
        );
        assert_eq!(v["applied"], true, "error: {:?}", v["error"]);
        assert!(std::fs::read_to_string(target.join("a.txt")).unwrap().contains("TWO"));
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn stale_digest_refuses_the_apply() {
        let src = repo();
        std::fs::write(src.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        let h = hunked_diff("t1", src.to_str().unwrap(), Some("HEAD"));
        let digest = h["digest"].as_str().unwrap().to_string();

        // The worktree moves after the review was fetched…
        std::fs::write(src.join("a.txt"), "one\nTWO\nTHREE\n").unwrap();
        let v = apply_selection(
            src.to_str().unwrap(),
            Some("HEAD"),
            src.to_str().unwrap(),
            "revert",
            &json!({ "a.txt": [0] }),
            Some(&digest),
        );
        assert_eq!(v["applied"], false);
        assert_eq!(v["stale"], true, "a moved worktree must be a stale refusal: {v}");
        assert!(
            std::fs::read_to_string(src.join("a.txt")).unwrap().contains("THREE"),
            "nothing applied on a stale digest"
        );

        // A digest matching the CURRENT patch applies.
        let h = hunked_diff("t1", src.to_str().unwrap(), Some("HEAD"));
        let digest = h["digest"].as_str().unwrap().to_string();
        let v = apply_selection(
            src.to_str().unwrap(),
            Some("HEAD"),
            src.to_str().unwrap(),
            "revert",
            &json!({ "a.txt": [0] }),
            Some(&digest),
        );
        assert_eq!(v["applied"], true, "fresh digest applies: {:?}", v["error"]);
        let _ = std::fs::remove_dir_all(&src);
    }

    #[test]
    fn empty_untracked_file_is_merged_not_silently_dropped() {
        let src = repo();
        std::fs::write(src.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        std::fs::write(src.join("__init__.py"), "").unwrap();
        let target = std::env::temp_dir().join(format!("taime-target-{:08x}", rand::random::<u32>()));
        std::fs::create_dir_all(&target).unwrap();
        git(Path::new(&target), &["init", "-q"]);
        git(Path::new(&target), &["config", "user.email", "t@t"]);
        git(Path::new(&target), &["config", "user.name", "t"]);
        std::fs::write(target.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        git(Path::new(&target), &["add", "-A"]);
        git(Path::new(&target), &["commit", "-qm", "init"]);

        // The empty file is a header-only patch (zero hunks) — explicitly
        // selecting it must create it at the target, not vanish from the merge.
        let v = apply_selection(
            src.to_str().unwrap(),
            Some("HEAD"),
            target.to_str().unwrap(),
            "merge",
            &json!({ "a.txt": [0], "__init__.py": null }),
            None,
        );
        assert_eq!(v["applied"], true, "error: {:?}", v["error"]);
        assert!(target.join("__init__.py").exists(), "empty file landed: {v}");
        assert!(std::fs::read_to_string(target.join("a.txt")).unwrap().contains("TWO"));
        let files = v["files"].as_array().unwrap();
        assert!(files.iter().any(|f| f == "__init__.py"), "reported in files: {v}");
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn untracked_files_in_a_subdir_workspace_use_toplevel_relative_paths() {
        // Shared-mode agents can have project_root = a SUBDIR of the git repo;
        // cwd-relative patch paths would make `git apply` silently skip the
        // file (exit 0, nothing written) — paths must be toplevel-relative.
        let repo_dir = repo();
        let sub = repo_dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("new.txt"), "from subdir agent\n").unwrap();

        let h = hunked_diff("t1", sub.to_str().unwrap(), None);
        let files = h["files"].as_array().unwrap();
        assert!(
            files.iter().any(|f| f["path"] == "sub/new.txt"),
            "hunked path is toplevel-relative: {files:?}"
        );
        let fd = file_diffs("t1", sub.to_str().unwrap(), None);
        let entry = fd["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["path"] == "sub/new.txt")
            .expect("file_diffs keys match the hunked surface");
        assert!(entry["modified"].as_str().unwrap().contains("from subdir agent"));

        // Reverting from the subdir cwd actually removes the file (the old
        // cwd-relative form was a silent no-op success).
        let v = apply_selection(
            sub.to_str().unwrap(),
            None,
            sub.to_str().unwrap(),
            "revert",
            &json!({ "sub/new.txt": [0] }),
            None,
        );
        assert_eq!(v["applied"], true, "error: {:?}", v["error"]);
        assert!(!sub.join("new.txt").exists(), "subdir untracked file reverted");
        let _ = std::fs::remove_dir_all(&repo_dir);
    }

    #[test]
    fn spaced_filenames_parse_without_the_tab_suffix() {
        let dir = repo();
        std::fs::write(dir.join("design notes.md"), "agent wrote this\n").unwrap();
        let h = hunked_diff("t1", dir.to_str().unwrap(), Some("HEAD"));
        let files = h["files"].as_array().unwrap();
        let f = files
            .iter()
            .find(|f| f["path"] == "design notes.md")
            .unwrap_or_else(|| panic!("clean (untabbed) path key, got: {files:?}"));
        assert_eq!(f["hunks"].as_array().unwrap().len(), 1);

        // And the clean key actually selects it for apply.
        let v = apply_selection(
            dir.to_str().unwrap(),
            Some("HEAD"),
            dir.to_str().unwrap(),
            "revert",
            &json!({ "design notes.md": [0] }),
            None,
        );
        assert_eq!(v["applied"], true, "error: {:?}", v["error"]);
        assert!(!dir.join("design notes.md").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_utf8_untracked_content_is_never_corrupted_into_a_merge() {
        let src = repo();
        // ISO-8859-1 "café latte" — text to the user, invalid UTF-8 to Rust.
        std::fs::write(src.join("latin1.txt"), b"caf\xe9 latte\n").unwrap();
        let target = std::env::temp_dir().join(format!("taime-target-{:08x}", rand::random::<u32>()));
        std::fs::create_dir_all(&target).unwrap();
        git(Path::new(&target), &["init", "-q"]);

        // Listed for visibility, but NOT hunked (a lossy U+FFFD round-trip
        // would silently corrupt the merged bytes — worse than not merging).
        let t = terminal_diff("t1", src.to_str().unwrap(), Some("HEAD"));
        assert!(t["files"].as_array().unwrap().iter().any(|f| f == "latin1.txt"));
        let h = hunked_diff("t1", src.to_str().unwrap(), Some("HEAD"));
        assert!(
            !h["files"].as_array().unwrap().iter().any(|f| f["path"] == "latin1.txt"),
            "non-UTF8 file must not be hunked/mergeable: {h}"
        );
        let v = apply_selection(
            src.to_str().unwrap(),
            Some("HEAD"),
            target.to_str().unwrap(),
            "merge",
            &json!({ "latin1.txt": null }),
            None,
        );
        assert_eq!(v["applied"], false, "nothing to apply for a non-UTF8-only selection");
        assert!(!target.join("latin1.txt").exists(), "no corrupted bytes written");
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&target);
    }

    /// A target clone at the same base as [`repo`] (a separate checkout with
    /// `a.txt` at its committed content + a git identity, ready to commit into).
    fn target_clone() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("taime-target-{:08x}", rand::random::<u32>()));
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        git(&dir, &["config", "user.email", "t@t"]);
        git(&dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-qm", "init"]);
        dir
    }

    #[test]
    fn commit_selection_records_one_provenance_commit() {
        let src = repo();
        std::fs::write(src.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        let target = target_clone();
        let before = git_ok(Path::new(&target), &["rev-parse", "HEAD"]).unwrap();

        let msg = "taime: merge Claude Code agent abcd1234 → main\n\n1 file(s).\n\nCo-authored-by: Claude Code <agent-abcd1234@taime.local>\nTaime-Agent-Id: abcd1234";
        let v = commit_selection(
            src.to_str().unwrap(),
            Some("HEAD"),
            target.to_str().unwrap(),
            &json!({ "a.txt": [0] }),
            None,
            msg,
        );
        assert_eq!(v["applied"], true, "error: {:?}", v["error"]);
        assert_eq!(v["committed"], true, "error: {:?}", v["error"]);
        assert!(v["commit"].as_str().is_some_and(|s| !s.is_empty()), "commit sha: {v}");

        // HEAD advanced by exactly one commit carrying the trailer.
        let after = git_ok(Path::new(&target), &["rev-parse", "HEAD"]).unwrap();
        assert_ne!(before, after, "a new commit was recorded");
        let body = git_ok(Path::new(&target), &["log", "-1", "--format=%B"]).unwrap();
        assert!(body.contains("Taime-Agent-Id: abcd1234"), "trailer present: {body}");
        assert!(body.contains("Co-authored-by: Claude Code"), "co-author present: {body}");
        // Exactly one file changed by the commit.
        let changed = git_ok(Path::new(&target), &["show", "--name-only", "--format=", "HEAD"]).unwrap();
        assert_eq!(changed.lines().filter(|l| !l.is_empty()).count(), 1, "only a.txt: {changed}");
        assert!(changed.contains("a.txt"));
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn commit_selection_refuses_a_dirty_target_path() {
        let src = repo();
        std::fs::write(src.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        let target = target_clone();
        // The target already has an uncommitted edit to the same file — merging
        // would otherwise fold it into the provenance commit.
        std::fs::write(target.join("a.txt"), "one\ntwo\nthree\nlocal\n").unwrap();
        let before = git_ok(Path::new(&target), &["rev-parse", "HEAD"]).unwrap();

        let v = commit_selection(
            src.to_str().unwrap(),
            Some("HEAD"),
            target.to_str().unwrap(),
            &json!({ "a.txt": [0] }),
            None,
            "msg",
        );
        assert_eq!(v["applied"], false, "refused before applying: {v}");
        assert_eq!(v["committed"], false);
        assert!(v["error"].as_str().unwrap().contains("a.txt"), "names the dirty file: {v}");
        // No commit, and the target's local edit is untouched.
        assert_eq!(before, git_ok(Path::new(&target), &["rev-parse", "HEAD"]).unwrap());
        assert!(std::fs::read_to_string(target.join("a.txt")).unwrap().contains("local"));
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn commit_selection_does_not_commit_on_conflict() {
        let src = repo();
        std::fs::write(src.join("b.txt"), "agent-created\n").unwrap();
        let target = target_clone();
        // A committed b.txt with DIFFERENT content ⇒ new-file collision (a
        // pre-conflict refusal); HEAD must not move.
        std::fs::write(target.join("b.txt"), "different\n").unwrap();
        git(&target, &["add", "-A"]);
        git(&target, &["commit", "-qm", "add b"]);
        let before = git_ok(Path::new(&target), &["rev-parse", "HEAD"]).unwrap();

        let v = commit_selection(
            src.to_str().unwrap(),
            Some("HEAD"),
            target.to_str().unwrap(),
            &json!({ "b.txt": [0] }),
            None,
            "msg",
        );
        assert_eq!(v["committed"], false, "conflict never commits: {v}");
        assert_eq!(v["conflicts"][0], "b.txt", "names the conflicting file: {v}");
        assert_eq!(before, git_ok(Path::new(&target), &["rev-parse", "HEAD"]).unwrap(), "HEAD unmoved");
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn commit_selection_archived_commits_from_the_ref() {
        // Build a real archive snapshot, then commit it forward from the archive
        // (the reclaimed-agent path: no live checkout).
        let repo = repo();
        let base = git_ok(&repo, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        std::fs::write(repo.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        let outcome = crate::worktree::archive_agent(
            "agent-x",
            repo.to_str().unwrap(),
            repo.to_str().unwrap(),
            &base,
        );
        let res = match outcome {
            crate::worktree::ArchiveOutcome::Archived(r) => r,
            other => panic!("expected Archived, got {other:?}"),
        };
        // Simulate reclaim: the worktree returns to base (work lives only in the ref).
        git(&repo, &["checkout", "-q", "--", "a.txt"]);

        let target = target_clone();
        let msg = "taime: merge agent agent-x\n\nTaime-Agent-Id: agent-x\nTaime-Archive-Ref: refs/taime/archive/agent-x";
        let v = commit_selection_archived(
            repo.to_str().unwrap(),
            &res.archive_ref,
            &res.diff_blob,
            target.to_str().unwrap(),
            &json!({ "a.txt": [0] }),
            Some(&res.digest),
            msg,
        );
        assert_eq!(v["applied"], true, "error: {:?}", v["error"]);
        assert_eq!(v["committed"], true, "error: {:?}", v["error"]);
        let body = git_ok(Path::new(&target), &["log", "-1", "--format=%B"]).unwrap();
        assert!(body.contains("Taime-Archive-Ref: refs/taime/archive/agent-x"), "trailer: {body}");
        assert!(std::fs::read_to_string(target.join("a.txt")).unwrap().contains("TWO"));
        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn commit_selection_noop_when_already_present_is_not_a_failure() {
        // Re-merging an agent-created file whose identical content is ALREADY
        // committed at the target: apply reports applied (no-op), and the commit
        // step must report a clean no-op, not a confusing "nothing to commit" error.
        let src = repo();
        std::fs::write(src.join("b.txt"), "agent-created\n").unwrap();
        let target = target_clone();
        // The same file, identical content, already committed at the target.
        std::fs::write(target.join("b.txt"), "agent-created\n").unwrap();
        git(&target, &["add", "-A"]);
        git(&target, &["commit", "-qm", "add b"]);
        let before = git_ok(Path::new(&target), &["rev-parse", "HEAD"]).unwrap();

        let v = commit_selection(
            src.to_str().unwrap(),
            Some("HEAD"),
            target.to_str().unwrap(),
            &json!({ "b.txt": [0] }),
            None,
            "msg",
        );
        assert_eq!(v["applied"], true, "{v}");
        assert_eq!(v["committed"], false, "{v}");
        assert_eq!(v["noop"], true, "already-present is a clean no-op: {v}");
        assert_eq!(v["error"], serde_json::Value::Null, "no error on a no-op: {v}");
        assert_eq!(before, git_ok(Path::new(&target), &["rev-parse", "HEAD"]).unwrap(), "HEAD unmoved");
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn commit_selection_stale_digest_does_not_commit() {
        let src = repo();
        std::fs::write(src.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        let target = target_clone();
        let before = git_ok(Path::new(&target), &["rev-parse", "HEAD"]).unwrap();
        // A digest that can't match the patch assembled now ⇒ stale refusal.
        let v = commit_selection(
            src.to_str().unwrap(),
            Some("HEAD"),
            target.to_str().unwrap(),
            &json!({ "a.txt": [0] }),
            Some("deadbeefdeadbeef"),
            "msg",
        );
        assert_eq!(v["stale"], true, "stale refusal: {v}");
        assert_eq!(v["committed"], false);
        assert_eq!(before, git_ok(Path::new(&target), &["rev-parse", "HEAD"]).unwrap());
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&target);
    }
}
