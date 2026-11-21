# The 3am Runbook: Debugging Crashes

You have been paged. A Kāra service crashed. This chapter is the short version
of what to do next: enough to triage the common cases without reading the whole
book at 3am.

## What a panic leaves behind

A panic is for a bug or an unrecoverable condition. When one happens, Kāra:

1. writes a **panic record** to stderr,
2. runs the program's custom panic handler, if it has one,
3. ends the process with exit code **101**.

No `Drop` body, `defer` or `errdefer` runs, in the panicking task or anywhere
else. Nothing is written to disk: there is no crash file. So the first thing to
find is the process's **stderr**, in your service manager's log, the container
log, or wherever the supervisor sends it. Exit code 101 tells you it was a
panic; an ordinary error exit from `main` is code 1.

## First move: read the record

Here is a small program that indexes past the end of a vector:

```kara,exit=101
fn dashboard(ids: Vec[i64], k: i64) -> i64 {
    ids[k]
}

fn main() {
    let ids: Vec[i64] = [10, 20, 30];
    println(f"{dashboard(ids, 99)}");
}
```

Its stderr is:

```text
runtime error: index out of bounds
  at main.kara:2:5
```

and its exit code is 101. The record has three parts worth reading in order:

- **The kind**: `index out of bounds`. An index past the end is a logic bug (a
  length assumption that failed), not an outage. The kinds are `explicit`
  (`panic(...)`), `index_out_of_bounds`, `overflow`, `div_by_zero`, `unwrap`,
  `borrow_conflict`, `assertion`, `alloc_failure`, `unreachable` and `deadlock`.
- **The location**: `main.kara:2:5`. Go straight there. For a panic raised
  inside a standard-library function such as `unwrap()`, the location is *your*
  call, not a line in the library.
- **The error-return trace**, when there is one (next section).

**Verdict:** logic bug at `main.kara:2`. File a bug, fix the index.

## Worked example: an `unwrap` at the end of a `?` chain

The most common production panic is an `unwrap()` on an error that travelled up
through several `?`s. The record shows the path it took:

```kara,exit=101
fn parse_port(s: Str) -> Result[i64, String] {
    match i64.parse(s) {
        Ok(n) => Ok(n),
        Err(_) => Err(f"bad port: {s}"),
    }
}

fn load(s: Str) -> Result[i64, String] {
    let p = parse_port(s)?;
    Ok(p + 1)
}

fn main() {
    let port = load("80x").unwrap();
    println(f"{port}");
}
```

```text
runtime error: called unwrap() on Err
  at trace.kara:14:16
Error return trace:
  trace.kara:9:13  parse_port(s)?
  trace.kara:14:16
```

**How to read it:**

- **`called unwrap() on Err` at line 14**: the panic itself. `main` assumed the
  configuration would load.
- **`Error return trace`**: every `?` the error passed through, innermost first.
  The error was made in `parse_port`, left it through the `?` at line 9, and
  reached the `unwrap()` at line 14. Debug builds record it; a panic with no `?`
  behind it, like the first example, prints none.

**Verdict:** bad input (`"80x"`), and a `main` that panics on it. Fix the
configuration now; then make `main` report the error and exit 1 instead of
unwrapping (`fn main() -> Result[(), String]`).

## Worked example: a panic in a parallel branch

```kara,exit=101
fn fetch(ids: Vec[i64], k: i64) -> i64 { ids[k] }

fn main() {
    let ids: Vec[i64] = [1, 2, 3];
    let (a, b) = par { fetch(ids, 0), fetch(ids, 7) };
    println(f"{a} {b}");
}
```

```text
runtime error: index out of bounds
  at par.kara:1:42
```

A panic in any branch of a `par` block, or in any task, ends the whole process.
The sibling branches don't get to finish, and nothing they hold is dropped.
That has two consequences for triage:

- **The location is the whole story.** There is no "cancelled sibling" to chase:
  the record names the one place that panicked.
- **Check for half-done work outside the process.** No cleanup ran, so a
  sibling that was in the middle of writing a file or a database row may have
  left it partial. Design writes so a restart can repair them (write to a
  temporary name and rename, or use a transaction).

An *error* in a branch is different: `?` ends that branch normally, its
`defer`s run, and the function returns the source-earliest error once every
branch is done ([Chapter 14](./ch14-concurrency.md#parallel-failure)).

## Getting the record as JSON

Under `karac run` and `karac test` with `--output=jsonl` (or `--output=json`),
the panic record arrives as structured events instead of text, so a tool can
group panics without parsing:

```bash
karac run --output=jsonl trace.kara
```

```text
{"type":"runtime_error","message":"called unwrap() on Err","location":{"file":"trace.kara","line":14,"col":16},"kind":"unwrap"}
{"type":"error_return_trace","frames":[{"file":"trace.kara","line":9,"column":13,"expr":"parse_port(s)?","context":null},{"file":"trace.kara","line":14,"column":16,"expr":null,"context":null}],"truncated":false}
```

The fields are part of the language contract and stable within a major
version. Tools deduplicate on the `kind` and the `location`: two panics with
the same pair are the same bug, even across builds. A built binary run directly
writes only the text form.

## Running code on a panic

A program can define one panic handler. It runs after the record is written and
before the process exits; it can't resume execution. Use it to feed a watchdog,
release a hardware handle, or send one last line of telemetry:

```kara,exit=101
#[panic_handler]
fn on_panic(info: PanicInfo) {
    eprintln(f"PANIC: {info.message} at {info.location}");
}

fn main() {
    let v: Vec[i64] = Vec.new();
    println(f"{v[3]}");
}
```

```text
runtime error: index out of bounds
  at hand.kara:8:16
PANIC: index out of bounds at hand.kara:8:16
```

A panic inside the handler ends the process at once, still with code 101.

## Common patterns

| What the record shows | Where to look first |
|---|---|
| `unwrap` with an error-return trace | The innermost frame of the trace: that is where the error was made. The `unwrap` is only where it was ignored. |
| `index out of bounds`, no trace | A length assumption at the location: an empty result indexed, an off-by-one. |
| `overflow` or `div_by_zero` | Arithmetic on a value from input; check its range where it enters the program. |
| `borrow_conflict` | Two live borrows of a `shared` value at once; the location is the second one. |
| `deadlock` | Every task is waiting on a channel no running task can serve: a missing `send`, or a `Sender` kept alive by mistake. |

For the field-by-field contract, see design.md § Panic record. This runbook is
deliberately the short version.
