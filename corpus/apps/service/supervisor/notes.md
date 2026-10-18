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

## Library migration, 2026-10-18

Thread A's a0ca700228 and acd593fc77 changed the library to match docs/library: a number's `parse` returns `Result[T, ParseError]`, and `split`, `lines`, `split_whitespace` and `trim` give `Str` views (`split` and `lines` are now lazy iterators). Arguments forwarded to a `mut ref` parameter are now marked too. `karac fix` made the edits, with one more by hand: `karac fix` wrote `match parsed.ok()` and a later `parsed.unwrap_or(0)` then used a moved value; the `.ok()` moved to the `let` (B-2026-10-18-35). The app matches expected.out on all four MIR backends again (karac at a4b0967495).
