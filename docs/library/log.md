# Logging

`std.log` is the standard library's production logging: always present, with severity levels, structured fields and a configurable backend.

Logging is observability, so it is transparent to the effect system ([design.md §12](../design.md#observability)). The functions of `std.log` are declared with no effects: a call never changes a function's effect set and never conflicts with anything, so adding a log line cannot change a signature or forbid parallelism. The standard library marks them with an internal attribute that user code cannot write, as it does with `#[compiler_builtin]`. Because log output does not conflict, lines from concurrent branches may interleave; within one branch they follow source order.

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

## Span context

The runtime keeps a span context (trace ID, span ID, task ID) for each task. When a `par` block, a `par for` loop or a `TaskGroup` starts tasks, the runtime passes the parent's span context to each child. Log entries carry this context without the program threading it by hand:

```json
{"level":"info","msg":"request received","method":"GET","path":"/users/42","trace_id":"abc123","span_id":"s1","task_id":null}
{"level":"debug","msg":"authenticated","user_id":42,"trace_id":"abc123","span_id":"s2","task_id":3}
```

## Backends

The backend is configured once, at program start, with `log.init`:

```kara
import std.log;

fn main() {
    log.init(log.json_backend(stdout));          // JSON to stdout
    // or: log.init(log.text_backend(stderr));   // human-readable text to stderr
    // or: log.init(my_backend);                 // any impl of log.Backend
    serve();
}
```

The `Backend` trait has one method, `fn emit(mut ref self, record: log.Record)`. Custom backends (file rotation, external collectors, buffered batching) implement it. If `log.init` is never called, the default backend writes JSON to stderr.

## `dbg` and `log`

`dbg()` is for temporary instrumentation: it prints an expression's source text and value, and it is removed from release builds ([design.md §12](../design.md#observability)). `log` is for production observability: always present, with structured fields, filtered by severity and routed to a backend. Both are transparent.
