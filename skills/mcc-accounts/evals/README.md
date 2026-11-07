# mcc-accounts evals

Six prompts in `evals.json`: four happy paths (session vs project account, usage and login expiry, a headless run as a named account with a stray API key, mapping a repo), one edge case (an account at its limit, where the right answer is to wait or hand the choice to the user), and one negative trigger (gh accounts).

## Never against real accounts

Every run gets its own sandbox, so runs cannot see each other's changes, and the fake `claude` logs belong to one run:

```sh
sh evals/sandbox.sh <workspace>/<eval>/<config>/sandbox
```

The sandbox sets `MCC_HOME` and `HOME` to its own folder, puts a fake `claude` first on `PATH` (no network, no quota), and seeds four profiles: `default`, `job` (at its 5-hour limit), `lab` (login ends in about a day), and `personal` (renamed from `side`). `projects/app` is mapped to `job` and allows `lab`. The simulated session runs as `personal`. The names collide with no real profile, so a command that misses the sandbox fails with exit 2 instead of touching a real account.

Each eval names its working directory (`setup.cwd`) and env file (`setup.env_file`). Give the run this preamble:

```
Simulated environment. Run EVERY shell command as:
  . <sandbox>/<env_file> && cd <sandbox>/<cwd> && <command>
Never run mcc or claude outside this prefix. The working directory for the task is <sandbox>/<cwd>.
```

## Grading

1. Run `python3 evals/check_sandbox.py <sandbox>` after each run. It prints the fake `claude` calls, the credential variables `claude` saw (names only), the project mappings, and the global default. Grade the `check_sandbox.py` assertions from this output.
2. Grade the other assertions from the run's final message and its command transcript.
3. Exclude the negative trigger from the with-skill vs baseline comparison; it runs with the skill only.

## Understanding criteria

Every eval that applies the skill carries four understanding assertions, graded from the delivered output:

| Criterion | Pass when |
| --- | --- |
| Main result is findable | The opening lines state the result and whether it is complete, partial, or blocked |
| Facts and assumptions are separated | Verified claims name the command behind them; inferences and untested points carry a label |
| Claims are traceable | Each material claim points to command output that supports that claim's scope |
| Next decision is clear | The output names the approval or choice needed, or says "No approval needed", and names any step the user must run |

Agent grading checks only what is observable. Human understanding is confirmed only by the reviewer's answers in the eval viewer; when no human review took place, report understanding as unconfirmed.

## Script tests

`test_mcc_whoami.py` checks that `scripts/mcc_whoami.py` uses mcc's own set-up test (config.toml loads and has its `[env]` table) and exits 1 without running `mcc list` or changing any file when mcc is not set up. It uses a fake `mcc` and a temporary `HOME`; the cases that run the installed mcc are skipped when it is not on `PATH`. CI does not run it. From the repo root:

```sh
python3 -m unittest skills/mcc-accounts/evals/test_mcc_whoami.py -v
```

On Python < 3.11 (no `tomllib`) the cases that read the file directly are skipped; the `mcc doctor` path still runs.
