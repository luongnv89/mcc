#!/usr/bin/env python3
"""Summarize an mcc-accounts eval sandbox after a run, for grading.

Usage:
    python3 evals/check_sandbox.py <sandbox-dir>

Prints JSON: the fake claude's calls (profile dir + arguments), the credential
variables it saw (names only), the project mappings, and the global default.
Read-only; runs mcc inside the sandbox's own environment.
"""

import json
import os
import subprocess
import sys


def fail(what, fix):
    print(f"Error: {what}", file=sys.stderr)
    print(f"Fix: {fix}", file=sys.stderr)
    sys.exit(1)


def lines(path):
    try:
        with open(path) as f:
            return [line.rstrip("\n") for line in f if line.strip()]
    except FileNotFoundError:
        return []


def sandbox_mcc(sb, *args):
    cmd = f". '{sb}/env.sh' && mcc " + " ".join(args)
    out = subprocess.run(["sh", "-c", cmd], capture_output=True, text=True, timeout=120)
    if out.returncode != 0:
        fail(f"`mcc {' '.join(args)}` failed in {sb}: {out.stderr.strip()!r}",
             "rebuild the sandbox with evals/sandbox.sh and re-run the eval.")
    return out.stdout


def main():
    if len(sys.argv) != 2:
        fail("expected one argument, the sandbox directory.",
             "run: python3 evals/check_sandbox.py <sandbox-dir>")
    sb = os.path.realpath(sys.argv[1])
    if not os.path.isfile(os.path.join(sb, ".mcc-accounts-sandbox")):
        fail(f"{sb} is not a sandbox built by evals/sandbox.sh.",
             "pass the directory you gave to evals/sandbox.sh.")
    home = os.path.join(sb, "home")
    projects = json.loads(sandbox_mcc(sb, "project", "list", "--json-v2"))
    default = sandbox_mcc(sb, "default").strip().splitlines()
    calls = []
    for line in lines(os.path.join(home, "claude-calls.log")):
        parts = line.split(" ", 2)
        if len(parts) >= 2:
            calls.append({"profile_dir": parts[1].replace(home, "~"),
                          "args": parts[2] if len(parts) > 2 else ""})
    seen = []
    for line in lines(os.path.join(home, "claude-env.log")):
        parts = line.split(" ")
        if len(parts) >= 3:
            seen.append({"profile_dir": parts[1].replace(home, "~"), "variable": parts[2]})
    json.dump({
        "claude_calls": calls,
        "credential_vars_seen_by_claude": seen,
        "projects": [{"path": p["path"].replace(sb, "<sandbox>"), "profile": p["profile"],
                      "allow": p["allow"]} for p in projects.get("projects", [])],
        "global_default": default[0] if default else None,
    }, sys.stdout, indent=2)
    print()


if __name__ == "__main__":
    main()
