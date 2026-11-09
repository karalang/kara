# uptime_monitor

An endpoint monitor in the shape of [Gatus](https://github.com/TwiN/gatus):
it reads a config of HTTP and TCP endpoints, checks them in rounds against
conditions, triggers and resolves alerts on failure and success
thresholds, appends incidents to a log, and keeps its state in a file it
replaces atomically, so a restart resumes the streaks.

**Config.** An INI file of `[endpoint <name>]` sections with `url`
(`http://`, `https://` or `tcp://`), `send` (a line a TCP check sends),
`timeout` (`300ms`, `2s`), any number of `condition`s,
`failure-threshold` (default 3), `success-threshold` (default 2) and
`enabled`. Errors are reported with their line numbers.

**Conditions** are Gatus's: `[STATUS]`, `[RESPONSE_TIME]` (ms),
`[CONNECTED]`, `[BODY]` and a JSON path into it (`[BODY].items[0].state`),
`len(..)`, the operators `== != < <= > >=` (numeric when both sides are
integers), and `pat(*glob*)`. A failure is shown with the value seen:
`[STATUS] (503) == 200`.

**Self-test.** The checker, the config parser and the state file are
tested first against fakes installed with `with_provider`: a `Network`
provider that serves canned pages and a socket, a `Clock` provider whose
every reading is 120 ms after the last (so `[RESPONSE_TIME]` is exactly
120), and an in-memory `FileSystem` provider, one of whose copies fails
every write as a full disk does (the old state file must survive).

**Live run.** An HTTP server on `127.0.0.1:47751` (a `sync` hit counter
behind a `Mutex`, shared by the handler's connection tasks) and a
line-protocol cache on `127.0.0.1:47752` misbehave on chosen hits: a 503
three times, a slow answer twice, a hang past the client's timeout twice,
a 500, a body that fails a JSON-path condition, a silent cache that trips
a socket read timeout, and a cache still loading. Eight rounds run, then a
restart from the state file and a ninth round, whose alert only fires
because the failure streak was restored. The incident log is read back and
the uptime of each endpoint printed. A response time off the real clock is
shown to the second (`~1s`), so the output does not move with the load.

Oracle: `uptime_monitor.py`, a port over real sockets (`http.server`,
`urllib`, `socket`) whose fakes are objects passed in. `expected.out` is its
output. A status of 400 or more is an `Err` carrying only a message in the
Kāra client (B-2026-11-09-44), so a 503 is `[STATUS] (0)` with the message
printed, and a real 503 cannot be told from a timeout by `[CONNECTED]`;
under the fake `Network`, whose `Response` comes back as it is, the same
503 is `[STATUS] (503)`.

## Edits from uptime_monitor.orig.kara to uptime_monitor.kara

By `karac fix`, from machine-applicable diagnostics (the diagnostics were
right):
- `[Str; 6]` is Rust's array type; `Array[Str, 6]`.
- `{pct:.1f}` is Python's spelling; `{pct:.1}` (B-2026-11-09-68).
- `Endpoint` and `Health` derive `Clone` for `.clone()`.
- `let h = hs[i]` would move out of an index; `ref hs[i]` (4 sites).
- `error: String` is given away into the outcome; `own String`.
- `*status` on a copied `i64` binding; the `*` is dropped.
- `unwrap_or(String.new())` kept a reference to a temporary; `""`.
- `line?` in `main`: `fn main() -> Result[(), AnyError]` with `Ok(())`,
  and each bare `return;` of `main` becomes `return Ok(());`
  (B-2026-11-09-67).

By hand:
- `drop(log)` to close the incident log's `BufWriter` early: Kāra has no
  `drop` function (B-2026-11-09-72), so each writer lives in a block. This
  came first: the `undefined name` stops the type checker, so the fixes
  above appear on the second `karac fix` run.
- `eps.last_mut().unwrap()` is refused by design ("write through an index
  instead"); `mut ref eps[last]`.
- A nested f-string in a call (`fs.exists(f"{state_path}.tmp")`) is
  deprecated by design.md §3; the path is bound first.
- The in-memory `FileSystem` fake lacked `exists`, which the self-test
  calls; a provider method a fake leaves out panics, naming it.
- `evaluate` and `judge` take `exact_times`, so the live run shows a
  response time to the second (the fake clock's are exact).

Compiler gaps this app found:
- A compiled program under a `FileSystem` override opened a `String` path
  built at run time as a garbage path, so the fake's state file vanished
  (B-2026-11-09-71, fixed).
- An HTTP timeout in `karac run` read "Network Error: Network Error: Error
  encountered in the status line: timed out reading response", where a
  compiled program says `<url>: request timed out after 300 ms`
  (B-2026-11-09-69, fixed).
- `File` had no `write_all` (B-2026-11-09-70, fixed).
- `karac fix`'s `main` fix left bare `return;`s that no longer checked
  (B-2026-11-09-67, fixed), and `.1f` had no fix (B-2026-11-09-68, fixed).
- No `drop(x)` (B-2026-11-09-72, open).
- `set_read_timeout` on a fake `Network` socket is `Unsupported`, so a TCP
  check that sets a timeout cannot be tested against a fake: the
  self-test's `cache` fails with `set timeout: unsupported`
  (B-2026-11-09-73, open).
