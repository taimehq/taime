#!/usr/bin/env python3
"""Headless end-to-end proof of Taime's flagship loop:

    worktree isolation -> per-turn attributed dirty state -> contention
    -> selective merge to main

Runs a THROWAWAY cao-server (isolated HOME + a free port) against a TEMP git
repo and drives deterministic, simulated agent edits (direct file writes between
turn checkpoints — no real CLIs). It never touches the live :9889 server, the
real CAO db, or any of your sessions.

This proves the *logic* of the flagship (the backend attribution loop). It does
NOT cover the native-webview rendering (Monaco provenance chips, the gate
dialog) or terminal fidelity — those need the parity audit at the native window.

Usage:  python3 tests/attribution_loop.py
Exit 0 = all assertions passed.
"""

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

CAO_SERVER = os.path.expanduser("~/.local/bin/cao-server")


def free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def git(repo: str, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", repo, *args],
        check=True,
        capture_output=True,
        text=True,
    ).stdout


def write(path: str, content: str) -> None:
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(content)


def read(path: str) -> str:
    with open(path) as f:
        return f.read()


class Client:
    def __init__(self, base: str):
        self.base = base

    def __call__(self, method: str, path: str, body=None):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(
            self.base + path,
            data=data,
            method=method,
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(req, timeout=30) as resp:
            raw = resp.read()
            return json.loads(raw) if raw else None


def main() -> int:
    home = tempfile.mkdtemp(prefix="taime_test_home_")
    repo = tempfile.mkdtemp(prefix="taime_test_repo_")
    port = free_port()
    base = f"http://127.0.0.1:{port}"
    api = Client(base)

    results: list[tuple[bool, str, str]] = []

    def check(name: str, cond: bool, detail="") -> None:
        results.append((bool(cond), name, str(detail)))

    proc = None
    try:
        # --- temp git repo with a baseline commit -----------------------------
        git(repo, "init", "-q", "-b", "main")
        git(repo, "config", "user.email", "test@taime.dev")
        git(repo, "config", "user.name", "taime-test")
        write(os.path.join(repo, "app.py"), "v0\n")
        write(os.path.join(repo, "shared.py"), "base\n")
        git(repo, "add", "-A")
        git(repo, "commit", "-qm", "baseline")

        # --- throwaway cao-server (isolated HOME, free port) ------------------
        env = {**os.environ, "HOME": home}
        proc = subprocess.Popen(
            [CAO_SERVER, "--host", "127.0.0.1", "--port", str(port)],
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        # wait for health
        up = False
        for _ in range(60):
            try:
                api("GET", "/health")
                up = True
                break
            except (urllib.error.URLError, ConnectionError):
                time.sleep(0.5)
        if not up:
            check("server came up", False, "health never returned")
            raise SystemExit
        check("server came up", True)

        session = "attrtest"

        # --- provision two isolated agents in the same session ----------------
        A = api("POST", "/worktrees/provision", {
            "project_root": repo, "provider": "claude_code",
            "isolate": True, "session_name": session,
        })
        B = api("POST", "/worktrees/provision", {
            "project_root": repo, "provider": "claude_code",
            "isolate": True, "session_name": session,
        })
        ta, tb = A["terminal_id"], B["terminal_id"]
        check("A got an isolated worktree",
              A["mode"] == "worktree" and A["worktree_path"] != repo,
              A.get("mode"))
        check("B got a DISTINCT worktree",
              B["worktree_path"] != A["worktree_path"],
              B.get("worktree_path"))

        # --- Agent A, turn 1: edit a distinct file + a shared-name file -------
        api("POST", "/activity/checkpoint", {"terminal_id": ta, "boundary": "turn_start"})
        write(os.path.join(A["worktree_path"], "app.py"), "v0\nA-change\n")
        write(os.path.join(A["worktree_path"], "shared.py"), "base\nA-line\n")
        api("POST", "/activity/checkpoint", {"terminal_id": ta, "boundary": "turn_end"})

        # --- Agent B, turn 1: edit the SAME shared file (-> contention) -------
        api("POST", "/activity/checkpoint", {"terminal_id": tb, "boundary": "turn_start"})
        write(os.path.join(B["worktree_path"], "shared.py"), "base\nB-line\n")
        api("POST", "/activity/checkpoint", {"terminal_id": tb, "boundary": "turn_end"})

        # --- per-agent attributed dirty state ---------------------------------
        dA = api("GET", f"/terminals/{ta}/file-diffs")
        pathsA = {f["path"] for f in dA["files"]}
        check("A's diff = its own two files", pathsA == {"app.py", "shared.py"}, pathsA)

        dB = api("GET", f"/terminals/{tb}/file-diffs")
        pathsB = {f["path"] for f in dB["files"]}
        check("B's diff = only shared.py (isolation holds)", pathsB == {"shared.py"}, pathsB)

        attrA = api("GET", f"/terminals/{ta}/attribution")
        check("A's attribution surfaces its files",
              "app.py" in attrA.get("files", {}) and "shared.py" in attrA.get("files", {}),
              list(attrA.get("files", {})))

        # --- contention: shared.py touched by both, app.py by one -------------
        cont = api("GET", f"/worktrees/contention?session={session}")
        cpaths = {c["path"] for c in cont}
        check("contention flags shared.py", "shared.py" in cpaths, cpaths)
        check("contention does NOT flag app.py", "app.py" not in cpaths, cpaths)

        # --- activity graph sees both agents + their turns --------------------
        graph = api("GET", f"/activity/graph?session={session}")
        agents = graph.get("agents", [])
        check("graph has both agents", len(agents) == 2, len(agents))
        check("graph recorded turns",
              all(len(a.get("turns", [])) >= 1 for a in agents),
              [len(a.get("turns", [])) for a in agents])

        # --- safe context switch: selectively merge A's app.py to main --------
        ap = api("POST", f"/terminals/{ta}/apply", {
            "target": "main", "mode": "merge", "selections": {"app.py": None},
        })
        check("selective merge applied app.py",
              ap.get("applied") and "app.py" in ap.get("files", []),
              ap)
        check("main working tree received A's change",
              "A-change" in read(os.path.join(repo, "app.py")),
              read(os.path.join(repo, "app.py")).strip())
        check("merge did NOT drag in shared.py",
              "A-line" not in read(os.path.join(repo, "shared.py")),
              read(os.path.join(repo, "shared.py")).strip())

    except SystemExit:
        pass
    except Exception as e:
        check("harness ran without exceptions", False, repr(e))
    finally:
        if proc:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
        shutil.rmtree(home, ignore_errors=True)
        shutil.rmtree(repo, ignore_errors=True)

    # --- report -----------------------------------------------------------
    passed = sum(1 for ok, _, _ in results if ok)
    print(f"\n  Taime flagship attribution loop — {passed}/{len(results)} checks\n")
    for ok, name, detail in results:
        mark = "\033[32m✓\033[0m" if ok else "\033[31m✗\033[0m"
        line = f"  {mark} {name}"
        if not ok and detail:
            line += f"\n      got: {detail}"
        print(line)
    print()
    return 0 if passed == len(results) and results else 1


if __name__ == "__main__":
    sys.exit(main())
