#!/usr/bin/env python3
"""Summarize an mcc-accounts eval sandbox after a run, for grading.

Usage:
    python3 evals/check_sandbox.py <sandbox-dir>

Prints JSON: the fake claude's calls (profile dir + arguments), the credential
variables it saw (names only), the project mappings, and two signs that the
default profile's account changed: `auth_changes`, the `claude auth login` and
`auth logout` calls (how `mcc default PROFILE` and `--new` switch it from mcc
0.10.0), and `default_pointer`, the `default_profile` an mcc 0.7.0-0.9.x
`mcc default PROFILE` wrote to `~/.mcc/config.toml` (null when unset).
Read-only. It reads the logs and config.toml before it runs any mcc command,
and runs only `mcc project list` (inside the sandbox's own environment), which
starts no `claude`, so the output holds only what the eval run did.

The fake claude's logs hold one tab-separated line per entry (time, profile
dir, arguments or variable name), so a sandbox path with spaces parses. A line
in any other format stops with an error: rebuild the sandbox.
"""

import json
import os
import re
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


def log_entries(path):
    """(profile dir, last field) per line of a tab-separated fake claude log."""
    entries = []
    for line in lines(path):
        parts = line.split("\t", 2)
        if len(parts) < 3:
            fail(f"{path} has a line that is not tab-separated: {line!r}",
                 "rebuild the sandbox with the current evals/sandbox.sh and re-run the eval.")
        entries.append((parts[1], parts[2]))
    return entries


def default_pointer(config):
    """`default_profile` from the top level of mcc's config.toml, or None.

    mcc 0.7.0-0.9.x wrote it there for `mcc default PROFILE`; 0.10.0 removes it
    on its next command. Read as text so it works on any Python 3.
    """
    for line in lines(config):
        if line.lstrip().startswith("["):
            break
        m = re.match(r'\s*default_profile\s*=\s*"([^"]*)"', line)
        if m:
            return m.group(1)
    return None


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
    # Read everything the run left before running mcc: no mcc command here may
    # add to the logs, and mcc 0.10.0 drops an old default_profile when it runs.
    calls = [{"profile_dir": d.replace(home, "~"), "args": args}
             for d, args in log_entries(os.path.join(home, "claude-calls.log"))]
    seen = [{"profile_dir": d.replace(home, "~"), "variable": v}
            for d, v in log_entries(os.path.join(home, "claude-env.log"))]
    pointer = default_pointer(os.path.join(home, ".mcc", "config.toml"))
    projects = json.loads(sandbox_mcc(sb, "project", "list", "--json-v2"))
    json.dump({
        "claude_calls": calls,
        "credential_vars_seen_by_claude": seen,
        "projects": [{"path": p["path"].replace(sb, "<sandbox>"), "profile": p["profile"],
                      "allow": p["allow"]} for p in projects.get("projects", [])],
        # mcc 0.10.0 also runs `claude auth login --help` to see whether it takes
        # --email: that signs nothing in or out.
        "auth_changes": [c for c in calls
                         if re.match(r"auth (login|logout)(\s|$)", c["args"])
                         and "--help" not in c["args"].split()],
        "default_pointer": pointer,
    }, sys.stdout, indent=2)
    print()


if __name__ == "__main__":
    main()
