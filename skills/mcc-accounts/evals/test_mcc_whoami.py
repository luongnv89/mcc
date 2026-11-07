#!/usr/bin/env python3
"""Tests for scripts/mcc_whoami.py: it must check set-up the way mcc does
(src/main.rs `set_up()`: config.toml loads and has its `[env]` table) and
never run `mcc list` when mcc is not set up, because `mcc list` would set mcc
up (first-run setup writes `[env]` and creates ~/.mcc/logs). With tomllib it
must decide without running `mcc doctor`, which starts `claude` per profile.

Run from the repo root:
    python3 -m unittest skills/mcc-accounts/evals/test_mcc_whoami.py -v

Most cases put a fake `mcc` first on PATH. It answers `--version`, returns a
scripted `doctor --json-v2` answer (FAKE_MCC_DOCTOR), and for any other
command logs the call and does what first-run setup would do, so a run that
reaches it changes HOME. The RealMcc cases run the installed mcc and are
skipped when it is not on PATH. Every run gets its own HOME = MCC_HOME in a
temporary directory; the real ~/.mcc and ~/.claude are never touched.
"""

import hashlib
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest

SCRIPT = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                      "scripts", "mcc_whoami.py")
NOT_SET_UP = "mcc is not set up yet"
HAS_TOMLLIB = sys.version_info >= (3, 11)

# A complete, valid [env]: every required field of src/env_detect.rs EnvInfo.
ENV_OK = (
    '[env]\n'
    'os = "macos"\n'
    'os_version = "15.0"\n'
    'arch = "aarch64"\n'
    'terminal = "Apple_Terminal"\n'
    'shell_path = "/bin/zsh"\n'
    'local_bin_on_path = true\n'
    'detected_at = "2026-01-01T00:00:00Z"\n'
)

# Runs the script with tomllib hidden, as on Python < 3.11.
NO_TOMLLIB = ("import sys, runpy; sys.modules['tomllib'] = None; "
              "sys.argv = sys.argv[1:]; runpy.run_path(sys.argv[0], run_name='__main__')")

INIT_OK = {"profile": None, "check": "init", "status": "ok", "message": "mcc is set up", "fix": None}
INIT_PROBLEM = {"profile": None, "check": "init", "status": "problem",
                "message": "mcc is not set up yet (no [env] in ~/.mcc/config.toml)", "fix": "mcc init"}
CLAUDE_PROBLEM = {"profile": None, "check": "claude", "status": "problem",
                  "message": "the `claude` command was not found on your PATH", "fix": "install it"}

FAKE_MCC = r"""#!/bin/sh
cfg="$MCC_HOME/.mcc/config.toml"
case "$1" in
--version)
    echo "mcc 0.8.0"
    exit 0 ;;
doctor)
    echo "$*" >> "$FAKE_MCC_CALLS"
    case "$FAKE_MCC_DOCTOR" in
    broken)
        echo "error: $cfg is not valid mcc config: missing field \`os\`" >&2
        exit 3 ;;
    *)
        cat "$FAKE_MCC_DIR/doctor.json"
        exit "$FAKE_MCC_DOCTOR_EXIT" ;;
    esac ;;
esac
# Any other command: what mcc's log::init and first-run setup would do.
echo "$*" >> "$FAKE_MCC_CALLS"
mkdir -p "$MCC_HOME/.mcc/logs" && echo "$*" >> "$MCC_HOME/.mcc/logs/mcc.log"
grep -q '^\[env\]' "$cfg" 2>/dev/null || printf '\n[env]\nos = "fake"\n' >> "$cfg"
case "$1" in
list) echo '{"schema": 2, "profiles": []}' ;;
run) echo "using default" >&2; echo "default" ;;
esac
exit 0
"""


def snapshot(root):
    """Every path under root with its type, mode and content hash (Apple
    python's Library/Caches left out)."""
    snap = {}
    for dirpath, dirnames, filenames in os.walk(root):
        rel_dir = os.path.relpath(dirpath, root)
        if rel_dir == "Library" and "Caches" in dirnames:
            dirnames.remove("Caches")
        for name in dirnames + filenames:
            path = os.path.join(dirpath, name)
            st = os.lstat(path)
            entry = (stat.S_IFMT(st.st_mode), stat.S_IMODE(st.st_mode))
            if stat.S_ISREG(st.st_mode):
                try:
                    with open(path, "rb") as f:
                        entry += (hashlib.sha256(f.read()).hexdigest(),)
                except OSError:
                    entry += (st.st_size,)
            snap[os.path.relpath(path, root)] = entry
    return snap


