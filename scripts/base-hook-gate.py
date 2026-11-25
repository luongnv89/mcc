"""scripts/base-hook-gate.py: judge a change to the hooks with the base branch's hooks.

Usage: python scripts/base-hook-gate.py --base <sha> <stage> [<stage> ...]

Does nothing unless the diff from <base> to HEAD touches .pre-commit-config.yaml,
a workflow, or a hook script (anything under scripts/, or a file a local hook's
entry or args names). Then it fails on every hook id the base config runs in a
given stage that the PR's config no longer runs there, restores the base's config
and the scripts its local hooks name, re-runs each stage over the PR diff, then
puts the PR's files back for the guard's own run.
The CI guard reads this file from the base branch, so a PR cannot edit it either.
"""

import shlex
import subprocess
import sys

import yaml  # installed with pre-commit

sys.stdout.reconfigure(line_buffering=True)  # keep our lines in order with pre-commit's

CONFIG = ".pre-commit-config.yaml"
LEGACY = {"commit": "pre-commit", "push": "pre-push", "merge-commit": "pre-merge-commit"}


def git(*args):
    return subprocess.run(["git", *args], capture_output=True, text=True)


def config_at(rev):
    shown = git("show", f"{rev}:{CONFIG}")
    return (yaml.safe_load(shown.stdout) or {}) if shown.returncode == 0 else None


def hooks(cfg):
    for repo in cfg.get("repos") or []:
        for hook in repo.get("hooks") or []:
            yield repo.get("repo"), hook


def ids_in_stage(cfg, stage):
    default = cfg.get("default_stages")
    found = set()
    for _, hook in hooks(cfg):
        stages = hook.get("stages") or default
        if stages is None or stage in {LEGACY.get(s, s) for s in stages}:
            found.add(hook["id"])
    return found


def named_files(cfg):
    names = set()
    for repo, hook in hooks(cfg):
        if repo == "local":
            for arg in [*shlex.split(str(hook.get("entry", ""))), *map(str, hook.get("args") or [])]:
                names.add(arg.removeprefix("./"))
    return names


args = sys.argv[1:]
if args[:1] != ["--base"] or len(args) < 3:
    sys.exit("usage: base-hook-gate.py --base <sha> <stage> [<stage> ...]")
base, stages = args[1], args[2:]

base_cfg = config_at(base)
if base_cfg is None:
    print(f"base-hook-gate: {base} has no {CONFIG}; nothing to compare.")
    sys.exit(0)
head_cfg = config_at("HEAD") or {}

diff = git("diff", "--name-only", "-z", base, "HEAD")
if diff.returncode != 0:
    sys.exit(f"base-hook-gate: git diff failed: {diff.stderr.strip()}")
scripts = named_files(base_cfg) | named_files(head_cfg)
gated = [
    p for p in diff.stdout.split("\0") if p and (
        p == CONFIG or p.startswith((".github/workflows/", "scripts/")) or p in scripts
    )
]
if not gated:
    print("base-hook-gate: no hook config, workflow or hook script changed; skipped.")
    sys.exit(0)
print("base-hook-gate: the PR changes " + ", ".join(gated))

failed = False
for stage in stages:
    for hook_id in sorted(ids_in_stage(base_cfg, stage) - ids_in_stage(head_cfg, stage)):
        print(f"::error::The base branch runs hook '{hook_id}' on {stage}; this PR removes it.")
        failed = True

# The base's config and the scripts its local hooks name; every other file keeps
# the PR's content, so the base hooks still check it. Files only: a directory
# such as `src` in `mypy src` keeps the PR's content too. A file the base lacks
# has no base version to restore.
restore = [p for p in [CONFIG, *sorted(named_files(base_cfg))]
           if git("cat-file", "-t", f"{base}:{p}").stdout.strip() == "blob"]


def checkout(source):
    if restore and subprocess.call(["git", "restore", f"--source={source}", "--staged", "--worktree", "--", *restore]):
        sys.exit(f"base-hook-gate: could not restore the hook files from {source}.")


checkout(base)
try:
    for stage in stages:
        print(f"base-hook-gate: {stage} stage, base branch's hooks, PR diff")
        run = ["pre-commit", "run", "--hook-stage", stage, "--from-ref", base, "--to-ref", "HEAD"]
        failed |= subprocess.call(run) != 0
finally:
    checkout("HEAD")  # the guard's own run, next, uses the PR's config
sys.exit(1 if failed else 0)
