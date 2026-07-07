# Mend harness

Driver for the Mend demo loop. See [`../README.md`](../README.md) for
the demo's overall thesis and current scope.

## Quick start

```sh
# from the repo root — pick any example under examples/mend/examples/

# live mode (default, uses your Claude Code login)
python3 examples/mend/harness/mend.py welcome_emails
python3 examples/mend/harness/mend.py order_status
python3 examples/mend/harness/mend.py user_lookup

# dry-run mode (deterministic, no API call)
python3 examples/mend/harness/mend.py welcome_emails --dry-run
```

The harness reads `task.md`, sends it through the LLM (or replays
canned responses), runs `karac check`, applies `karac fix` where
machine-applicable, feeds remaining diagnostics back, and writes the
per-iteration transcript under `examples/mend/runs/<timestamp>/`.

The harness invokes `karac` via `cargo run --quiet --release --bin
karac --`. Set `MEND_KARAC_BIN=/path/to/karac` to skip the cargo step
once you have a built binary — saves ~80 ms per call after warm-up.

## Live mode (default)

```sh
python3 examples/mend/harness/mend.py welcome_emails
```

Live mode subprocesses `claude -p` (Claude Code's non-interactive mode).
Auth is inherited from your existing Claude Code login (keychain /
OAuth), so the demo runs on your Max subscription with **no separate
API key and no incremental cost**. Each iteration is a fresh
invocation; the conversation transcript is reconstructed inline in the
follow-up prompt rather than via session state.

Flags passed to the subprocess:

- `-p` non-interactive mode, prompt via stdin
- `--tools ""` disables tool use (Read / Edit / Bash) — we want pure
  text generation only; the LLM should never touch the working directory
- `--system-prompt <…>` replaces the default Claude Code system prompt
  with `system_prompt.md` (the Mend-specific primer)
- `--output-format text` plain text response on stdout

## Output layout

```
examples/mend/runs/<timestamp>/
├── current.kara                  the working file (last iteration)
├── final.kara                    the converged source (if loop succeeded)
└── iter_NNN/
    ├── response.kara             the LLM's reply this iteration
    ├── response.note.txt         dry-run only — annotation from canned data
    ├── diagnostics.json          karac check output BEFORE karac fix
    ├── diagnostics.after_fix.json  same, AFTER karac fix (if fix ran)
    ├── fix.log                   karac fix human-readable output
    ├── followup.txt              feedback prompt sent to the LLM next iteration
    └── outcome.txt               "clean-on-arrival" | "clean-after-karac-fix"
```

## Scoring the runs

`mend.py` produces one transcript per run; `mend_score.py` aggregates
**all** of them into the measured numbers the AI-first pitch needs. It
reads the transcripts only — never runs `karac` or an LLM.

```sh
python3 examples/mend/harness/mend_score.py            # score runs/, print report
python3 examples/mend/harness/mend_score.py --json      # machine summary on stdout
python3 examples/mend/harness/mend_score.py path/to/runs # score an alternate dir
```

It writes `results.jsonl` (one flat record per run) and `summary.json`
into the runs directory (both gitignored), and prints a report:

- **Outcome, split by who closed the loop.** The key distinction the
  raw `outcome.txt` tag hides: `mend.py` writes `clean-on-arrival`
  whenever a fresh LLM response checks clean, which at iteration 0 means
  the LLM nailed it but at a later iteration means it *fixed the code
  from the diagnostics it was fed*. The scorer separates these by the
  iteration convergence happened at:
  - `clean-on-arrival` — LLM correct first try (ergonomics, not the loop)
  - `fixed-by-karac` — `karac fix` closed the build (**the wedge**)
  - `fixed-by-llm` — LLM rewrote from a prose diagnostic (no human)
  - `non-converged` — hit `--max-iterations`
- **Headline rates** — machine-fix rate (the wedge), agent-resolved rate
  (converged with no human, by fix *or* actionable diagnostic),
  clean-on-arrival rate, non-converged rate.
- **Fix mechanics** — diagnostics offering a `replacement`, fixes
  applied, fixes resolved, fix precision (resolved / applied), and
  whether any fix introduced a new error.
- **Gap ledger** — every error code the LLM hit, ranked worst-covered
  first, flagged by whether that code has *ever* carried a
  machine-applicable fix. Codes with no coverage are the ranked backlog
  for making the compiler more agent-fixable: give the diagnostic a
  `replacement`, or make it precise enough to fix from prose.

The gap ledger is the point: it turns each run batch into a to-do list
for the compiler, not just a demo you watch.

## Adding a new example

A example is a directory under `examples/mend/examples/<name>/`
containing:

| File                    | Required | Role                                                              |
|-------------------------|----------|-------------------------------------------------------------------|
| `task.md`               | yes      | The natural-language prompt fed to the LLM.                       |
| `solution.kara`         | yes      | Reference solution that compiles clean. Used for documentation, not by the harness directly. |
| `canned_responses.json` | dry-run  | List of LLM responses for `--dry-run` mode.                       |
| `python_buggy.py`       | optional | Same task in Python; demonstrates the bug Kāra catches.           |
| `notes.md`              | optional | Pedagogy: what Python misses, what Kāra catches.                  |

## Caveats

- Slice 0 only — the harness has no retry logic, no rate-limit handling,
  and no resumable transcripts. It runs once per invocation.
- The LLM's output is written to disk verbatim. If the LLM wraps its
  output in markdown fences or adds prose, the build will fail at parse
  time and the harness will surface that as a diagnostic; it does not
  attempt to strip fences.
- `karac fix` is invoked on the same path the LLM wrote to. There's no
  staging; if a fix is wrong, the corrupted file is what the next
  iteration sees.
