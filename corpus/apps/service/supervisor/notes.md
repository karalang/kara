# supervisor: a process supervisor

This app is a small supervisord. It reads an INI file of programs, starts
each one as a real child process with its stdout and stderr piped, polls
them with `try_wait`, and applies each program's restart policy when one
exits:

- `autorestart`: `true`, `false`, or `unexpected` (restart only on an exit
  code not listed in `exitcodes`);
- `startretries` with a linear backoff (`backoff_ms` times the retry
  number); after the last retry the program is FATAL;
- `max_runtime_ms`: a child that runs too long is killed;
- `environment`: `KEY=VALUE` pairs with quoted values, plus
  `SUPERVISOR_PROGRAM`, passed with `Command.env`;
- `stdout_logfile` with size-based rotation to `.1`, `.2` and so on,
  through `fs.metadata`, `fs.rename` and `fs.remove_file`;
- a missing binary is a failed start, and is retried like an exit.

Commands are split with POSIX shell quoting rules; the mirror checks the
splitter against Python's `shlex`. Unknown keys are warnings; bad values and
a program without a command are errors.

The supervisor keeps a virtual clock. It sleeps `poll_ms` between polls and
adds `poll_ms` to the clock, so backoff and timeouts count polls and the
output does not depend on how fast the machine is. Each program's events are
printed together at the end, because the order in which different children
finish is up to the OS.

Oracle: `supervisor.py`, a port using `subprocess.Popen`, `poll()` and
`kill()`. `expected.out` is its output.

## Edits from supervisor.orig.kara to supervisor.kara

Author errors (the diagnostics were right):
- `match p.child { Some(c) => c }` moved the `Child` out of a borrowed
  field. `Child` has no `clone`, although process.kara's comment calls it
  "a plain copyable struct" (it holds only the pid). The app uses
  `p.child.take()` and puts it back while the child is still running.

Compiler gaps, worked around and listed in the source header:
- **B-2026-10-14-53** also covers `fs.exists`, `fs.remove_file`,
  and `fs.rename`. Even a `Str` loop element passed to
  `fs.exists(name)` counts as moved. The app clones each path.
- **B-2026-10-15-10:** there is no `Clock.now()`, which is why the clock is
  virtual.

Not filed, because there is no process spec yet (B-2026-10-15-6): after
`child.wait()`, `child.stdout()` returns `None`, so output has to be taken
before waiting. Rust's `Child::wait` keeps the output handles.

Written 2026-10-15 by the real-world apps thread; karac at f32a0eebf2.
