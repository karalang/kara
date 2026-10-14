# cli/task_runner

A task runner: writes a task file, reads and parses it, orders the tasks by
dependency (Kahn, ties to declaration order), runs each as `sh -c` through
`Command` with per-task environment, feeds a task the stdout of the task it
names on a piped stdin, captures stdout, stderr and the exit code, skips
everything downstream of a failure, writes a JSON report with the derived
`Serialize` and reads it back with `Deserialize`. The mirror `task_runner.py`
uses `subprocess` and wrote `expected.out`. Needs `sh`, `printf`, `sort`,
`uniq`, `wc` on PATH.

Written 2026-10-14 by the real-world apps thread; karac at c8dad803.

## Edits from source.orig.kara to source.kara

Author errors under the v2 rules:
- `IoError.Other(m) => m` moves out of a borrowed `e` (bare params borrow):
  `m.clone()`.
- `run_task(.., input: Option[String])` hands the text to `w.write`, which
  takes `own String`: the parameter is `own`.
- `Command.arg` / `env` take `own String` (process.kara; io.md does not
  specify processes yet), so the borrowed task fields are cloned.
- `let t = tasks[i]` moves out of an index: `ref tasks[i]`.

Compiler gaps, worked around and listed in the source header:
- B-2026-10-12-7: `split` / `lines` are eager `Vec[String]`, `.iter()` before
  `map` and `next`.
- B-2026-10-14-44: `fs.write` takes `ref String`, so the `Str` const is copied.

Not worked around: B-2026-10-12-23 (`karac check` does not know
`DeserializeError`), so the entry FAILS (refused) on every mir backend. Without
check all four mir surfaces print `expected.out` exactly (measured).