def run_script(env, no_tomllib=False):
    if no_tomllib:
        cmd = [sys.executable, "-c", NO_TOMLLIB, SCRIPT]
    else:
        cmd = [sys.executable, SCRIPT]
    return subprocess.run(cmd, env=env, cwd=env["HOME"], capture_output=True, text=True, timeout=120)


def base_env(home, path):
    return {
        "PATH": path,
        "HOME": home,
        "MCC_HOME": home,
        "SHELL": "/bin/zsh",
        "PYTHONDONTWRITEBYTECODE": "1",
        "MCC_NON_INTERACTIVE": "1",
        "MCC_NO_SHELL_PROBE": "1",
    }


class FakeMccTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        tmp = os.path.realpath(self._tmp.name)
        self.home = os.path.join(tmp, "home")
        self.bin = os.path.join(tmp, "bin")
        self.calls = os.path.join(tmp, "calls.log")
        os.makedirs(os.path.join(self.home, ".mcc"))
        os.makedirs(self.bin)
        shim = os.path.join(self.bin, "mcc")
        with open(shim, "w") as f:
            f.write(FAKE_MCC)
        os.chmod(shim, 0o755)
        self.config = os.path.join(self.home, ".mcc", "config.toml")

    def tearDown(self):
        if os.path.exists(self.config):
            os.chmod(self.config, 0o644)

    def write_config(self, text):
        with open(self.config, "w") as f:
            f.write(text)

    def run_whoami(self, doctor="ok", no_tomllib=False):
        """Run the script with the fake mcc. doctor: ok, problem, noinit
        (valid JSON without the init check) or broken (exit 3, no JSON)."""
        checks = {"ok": [INIT_OK, CLAUDE_PROBLEM], "problem": [INIT_PROBLEM, CLAUDE_PROBLEM],
                  "noinit": [CLAUDE_PROBLEM], "broken": []}[doctor]
        with open(os.path.join(self.bin, "doctor.json"), "w") as f:
            json.dump({"schema": 2, "checks": checks}, f)
        env = base_env(self.home, self.bin + ":/usr/bin:/bin")
        # Exit 1 with valid JSON is normal: any problem (here `claude`) sets it.
        env.update(FAKE_MCC_DIR=self.bin, FAKE_MCC_CALLS=self.calls,
                   FAKE_MCC_DOCTOR=doctor, FAKE_MCC_DOCTOR_EXIT="1")
        return run_script(env, no_tomllib)

    def commands_run(self):
        try:
            with open(self.calls) as f:
                return [line.split()[0] for line in f if line.strip()]
        except FileNotFoundError:
            return []

    def assert_not_set_up(self, before, **kwargs):
        result = self.run_whoami(**kwargs)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn(NOT_SET_UP, result.stderr)
        self.assertNotIn("list", self.commands_run())
        self.assertNotIn("run", self.commands_run())
        self.assertEqual(snapshot(self.home), before)
        return result

    # --- tomllib (Python 3.11+): the file alone decides; no mcc subcommand runs ---
    # The fake logs every command but `--version`, so an empty log means
    # nothing but `mcc --version` ran.

    def test_config_without_env_exits_1_and_changes_nothing(self):
        if not HAS_TOMLLIB:
            self.skipTest("needs tomllib")
        self.write_config("[profiles]\n")
        # doctor is scripted to say "set up": only the [env] check can stop it.
        self.assert_not_set_up(snapshot(self.home), doctor="ok")
        self.assertEqual(self.commands_run(), [])

    def test_incomplete_env_exits_1_without_doctor(self):
        if not HAS_TOMLLIB:
            self.skipTest("needs tomllib")
        configs = {
            "empty [env]": ("[env]\n", "os, os_version, arch, terminal, shell_path, "
                                       "local_bin_on_path, detected_at"),
            "partial [env]": ('[env]\nos = "x"\narch = "y"\n',
                              "os_version, terminal, shell_path, local_bin_on_path, detected_at"),
            "wrong-typed field": (ENV_OK.replace("local_bin_on_path = true",
                                                 'local_bin_on_path = "yes"'),
                                  "local_bin_on_path"),
            "string field not a string": (ENV_OK.replace('arch = "aarch64"', "arch = 64"), "arch"),
        }
        for label, (text, bad) in configs.items():
            with self.subTest(config=label):
                if os.path.exists(self.calls):
                    os.remove(self.calls)
                self.write_config(text)
                # doctor is scripted to say "set up": only the field check can stop it.
                result = self.assert_not_set_up(snapshot(self.home), doctor="ok")
                self.assertIn(f"[env] table is incomplete (missing/invalid: {bad})", result.stderr)
                self.assertIn("mcc init --force", result.stderr)
                self.assertEqual(self.commands_run(), [])

    def test_missing_config_exits_1(self):
        for no_tomllib in (False, True):
            with self.subTest(no_tomllib=no_tomllib):
                result = self.assert_not_set_up(snapshot(self.home), doctor="ok", no_tomllib=no_tomllib)
                self.assertIn("does not exist", result.stderr)
                self.assertEqual(self.commands_run(), [])

    def test_unparseable_config_exits_1(self):
        if not HAS_TOMLLIB:
            self.skipTest("needs tomllib")
        self.write_config("[env\nos = \n")
        result = self.assert_not_set_up(snapshot(self.home), doctor="ok")
        self.assertIn("mcc init --force", result.stderr)
        self.assertEqual(self.commands_run(), [])

    def test_env_not_a_table_exits_1(self):
        if not HAS_TOMLLIB:
            self.skipTest("needs tomllib")
        self.write_config("env = 1\n")
        self.assert_not_set_up(snapshot(self.home), doctor="ok")
        self.assertEqual(self.commands_run(), [])

    def test_unreadable_config_exits_1(self):
        if not HAS_TOMLLIB:
            self.skipTest("needs tomllib")
        if os.geteuid() == 0:
            self.skipTest("root reads any file")
        self.write_config(ENV_OK)
        os.chmod(self.config, 0)
        self.assert_not_set_up(snapshot(self.home), doctor="ok")
        self.assertEqual(self.commands_run(), [])

    # --- no tomllib (Python < 3.11): mcc doctor alone decides ---

    def test_doctor_init_problem_exits_1(self):
        self.write_config("[profiles]\n")
        self.assert_not_set_up(snapshot(self.home), doctor="problem", no_tomllib=True)
        self.assertIn("doctor", self.commands_run())

    def test_doctor_without_json_exits_1(self):
        # mcc cannot load this file (EnvInfo has required fields): doctor
        # prints no JSON and its stderr names the error.
        self.write_config("[env]\nos = \"x\"\n")
        result = self.assert_not_set_up(snapshot(self.home), doctor="broken", no_tomllib=True)
        self.assertIn("not valid mcc config", result.stderr)

    def test_doctor_without_init_check_exits_1(self):
        self.write_config(ENV_OK)
        self.assert_not_set_up(snapshot(self.home), doctor="noinit", no_tomllib=True)

    # --- positive control: set up, so the script goes on to `mcc list` ---

    def test_set_up_runs_mcc_list(self):
        for no_tomllib in (False, True):
            with self.subTest(no_tomllib=no_tomllib):
                if os.path.exists(self.calls):
                    os.remove(self.calls)
                self.write_config(ENV_OK)
                result = self.run_whoami(doctor="ok", no_tomllib=no_tomllib)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("list", self.commands_run())
                self.assertEqual(json.loads(result.stdout)["here"]["profile"], "default")
                if no_tomllib or not HAS_TOMLLIB:
                    self.assertIn("doctor", self.commands_run())
                else:
                    self.assertNotIn("doctor", self.commands_run())

    def test_complete_env_with_tomllib_never_runs_doctor(self):
        if not HAS_TOMLLIB:
            self.skipTest("needs tomllib")
        self.write_config(ENV_OK)
        # doctor is scripted to fail: a run that asks it cannot reach `mcc list`.
        result = self.run_whoami(doctor="broken")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("doctor", self.commands_run())
        self.assertIn("list", self.commands_run())


