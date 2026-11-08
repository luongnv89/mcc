#!/usr/bin/env python3
"""Tests for evals/check_sandbox.py and the fake `claude` logs it reads.

The fake `claude` that evals/sandbox.sh installs writes one tab-separated
line per call (time, profile dir, arguments) and per credential variable it
sees (time, profile dir, variable name). A sandbox path may contain spaces, so
check_sandbox.py must split on tabs, and a log in any other format must stop
grading with an Error/Fix instead of a wrong summary.

Run from the repo root:
    python3 -m unittest skills/mcc-accounts/evals/test_check_sandbox.py -v

FakeSandboxTest builds a minimal sandbox by hand: the marker file, an env.sh
that puts a fake `mcc` first on PATH (check_sandbox.py asks it for the project
mappings and the global default), and hand-written logs. RealSandboxTest builds
a real sandbox with evals/sandbox.sh and the installed mcc, and is skipped when
mcc is not on PATH. Everything lives in a temporary directory whose path
contains a space; the real ~/.mcc and ~/.claude are never touched.
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

EVALS = os.path.dirname(os.path.abspath(__file__))
CHECK = os.path.join(EVALS, "check_sandbox.py")
SANDBOX_SH = os.path.join(EVALS, "sandbox.sh")

FAKE_MCC = r"""#!/bin/sh
case "$1" in
project) echo '{"schema": 2, "projects": []}' ;;
default) echo default ;;
*) echo "fake mcc: unexpected $*" >&2; exit 2 ;;
esac
"""


def run_check(sb):
    return subprocess.run([sys.executable, CHECK, sb], capture_output=True, text=True, timeout=120,
                          env={"PATH": "/usr/bin:/bin", "PYTHONDONTWRITEBYTECODE": "1"})


class FakeSandboxTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.sb = os.path.join(os.path.realpath(self._tmp.name), "eval sandbox")
        self.home = os.path.join(self.sb, "home")
        fake_bin = os.path.join(self.sb, "bin")
        os.makedirs(self.home)
        os.makedirs(fake_bin)
        open(os.path.join(self.sb, ".mcc-accounts-sandbox"), "w").close()
        with open(os.path.join(fake_bin, "mcc"), "w") as f:
            f.write(FAKE_MCC)
        os.chmod(os.path.join(fake_bin, "mcc"), 0o755)
        with open(os.path.join(self.sb, "env.sh"), "w") as f:
            f.write(f"export PATH='{fake_bin}:/usr/bin:/bin'\n")

    def write_log(self, name, *lines):
        with open(os.path.join(self.home, name), "w") as f:
            f.write("".join(line + "\n" for line in lines))

    def test_tab_separated_logs_under_a_spaced_path(self):
        job = os.path.join(self.home, ".mcc", "profiles", "job")
        self.write_log("claude-calls.log",
                       f"07:00:00\t{job}\t-p hi",
                       f"07:00:01\t{self.home}/.claude\t",  # argless: a session start
                       f"07:00:02\t{job}\t-p a\tb")  # a tab inside the arguments stays there
        self.write_log("claude-env.log", f"07:00:00\t{job}\tANTHROPIC_API_KEY")
        result = run_check(self.sb)
        self.assertEqual(result.returncode, 0, result.stderr)
        out = json.loads(result.stdout)
        self.assertEqual(out["claude_calls"], [
            {"profile_dir": "~/.mcc/profiles/job", "args": "-p hi"},
            {"profile_dir": "~/.claude", "args": ""},
            {"profile_dir": "~/.mcc/profiles/job", "args": "-p a\tb"},
        ])
        self.assertEqual(out["credential_vars_seen_by_claude"],
                         [{"profile_dir": "~/.mcc/profiles/job", "variable": "ANTHROPIC_API_KEY"}])
        self.assertEqual(out["global_default"], "default")

    def test_space_separated_log_fails_with_a_fix(self):
        # The format the fake wrote before logs became tab-separated.
        old = f"07:00:00 {self.home}/.mcc/profiles/job -p hi"
        for name in ("claude-calls.log", "claude-env.log"):
            with self.subTest(log=name):
                for log in ("claude-calls.log", "claude-env.log"):
                    self.write_log(log)
                self.write_log(name, old)
                result = run_check(self.sb)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertEqual(result.stdout, "")
                self.assertIn(f"Error: {os.path.join(self.home, name)}", result.stderr)
                self.assertIn("Fix: rebuild the sandbox with the current evals/sandbox.sh", result.stderr)


class SandboxPathTest(unittest.TestCase):
    def test_sandbox_sh_refuses_a_tab_or_newline_in_the_path(self):
        with tempfile.TemporaryDirectory() as tmp:
            for label, char in (("tab", "\t"), ("newline", "\n")):
                with self.subTest(char=label):
                    sb = os.path.join(tmp, f"eval{char}sandbox")
                    # No mcc on PATH: the path check must come first.
                    result = subprocess.run(["sh", SANDBOX_SH, sb], capture_output=True, text=True,
                                            timeout=60, env={"PATH": "/usr/bin:/bin", "HOME": tmp})
                    self.assertEqual(result.returncode, 1, result.stderr)
                    self.assertIn("Error: ", result.stderr)
                    self.assertIn("tab or newline", result.stderr)
                    self.assertFalse(os.path.lexists(sb))


@unittest.skipIf(shutil.which("mcc") is None, "mcc is not on PATH")
class RealSandboxTest(unittest.TestCase):
    def test_spaced_sandbox_path_end_to_end(self):
        with tempfile.TemporaryDirectory() as tmp:
            sb = os.path.join(os.path.realpath(tmp), "eval sandbox")
            env = {"PATH": os.environ["PATH"], "HOME": os.path.realpath(tmp), "SB": sb}
            build = subprocess.run(["sh", SANDBOX_SH, sb], capture_output=True, text=True,
                                   timeout=120, env=env)
            self.assertEqual(build.returncode, 0, build.stderr)
            for script in ('. "$SB/env.sh" && mcc exec job -- claude -p hi',
                           '. "$SB/env-alt.sh" && mcc exec job -- claude -p hi',
                           '. "$SB/env.sh" && mcc exec job -- claude -p "$(printf \'line one\\nline two\')"'):
                run = subprocess.run(["sh", "-c", script], capture_output=True, text=True,
                                     timeout=120, env=env)
                self.assertEqual(run.returncode, 0, run.stderr)
            result = run_check(sb)
            self.assertEqual(result.returncode, 0, result.stderr)
            out = json.loads(result.stdout)
            self.assertEqual(out["claude_calls"], [
                {"profile_dir": "~/.mcc/profiles/job", "args": "-p hi"},
                {"profile_dir": "~/.mcc/profiles/job", "args": "-p hi"},
                {"profile_dir": "~/.mcc/profiles/job", "args": "-p line one line two"},
            ])
            self.assertEqual(out["credential_vars_seen_by_claude"],
                             [{"profile_dir": "~/.mcc/profiles/job", "variable": "ANTHROPIC_API_KEY"}])
            self.assertEqual(out["projects"], [{"path": "<sandbox>/projects/app", "profile": "job",
                                                "allow": ["lab"]}])
            self.assertEqual(out["global_default"], "default")


if __name__ == "__main__":
    unittest.main()
