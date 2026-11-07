#!/usr/bin/env python3
"""Report which mcc profile (Claude Code account) this shell runs as, and which
profile mcc would start in a directory. Read-only: runs `mcc --version`,
`mcc list --json-v2 --no-usage --hide-email` and `mcc run --dry-run` with
MCC_NO_USAGE=1, so it never looks usage up (no token is read, even with
`[usage] live = true`) and never prints an email.

Usage:
    python3 mcc_whoami.py [--dir DIR]

Prints one JSON object on stdout:
    session          the profile whose directory is this shell's config dir
                     (CLAUDE_CONFIG_DIR, or ~/.claude when it is unset)
    in_claude_code   whether this shell runs inside Claude Code (CLAUDECODE=1)
    here             the profile `mcc run` would start in DIR (default: cwd)
    same_account     whether session and here are the same profile
    credential_env   names (never values) of variables that make `claude`
                     use another credential than the profile's own login
    notes            facts worth telling the user

Exit codes: 0 ok, 1 mcc missing, not set up or failing, 2 bad arguments,
3 mcc printed JSON this script does not understand.
"""

import json
import os
import re
import subprocess
import sys

# The variables mcc itself strips before it runs `claude` for a profile
# (src/refresh.rs STRIPPED_ENV); `mcc exec` and `mcc env` keep them.
CREDENTIAL_ENV = (
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "ANTHROPIC_BASE_URL",
)
MIN_VERSION = (0, 4, 0)  # first release with --json-v2, exec and project defaults


def fail(code, what, fix):
    print(f"Error: {what}", file=sys.stderr)
    print(f"Fix: {fix}", file=sys.stderr)
    sys.exit(code)


def run_mcc(args, cwd=None, timeout=180):
    env = dict(os.environ, NO_COLOR="1", MCC_NO_USAGE="1")
    try:
        return subprocess.run(
            ["mcc", *args], cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout
        )
    except FileNotFoundError:
        fail(1, "mcc is not on PATH.",
             "install it (brew install luongnv89/tap/mcc, or the install script at "
             "https://github.com/luongnv89/mcc), then re-run.")
    except subprocess.TimeoutExpired:
        fail(1, f"`mcc {' '.join(args)}` did not finish within {timeout}s.",
             "run it by hand to see where it stalls (add -v), or run `mcc doctor`.")


def parse_args(argv):
    target = os.getcwd()
    rest = list(argv)
    while rest:
        arg = rest.pop(0)
        if arg in ("-h", "--help"):
            print(__doc__.strip())
            sys.exit(0)
        if arg == "--dir" and rest:
            target = rest.pop(0)
        elif arg.startswith("--dir="):
            target = arg.split("=", 1)[1]
        else:
            fail(2, f"unexpected argument {arg!r}.", "usage: python3 mcc_whoami.py [--dir DIR]")
    if not os.path.isdir(target):
        fail(2, f"--dir {target!r} is not a directory.", "pass an existing directory, or omit --dir.")
    return os.path.realpath(target)


def mcc_home():
    return os.environ.get("MCC_HOME") or os.environ.get("HOME") or os.path.expanduser("~")


def expand(path, home):
    if path == "~":
        return home
    if path.startswith("~/"):
        return os.path.join(home, path[2:])
    return path


def same_path(a, b):
    return os.path.realpath(a).rstrip("/") == os.path.realpath(b).rstrip("/")


