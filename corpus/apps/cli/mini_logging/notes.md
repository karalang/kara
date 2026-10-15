# mini_logging

A web service's logging, from startup to shutdown, as Python 3.13's
`logging` package does it. Almost every Python service configures its
logs with `basicConfig` or `dictConfig`, writes them to the console and
to rotating files, and reloads that configuration while it runs. The
program ports the parts of `logging`, `logging.handlers` and
`logging.config` a service uses, and runs `data/run.txt`, the script
of what one service logs, through them.

The script covers:
- **before any configuration**: the last-resort handler, which shows
  only WARNING and above, as the bare message on stderr.
- **basicConfig**: a second call that silently does nothing, and
  `force=True`, which replaces the handler.
- **messages**: `%`-formatting of the arguments with flags, widths,
  precision, `%r` and `%x`/`%X`/`%o`, and the `KeyError` for an `extra`
  field that would replace a record attribute.
- **dictConfig** from `data/config/base.json`:
  - formatters in all three styles (`%`, `{` and `$`), with a `datefmt`,
    `defaults` for a missing `extra` field, and `relativeCreated`;
  - a name filter on the console handler;
  - a RotatingFileHandler that rolls over a log a previous run left
    behind and keeps two backups;
  - a MemoryHandler that flushes to a file at capacity or at ERROR, and
    is configured after its target;
  - every logger that existed before the config is disabled, including
    third-party ones.
- **runtime changes**: `setLevel`, `logging.disable`, a custom level
  name, and levels with no name.
- **a reload** from `verbose.json` with `disable_existing_loggers` set
  to false. Children of a configured logger are reset (and stay
  disabled), and the old MemoryHandler flushes on close.
- **fifteen broken reloads**, each followed by more logging. Each one
  prints dictConfig's error and the exceptions it was raised from.
  Each one also leaves behind whatever part of it was already applied:
  - the old handlers are closed, but files reopen and streams keep
    writing;
  - a file handler made before the failing one truncates its file;
  - a logger loses its handlers part way.
- **shutdown**: the handlers close newest first, so a MemoryHandler
  flushes into a handler that is still open. Then the program prints
  each file in `data/logs`.

The program prints the logger tree, with levels, effective levels,
handlers and which handlers are closed, at six points.

Not supported: exception and stack info in records, `LoggerAdapter`,
`QueueHandler`, the network and syslog handlers,
`TimedRotatingFileHandler`, `fileConfig`, incremental dictConfig, `()`
factories, and the caller fields (`lineno`, `funcName`, `pathname`).
The clock is a fake one that the script advances, and times are shown
in UTC. stderr is shown on stdout with a `stderr| ` tag.

**Oracle.** `expected.out` is the output of `oracle.py`, which runs
the same script through Python 3.13's real `logging`. The oracle puts
`time.time_ns` on the fake clock and sets `Formatter.converter` to
`time.gmtime`. Run it from this directory after `mk.py` writes `data/`.

## Edits from mini_logging.orig.kara to mini_logging.kara

Compiler gaps: none.

Author errors:
- `wrap` was written with `mut chain: Vec[String]`, which is Rust
  syntax. `karac check` said so and named the fix (`chain: own ...`,
  then `let mut chain = chain;`).
- `new_formatter` stores its `fmt` and `datefmt` parameters, so both
  have to be `own`.
- The JSON enum was named `Json`, which shadows the prelude type
  (a warning). It is now `JValue`.

After that, the output matched `expected.out` on `__mir-run`,
`__mir-llvm-run` and `__mir-llvm-asan` on the first run.

Written 2026-10-15 by the real-world apps thread; karac at eb605b2e5b.
