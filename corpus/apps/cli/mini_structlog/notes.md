# mini_structlog

Structured logging as structlog 24.4.0 does it: a log call builds an
event dict from the logger's bound context and the call's keywords, a
chain of processors adds to it and renders it, and the result goes to a
wrapped logger. structlog is the structured logging library many Python
services use, often behind JSON logs that a log pipeline reads. The
port covers:
- the loggers `make_filtering_bound_logger` makes, with their levels,
  aliases (`warn`, `fatal`, `msg`), `log()` and `exception()`, and the
  %-formatting of positional arguments; the generic `BoundLogger`, which
  takes any method name;
- `bind`, `unbind`, `try_unbind` and `new`, the lazy proxies
  `get_logger` and `wrap_logger` return, `configure`, `reset_defaults`
  and `cache_logger_on_first_use`;
- the processors `add_log_level`, `TimeStamper`, `MaybeTimeStamper`,
  `KeyValueRenderer`, `LogfmtRenderer`, `JSONRenderer`, `EventRenamer`,
  `format_exc_info`, `dict_tracebacks`, `set_exc_info`,
  `UnicodeEncoder`, `UnicodeDecoder`, `merge_contextvars` and the
  `ConsoleRenderer`, with and without colors;
- `PrintLogger`, `WriteLogger`, `BytesLogger`, `ReturnLogger`,
  `CapturingLogger`, `LogCapture`, `capture_logs`, and the contextvars
  functions;
- two programs:
  - an API's access log. Each request in `data/requests.csv` binds its
    request id and client address as context variables and logs a
    JSON line to `data/out/api.jsonl` (a slow one logs a second), with the authorization header
    redacted and failures carrying their exception. The program then
    reads the file back and summarizes it by level, message, status,
    latency percentile and user;
  - a task worker. It runs the tasks in `data/tasks.jsonl`, retrying
    failures, and writes a logfmt log to `data/out/worker.log`. A
    sampling processor keeps one heartbeat in four, a secret in a
    task's payload is redacted, and each task that fails for good also
    goes to stdout through a `ConsoleRenderer`.

The run writes `data/`, works through the 48 jobs in `data/jobs.jsonl`,
and then runs the two programs. `mk.py` wrote `data/`.

`oracle.py` runs the same jobs through structlog itself and wrote
`expected.out`. It uses the `structlog/` package from the 24.4.0 wheel,
unchanged. The clock is fake: each time a `TimeStamper` asks for the
time it gets the clock, and the clock moves on a quarter of a second.
Local time is UTC+2. rich, colorama and better_exceptions are kept out,
so exceptions print as plain tracebacks. The exceptions have no frames,
so a traceback is just the exception's own lines and its chain. The
colored output keeps its escape codes.

The quirks are kept. Some examples:
- `fatal()` logs at the error level, not critical, and `msg()` at info.
- `log(25, ...)` and `log(0, ...)` raise `KeyError`, since no level has
  those numbers, and a level given as a str fails comparing with an
  int.
- `exception()` outside an `except` block adds `exc_info=True`, and
  `format_exc_info` then prints `MISSING`. `dict_tracebacks` there
  raises an `AttributeError` instead.
- `new()` clears the context of the logger it is called on, not only
  the new one. On a lazy proxy it keeps the initial values. On a cached
  logger it clears what the proxy itself logs with.
- A cached logger keeps the processor list it was built with, so
  `capture_logs`, which changes that list in place, still reaches it,
  while a later `configure` does not.
- `ConsoleRenderer` adds the bright style to the `level_styles` dict it
  is given, in place, so a second renderer built from the same dict
  adds it twice, and an empty dict fails in `max()`.
- `MaybeTimeStamper(key="ts")` still looks for `timestamp`.
- `EventRenamer` raises `KeyError` for an event with no `event`.
- A dict with no renderer becomes keyword arguments, which a
  `PrintLogger` refuses one at a time; bytes print as their repr.
- JSON keys `1` and `True` are one key, which keeps the first spelling
  and the last value.

Not ported: the stdlib `logging` integration, `stack_info=True` and
`CallsiteParameterAdder` (they read the caller's frames), the async log
methods, the custom `columns` of `ConsoleRenderer`, and the threadlocal
and twisted modules.

## License

structlog is under the MIT License and the Apache License 2.0, either
one, in `LICENSE-structlog-MIT.txt` and `LICENSE-structlog-APACHE.txt`,
with `NOTICE-structlog.txt`:

    Copyright 2013 Hynek Schlawack and the structlog contributors

That covers `structlog/`.
