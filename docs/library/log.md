# Logging

`std.log` is the standard library's production logging: always present, with severity levels, structured fields and a configurable backend.

Logging is observability, so it is transparent to the effect system ([design.md §12](../design.md#observability)). The functions of `std.log` are declared with no effects: a call never changes a function's effect set and never conflicts with anything, so adding a log line cannot change a signature or forbid parallelism. The standard library marks them with an internal attribute that user code cannot write, as it does with `#[compiler_builtin]`. Because log output does not conflict, lines from concurrent branches may interleave; within one branch they follow source order.

The compiler's own parallelization (statement and loop auto-par) is invisible to logging. A task it creates is not a task to the program: its records carry the enclosing task's `task_id` and span, not a new id, and they reach the backend in the order the sequential program would emit them, held per auto-par task until every task before it in statement order has finished (the first unfinished one's records go on as it logs them). Records leave on standard output or standard error interleaved with the program's own console writes, and a reader has to be able to trust that order, so auto-par keeps a statement that logs in order against one that writes standard output or standard error or calls `init`, and against nothing else; a statement that does both stays where it was written. When a panic is about to end the program, every held record goes to the backend, in statement order, before the panic is reported; a task still running gives what it logged so far. A task the program starts itself, with `par` or a `TaskGroup`, is a real task, with its own id and no order relative to its siblings.

## Severity levels

`debug`, `info`, `warn` and `error` each take a message followed by zero or more `key: value` structured fields. The field values are borrowed, so logging a value does not move it.

```kara
import std.log.{debug, info, error};

fn handle_request(req: Request) -> Result[Response, AuthError] {
    info("request received", method: req.method, path: req.path);
    let user = authenticate(req)?;
    debug("authenticated", user_id: user.id);

    match process(req) {
        Ok(resp) => {
            info("request handled", status: resp.status);
            Ok(resp)
        }
        Err(e) => {
            error("request failed", err: e.message(), status: 500);
            Ok(Response.internal_error())
        }
    }
}
```

### Fields

A field is written `key: value` after the message. It looks like a named argument ([design.md § Named and default parameters](../design.md#named-and-default-parameters)), but the four level functions are the only functions whose labels are open: any label is accepted, and no user function can declare such a parameter list.

- **Labels.** A label is an identifier. A label used twice in one call is an error, and so is one of the five names a record already carries: `level`, `msg`, `trace_id`, `span_id`, `task_id`.
- **Values.** A value is any type that implements `Serialize` ([design.md § Serialize and Deserialize](../design.md#serialize-and-deserialize)): the integers, floats, `bool`, `char`, `String`, `Str`, `Option`, `Vec`, `Map` and any `#[derive(Serialize)]` type. A value of another type is an error, whose fix-it formats it as text with `f"{v}"` when it implements `Display`. The value is borrowed, so logging it does not move it.
- **Evaluation.** The message and the values are ordinary arguments: they are evaluated at every call, in the order written ([core-semantics.md §8.1](../core-semantics.md#8-evaluation-order)), whether or not the record is kept. Turning a value into a `Json` tree happens only for a kept record, so a filtered `debug` line costs its argument evaluation and nothing more.
- **Conversion.** A kept record's field holds the `Json` tree that the value's `Serialize` impl builds ([json.md](json.md)): the tree `Json.parse` would return from the text `Json.serialize` writes. An integer that fits an `i64` is `Json.Int`; a float is `Json.Number`.

### Filtering

Each record has a `Level`, ordered `Debug < Info < Warn < Error`. `log.init` sets the lowest level kept, `Info` by default, so `debug` lines are dropped until a program asks for them with `log.init(backend, level: log.Level.Debug)`. A dropped record never reaches the backend.

## Span context

Each record carries the span context of the task that logged it: a trace ID, a span ID and a task ID. When a `par` block, a `par for` loop or a `TaskGroup` starts tasks, each child inherits its parent's trace and span, so log entries carry them without the program threading them by hand.

- **`task_id`** is a positive `i64` the runtime gives each task when it starts. It is unique within the process and never reused, and the numbers say nothing about the order tasks started in. Code outside every task, such as `main` before it starts one, has `task_id` `None`.
- **`trace_id` and `span_id`** are `None` until the program starts a trace. An API to start a trace or a span, and to read one from an incoming request's `traceparent` header, is deferred ([deferred.md](../deferred.md)); until it lands both are always `None`.

```json
{"level":"info","msg":"request received","method":"GET","path":"/users/42","task_id":null}
{"level":"debug","msg":"authenticated","user_id":42,"task_id":3}
```

Once traces exist, the same lines carry them:

```json
{"level":"info","msg":"request received","method":"GET","path":"/users/42","trace_id":"abc123","span_id":"s1","task_id":null}
```

## Backends

The backend is configured with `log.init`, normally once, at program start:

```kara
import std.log;

fn main() {
    log.init(log.json_backend(log.Stream.Stdout));                       // JSON to stdout
    // or: log.init(log.text_backend(log.Stream.Stderr));                // human-readable text to stderr
    // or: log.init(my_backend, level: log.Level.Debug);                 // any impl of log.Backend, keeping debug lines
    serve();
}
```

If `log.init` is never called, records at `Info` and above go to `log.json_backend(log.Stream.Stderr)`. A later `log.init` replaces the backend and the level; records already emitted stay where they went.

### Items

| Item | Signature | Notes |
|---|---|---|
| `debug`, `info`, `warn`, `error` | `fn info(msg: Str)`, then open `key: value` fields | See [Fields](#fields) |
| `Level` | `enum Level { Debug, Info, Warn, Error }` | Ordered as written; `Copy`, `Eq`, `Ord` |
| `Record` | `struct Record { level: Level, msg: String, fields: Vec[(String, Json)], trace_id: Option[String], span_id: Option[String], task_id: Option[i64] }` | `fields` in the order the call wrote them. `Json` is [`std.json.Json`](json.md) |
| `Backend` | `trait Backend { fn emit(mut ref self, record: Record); }` | `record` is borrowed |
| `init` | `fn init[B: Backend](backend: own B; level: Level = Level.Info)` | Replaces the backend and the lowest level kept. `B` must cross tasks (`CrossTask`, [core-semantics.md §6.3](../core-semantics.md#6-sharing-c7)), since `emit` runs on whichever task logs |
| `Stream` | `enum Stream { Stdout, Stderr }` | Where a built-in backend writes |
| `json_backend` | `fn json_backend(to: Stream) -> JsonBackend` | One JSON object per line |
| `text_backend` | `fn text_backend(to: Stream) -> TextBackend` | One line of text per record |

**One record at a time.** The runtime holds a lock around `emit`, so a backend never sees two records at once and a line is never torn. A record logged from inside a backend's own `emit` goes to the default backend, `json_backend(Stream.Stderr)`, rather than waiting on the lock its task holds. Records from concurrent tasks reach the backend in an order the program does not choose ([core-semantics.md §11.1](../core-semantics.md#11-concurrency-c9)); records from one task arrive in the order it logged them.

**A backend's own effects are hidden.** `emit` may write a file or a socket, but the log call that reached it still has no effects (see the top of this page). A backend that fails to write drops the record; it must not panic.

### The JSON format

`json_backend` writes each record as one compact JSON object and a newline. The keys come in this order: `level` (the level's name in lower case), `msg`, the fields in call order, then `trace_id` and `span_id` when they are set, then `task_id`, which is written as `null` outside a task. Each field value is written as `Json.stringify` writes it. Nothing else is added: in particular no timestamp, because the collectors that read a service's stdout or stderr (journald, Docker, Kubernetes) stamp each line as it arrives, and an unstamped line keeps a program's log output reproducible in tests. A backend that wants a timestamp reads the clock in `emit`.

### The text format

`text_backend` writes each record as one line: the level in upper case padded to five characters (`DEBUG`, `INFO `, `WARN `, `ERROR`), a space, the message, then ` key=value` for each field in call order, then ` trace_id=…` and ` span_id=…` when they are set and ` task_id=n` inside a task.

A field value that is a `Json.String` is written as its text when the text is not empty and holds no space, `"`, `=` or control character; otherwise it is written quoted, as `Json.stringify` quotes it. Any other value is written as `Json.stringify` writes it.

```
INFO  request received method=GET path=/users/42
DEBUG authenticated user_id=42 task_id=3
ERROR request failed err="token expired" status=500 task_id=3
```

## `dbg` and `log`

`dbg()` is for temporary instrumentation: it prints an expression's source text and value, and it is removed from release builds ([design.md §12](../design.md#observability)). `log` is for production observability: always present, with structured fields, filtered by severity and routed to a backend. Both are transparent.

## Not yet implemented

- `init` does not yet refuse a backend that cannot cross tasks (B-2026-10-27-21).
- Renaming a type of `std.log` on import (`import std.log.{Level as L}`) makes a second type, which the library's functions do not accept; import the types under their own names (B-2026-10-27-23).