def main():
    target = parse_args(sys.argv[1:])
    home = mcc_home()
    notes = []

    version = run_mcc(["--version"])
    m = re.search(r"(\d+)\.(\d+)\.(\d+)", version.stdout or "")
    if version.returncode != 0 or not m:
        fail(1, f"`mcc --version` failed: {(version.stderr or version.stdout).strip()!r}.",
             "reinstall mcc, then re-run.")
    ver = tuple(int(x) for x in m.groups())
    if ver < MIN_VERSION:
        fail(1, f"mcc {'.'.join(map(str, ver))} is too old for this skill (needs 0.4.0+).",
             "upgrade it: brew upgrade mcc (or re-run the install script).")

    if not os.path.isfile(os.path.join(home, ".mcc", "config.toml")):
        fail(1, f"mcc is not set up yet: {home}/.mcc/config.toml does not exist.",
             "ask the user to run `mcc init` (or `mcc add NAME --clone` for a first profile); "
             "this script does not set mcc up on its own.")

    listing = run_mcc(["list", "--json-v2", "--no-usage", "--hide-email"])
    if listing.returncode != 0:
        fail(1, f"`mcc list --json-v2` exited {listing.returncode}: {listing.stderr.strip()!r}.",
             "exit 3 means ~/.mcc/config.toml or a JSON file is corrupt; run `mcc doctor` "
             "and follow the fix it prints.")
    try:
        data = json.loads(listing.stdout)
        if data.get("schema") != 2 or not isinstance(data.get("profiles"), list):
            raise ValueError(f"schema {data.get('schema')!r}")
    except (ValueError, AttributeError) as e:
        fail(3, f"`mcc list --json-v2` printed JSON this script does not understand ({e}).",
             "check `mcc --version` and the README section 'JSON output and its schema version'; "
             "update this skill if mcc moved to a new schema.")

    env_dir = os.environ.get("CLAUDE_CONFIG_DIR", "")
    session_dir = env_dir or os.path.join(os.environ.get("HOME") or home, ".claude")
    session = None
    for p in data["profiles"]:
        if same_path(expand(p.get("dir", ""), home), session_dir):
            session = p
            break

    profile_names = [p.get("name") for p in data["profiles"]]
    session_out = {
        "profile": session.get("name") if session else None,
        "config_dir": session_dir,
        "from_env": bool(env_dir),
    }
    if session:
        basename = os.path.basename(expand(session.get("dir", ""), home).rstrip("/"))
        session_out.update({
            "dir": session.get("dir"),
            "organization": session.get("organization"),
            "label": session.get("label"),
            "is_global_default": session.get("is_global_default"),
        })
        if session.get("name") != "default" and basename != session.get("name"):
            notes.append(f"profile `{session['name']}` lives in {session.get('dir')}: its folder keeps "
                         "the name it was created with, so never infer a profile from a folder name.")
    else:
        notes.append(f"this shell's config dir {session_dir} is not the directory of any mcc profile "
                     "(set by hand, or a profile mcc does not manage).")
    if not env_dir:
        notes.append("CLAUDE_CONFIG_DIR is unset, so this shell uses the default profile (~/.claude).")
    in_claude_code = os.environ.get("CLAUDECODE") == "1"
    if not in_claude_code:
        notes.append("this shell is not inside Claude Code (CLAUDECODE is not 1): there is no Claude Code "
                     "session, so report `session` as this shell's account only.")

    dry = run_mcc(["run", "--dry-run"], cwd=target, timeout=60)
    here = {"dir": target, "profile": None, "why": dry.stderr.strip()}
    if dry.returncode == 0 and dry.stdout.strip():
        here["profile"] = dry.stdout.strip().splitlines()[-1]
    else:
        here["error"] = f"`mcc run --dry-run` exited {dry.returncode}"

    same = None
    if session_out["profile"] and here["profile"]:
        same = session_out["profile"] == here["profile"]
        if not same:
            who = "this session" if in_claude_code else "this shell"
            notes.append(f"{who} runs as `{session_out['profile']}`, but `mcc run` in {target} "
                         f"would start `{here['profile']}`: they are different accounts.")

    cred = [v for v in CREDENTIAL_ENV if v in os.environ]
    if cred:
        notes.append("credential variables are set (" + ", ".join(cred) + "): a `claude` started with them "
                     "uses that credential or provider, not the profile's own login.")

    json.dump({
        "mcc_version": ".".join(map(str, ver)),
        "session": session_out,
        "in_claude_code": in_claude_code,
        "here": here,
        "same_account": same,
        "credential_env": cred,
        "profiles": profile_names,
        "notes": notes,
    }, sys.stdout, indent=2)
    print()


if __name__ == "__main__":
    main()
