//! Boot-time orphan reconciliation (review H1/H2 backstop).
//!
//! `killpg` (session.rs) reaches every helper a CLI forks into its process group,
//! but a grandchild that itself `setsid()`'d — or any process surviving a daemon
//! that was SIGKILLed/crashed before it could signal anything — escapes. Every
//! agent child is spawned with `TAIME_SESSION_ID` in its env (inherited across
//! `setsid`), so on boot — while we hold the single-instance lock, meaning no live
//! daemon owns anything — any process still carrying the marker is necessarily an
//! orphan from a previous daemon. We SIGKILL its process group.
//!
//! Best-effort and conservative: same-uid only (we can only signal our own
//! processes anyway), never our own group, logged per group. macOS/BSD `ps -E`
//! appends each process's environment to the command column, which is how we
//! match the marker without `/proc`.

/// SIGKILL the process group of every still-running process carrying
/// `TAIME_SESSION_ID` in its environment. Call once at startup, AFTER acquiring
/// the liveness lock and BEFORE serving.
#[cfg(unix)]
pub fn sweep_orphan_agents() {
    use std::collections::BTreeSet;

    // -A all processes, -E append environment, -o no-header pid/pgid/command.
    let output = std::process::Command::new("ps")
        .args(["-A", "-E", "-o", "pid=,pgid=,command="])
        .output();
    let stdout = match output {
        Ok(o) if o.status.success() => o.stdout,
        _ => return, // ps unavailable / errored — skip the sweep
    };
    let text = String::from_utf8_lossy(&stdout);

    let me = std::process::id() as i32;
    // SAFETY: getpgrp() is always-succeeds and has no preconditions.
    let my_pgid = unsafe { libc::getpgrp() };

    let mut groups: BTreeSet<i32> = BTreeSet::new();
    for line in text.lines() {
        if !line.contains("TAIME_SESSION_ID=") {
            continue;
        }
        let mut cols = line.split_whitespace();
        let pid: i32 = match cols.next().and_then(|s| s.parse().ok()) {
            Some(p) => p,
            None => continue,
        };
        let pgid: i32 = match cols.next().and_then(|s| s.parse().ok()) {
            Some(p) => p,
            None => continue,
        };
        // Never ourselves, never our own group, never pgid 0/1 (init/no-group).
        if pid == me || pgid == my_pgid || pgid <= 1 {
            continue;
        }
        groups.insert(pgid);
    }

    for pgid in groups {
        // SAFETY: killpg(2) on a same-uid group; errors (already gone) ignored.
        unsafe {
            let _ = libc::killpg(pgid, libc::SIGKILL);
        }
        eprintln!("[taime-daemon] boot reconcile: killed orphan agent group {pgid}");
    }
}

#[cfg(not(unix))]
pub fn sweep_orphan_agents() {}