@unittest.skipIf(shutil.which("mcc") is None, "mcc is not on PATH")
class RealMccTest(unittest.TestCase):
    """The installed mcc must never be set up by the script."""

    def test_not_set_up_changes_nothing(self):
        mcc_dir = os.path.dirname(os.path.realpath(shutil.which("mcc")))
        configs = {
            "no [env]": "[profiles]\n",
            "empty [env]": "[env]\n",
            "partial [env]": "[env]\nos = \"x\"\n",
            "wrong-typed [env]": ENV_OK.replace("local_bin_on_path = true", 'local_bin_on_path = "yes"'),
        }
        for label, text in configs.items():
            for no_tomllib in (False, True):
                with self.subTest(config=label, no_tomllib=no_tomllib), \
                        tempfile.TemporaryDirectory() as tmp:
                    home = os.path.realpath(tmp)
                    os.makedirs(os.path.join(home, ".mcc"))
                    with open(os.path.join(home, ".mcc", "config.toml"), "w") as f:
                        f.write(text)
                    before = snapshot(home)
                    result = run_script(base_env(home, mcc_dir + ":/usr/bin:/bin"), no_tomllib)
                    self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                    self.assertIn(NOT_SET_UP, result.stderr)
                    self.assertEqual(snapshot(home), before)


if __name__ == "__main__":
    unittest.main()
