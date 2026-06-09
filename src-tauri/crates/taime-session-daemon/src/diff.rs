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
    // Untracked files as synthetic added diffs (skipping transient temps).
    for f in git_ok(path, &["ls-files", "--others", "--exclude-standard"])
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty() && !is_transient(l))
    {
        if let Ok(o) = git_raw(path, &["diff", "--no-index", "--", "/dev/null", f]) {
            diff.push_str(&String::from_utf8_lossy(&o.stdout));
        }
        files.push(f.to_string());
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
    // Untracked → added (skipping transient temps).
    for f in git_ok(path, &["ls-files", "--others", "--exclude-standard"])
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty() && !is_transient(l))
    {
        let (modified, binary) = read_worktree_file(path, f);
        let adds = modified.lines().count() as i64;
        files.push(json!({
            "path": f, "status": "added", "old_path": null,
            "original": "", "modified": modified, "additions": adds, "deletions": 0, "binary": binary
        }));
    }
    json!({ "agent_id": agent_id, "files": files })
}

/// Per-file, per-hunk diff for selective merge/revert (CAO `get_hunked_diff`).
pub fn hunked_diff(agent_id: &str, cwd: &str, base: Option<&str>) -> Value {
    let path = Path::new(cwd);
    if !is_git(path) {
        return json!({ "agent_id": agent_id, "base": null, "files": [] });
    }
    let base = resolve_base(path, base);
    let raw = git_ok(path, &["diff", &base]).unwrap_or_default();
    json!({ "agent_id": agent_id, "base": base, "files": parse_unified(&raw) })
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
            if line.starts_with("--- ") {
                old_path = line.strip_prefix("--- a/").map(String::from);
            } else if line.starts_with("+++ ") {
                if let Some(p) = line.strip_prefix("+++ b/") {
                    cur_path = Some(p.to_string());
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
/// file). Returns `{applied, target_dir, files, conflicts, error}`.
pub fn apply_selection(
    cwd: &str,
    base: Option<&str>,
    target_dir: &str,
    mode: &str, // "merge" | "revert"
    selections: &Value,
) -> Value {
    let src = Path::new(cwd);
    let base = resolve_base(src, base);
    let raw = git_ok(src, &["diff", &base]).unwrap_or_default();
    let parsed = parse_unified(&raw);

    // Reassemble a patch from the selected hunks.
    let mut patch = String::new();
    let mut applied_files: Vec<String> = Vec::new();
    let sel_obj = selections.as_object();
    for file in &parsed {
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

    if patch.trim().is_empty() {
        return json!({ "applied": false, "target_dir": target_dir, "files": [], "conflicts": [], "error": "nothing selected" });
    }

    // Write the patch to a temp file and git-apply it in the target dir.
    let tmp = std::env::temp_dir().join(format!("taime-apply-{:08x}.patch", rand::random::<u32>()));
    if std::fs::write(&tmp, &patch).is_err() {
        return json!({ "applied": false, "target_dir": target_dir, "files": [], "conflicts": [], "error": "write patch failed" });
    }
    let tmp_str = tmp.to_string_lossy().to_string();
    let mut args = vec!["apply", "--3way"];
    if mode == "revert" {
        args.push("--reverse");
    }
    args.push(&tmp_str);
    let out = git_raw(Path::new(target_dir), &args);
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
        );
        assert_eq!(v["applied"], true, "error: {:?}", v["error"]);
        assert!(std::fs::read_to_string(target.join("a.txt")).unwrap().contains("TWO"));
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&target);
    }
}
