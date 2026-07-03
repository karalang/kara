use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, RwLock};

use crate::ast::*;
use crate::token::Span;
use crate::typechecker::TypeCheckResult;

mod builtin;
mod comptime_builtins;
mod eval_call;
mod eval_expr;
mod eval_ops;
mod eval_stmt;
mod exec;
mod helpers;
mod iter_eval;
mod layout_query;
mod method_call;
mod method_call_arena;
mod method_call_bounded_channel;
mod method_call_bufreader;
mod method_call_bufwriter;
mod method_call_channel;
mod method_call_column;
mod method_call_dataframe;
mod method_call_file;
mod method_call_http;
mod method_call_interner;
mod method_call_iter;
mod method_call_map;
mod method_call_once;
mod method_call_optres;
mod method_call_pool;
mod method_call_process;
mod method_call_rate_limiter;
mod method_call_regex;
mod method_call_semaphore;
mod method_call_seq;
mod method_call_set;
mod method_call_tensor;
mod pattern_match;
mod reflection;
mod resource_method;
mod value;

use exec::{deep_clone_value, option_value_from, ControlFlow, Env};
use value::{
    try_write_or_panic, upgrade_weak_to_option, EnumData, FieldCell, SharedStructInner,
    ERROR_TRACE_MAX_DEPTH,
};
pub use value::{ErrorTraceFrame, RuntimeError, TestOutcome, Value};

/// Outcome of evaluating one contract predicate (design.md § Contracts
/// rule 2). The two failure modes are reported as distinct fault
/// categories: `contract violated` (the checked code is wrong) vs
/// `contract predicate panicked` (the predicate itself faulted).
pub(crate) enum ContractOutcome {
    Held,
    Violated,
    Panicked(String),
}

// ── Interpreter ─────────────────────────────────────────────────

pub struct Interpreter<'a> {
    pub(crate) program: &'a Program,
    #[allow(dead_code)]
    pub(crate) typecheck_result: &'a TypeCheckResult,
    pub(crate) env: Env,
    /// Captured output for testing (when Some, print/println write here instead of stdout)
    pub captured_output: Option<Vec<String>>,
    /// Pending control flow signal (return/break/continue)
    pub(crate) pending_cf: Option<ControlFlow>,
    /// Runtime effect tracking: records effects performed during execution
    pub tracked_effects: Vec<String>,
    /// Tracks variables that have been moved (ownership simulation)
    #[allow(dead_code)]
    pub(crate) moved_vars: std::collections::HashSet<String>,
    /// Error return trace: ring buffer of (file, line, expr_text) for ? propagation
    pub(crate) error_trace: Vec<ErrorTraceFrame>,
    /// Whether oldest entries were dropped from the trace ring buffer
    pub(crate) error_trace_truncated: bool,
    /// Source filename for error trace frames
    pub(crate) source_filename: String,
    /// When true, par {} blocks execute sequentially (--sequential mode)
    pub sequential_mode: bool,
    /// User-triggered runtime errors collected during execution. Populated by
    /// `record_runtime_error`; inspected by tests / CLI to surface program-level
    /// failures (div by zero, overflow, unwrap of None, index out of bounds, etc.).
    pub runtime_errors: Vec<RuntimeError>,
    /// Comptime user diagnostics emitted via `compiler.error(msg)`
    /// (substrate 3). Unlike `runtime_errors`, recording one does NOT set
    /// `pending_cf` — `compiler.error` is a non-halting diagnostic effect, so
    /// comptime evaluation continues. The comptime fold pass drains these into
    /// `E_COMPTIME_ERROR` compile diagnostics after each block evaluation.
    pub comptime_user_errors: Vec<RuntimeError>,
    /// Per-task stack of provider maps for `with_provider` (design.md §
    /// Provider-Rooted Resources > Runtime mechanics). Each frame binds
    /// `effect resource R` names (keyed by the resolver's fully-qualified
    /// path — currently the bare name at the module-tree level) to an
    /// `Arc`-wrapped provider `Value`. `with_provider[R](p, closure)`
    /// pushes a frame, runs the closure, pops. Resource method calls
    /// `UserDB.foo(...)` resolve by top-down search for the resource name.
    /// The base frame (index 0) holds defaults for ambient program-rooted
    /// resources (planted by a later CR); the tree-walk interpreter is
    /// single-threaded so all frames live on one stack.
    pub(crate) provider_stack: Vec<HashMap<String, Arc<Value>>>,
    /// Stack of active span ids for `std.tracing` (phase-8 line 153).
    /// `with_span(span, || body)` pushes `span.span_id` for the body's
    /// dynamic extent and pops on exit; `tracing_active_span()` reads the
    /// top (0 = no active span), which the `LogEvent` constructors use to
    /// auto-stamp events. The tree-walk interpreter is synchronous (no
    /// real suspend), so unlike codegen there is no cross-suspend frame to
    /// preserve — the stack is the whole story here.
    pub(crate) active_span_stack: Vec<i64>,
    /// Configurable ambient-logging state (phase-8 line 156, interpreter
    /// half). `tracing_min_level` is the process-global minimum level —
    /// `Log.*` calls below it are dropped (rank: trace 0 < debug 1 < info 2
    /// < warn 3 < error 4; default 0 = emit everything). `tracing_exporter`
    /// is the registered ambient sink: `None` → the default per-call
    /// `StdoutExporter` (the existing `Log.*` body runs); `Some(value)` →
    /// that exporter's `export_event` is dispatched instead (e.g. a
    /// `NoOpExporter` to silence, or a custom `Exporter`). `Log.set_min_level`
    /// / `set_exporter` / `reset` write these; `Log.*` consult them. Codegen
    /// does not yet honor this (the runtime-global half is deferred), so a
    /// compiled `Log.*` always emits to stdout — see the phase-8 entry.
    pub(crate) tracing_min_level: i64,
    pub(crate) tracing_exporter: Option<Value>,
    /// Names of `effect resource` declarations in the program, collected
    /// at [`register_items`] time. Used by [`eval_method_call`] to detect
    /// receivers of the form `UserDB.query(...)` — where `UserDB` is not
    /// a value binding — and dispatch via the provider stack instead of
    /// normal method lookup.
    pub(crate) effect_resources: HashSet<String>,
    /// Names of `distinct type` declarations defined in the baked stdlib
    /// (e.g. `ExitCode`). `is_distinct_type` scans the user program's
    /// `items` directly, but baked-stdlib sources are not merged into
    /// `program.items`, so their distinct-type constructors
    /// (`ExitCode(code)` inside `ExitCode.from`) would otherwise be
    /// unrecognized. Populated once in `register_items` from
    /// `STDLIB_PROGRAMS`. Phase-8 entry-point contract Slice B.
    pub(crate) stdlib_distinct_types: HashSet<String>,
    /// Xorshift64 state backing the default `RandomSource` provider.
    /// Seeded once per [`Interpreter::new`] from the system clock's
    /// sub-second nanoseconds so repeated `cargo test` runs see fresh
    /// sequences. `with_provider[RandomSource](Fake…)` shadows this
    /// entirely; determinism-sensitive tests must opt in via a fake.
    pub(crate) rand_state: u64,
    /// Per-call frame of generic-param substitutions: name → concrete type
    /// name. Pushed at every generic call (using
    /// `TypeCheckResult.call_type_subs` keyed by call span); popped on
    /// return. `T.method()` and bare-call dispatch in trait associated
    /// function bodies look up `T` through this stack to find the concrete
    /// impl to dispatch to. Outer-frame entries are visible (transitive
    /// resolution: a callee's `T → "U"` where `U` is itself a generic param
    /// of the caller resolves via the next frame down).
    pub(crate) type_subs_stack: Vec<HashMap<String, String>>,
    /// `par {}` shared cancellation flag. Set by `eval_par_block` on
    /// each branch interpreter; observed by `eval_block_inner` between
    /// top-level statements as a minimal effect-boundary check. When
    /// observed, the running branch raises `ControlFlow::Cancelled`,
    /// which classifies as `ExitPath::Cancelled(sentinel)` so any
    /// `errdefer(e)` in the active scope binds `e` to the sentinel
    /// during the errdefer phase. None outside `par {}` branches.
    pub(crate) cancel_flag: Option<Arc<AtomicBool>>,
    /// Records the order in which `CleanupAction::Drop` slots fire —
    /// both NLL early-drops (mid-block, after a binding's last use)
    /// and scope-exit drops drained from the unified cleanup stack.
    /// Each entry is the binding's name; tests inspect this trace to
    /// verify drop placement and ordering since the interpreter has
    /// no observable user-`impl Drop` dispatch yet. Always populated
    /// (cheap; small in real programs) — a public accessor is exposed
    /// so test harness functions can read it after `run()`.
    pub drop_trace: Vec<String>,
    /// Element-type hint for a `Tensor.zeros` / `Tensor.ones` fill,
    /// threaded from the enclosing `let`'s annotation. The tree-walk
    /// interpreter is dynamically typed and the typechecker records only
    /// the *declared* return type `Tensor[T, S]` (T unresolved) at the
    /// call span, so the concrete element type `T` is recoverable only
    /// from the binding annotation — exactly the source codegen reads via
    /// `pending_let_tensor_info`. `eval_stmt_cf`'s `Let` arm sets this
    /// from a `Tensor[Elem, …]` annotation (save/restore around the RHS
    /// so nested `let`s nest correctly), and `tensor_scalar_fill`
    /// consumes it; `None` falls back to the historical `f64` fill. See
    /// `src/interpreter/method_call_tensor.rs`.
    pub(crate) pending_tensor_fill: Option<method_call_tensor::TensorElemFill>,
    /// Full source text of the program being executed. Used by
    /// `eval_builtin_dbg` to slice the argument's `Span.offset/length`
    /// for the `expr` field (terminal mode) and `"expr":"…"` field
    /// (structured mode). Empty until [`set_source_text`] is called by
    /// the CLI; tests may leave it empty in which case `dbg()` falls
    /// back to a placeholder.
    pub(crate) source_text: String,
    /// Format mode for `dbg()` output. `Terminal` (default) prints a
    /// human-readable line; `Json` prints a single JSON object per
    /// call. Selected by the CLI based on `--output=…`. See design.md
    /// § dbg() — Output formats.
    pub(crate) dbg_output_mode: DbgOutputMode,
    /// Per-task identifier for `dbg()` tagging in `par {}` regions.
    /// `None` outside `par {}`; `Some(N)` inside a branch. Allocated
    /// from `task_id_counter` on branch entry; nested `par {}` inside
    /// a branch shadows the parent's id so each `dbg()` reports the
    /// innermost task.
    pub current_task_id: Option<u64>,
    /// Shared monotonic counter for `par {}` task ids. Cloned across
    /// every branch interpreter so nested `par {}` regions allocate
    /// from the same sequence.
    pub(crate) task_id_counter: Arc<AtomicU64>,
    /// Test-only capture buffer for `dbg()` output. When `Some`,
    /// `eval_builtin_dbg` pushes its formatted line here instead of
    /// writing to stderr. Tests inspect this to assert the exact
    /// terminal-mode or JSON-mode output. In `par {}` branches the
    /// parent's buffer is mirrored into each branch and merged on
    /// join (same pattern as `captured_output`).
    pub captured_dbg: Option<Vec<String>>,
    /// `std.process` intrinsic side-table — keyed by OS pid, holds the
    /// `std::process::Child` handle so subsequent `child.wait()` /
    /// `try_wait()` / `kill()` calls can locate the same OS process.
    /// `Command.spawn` populates this; `wait` removes the entry on
    /// success; `try_wait` removes only when the child has exited;
    /// `kill` leaves the entry in place (caller still needs to wait
    /// to reap). Entries that outlive the interpreter become zombie
    /// processes — same behavior as a Rust `Child` that's dropped
    /// without `wait`. See `src/interpreter/method_call_process.rs`.
    pub(crate) child_table: HashMap<i64, std::process::Child>,
    /// Captured-pipe handle tables for `Stdio.Piped` streams, keyed by
    /// the owning child's pid. `Child.stdout()` / `.stderr()` / `.stdin()`
    /// `take()` the stream off the `std::process::Child` in `child_table`
    /// into the matching table here and hand back a Kāra handle struct;
    /// `read_to_string` reads from a read table (removing the exhausted
    /// entry), `ChildStdin.write` writes the stdin table entry, and
    /// `ChildStdin.close` removes its entry — dropping the OS handle,
    /// which signals EOF to the child. See
    /// `src/interpreter/method_call_process.rs`.
    pub(crate) child_stdout_table: HashMap<i64, std::process::ChildStdout>,
    pub(crate) child_stderr_table: HashMap<i64, std::process::ChildStderr>,
    pub(crate) child_stdin_table: HashMap<i64, std::process::ChildStdin>,
    /// `Pool[T]` intrinsic side-table — keyed by `Pool.handle_id`,
    /// holds the per-pool state (factory closure + bounds + slot
    /// vec). `Pool.new` populates an entry and returns a handle;
    /// `acquire` / `release` walk the table by handle. Generic T
    /// erases at runtime — the slot is just a `Value`. See
    /// `src/interpreter/method_call_pool.rs`.
    pub(crate) pool_table: HashMap<i64, PoolEntry>,
    /// Monotonic counter for `Pool.handle_id` minting. Starts at 1
    /// so a default-constructed `Pool { handle_id: 0 }` (e.g. a
    /// hand-rolled struct literal that bypassed `Pool.new`) can be
    /// distinguished from a legitimate pool.
    pub(crate) pool_handle_counter: i64,
    /// `Arena[T]` intrinsic side-table — keyed by `Arena.handle_id`,
    /// holds the per-arena backing vec of bump-allocated values.
    /// `Arena.new` allocates a fresh empty vec and returns a handle;
    /// `push` appends, `get` reads by `ArenaRef.index`,
    /// `high_water_mark` / `rewind_to` snapshot + truncate. Generic T
    /// erases at runtime — a slot is just a `Value`. See
    /// `src/interpreter/method_call_arena.rs`.
    pub(crate) arena_table: HashMap<i64, Vec<Value>>,
    /// Monotonic counter for `Arena.handle_id` minting. Starts at 1
    /// so a hand-rolled `Arena { handle_id: 0 }` literal that bypassed
    /// `Arena.new` (no table entry) is distinguishable from a real
    /// arena.
    pub(crate) arena_handle_counter: i64,
    /// `Interner` intrinsic side-table — keyed by `Interner.handle_id`,
    /// holds the per-interner `(Vec<String>, HashMap<String, i64>)`: the
    /// `Vec` is the symbol-id → string lookup, the `HashMap` is the dedup
    /// index. `Interner.new` allocates a fresh pair and returns a handle;
    /// `intern` appends-or-dedups, `resolve` reads by id. See
    /// `src/interpreter/method_call_interner.rs`.
    #[allow(clippy::type_complexity)]
    pub(crate) interner_table: HashMap<i64, (Vec<String>, HashMap<String, i64>)>,
    /// Monotonic counter for `Interner.handle_id` minting. Starts at 1
    /// so a hand-rolled `Interner { handle_id: 0 }` literal that bypassed
    /// `Interner.new` (no table entry) is distinguishable from a real
    /// interner.
    pub(crate) interner_handle_counter: i64,
    /// `OnceLock[T]` / `OnceCell[T]` intrinsic side-table — keyed by the
    /// cell's `handle_id`, holds the write-once slot (`None` = empty,
    /// `Some(v)` = filled). Both cell types share this table; the
    /// cross-task vs. single-task split is a typecheck-time structural
    /// guarantee, not a runtime one. See
    /// `src/interpreter/method_call_once.rs`.
    pub(crate) once_table: HashMap<i64, Option<Value>>,
    /// Monotonic counter for `OnceLock`/`OnceCell` `handle_id` minting.
    /// Starts at 1 so a hand-rolled `handle_id: 0` literal that bypassed
    /// `*.new` (no table entry) is distinguishable from a real cell.
    pub(crate) once_handle_counter: i64,
    /// `Semaphore` backpressure primitive — permit counter keyed by
    /// `Semaphore.handle_id`. `Semaphore.new` populates an entry;
    /// `acquire` / `release` adjust `available`. See
    /// `src/interpreter/method_call_semaphore.rs`.
    pub(crate) semaphore_table: HashMap<i64, SemEntry>,
    /// Monotonic counter for `Semaphore.handle_id`, starting at 1 so a
    /// hand-rolled `Semaphore { handle_id: 0 }` (bypassing
    /// `Semaphore.new`) is distinguishable from a real semaphore.
    pub(crate) semaphore_handle_counter: i64,
    /// `RateLimiter` token-bucket state keyed by `RateLimiter.handle_id`.
    /// `new_token_bucket` populates an entry; `try_acquire` refills +
    /// consumes a token per key. See
    /// `src/interpreter/method_call_rate_limiter.rs`.
    pub(crate) rate_limiter_table: HashMap<i64, RateLimiterEntry>,
    /// Monotonic counter for `RateLimiter.handle_id`, starting at 1 so a
    /// hand-rolled `RateLimiter { handle_id: 0 }` is distinguishable
    /// from a real limiter.
    pub(crate) rate_limiter_handle_counter: i64,
    /// `BoundedChannel[T]` buffers keyed by `BoundedChannel.handle_id`.
    /// `new` populates an entry; `send` enqueues (or fails on a full
    /// `FailFast` buffer), `recv` dequeues. T erases at runtime — the
    /// buffer is a `VecDeque<Value>`. See
    /// `src/interpreter/method_call_bounded_channel.rs`.
    pub(crate) bounded_channel_table: HashMap<i64, BoundedChannelEntry>,
    /// Monotonic counter for `BoundedChannel.handle_id`, starting at 1.
    pub(crate) bounded_channel_handle_counter: i64,
    /// REPL value-snapshot replay. When a `StmtKind::Let { pattern:
    /// PatternKind::Binding(..), .. }` evaluates and the *binder
    /// pattern's span* is a key here, the RHS is **not** evaluated —
    /// the binding is created from the pre-loaded value instead. Empty
    /// for ordinary single-file runs; populated by the REPL between
    /// cells so a `let x = expensive()` from cell N does not re-run
    /// `expensive()` when cell N+1's synthetic source-replay includes
    /// the same `let`. Keyed by span (not name) so that when a name
    /// has been shadowed across cells (`let x = 1; … let x = 99;` —
    /// legal, and both slices replay) only the LAST binder
    /// short-circuits to the snapshot; earlier binders re-run their
    /// true RHS in order, keeping every intermediate reader
    /// historically correct (B-2026-07-02-33). Pattern lets (tuple /
    /// struct destructuring / `let-else`) fall through the normal
    /// RHS-eval path; the source-replay model retains its semantics
    /// for those forms (RHS re-runs each cell, mutation does not
    /// survive).
    pub let_value_overrides: HashMap<crate::resolver::SpanKey, Value>,
    /// REPL value-snapshot capture set. The bound value of any
    /// `StmtKind::Let { pattern: PatternKind::Binding(name), .. }` whose
    /// `name` is in this set is recorded into `captured_let_values`
    /// after binding. The REPL drains this map after `run()` returns
    /// so the next cell can use the captured values as overrides.
    pub let_snapshot_watch: HashSet<String>,
    /// REPL value-snapshot output channel. Populated by the Let arm of
    /// `eval_stmt_cf` whenever the binding name is in
    /// `let_snapshot_watch`. The REPL reads this after `run()` returns;
    /// non-REPL callers ignore it.
    pub captured_let_values: HashMap<String, Value>,
    /// `karac test` per-test deadline. `None` outside the test runner
    /// (so a normal `karac run` of a user program never times out at
    /// this layer — see the separate `karac run --timeout` flag for
    /// the runtime side of that). When `Some(deadline)`,
    /// `eval_block_inner` polls `Instant::now() >= deadline` at every
    /// statement boundary (alongside the existing
    /// `observed_cancellation()` check) and raises
    /// `ControlFlow::TimedOut` on the first observation. Cleanup
    /// (Drop / Defer) still drains, but errdefer does not fire — the
    /// timeout is a runner guardrail, not a user-observable error.
    pub(crate) test_deadline: Option<std::time::Instant>,
    /// Set to `true` when the interpreter observed its
    /// `test_deadline` and raised `ControlFlow::TimedOut`. The test
    /// runner reads this after `run_test_function` returns so the
    /// JSONL event can be a structured `test_timeout` (with the
    /// configured timeout seconds + observed elapsed wall-clock)
    /// rather than a generic `test_fail`.
    pub timed_out: bool,
    /// Per-call stack of `old(expr)` pre-state snapshots for `ensures`
    /// clauses (design.md § Contracts rule 4). At a contracted function's
    /// entry, each `old(arg)` occurrence in its `ensures` clauses is
    /// evaluated and stored here keyed by the arg expression's span; when
    /// the postcondition runs at exit, the `old(arg)` call reads the
    /// snapshot back. A stack so nested contracted calls don't collide.
    pub(crate) old_snapshots: Vec<HashMap<crate::resolver::SpanKey, Value>>,
}

/// Per-pool state for the `Pool[T]` intrinsic. Lives in
/// [`Interpreter::pool_table`]. `slots` holds connections that
/// `release` has returned to the pool and that `acquire` can hand
/// straight back without invoking `create_fn`; `active_count` is
/// the number of T values the pool has minted so far (slots +
/// checked-out connections combined) and is bounded by
/// `max_connections`. `max_waiters` is honored at the API surface
/// (the parameter exists for forward compatibility with a
/// threaded backend) but doesn't gate any wait queue today —
/// `acquire` in the single-threaded tree-walk interpreter is
/// either immediately served or immediately fails with `Timeout`.
pub struct PoolEntry {
    pub create_fn: Value,
    pub max_connections: i64,
    pub max_waiters: i64,
    pub slots: Vec<Value>,
    pub active_count: i64,
    /// Optional health-check hook (`Fn(T) -> bool`), registered via
    /// `Pool.with_health_check`. When present, `acquire` validates each
    /// idle slot it pops: a `false` verdict evicts the slot (decrementing
    /// `active_count` so the connection no longer counts against the cap)
    /// and `acquire` falls through to mint a fresh one. `None` → idle slots
    /// are handed back unchecked (the v1 default).
    pub health_check: Option<Value>,
    /// Conn-ids currently checked out (handed to a caller and not yet
    /// returned). Both explicit `Pool.release` and the `PooledConnection`
    /// auto-`Drop` route their slot return through `checked_out` for
    /// idempotency: returning a conn-id already absent is a no-op, so an
    /// explicit `release` followed by the binding's scope-exit drop hands
    /// the slot back exactly once (never inflating one connection into two
    /// idle slots). See `src/interpreter/method_call_pool.rs`.
    pub checked_out: HashSet<i64>,
    /// Monotonic source of the per-connection ids minted at `acquire`;
    /// starts at `1` so a hand-rolled `PooledConnection` literal (no
    /// `conn_id`, reads as `0`) never collides with a live checkout.
    pub next_conn_id: i64,
}

/// Per-semaphore permit state. `available` is the live count an
/// `acquire` decrements and a `release` increments; `max` is the
/// initial budget `release` saturates at. See
/// `src/interpreter/method_call_semaphore.rs`.
pub struct SemEntry {
    pub available: i64,
    pub max: i64,
}

/// Per-`RateLimiter` token-bucket configuration plus the live per-key
/// buckets. `rate` is tokens/second, `capacity` the max tokens a key's
/// bucket holds (and starts at). See
/// `src/interpreter/method_call_rate_limiter.rs`.
pub struct RateLimiterEntry {
    pub rate: f64,
    pub capacity: f64,
    pub buckets: HashMap<String, TokenBucket>,
}

/// One key's bucket: `tokens` available right now, last refilled at
/// `last`. Refill is lazy — computed from elapsed time on each
/// `try_acquire`, so idle keys cost nothing.
pub struct TokenBucket {
    pub tokens: f64,
    pub last: std::time::Instant,
}

/// A `BoundedChannel[T]` buffer. `capacity` bounds `queue`'s length;
/// `fail_fast` records whether a full `send` errors (`OnFull::FailFast`,
/// and `Block` too under v1's collapsed semantics). See
/// `src/interpreter/method_call_bounded_channel.rs`.
pub struct BoundedChannelEntry {
    pub capacity: i64,
    pub fail_fast: bool,
    pub queue: std::collections::VecDeque<Value>,
}

/// Format mode for [`Interpreter`]'s `dbg()` output. See design.md §
/// dbg() — Output formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DbgOutputMode {
    /// Human-readable single line: `[task:N file:line] expr = value`
    /// (task prefix omitted outside `par {}`).
    Terminal,
    /// One JSON object per call:
    /// `{"kind":"dbg","task_id":N,"file":"…","line":N,"expr":"…","type":"…","value":"…"}`.
    Json,
}

/// JSON-string escape with surrounding quotes. Used by the `dbg()`
/// structured output mode. Kept private to interpreter.rs; the cli /
/// doc modules each carry their own copies for the same reason
/// (decoupling, tiny helper).
pub(crate) fn dbg_json_escape(s: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                write!(out, "\\u{:04x}", c as u32).unwrap();
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Convert a PascalCase identifier to lower_snake_case.
/// `InProgress` → `in_progress`, `Up` → `up`, `HTTPError` → `h_t_t_p_error`.
pub(crate) fn pascal_to_snake(s: &str) -> String {
    let mut result = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            result.push('_');
        }
        result.extend(ch.to_lowercase());
    }
    result
}

/// True when `ty` is a concurrency primitive (`Atomic[T]` / `Mutex[T]`) — a
/// field of this type provides interior mutability without the `mut` keyword,
/// so it must be stored in a `RwLock` cell on a shared/par struct so writes
/// (`.fetch_add` / `.store` / `lock`) persist through the shared `Arc`.
fn is_interior_mutable_field_type(ty: &TypeExpr) -> bool {
    matches!(
        &ty.kind,
        TypeKind::Path(p)
            if matches!(p.segments.last().map(String::as_str), Some("Atomic") | Some("Mutex"))
    )
}

/// True when `return_type` denotes `Self` or the named type `type_name` — the
/// constructor return shape (a `-> Self` parses as `TypeKind::Path(["Self"])`;
/// an explicit `-> Type` is `Path([… , "Type"])`). Used to distinguish a
/// constructor (whose return value carries the type's invariants) from a static
/// associated function returning some unrelated type.
fn returns_self_or_type(return_type: Option<&TypeExpr>, type_name: &str) -> bool {
    match return_type.map(|t| &t.kind) {
        Some(TypeKind::Path(p)) => {
            matches!(p.segments.last().map(String::as_str), Some(seg) if seg == "Self" || seg == type_name)
        }
        _ => false,
    }
}

/// Seed the per-interpreter xorshift state from the system clock's
/// sub-second nanoseconds, OR'd with `1` so the state can never be zero
/// (xorshift's fixed point).
#[cfg(not(target_arch = "wasm32"))]
fn seed_rand_state() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    nanos | 1
}

/// wasm32 (the browser playground): `SystemTime::now()` panics
/// (`sys/time/unsupported`) — and it ran in `Interpreter::new`, so it
/// trapped EVERY playground run before the program even started. Fixed
/// golden-ratio seed instead: `RandomSource` is deterministic in the
/// playground, which beats trapping the whole wasm module.
#[cfg(target_arch = "wasm32")]
fn seed_rand_state() -> u64 {
    0x9E37_79B9_7F4A_7C15 | 1
}

impl<'a> Interpreter<'a> {
    pub fn new(program: &'a Program, typecheck_result: &'a TypeCheckResult) -> Self {
        Interpreter {
            program,
            typecheck_result,
            env: Env::new(),
            captured_output: None,
            pending_cf: None,
            tracked_effects: Vec::new(),
            moved_vars: std::collections::HashSet::new(),
            error_trace: Vec::new(),
            error_trace_truncated: false,
            source_filename: String::new(),
            // wasm32 (the browser playground) has no threads, so
            // `eval_par_block`'s `thread::scope` would trap; sequential
            // `par {}` matches the wasm codegen story (seq_scheduler,
            // FIFO-deterministic).
            sequential_mode: cfg!(target_arch = "wasm32"),
            runtime_errors: Vec::new(),
            comptime_user_errors: Vec::new(),
            provider_stack: vec![HashMap::new()],
            active_span_stack: Vec::new(),
            tracing_min_level: 0,
            tracing_exporter: None,
            effect_resources: HashSet::new(),
            stdlib_distinct_types: HashSet::new(),
            rand_state: seed_rand_state(),
            type_subs_stack: Vec::new(),
            cancel_flag: None,
            drop_trace: Vec::new(),
            pending_tensor_fill: None,
            source_text: String::new(),
            dbg_output_mode: DbgOutputMode::Terminal,
            current_task_id: None,
            task_id_counter: Arc::new(AtomicU64::new(0)),
            captured_dbg: None,
            child_table: HashMap::new(),
            child_stdout_table: HashMap::new(),
            child_stderr_table: HashMap::new(),
            child_stdin_table: HashMap::new(),
            pool_table: HashMap::new(),
            pool_handle_counter: 0,
            arena_table: HashMap::new(),
            arena_handle_counter: 0,
            interner_table: HashMap::new(),
            interner_handle_counter: 0,
            once_table: HashMap::new(),
            once_handle_counter: 0,
            semaphore_table: HashMap::new(),
            semaphore_handle_counter: 0,
            rate_limiter_table: HashMap::new(),
            rate_limiter_handle_counter: 0,
            bounded_channel_table: HashMap::new(),
            bounded_channel_handle_counter: 0,
            let_value_overrides: HashMap::new(),
            let_snapshot_watch: HashSet::new(),
            captured_let_values: HashMap::new(),
            test_deadline: None,
            timed_out: false,
            old_snapshots: Vec::new(),
        }
    }

    /// Set the per-test wall-clock deadline for the next
    /// `run_test_function` invocation. `karac test` calls this with
    /// `Some(Instant::now() + timeout)` immediately before each test
    /// fires; `eval_block_inner` polls the deadline at every statement
    /// boundary and raises `ControlFlow::TimedOut` on the first
    /// observation past the deadline. The companion `timed_out` flag
    /// is reset to `false` here so a runner reusing the same
    /// Interpreter across multiple tests (today's pattern in `cmd_test`)
    /// doesn't carry a previous test's timeout state into the next
    /// run. Pass `None` to clear the deadline outside the test
    /// runner (e.g. when the same Interpreter is reused for non-test
    /// evaluation).
    pub fn set_test_deadline(&mut self, deadline: Option<std::time::Instant>) {
        self.test_deadline = deadline;
        self.timed_out = false;
    }

    /// Record a user-triggered runtime error and begin unwinding. Returns
    /// `Value::Unit` so call sites can write `return self.record_runtime_error(...)`
    /// in `Value`-returning contexts; the pending `ControlFlow::RuntimeError`
    /// short-circuits subsequent evaluation.
    fn record_runtime_error(&mut self, message: impl Into<String>, span: &Span) -> Value {
        self.runtime_errors.push(RuntimeError {
            message: message.into(),
            span: span.clone(),
            left: None,
            right: None,
        });
        self.push_error_trace(span.line, span.column);
        self.pending_cf = Some(ControlFlow::RuntimeError);
        Value::Unit
    }

    /// Look up the refinement predicate for a named refinement type, if any
    /// (phase-9 step 5b). Predicates live on `Item::TypeAlias.refinement` in
    /// the AST; the interpreter evaluates them at construction sites
    /// (`Name.try_from(x)`, `x as Name`) to enforce the invariant at runtime.
    pub(crate) fn refinement_predicate(&self, name: &str) -> Option<Expr> {
        self.program.items.iter().find_map(|item| match item {
            Item::TypeAlias(t) if t.name == name => t.refinement.clone(),
            // Combined `distinct type T = Base where pred` carries the same
            // construction-time predicate; the `T(value)` constructor and
            // `T.try_from` enforce it identically (design.md § Distinct Types).
            Item::DistinctType(d) if d.name == name => d.refinement.clone(),
            _ => None,
        })
    }

    /// Whether `name` is a `distinct type` declaration. Distinct types are
    /// zero-cost: the `Name(value)` constructor wraps to the base value
    /// unchanged and `.raw()` unwraps it, so the runtime carries no wrapper —
    /// this lookup only gates the constructor-call interception in
    /// `eval_call`. design.md § Distinct Types (Newtypes).
    pub(crate) fn is_distinct_type(&self, name: &str) -> bool {
        self.stdlib_distinct_types.contains(name)
            || self
                .program
                .items
                .iter()
                .any(|item| matches!(item, Item::DistinctType(d) if d.name == name))
    }

    /// The `requires` / `ensures` contract clauses of the top-level free
    /// function `name`, if it declares any (design.md § Contracts). Returns
    /// `None` for a function with no contract. Looked up from the AST at the
    /// call site (mirrors the refinement-predicate / distinct-type lookups);
    /// the interpreter runs every clause in debug — `requires` at entry,
    /// `ensures` at the return point — faulting `contract violated` on a
    /// false predicate. v1 covers free functions; method contracts are a
    /// follow-on (they dispatch through a separate path).
    /// Per-parameter "is a mutate-through-borrow mode" flags for the named
    /// top-level function — `mut ref T` (`TypeKind::MutRef`) or `mut Slice[T]`
    /// (`TypeKind::MutSlice`). Drives the CICO write-back in `eval_call`: a
    /// `mut ref` param must propagate the callee's final value back to the
    /// caller's variable, and — crucially — that must happen even when the arg
    /// is an already-in-scope `mut ref` binding FORWARDED into a nested call.
    /// Per design.md § Call-site mutation markers, such a forwarded borrow
    /// carries no `mut` marker, so keying write-back on the marker alone drops
    /// the chain (a recursive/forwarded `mut ref i64` accumulator silently
    /// never updates). Keying on the callee's declared param mode restores it.
    /// Returns `None` for a name with no matching AST function (e.g. closures),
    /// leaving those on the marker-only path.
    pub(crate) fn fn_param_mut_ref_flags(&self, name: &str) -> Option<Vec<bool>> {
        self.program.items.iter().find_map(|item| match item {
            Item::Function(f) if f.name == name => Some(
                f.params
                    .iter()
                    .map(|p| matches!(p.ty.kind, TypeKind::MutRef(_) | TypeKind::MutSlice(_)))
                    .collect(),
            ),
            _ => None,
        })
    }

    #[allow(clippy::type_complexity)]
    pub(crate) fn function_contract(
        &self,
        name: &str,
    ) -> Option<(Vec<Expr>, Vec<crate::ast::EnsuresClause>)> {
        self.program.items.iter().find_map(|item| match item {
            Item::Function(f)
                if f.name == name && (!f.requires.is_empty() || !f.ensures.is_empty()) =>
            {
                Some((f.requires.clone(), f.ensures.clone()))
            }
            _ => None,
        })
    }

    /// The `requires` / `ensures` contract clauses of the instance method
    /// `method` on `type_name`, if it declares any (design.md § Contracts).
    /// Mirrors [`function_contract`] for the impl-method dispatch path so
    /// method contracts (and `old(...)`) are enforced at runtime too.
    #[allow(clippy::type_complexity)]
    pub(crate) fn method_contract(
        &self,
        type_name: &str,
        method: &str,
    ) -> Option<(Vec<Expr>, Vec<crate::ast::EnsuresClause>)> {
        self.program.items.iter().find_map(|item| match item {
            Item::ImplBlock(imp) => {
                let target = match &imp.target_type.kind {
                    TypeKind::Path(p) => p.segments.last().map(String::as_str),
                    _ => None,
                };
                if target != Some(type_name) {
                    return None;
                }
                imp.items.iter().find_map(|it| match it {
                    ImplItem::Method(m)
                        if m.name == method
                            && (!m.requires.is_empty() || !m.ensures.is_empty()) =>
                    {
                        Some((m.requires.clone(), m.ensures.clone()))
                    }
                    _ => None,
                })
            }
            _ => None,
        })
    }

    /// The receiver mode (`self` / `ref self` / `mut ref self`) of method
    /// `method` on `type_name`, or `None` for an associated function (no
    /// receiver) or an unknown method. Drives the CICO write-back on the
    /// method-call dispatch path: only a `MutRef` receiver's post-body value
    /// is copied back to the call-site place (mirrors the free-function
    /// `mut ref T` write-back in `eval_call.rs`).
    pub(crate) fn method_self_param(
        &self,
        type_name: &str,
        method: &str,
    ) -> Option<crate::ast::SelfParam> {
        fn find_in(items: &[Item], type_name: &str, method: &str) -> Option<crate::ast::SelfParam> {
            items.iter().find_map(|item| match item {
                Item::ImplBlock(imp) => {
                    let target = match &imp.target_type.kind {
                        TypeKind::Path(p) => p.segments.last().map(String::as_str),
                        _ => None,
                    };
                    if target != Some(type_name) {
                        return None;
                    }
                    imp.items.iter().find_map(|it| match it {
                        ImplItem::Method(m) if m.name == method => m.self_param.clone(),
                        _ => None,
                    })
                }
                _ => None,
            })
        }
        // The user program first, then the baked stdlib. Without the stdlib
        // fallback a `mut ref self` method on a baked-stdlib struct (e.g.
        // `ProtoReader.read_varint`) would not get its mutated receiver
        // written back to the call-site binding — the stdlib impls live in
        // `STDLIB_PROGRAMS`, not `self.program.items`.
        find_in(&self.program.items, type_name, method).or_else(|| {
            crate::prelude::STDLIB_PROGRAMS
                .iter()
                .find_map(|(_, p)| find_in(&p.items, type_name, method))
        })
    }

    /// Walk a contract expression and, for every `old(arg)` occurrence,
    /// evaluate `arg` *now* (function entry, pre-state) and record the value
    /// in `snap` keyed by the arg's span. Used to build the `old(...)`
    /// snapshot for an `ensures` clause before the body runs (design.md
    /// § Contracts rule 4). Recurses through the contract-expression grammar;
    /// the arg of an `old(...)` is captured but not recursed into (no nested
    /// `old`).
    pub(crate) fn capture_old_in_expr(
        &mut self,
        expr: &Expr,
        snap: &mut HashMap<crate::resolver::SpanKey, Value>,
    ) {
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                if let ExprKind::Identifier(n) = &callee.kind {
                    if n == "old" && args.len() == 1 {
                        let val = self.eval_expr_inner(&args[0].value);
                        snap.insert(
                            crate::resolver::SpanKey::from_span(&args[0].value.span),
                            val,
                        );
                        return;
                    }
                }
                self.capture_old_in_expr(callee, snap);
                for a in args {
                    self.capture_old_in_expr(&a.value, snap);
                }
            }
            ExprKind::Binary { left, right, .. } => {
                self.capture_old_in_expr(left, snap);
                self.capture_old_in_expr(right, snap);
            }
            ExprKind::Unary { operand, .. } => self.capture_old_in_expr(operand, snap),
            ExprKind::FieldAccess { object, .. } => self.capture_old_in_expr(object, snap),
            ExprKind::MethodCall { object, args, .. } => {
                self.capture_old_in_expr(object, snap);
                for a in args {
                    self.capture_old_in_expr(&a.value, snap);
                }
            }
            ExprKind::Index { object, index } => {
                self.capture_old_in_expr(object, snap);
                self.capture_old_in_expr(index, snap);
            }
            _ => {}
        }
    }

    /// Evaluate one contract predicate and classify the outcome (design.md
    /// § Contracts rule 2). A predicate that evaluates to `true` *holds*; one
    /// that evaluates to anything else *is violated*; one whose evaluation
    /// itself faults (indexing OOB, div-by-zero, `unwrap` of `None`, explicit
    /// `panic()`) *panicked* — a distinct fault category. On a panic the raw
    /// fault is removed from `runtime_errors` and `pending_cf` is cleared so
    /// the caller can re-record the distinct `contract predicate panicked`
    /// category.
    pub(crate) fn eval_contract_predicate(&mut self, pred: &Expr) -> ContractOutcome {
        let errs_before = self.runtime_errors.len();
        let result = self.eval_expr_inner(pred);
        if self.pending_cf.is_some() {
            self.pending_cf = None;
            let drained: Vec<_> = self.runtime_errors.drain(errs_before..).collect();
            let msg = drained
                .last()
                .map(|e| e.message.clone())
                .unwrap_or_else(|| "predicate evaluation faulted".to_string());
            return ContractOutcome::Panicked(msg);
        }
        if result == Value::Bool(true) {
            ContractOutcome::Held
        } else {
            ContractOutcome::Violated
        }
    }

    /// The struct invariant predicates to check at the exit of instance
    /// method `method` on `type_name` (design.md § Contracts rule 3):
    /// `impl invariant` predicates fire at *every* method exit (pub and
    /// private); plain `invariant` predicates fire only at `pub` method
    /// exits. Returns the combined list (empty when nothing applies). v1
    /// covers instance methods; constructors (assoc fns returning `Self`)
    /// and shared struct/enum receivers are follow-ons.
    pub(crate) fn method_invariants_to_check(&self, type_name: &str, method: &str) -> Vec<Expr> {
        let Some((invariants, impl_invariants)) =
            self.program.items.iter().find_map(|item| match item {
                Item::StructDef(s) if s.name == type_name => {
                    Some((s.invariants.clone(), s.impl_invariants.clone()))
                }
                _ => None,
            })
        else {
            return Vec::new();
        };
        if invariants.is_empty() && impl_invariants.is_empty() {
            return Vec::new();
        }
        // `impl invariant` — every method exit.
        let mut result = impl_invariants;
        // plain `invariant` — only when `method` is a `pub` method of the type.
        if !invariants.is_empty() {
            let is_pub_method = self.program.items.iter().any(|item| match item {
                Item::ImplBlock(imp) => {
                    let target = match &imp.target_type.kind {
                        TypeKind::Path(p) => p.segments.last().map(String::as_str),
                        _ => None,
                    };
                    target == Some(type_name)
                        && imp.items.iter().any(
                            |it| matches!(it, ImplItem::Method(m) if m.name == method && m.is_pub),
                        )
                }
                _ => false,
            });
            if is_pub_method {
                result.extend(invariants);
            }
        }
        result
    }

    /// The struct/impl `invariant` predicates a constructor must satisfy at its
    /// return point (design.md § Contracts: "Constructors (pub associated
    /// functions that return `Self`) also check the invariant at their return
    /// point"). `fn_name` is the `Type.method` key of the called function. The
    /// list is non-empty only when `fn_name` names an associated function (no
    /// receiver) of `Type` whose return type is `Self` or `Type` itself — i.e.
    /// a constructor — so an ordinary static helper returning some other type
    /// (`pub fn count() -> i64`) is not mistaken for one. When it is a
    /// constructor, the same `impl invariant`-always / plain-`invariant`-if-pub
    /// rule as method exits applies, reusing [`method_invariants_to_check`]
    /// (a constructor is an `is_pub` `ImplItem::Method`, so the pub check there
    /// resolves correctly). The caller binds the *return value* as `self` and
    /// evaluates each predicate.
    pub(crate) fn constructor_invariants_to_check(&self, fn_name: &str) -> Vec<Expr> {
        let Some((type_name, method_name)) = fn_name.split_once('.') else {
            return Vec::new();
        };
        let is_self_returning_ctor = self.program.items.iter().any(|item| match item {
            Item::ImplBlock(imp) => {
                let target = match &imp.target_type.kind {
                    TypeKind::Path(p) => p.segments.last().map(String::as_str),
                    _ => None,
                };
                if target != Some(type_name) {
                    return false;
                }
                imp.items.iter().any(|it| match it {
                    ImplItem::Method(m) => {
                        m.name == method_name
                            && m.self_param.is_none()
                            && returns_self_or_type(m.return_type.as_ref(), type_name)
                    }
                    _ => false,
                })
            }
            _ => false,
        });
        if !is_self_returning_ctor {
            return Vec::new();
        }
        self.method_invariants_to_check(type_name, method_name)
    }

    /// The base type's name for a refinement (`type Email = String where …`
    /// → `"String"`), used to cast a refined value to its base
    /// representation before the predicate check. `None` for non-refinements.
    pub(crate) fn refinement_base_name(&self, name: &str) -> Option<String> {
        self.program.items.iter().find_map(|item| match item {
            Item::TypeAlias(t) if t.name == name && t.refinement.is_some() => match &t.ty.kind {
                TypeKind::Path(p) => p.segments.last().cloned(),
                _ => None,
            },
            _ => None,
        })
    }

    /// Evaluate a refinement predicate against a candidate value, with `self`
    /// bound to the value in a fresh scope (phase-9 step 5b). Returns the
    /// predicate's boolean result, or `None` if evaluation didn't yield a
    /// bool (a malformed predicate the typechecker should have rejected).
    pub(crate) fn eval_refinement_predicate(&mut self, pred: &Expr, value: Value) -> Option<bool> {
        self.env.push_scope();
        self.env.define("self".to_string(), value);
        let result = self.eval_expr_inner(pred);
        self.env.pop_scope();
        match result {
            Value::Bool(b) => Some(b),
            _ => None,
        }
    }

    /// Like [`record_runtime_error`] but also captures the left/right values
    /// that drove the failure. Used by the `assert_eq` / `assert_ne` builtins
    /// so the test runner can surface them in structured fail events.
    fn record_runtime_assertion(
        &mut self,
        message: impl Into<String>,
        left: String,
        right: String,
        span: &Span,
    ) -> Value {
        self.runtime_errors.push(RuntimeError {
            message: message.into(),
            span: span.clone(),
            left: Some(left),
            right: Some(right),
        });
        self.push_error_trace(span.line, span.column);
        self.pending_cf = Some(ControlFlow::RuntimeError);
        Value::Unit
    }

    /// Set the source filename used in error trace frames.
    pub fn set_source_filename(&mut self, filename: &str) {
        self.source_filename = filename.to_string();
    }

    /// Set the program's full source text. Used by `dbg()` to slice
    /// the argument's source span into the `expr` field. The CLI
    /// supplies this from the file already read into memory.
    pub fn set_source_text(&mut self, src: &str) {
        self.source_text = src.to_string();
    }

    /// Select `dbg()` output format. Defaults to [`DbgOutputMode::Terminal`].
    pub fn set_dbg_output_mode(&mut self, mode: DbgOutputMode) {
        self.dbg_output_mode = mode;
    }

    /// Get the error return trace frames collected during execution.
    pub fn error_trace(&self) -> &[ErrorTraceFrame] {
        &self.error_trace
    }

    /// Whether the trace ring buffer overflowed and oldest entries were dropped.
    pub fn error_trace_truncated(&self) -> bool {
        self.error_trace_truncated
    }

    /// Push a frame to the error trace ring buffer (max 64 entries).
    fn push_error_trace(&mut self, line: usize, column: usize) {
        if self.error_trace.len() >= ERROR_TRACE_MAX_DEPTH {
            self.error_trace.remove(0);
            self.error_trace_truncated = true;
        }
        self.error_trace.push(ErrorTraceFrame {
            file: self.source_filename.clone(),
            line,
            column,
            expr: String::new(),
        });
    }

    /// Clear the error trace (called when ? encounters Ok/Some).
    fn clear_error_trace(&mut self) {
        self.error_trace.clear();
        self.error_trace_truncated = false;
    }

    fn track_effect(&mut self, effect: &str) {
        self.tracked_effects.push(effect.to_string());
    }

    /// Push an empty provider frame. Paired with [`pop_provider_frame`].
    fn push_provider_frame(&mut self) {
        self.provider_stack.push(HashMap::new());
    }

    /// Pop the topmost provider frame. Invariant: base frame (index 0) is
    /// installed by [`register_items`] and never popped.
    fn pop_provider_frame(&mut self) {
        debug_assert!(
            self.provider_stack.len() > 1,
            "cannot pop base provider frame"
        );
        self.provider_stack.pop();
    }

    /// Bind a provider value to a resource name in the topmost frame.
    fn bind_provider(&mut self, resource: String, provider: Value) {
        if let Some(frame) = self.provider_stack.last_mut() {
            frame.insert(resource, Arc::new(provider));
        }
    }

    /// Top-down lookup for a provider bound to the given resource name.
    /// Returns `None` if no frame has a binding — the runtime raises a
    /// runtime error at the call site (design.md § Provider-Rooted
    /// Resources: ambient defaults are installed for program-rooted
    /// resources so only `effect resource R: Trait` without an active
    /// `with_provider` should miss).
    fn lookup_provider(&self, resource: &str) -> Option<Arc<Value>> {
        self.provider_stack
            .iter()
            .rev()
            .find_map(|frame| frame.get(resource).cloned())
    }

    /// Check if there's a pending control flow signal. If so, return early.
    fn check_cf(&self) -> bool {
        self.pending_cf.is_some()
    }

    /// Register top-level items so [`run_test_function`] can subsequently
    /// invoke individual test functions by name. Idempotent only in the
    /// sense that callers should invoke it exactly once per `Interpreter`
    /// instance before any [`run_test_function`] calls — invoking it twice
    /// would re-register every item.
    ///
    /// Used by `karac test`, which calls [`register_for_tests`] once per
    /// module then drives a sequence of [`run_test_function`] calls — one
    /// per discovered `test_*` function.
    pub fn register_for_tests(&mut self) {
        self.register_items();
    }

    /// Push a provider frame and bind `resource → provider` in it.
    /// Paired with [`test_pop_provider_frame`]. Used by the test runner
    /// to install `#[with_provider(R, ...)]` fixtures via the same
    /// frame primitive hand-written `with_provider` / `providers { }`
    /// scopes use. See design.md § `#[with_provider]` fixture
    /// ("runner uses the interpreter's frame-push/pop primitive
    /// directly — no AST rewrite").
    pub fn test_push_provider(&mut self, resource: String, provider: Value) {
        self.push_provider_frame();
        self.bind_provider(resource, provider);
    }

    /// Pop the topmost provider frame. Matches each
    /// [`test_push_provider`] call.
    pub fn test_pop_provider_frame(&mut self) {
        self.pop_provider_frame();
    }

    /// Evaluate an expression for use as a test provider constructor.
    /// Returns `Ok(value)` on success, `Err(message)` if the expression
    /// raised a runtime error or any control-flow signal (exit, return,
    /// panic). The caller is responsible for draining error state before
    /// the next test — the method does not roll back on failure because
    /// the runner uses [`reset_test_state`] per test anyway.
    pub fn test_eval_provider_constructor(&mut self, expr: &Expr) -> Result<Value, String> {
        let errors_before = self.runtime_errors.len();
        let had_pending = self.pending_cf.is_some();
        let value = self.eval_expr(expr);
        if self.runtime_errors.len() > errors_before {
            return Err(self.runtime_errors[errors_before].message.clone());
        }
        if !had_pending && self.pending_cf.is_some() {
            return Err("constructor did not complete normally".to_string());
        }
        Ok(value)
    }

    /// Reset per-test mutable state (`pending_cf`, `runtime_errors`,
    /// `tracked_effects`). The test runner calls this before evaluating
    /// `#[with_provider(R, ...)]` constructors so a clean slate persists
    /// whether or not constructors succeed. [`run_test_function`] already
    /// performs the same reset on entry, so calling both is harmless; the
    /// separate method exists so the runner can reset *before* the
    /// interpreter is handed a constructor expression.
    pub fn reset_test_state(&mut self) {
        self.pending_cf = None;
        self.runtime_errors.clear();
        self.tracked_effects.clear();
    }

    /// Invoke a previously-registered top-level function as a test and
    /// report whether it passed. Resets per-test mutable state
    /// (`pending_cf`, `runtime_errors`, `tracked_effects`) so each test
    /// runs from a clean slate, then dispatches into [`call_function`]
    /// and inspects [`runtime_errors`] for failure details. The first
    /// recorded `RuntimeError` becomes the [`TestOutcome::message`]; any
    /// `left` / `right` payload set by `assert_eq` / `assert_ne` flows
    /// through unchanged.
    pub fn run_test_function(&mut self, name: &str) -> TestOutcome {
        self.pending_cf = None;
        self.runtime_errors.clear();
        self.tracked_effects.clear();

        let _ = self.call_function(name, &[]);
        // Drain any pending unwind so the next test starts clean. We don't
        // act on the unwind variant here — RuntimeError populated
        // `runtime_errors`, and ExitUnwind from a test means the test
        // body called `process::exit`, which we treat as a failure.
        let unwind = self.pending_cf.take();

        // The `timed_out` flag is the runner's discriminator between a
        // normal-completed test and a timeout. Check first, ahead of
        // runtime_errors / ExitUnwind so a deadline observed during
        // recovery from a user error still surfaces as a timeout
        // (rather than a misleading "test failed" with the cleanup-
        // time runtime_error). The runner inspects `self.timed_out`
        // after this returns to emit `test_timeout` JSONL.
        if self.timed_out {
            return TestOutcome {
                passed: false,
                message: Some("test exceeded its per-test timeout".to_string()),
                span: None,
                left: None,
                right: None,
            };
        }
        if let Some(err) = self.runtime_errors.first().cloned() {
            return TestOutcome {
                passed: false,
                message: Some(err.message),
                span: Some(err.span),
                left: err.left,
                right: err.right,
            };
        }
        if let Some(ControlFlow::ExitUnwind { code }) = unwind {
            return TestOutcome {
                passed: false,
                message: Some(format!("test called process::exit({})", code)),
                span: None,
                left: None,
                right: None,
            };
        }
        TestOutcome {
            passed: true,
            message: None,
            span: None,
            left: None,
            right: None,
        }
    }

    /// Run the program: register top-level items, then call main().
    pub fn run(&mut self) -> Value {
        self.register_items();
        // Look for main()
        if self.env.get("main").is_some() {
            let result = self.call_function("main", &[]);
            // Handle ExitUnwind from process::exit(). Runtime errors also
            // drain pending_cf here; the errors themselves are in
            // `self.runtime_errors` for callers to inspect.
            match self.pending_cf.take() {
                Some(ControlFlow::ExitUnwind { code }) => std::process::exit(code),
                Some(ControlFlow::RuntimeError) => Value::Unit,
                _ => result,
            }
        } else {
            Value::Unit
        }
    }

    /// Register all top-level functions, structs, enums in the environment.
    ///
    /// `pub(crate)` so the comptime fold pass (`crate::comptime`) can prime
    /// a freshly-constructed interpreter with the same global environment
    /// `run()` builds before evaluating a `comptime { ... }` block at
    /// compile time — without invoking `main()`.
    pub(crate) fn register_items(&mut self) {
        // Register prelude variants
        self.env.define(
            "None".to_string(),
            Value::EnumVariant {
                enum_name: "Option".to_string(),
                variant: "None".to_string(),
                data: EnumData::Unit,
            },
        );

        // Register built-in comparison-Ordering enum variants
        // (Less / Equal / Greater — returned by `Ord.cmp`).
        //
        // Bind BOTH the qualified (`Ordering.Less`) and bare (`Less`) names.
        // The pattern matcher classifies a bare PascalCase identifier as a
        // unit-variant pattern only when `env.get(name)` returns a unit
        // `EnumVariant` (pattern_match.rs) — the same mechanism user-enum unit
        // variants rely on. Registering ONLY the qualified name made bare
        // `match o { Less => … }` fall through to a catch-all binding, so the
        // first arm always matched: `match x.cmp(y) { Less/Equal/Greater }`
        // silently returned Less under `karac run` while codegen was correct —
        // a run/build divergence for every `.cmp()` consumer, int and String
        // alike (bug-ledger B-2026-06-30-14). MemoryOrdering shared the bug.
        for variant in ["Less", "Equal", "Greater"] {
            let value = Value::EnumVariant {
                enum_name: "Ordering".to_string(),
                variant: variant.to_string(),
                data: EnumData::Unit,
            };
            self.env
                .define(format!("Ordering.{}", variant), value.clone());
            self.env.define(variant.to_string(), value);
        }
        // Register built-in MemoryOrdering enum variants
        // (Relaxed / Acquire / Release / AcqRel / SeqCst — used by Atomic[T]).
        for variant in ["Relaxed", "Acquire", "Release", "AcqRel", "SeqCst"] {
            let value = Value::EnumVariant {
                enum_name: "MemoryOrdering".to_string(),
                variant: variant.to_string(),
                data: EnumData::Unit,
            };
            self.env
                .define(format!("MemoryOrdering.{}", variant), value.clone());
            self.env.define(variant.to_string(), value);
        }

        // Ambient program-rooted resources: register the names and install
        // a default provider in the base frame so `Clock.now()` etc. resolve
        // without any `with_provider` wrapping (design.md § Provider-Rooted
        // Resources "Scope of the rule"). The default provider is a
        // zero-field `Value::Struct` whose name encodes the resource;
        // `eval_resource_method` recognizes the `BuiltinDefault` prefix and
        // dispatches to a Rust handler.
        for name in crate::prelude::PRELUDE_EFFECT_RESOURCES {
            self.effect_resources.insert((*name).to_string());
            let default_provider = Value::Struct {
                name: format!("BuiltinDefault{}", name),
                fields: HashMap::new(),
            };
            self.bind_provider((*name).to_string(), default_provider);
        }

        // Register impl-method bodies from baked stdlib source. The
        // typechecker reads these via `register_baked_stdlib`; the
        // interpreter does the same dispatch through `Value::Function`
        // entries keyed by `Type.method`. Methods carrying
        // `#[compiler_builtin]` are skipped — their bodies are
        // placeholders, and the real dispatch lives in the path-string
        // match earlier in `eval_call` (`Stats.sum`, `Regex.compile`, …)
        // which fires before the env.get(method_key) fallback. Baked is
        // registered before user items so user-program impls can
        // override via last-write-wins on env.define.
        let baked_items: Vec<Item> = crate::prelude::STDLIB_PROGRAMS
            .iter()
            .flat_map(|(_, p)| p.items.iter().cloned())
            .collect();
        for item in &baked_items {
            match item {
                Item::ImplBlock(imp) => {
                    self.register_impl_methods(imp, /* skip_compiler_builtin = */ true);
                }
                // Register baked-stdlib `comptime fn` free functions so a
                // stdlib-provided derive (e.g. `derive_message` for
                // `#[derive(Message)]`) is callable from the comptime
                // expansion pass — it invokes the derive fn by name through
                // this same interpreter env. Only `comptime` free fns are
                // registered (never runtime-callable; the typechecker rejects
                // calling them at runtime), keeping the baked runtime surface
                // unchanged. User items register afterward and win ties.
                Item::Function(f) if f.is_comptime => {
                    let val = Value::Function {
                        name: f.name.clone(),
                        param_patterns: f.params.iter().map(|p| p.pattern.clone()).collect(),
                        param_defaults: f.params.iter().map(|p| p.default_value.clone()).collect(),
                        body: f.body.clone(),
                        closure_env: None,
                    };
                    self.env.define(f.name.clone(), val);
                }
                // Register baked-stdlib enum unit variants under their
                // qualified path (e.g. `IoError.NotFound`, `VarError.NotPresent`)
                // so they can be used as expressions, peer to the
                // hand-registered `Ordering.Less` / `MemoryOrdering.Relaxed`
                // entries above. Without this, baked enums are pattern-only
                // (match arms work because the resolver tags them, but
                // construction `let e = IoError.NotFound` fails the
                // env.get(path) lookup in `eval_path`). Tuple/struct variants
                // are handled at call sites, same as user-program enums.
                Item::EnumDef(e) => {
                    for variant in &e.variants {
                        if let VariantKind::Unit = variant.kind {
                            self.env.define(
                                format!("{}.{}", e.name, variant.name),
                                Value::EnumVariant {
                                    enum_name: e.name.clone(),
                                    variant: variant.name.clone(),
                                    data: EnumData::Unit,
                                },
                            );
                        }
                    }
                }
                // Record baked-stdlib `distinct type`s (e.g. `ExitCode`)
                // so `is_distinct_type` recognizes their constructors —
                // `program.items` carries only the user program, not the
                // baked sources. Slice B.
                Item::DistinctType(d) => {
                    self.stdlib_distinct_types.insert(d.name.clone());
                }
                _ => {}
            }
        }

        let items: Vec<Item> = self.program.items.clone();
        for item in &items {
            if let Item::EffectResource(r) = item {
                self.effect_resources.insert(r.name.clone());
            }
        }
        for item in &items {
            match item {
                Item::Function(f) => {
                    let val = Value::Function {
                        name: f.name.clone(),
                        param_patterns: f.params.iter().map(|p| p.pattern.clone()).collect(),
                        param_defaults: f.params.iter().map(|p| p.default_value.clone()).collect(),
                        body: f.body.clone(),
                        closure_env: None,
                    };
                    self.env.define(f.name.clone(), val);
                }
                Item::EnumDef(e) => {
                    // Register unit variants as values, tuple/struct variants as constructor functions
                    for variant in &e.variants {
                        match &variant.kind {
                            VariantKind::Unit => {
                                self.env.define(
                                    variant.name.clone(),
                                    Value::EnumVariant {
                                        enum_name: e.name.clone(),
                                        variant: variant.name.clone(),
                                        data: EnumData::Unit,
                                    },
                                );
                            }
                            _ => {
                                // Tuple/struct variants are handled at call sites
                            }
                        }
                    }
                }
                Item::ConstDecl(c) => {
                    let val = self.eval_expr_inner(&c.value);
                    self.env.define(c.name.clone(), val);
                }
                Item::ImplBlock(imp) => {
                    self.register_impl_methods(imp, /* skip_compiler_builtin = */ false);
                }
                _ => {}
            }
        }
    }

    /// Walk an impl block's methods and register each as a `Value::Function`
    /// keyed by `Type.method` in the interpreter env. Used by both the user-
    /// program walk and the baked-stdlib walk.
    ///
    /// `skip_compiler_builtin` is `true` for baked source: methods marked
    /// `#[compiler_builtin]` carry placeholder bodies, and dispatch for those
    /// happens in `eval_call`'s path-string match (`Stats.sum`, …) which
    /// fires before the env.get(method_key) fallback. Registering the
    /// placeholder body would still work — the real path-string match
    /// shadows it — but skipping is cleaner.
    fn register_impl_methods(&mut self, imp: &ImplBlock, skip_compiler_builtin: bool) {
        let type_name = match &imp.target_type.kind {
            TypeKind::Path(p) => p.segments.last().cloned().unwrap_or_default(),
            _ => return,
        };
        for item in &imp.items {
            let ImplItem::Method(method) = item else {
                continue;
            };
            if skip_compiler_builtin
                && method
                    .attributes
                    .iter()
                    .any(|a| a.is_bare("compiler_builtin"))
            {
                continue;
            }
            let method_key = format!("{}.{}", type_name, method.name);
            // For methods with a receiver, prepend a `self` binding pattern
            // so the unified Call dispatch can bind args[0] to `self` without
            // special-casing. Associated functions (no self_param) stay as-is.
            let mut patterns: Vec<Pattern> = Vec::new();
            if method.self_param.is_some() {
                patterns.push(Pattern {
                    span: method.span.clone(),
                    kind: PatternKind::Binding("self".to_string()),
                });
            }
            patterns.extend(method.params.iter().map(|p| p.pattern.clone()));
            // `self` has no default; align defaults with the extended
            // pattern list (None for the self slot).
            let mut defaults: Vec<Option<crate::ast::Expr>> = Vec::new();
            if method.self_param.is_some() {
                defaults.push(None);
            }
            defaults.extend(method.params.iter().map(|p| p.default_value.clone()));
            let val = Value::Function {
                name: method.name.clone(),
                param_patterns: patterns,
                param_defaults: defaults,
                body: method.body.clone(),
                closure_env: None,
            };
            self.env.define(method_key, val);
        }
    }

    /// Evaluate a user-level callable body — function, method, closure, or
    /// user `drop` — growing the Rust stack when the recursion nears the
    /// current segment's end. The tree-walk costs ~8 Rust frames per Kāra
    /// call, so deep-but-conforming Kāra recursion (one frame per node over
    /// a 5000-node list — LeetCode's linked-list bound, found by kata #25)
    /// exhausts the 16 MB interpreter thread stack (`run_on_interp_thread`,
    /// `main.rs`) long before the same program's AOT binary feels it.
    /// `stacker::maybe_grow` is a stack-pointer check until the 128 KiB red
    /// zone is hit, then re-homes the recursion onto a fresh 4 MiB heap
    /// segment — depth becomes heap-bounded, matching the AOT story instead
    /// of cliffing at a fixed thread-stack size, and it covers `par { }`
    /// worker threads (whatever their stack size) for free. Wrapping the
    /// *body* rather than `eval_expr_inner` keeps the check off the
    /// per-expression hot path: Rust-stack depth only compounds through
    /// Kāra-level calls. Every body-invocation site must route through here
    /// — a site that calls `eval_block_inner` directly re-opens the cliff
    /// for whatever callable shape it implements. wasm32 (browser
    /// playground) falls back to a plain call: psm has no stack-switching
    /// support there.
    #[allow(clippy::result_large_err)]
    pub(crate) fn eval_body_growing(&mut self, body: &Block) -> Result<Value, ControlFlow> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            // Red zone must exceed ONE Kāra call's worst-case Rust-stack
            // bill — the check only runs at call boundaries, so the chain
            // between two checks (eval_call → eval_expr_inner → eval_stmt
            // → … , several giant match-on-AST frames) must fit inside it.
            // Debug builds bill ~200+ KiB per call (the documented fib(10)
            // 2 MB Windows overflow ⇒ ≥ 200 KiB/frame-chain); 2 MiB gives
            // ~10× headroom. 32 MiB segments amortize the switch cost:
            // release frames (~3 KiB/call) fit thousands of calls per
            // segment, debug fits ~100+.
            stacker::maybe_grow(2 * 1024 * 1024, 32 * 1024 * 1024, || {
                self.eval_block_inner(body)
            })
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.eval_block_inner(body)
        }
    }

    fn call_function(&mut self, name: &str, args: &[Value]) -> Value {
        let func = self.env.get(name);
        let func_variant = func
            .as_ref()
            .map(|v| v.variant_name())
            .unwrap_or("<unbound>");
        match func {
            Some(Value::Function {
                param_patterns,
                body,
                closure_env,
                ..
            }) => {
                self.env.push_scope();
                if let Some(ref captured) = closure_env {
                    for (k, v) in captured {
                        self.env.define(k.clone(), v.clone());
                    }
                }
                for (i, pat) in param_patterns.iter().enumerate() {
                    if let Some(val) = args.get(i) {
                        self.bind_pattern(pat, val.clone());
                    }
                }
                let result = self.eval_body_growing(&body);
                self.env.pop_scope();
                match result {
                    Ok(v) => v,
                    Err(ControlFlow::Return(v)) => v,
                    Err(ControlFlow::Break { .. }) => {
                        unreachable!("break outside of loop; should be caught by resolver")
                    }
                    Err(ControlFlow::Continue { .. }) => {
                        unreachable!("continue outside of loop; should be caught by resolver")
                    }
                    Err(
                        cf @ (ControlFlow::ExitUnwind { .. }
                        | ControlFlow::RuntimeError
                        | ControlFlow::Cancelled
                        | ControlFlow::TimedOut),
                    ) => {
                        // Propagate unwind up the stack (defers already ran in eval_block_inner)
                        self.pending_cf = Some(cf);
                        Value::Unit
                    }
                }
            }
            _ => unreachable!(
                "internal call_function('{}') found Value::{} not Function; \
                 the interpreter bound the wrong variant in env or the entry was overwritten",
                name, func_variant
            ),
        }
    }

    // ── Expression evaluation ───────────────────────────────────

    /// Public API: evaluate an expression (panics on control flow signals).
    pub fn eval_expr(&mut self, expr: &Expr) -> Value {
        self.eval_expr_inner(expr)
    }

    // ── Block & Statement evaluation ──────────────────────────────

    /// Push a generic-param substitution frame for the call at `span` if the
    /// typechecker recorded one. Each entry is fully resolved against the
    /// current top-of-stack frame so transitive bindings (`make`'s `T → "T"`
    /// where the caller's `T` is `"Wrapper"`) flatten to a concrete type
    /// before the callee body executes. Returns true when a frame was
    /// pushed; the call site uses that to know whether to pop.
    fn push_type_subs_for_call(&mut self, span: &Span) -> bool {
        let frame = self
            .typecheck_result
            .call_type_subs
            .get(&crate::resolver::SpanKey::from_span(span));
        let frame = match frame {
            Some(f) => f,
            None => return false,
        };
        let mut resolved: HashMap<String, String> = HashMap::new();
        for (name, target) in frame {
            // Transitively resolve the target through the current top frame
            // (parent's bindings) so abstract-name propagation collapses to
            // the concrete dispatch target by the time the callee body runs.
            let mut current = target.clone();
            for _ in 0..16 {
                let next = self
                    .type_subs_stack
                    .last()
                    .and_then(|f| f.get(&current).cloned());
                match next {
                    Some(n) if n != current => current = n,
                    _ => break,
                }
            }
            resolved.insert(name.clone(), current);
        }
        self.type_subs_stack.push(resolved);
        true
    }

    /// Look up `name` through the runtime type-substitution stack from top
    /// to bottom and return the resolved concrete type name when found.
    /// Returns `None` if `name` is not bound in any visible frame.
    pub(crate) fn resolve_type_param(&self, name: &str) -> Option<String> {
        for frame in self.type_subs_stack.iter().rev() {
            if let Some(v) = frame.get(name) {
                return Some(v.clone());
            }
        }
        None
    }

    // ── Helpers ─────────────────────────────────────────────────

    fn is_truthy(&self, val: &Value) -> bool {
        match val {
            Value::Bool(b) => *b,
            _ => unreachable!(
                "condition was Value::{} not Bool; \
                 either an interpreter codepath produced the wrong variant \
                 or the typechecker accepted a non-Bool condition",
                val.variant_name()
            ),
        }
    }

    fn set_cf(&mut self, cf: ControlFlow) -> Value {
        self.pending_cf = Some(cf);
        Value::Unit
    }

    fn value_type_name(&self, val: &Value) -> String {
        match val {
            Value::Struct { name, .. } => name.clone(),
            Value::SharedStruct(inner) => inner.name.clone(),
            Value::EnumVariant { enum_name, .. } => enum_name.clone(),
            Value::Int(_) => "i64".to_string(),
            Value::Float(_) => "f64".to_string(),
            Value::Bool(_) => "bool".to_string(),
            Value::String(_) => "String".to_string(),
            Value::Char(_) => "char".to_string(),
            Value::TotalFloat32(_) => "F32".to_string(),
            Value::TotalFloat64(_) => "F64".to_string(),
            Value::Atomic(_) => "Atomic".to_string(),
            Value::Set(_) => "Set".to_string(),
            _ => "unknown".to_string(),
        }
    }

    fn find_struct_def(&self, name: &str) -> Option<&StructDef> {
        for item in &self.program.items {
            if let Item::StructDef(s) = item {
                if s.name == name {
                    return Some(s);
                }
            }
        }
        None
    }

    fn find_enum_for_variant(&self, variant_name: &str) -> Option<String> {
        for item in &self.program.items {
            if let Item::EnumDef(e) = item {
                for v in &e.variants {
                    if v.name == variant_name {
                        return Some(e.name.clone());
                    }
                }
            }
        }
        None
    }

    /// For a qualified path `Enum.Variant`, return `Some(is_unit)` when
    /// `Enum` is a known enum — in the user program OR a baked-stdlib program
    /// (`Result`, `Option`, `IoError`, …) — that declares `Variant`, where
    /// `is_unit` distinguishes a payload-free unit variant from a tuple /
    /// struct one. `None` when the pair isn't an enum variant, so callers can
    /// fall through to method / associated-fn dispatch. Backs qualified
    /// enum-variant construction (`Result.Ok(x)`, `Color.Blue(7)`,
    /// `Option.Some(v)`) — a form the resolver and codegen accept but the
    /// interpreter otherwise can't evaluate as a callee path. The generic
    /// peer of the hand-rolled `Ordering.*` / `Json.*` arms in `eval_call`.
    fn qualified_enum_variant_is_unit(&self, enum_name: &str, variant: &str) -> Option<bool> {
        fn scan(items: &[Item], enum_name: &str, variant: &str) -> Option<bool> {
            items.iter().find_map(|item| match item {
                Item::EnumDef(e) if e.name == enum_name => e
                    .variants
                    .iter()
                    .find(|v| v.name == variant)
                    .map(|v| matches!(v.kind, VariantKind::Unit)),
                _ => None,
            })
        }
        scan(&self.program.items, enum_name, variant).or_else(|| {
            crate::prelude::STDLIB_PROGRAMS
                .iter()
                .find_map(|(_, p)| scan(&p.items, enum_name, variant))
        })
    }

    /// For a qualified `Enum.Variant`, return the variant's declared
    /// struct-field names in declaration order when `Variant` is a
    /// struct-shaped variant of the (user or baked-stdlib) enum `Enum`;
    /// `None` otherwise. Drives source-level enum struct-variant
    /// construction (`AllocError.OutOfMemory { requested_bytes: n }`) so it
    /// builds a `Value::EnumVariant` (with `EnumData::Struct` in declared
    /// order, for deterministic Display + structural `==`) rather than
    /// falling through to a plain `Value::Struct`.
    fn qualified_enum_struct_variant_field_order(
        &self,
        enum_name: &str,
        variant: &str,
    ) -> Option<Vec<String>> {
        fn scan(items: &[Item], enum_name: &str, variant: &str) -> Option<Vec<String>> {
            items.iter().find_map(|item| match item {
                Item::EnumDef(e) if e.name == enum_name => {
                    e.variants.iter().find(|v| v.name == variant).and_then(|v| {
                        if let VariantKind::Struct(fields) = &v.kind {
                            Some(fields.iter().map(|f| f.name.clone()).collect())
                        } else {
                            None
                        }
                    })
                }
                _ => None,
            })
        }
        scan(&self.program.items, enum_name, variant).or_else(|| {
            crate::prelude::STDLIB_PROGRAMS
                .iter()
                .find_map(|(_, p)| scan(&p.items, enum_name, variant))
        })
    }

    /// Read a field from a struct value. Out of line from `eval_expr_inner`
    /// to keep the recursive evaluator's stack frame small.
    fn read_field(&mut self, obj: Value, field: &str, span: &Span) -> Value {
        let obj_variant = obj.variant_name();
        match obj {
            Value::Struct { fields, name } => fields.get(field).cloned().unwrap_or_else(|| {
                unreachable!(
                    "field '{}' not found on struct '{}' at {}:{}; \
                     either an interpreter codepath constructed the struct without this field \
                     or the typechecker accepted access to a missing field",
                    field, name, span.line, span.column
                )
            }),
            Value::SharedStruct(inner) => {
                if let Some(v) = inner.immutable_fields.get(field) {
                    return v.clone();
                }
                if let Some(cell) = inner.mut_fields.get(field) {
                    // Spec: reads are shared; multiple simultaneous
                    // readers OK. `try_read` fails iff a writer is
                    // active — runtime panic on conflict.
                    match cell.value.try_read() {
                        Ok(guard) => return guard.clone(),
                        Err(_) => {
                            return self.record_runtime_error(
                                format!(
                                    "shared struct field '{}.{}' read while a write borrow is active",
                                    inner.name, field
                                ),
                                span,
                            );
                        }
                    }
                }
                // Weak field reads are the upgrade point per spec
                // § Shared Types — Weak references. Yields
                // `Some(strong_ref)` if the referent is alive (RC
                // bumped), or `None` if it has been deallocated.
                if let Some(weak) = inner.weak_immutable_fields.get(field) {
                    return upgrade_weak_to_option(weak);
                }
                if let Some(slot) = inner.weak_mut_fields.get(field) {
                    match slot.try_read() {
                        Ok(guard) => return upgrade_weak_to_option(&guard),
                        Err(_) => {
                            return self.record_runtime_error(
                                format!(
                                    "shared struct field '{}.{}' read while a write borrow is active",
                                    inner.name, field
                                ),
                                span,
                            );
                        }
                    }
                }
                unreachable!(
                    "field '{}' not found on shared struct '{}' at {}:{}; \
                     either an interpreter codepath constructed the SharedStruct without this field \
                     or the typechecker accepted access to a missing field",
                    field, inner.name, span.line, span.column
                )
            }
            _ => unreachable!(
                "field access at {}:{}: receiver was Value::{} not Struct/SharedStruct; \
                 either an interpreter codepath produced the wrong variant \
                 or the typechecker accepted field access on a non-struct",
                span.line, span.column, obj_variant
            ),
        }
    }

    /// Build a struct literal value, dispatching on `is_shared` from the
    /// struct's definition. Out of line from `eval_expr_inner` to keep
    /// the recursive evaluator's stack frame small (debug builds default
    /// test stack is 2 MB; deep `fib`-style recursion is sensitive).
    fn eval_struct_literal(
        &mut self,
        path: &[String],
        fields: &[FieldInit],
        spread: Option<&Expr>,
    ) -> Value {
        let name = path.last().cloned().unwrap_or_default();
        let mut field_vals: HashMap<String, Value> = HashMap::new();
        // Weak handles encountered via spread or explicit field init are
        // routed here so the construction step can re-store them as
        // `Weak<SharedStructInner>` without an upgrade-then-downgrade
        // round trip (which would lose the "is the referent alive"
        // signal at the spread point).
        let mut weak_overrides: HashMap<String, std::sync::Weak<SharedStructInner>> =
            HashMap::new();
        if let Some(spread_expr) = spread {
            match self.eval_expr_inner(spread_expr) {
                Value::Struct {
                    fields: base_fields,
                    ..
                } => {
                    field_vals = base_fields;
                }
                Value::SharedStruct(inner) => {
                    for (k, v) in &inner.immutable_fields {
                        field_vals.insert(k.clone(), v.clone());
                    }
                    for (k, cell) in &inner.mut_fields {
                        let v = cell.value.try_read().expect(
                            "shared struct field write-locked during spread — unreachable in single-task interpreter",
                        );
                        field_vals.insert(k.clone(), v.clone());
                    }
                    for (k, weak) in &inner.weak_immutable_fields {
                        weak_overrides.insert(k.clone(), weak.clone());
                    }
                    for (k, slot) in &inner.weak_mut_fields {
                        let weak = slot.try_read().expect(
                            "shared struct weak field write-locked during spread — unreachable in single-task interpreter",
                        );
                        weak_overrides.insert(k.clone(), weak.clone());
                    }
                }
                _ => {}
            }
        }
        for field in fields {
            let val = self.eval_expr_inner(&field.value);
            field_vals.insert(field.name.clone(), val);
        }
        // Enum struct-variant construction `Enum.Variant { field: val, ... }`:
        // when the qualifier names a known enum whose `Variant` is
        // struct-shaped, build a `Value::EnumVariant` (payload in declared
        // field order) instead of falling through to a plain `Value::Struct`.
        // Keeps construction consistent with unit/tuple variants so `==`,
        // `match`, and Display all behave (the typechecker routes the same
        // shape via `infer_enum_struct_variant_literal`).
        if path.len() >= 2 {
            let enum_name = &path[path.len() - 2];
            if let Some(order) = self.qualified_enum_struct_variant_field_order(enum_name, &name) {
                // `EnumData::Struct` is keyed by field name (order-independent
                // for `==`); pull each declared field from the evaluated set.
                let mut data_fields: HashMap<String, Value> = HashMap::new();
                for fname in order {
                    let v = field_vals.remove(&fname).unwrap_or(Value::Unit);
                    data_fields.insert(fname, v);
                }
                return Value::EnumVariant {
                    enum_name: enum_name.clone(),
                    variant: name,
                    data: EnumData::Struct(data_fields),
                };
            }
        }
        // Unqualified struct-variant construction `Variant { field: val, ... }`:
        // the single-segment path carries only the bare variant name. When it
        // isn't a struct, find the enum that declares it (variant names are
        // globally unique once the resolver has bound them) and build a
        // `Value::EnumVariant` — otherwise the value falls through to a
        // `Value::Struct { name: "Variant" }`, whose `value_type_name` is the
        // variant, not the enum, so method dispatch on the binding reports
        // "method not found on type 'Variant'" even when annotated. Mirrors the
        // qualified branch above and the typechecker's unqualified routing
        // (B-2026-06-13-12).
        if path.len() == 1 && self.find_struct_def(&name).is_none() {
            if let Some(enum_name) = self.find_enum_for_variant(&name) {
                if let Some(order) =
                    self.qualified_enum_struct_variant_field_order(&enum_name, &name)
                {
                    let mut data_fields: HashMap<String, Value> = HashMap::new();
                    for fname in order {
                        let v = field_vals.remove(&fname).unwrap_or(Value::Unit);
                        data_fields.insert(fname, v);
                    }
                    return Value::EnumVariant {
                        enum_name,
                        variant: name,
                        data: EnumData::Struct(data_fields),
                    };
                }
            }
        }
        if let Some(def) = self.find_struct_def(&name) {
            // `par struct` is reference-semantic like `shared struct` (both are
            // RC/Arc-backed in codegen), so it must be a `SharedStruct` in the
            // interpreter too — otherwise a par struct is value-copied per
            // `ref self` call and field mutations never reach the caller.
            if def.is_shared || def.is_par {
                // A field is interior-mutable (stored in a `RwLock` cell so
                // writes persist through the shared `Arc`) if it is declared
                // `mut`, OR its type is a concurrency primitive `Atomic[T]` /
                // `Mutex[T]` — those provide interior mutability WITHOUT the
                // `mut` keyword (a `par struct`'s `count: Atomic[i64]` field is
                // mutated via `.fetch_add` / `lock`, not direct assignment).
                let mut_field_names: HashSet<String> = def
                    .fields
                    .iter()
                    .filter(|f| f.is_mut || is_interior_mutable_field_type(&f.ty))
                    .map(|f| f.name.clone())
                    .collect();
                let weak_field_names: HashSet<String> = def
                    .fields
                    .iter()
                    .filter(|f| matches!(f.ty.kind, TypeKind::Weak(_)))
                    .map(|f| f.name.clone())
                    .collect();
                let mut immutable_fields: HashMap<String, Value> = HashMap::new();
                let mut mut_fields: HashMap<String, FieldCell> = HashMap::new();
                let mut weak_immutable_fields: HashMap<String, std::sync::Weak<SharedStructInner>> =
                    HashMap::new();
                let mut weak_mut_fields: HashMap<
                    String,
                    RwLock<std::sync::Weak<SharedStructInner>>,
                > = HashMap::new();
                for (k, v) in field_vals {
                    let is_mut = mut_field_names.contains(&k);
                    let is_weak = weak_field_names.contains(&k);
                    if is_weak {
                        // Spec § Shared Types: assignment to a weak
                        // field accepts a strong reference and
                        // downgrades. Non-shared rhs is a typechecker
                        // error; we record a runtime error as
                        // defense-in-depth. Spread-only weak fields
                        // (no explicit init in the literal) are
                        // handled by the `weak_overrides` drain below.
                        let weak = match &v {
                            Value::SharedStruct(arc) => Arc::downgrade(arc),
                            _ => {
                                self.runtime_errors.push(RuntimeError {
                                    message: format!(
                                        "weak field '{}.{}' initialized with non-shared value",
                                        name, k
                                    ),
                                    span: Span::default(),
                                    left: None,
                                    right: None,
                                });
                                std::sync::Weak::new()
                            }
                        };
                        weak_overrides.insert(k, weak);
                    } else if is_mut {
                        mut_fields.insert(k, FieldCell::new(v));
                    } else {
                        immutable_fields.insert(k, v);
                    }
                }
                for (k, weak) in weak_overrides {
                    if mut_field_names.contains(&k) {
                        weak_mut_fields.insert(k, RwLock::new(weak));
                    } else {
                        weak_immutable_fields.insert(k, weak);
                    }
                }
                return Value::SharedStruct(Arc::new(SharedStructInner {
                    name,
                    immutable_fields,
                    mut_fields,
                    weak_immutable_fields,
                    weak_mut_fields,
                }));
            }
        }
        Value::Struct {
            name,
            fields: field_vals,
        }
    }

    fn set_field(&mut self, object: &Expr, field: &str, val: Value) {
        // A bare-identifier (or `self`) receiver is mutated in its env slot.
        // Plain structs are value types, so the modified struct must be
        // re-inserted; shared structs are Arc-backed and mutate in place.
        let target_name: Option<&str> = match &object.kind {
            ExprKind::Identifier(name) => Some(name.as_str()),
            ExprKind::SelfValue => Some("self"),
            _ => None,
        };
        if let Some(name) = target_name {
            match self.env.get(name) {
                Some(Value::Struct { name: sn, fields }) => {
                    let mut fields = fields;
                    fields.insert(field.to_string(), val);
                    self.env.set(name, Value::Struct { name: sn, fields });
                }
                Some(Value::SharedStruct(inner)) => {
                    self.write_shared_struct_field(&inner, field, val, &object.span);
                }
                _ => {}
            }
            return;
        }
        // Projection receiver (`a.b.field = x`, `v[i].field = x`, deeper
        // chains). Evaluate the receiver place once:
        //  - a `shared struct` is reference-semantic — a clone of the same Arc —
        //    so writing through it lands on the shared allocation directly.
        //  - a plain (value-type) struct is a *copy*; mutate the copy's field
        //    and write the whole updated struct back up the place chain via
        //    `assign_to_place`. Without this, nested writes (`o.inner.x = v`)
        //    were silently dropped.
        match self.eval_expr_inner(object) {
            Value::SharedStruct(inner) => {
                self.write_shared_struct_field(&inner, field, val, &object.span);
            }
            Value::Struct {
                name: sn,
                mut fields,
            } => {
                fields.insert(field.to_string(), val);
                self.assign_to_place(object, Value::Struct { name: sn, fields });
            }
            _ => {}
        }
    }

    /// Store `new_val` into the place denoted by `place`, recursing through
    /// `set_field` / `set_index` so nested places (`a.b.c = x`,
    /// `v[i].field = x`) write back up the whole chain. Handles every
    /// assignable place form (binding, `self`, field projection, index,
    /// `*deref`). Returns `false` if `place` is not an assignable form, so the
    /// statement-level caller can flag the should-never-happen case while
    /// recursive callers (always valid field/index places) ignore it.
    fn assign_to_place(&mut self, place: &Expr, new_val: Value) -> bool {
        match &place.kind {
            ExprKind::Identifier(name) => {
                self.env.set(name, new_val);
                true
            }
            ExprKind::SelfValue => {
                self.env.set("self", new_val);
                true
            }
            ExprKind::FieldAccess { object, field } => {
                self.set_field(object, field, new_val);
                true
            }
            ExprKind::Index { object, index } => {
                self.set_index(object, index, new_val);
                true
            }
            ExprKind::Unary {
                op: crate::ast::UnaryOp::Deref,
                operand,
            } => {
                if let ExprKind::Identifier(name) = &operand.kind {
                    // `*r = v` — `Env::set` writes through a `SharedCell` /
                    // `MapSlotRef` slot, or rebinds a plain CICO mut-ref local.
                    self.env.set(name, new_val);
                } else if let Value::MapSlotRef { map_var, key } = self.eval_expr_inner(operand) {
                    // `*<chain> = v` where the chain yields a `mut ref V` into a
                    // Map slot (`*m.entry(k).or_insert(d) += 1`). Write through
                    // to the live slot. Re-evaluating the chain is idempotent:
                    // `or_insert` inserts-if-absent, so this second pass finds
                    // the slot occupied and just hands back the same ref.
                    self.env.write_map_slot(&map_var, &key, new_val);
                }
                true
            }
            _ => false,
        }
    }

    /// Write `val` into shared-struct field `field` through the Arc-backed
    /// `inner`. The mutation is visible to every holder of the same Arc,
    /// independent of how the receiver place was reached (bare binding, field
    /// projection, or container element).
    fn write_shared_struct_field(
        &mut self,
        inner: &SharedStructInner,
        field: &str,
        val: Value,
        span: &crate::token::Span,
    ) {
        if inner.immutable_fields.contains_key(field)
            || inner.weak_immutable_fields.contains_key(field)
        {
            // Defense-in-depth: typechecker already rejects writes to non-`mut`
            // fields. If we reach here, the static check missed.
            self.record_runtime_error(
                format!(
                    "shared struct field '{}.{}' is not declared mut",
                    inner.name, field
                ),
                span,
            );
            return;
        }
        if let Some(cell) = inner.mut_fields.get(field) {
            // Spec: writes are exclusive — panic if any other borrow (read or
            // write) of the same field is active when a write begins.
            match cell.value.try_write() {
                Ok(mut guard) => {
                    *guard = val;
                }
                Err(_) => {
                    self.record_runtime_error(
                        format!(
                            "shared struct field '{}.{}' write while another borrow is active",
                            inner.name, field
                        ),
                        span,
                    );
                }
            }
        } else if let Some(slot) = inner.weak_mut_fields.get(field) {
            // Spec § Shared Types: assignment to a weak field accepts a strong
            // reference and downgrades it. `Weak::new()` (an empty weak) is the
            // safe fallback for a non-shared rhs; typechecker should reject that
            // case but record a runtime error as defense-in-depth.
            let weak = match &val {
                Value::SharedStruct(arc) => Arc::downgrade(arc),
                _ => {
                    self.record_runtime_error(
                        format!(
                            "weak field '{}.{}' assigned a non-shared value",
                            inner.name, field
                        ),
                        span,
                    );
                    std::sync::Weak::new()
                }
            };
            match slot.try_write() {
                Ok(mut guard) => {
                    *guard = weak;
                }
                Err(_) => {
                    self.record_runtime_error(
                        format!(
                            "shared struct field '{}.{}' write while another borrow is active",
                            inner.name, field
                        ),
                        span,
                    );
                }
            }
        } else {
            unreachable!(
                "shared struct field '{}.{}' not found at {}:{}; \
                 either an interpreter codepath constructed the SharedStruct without this field \
                 or the typechecker accepted assignment to a missing field",
                inner.name, field, span.line, span.column
            );
        }
    }

    fn set_index(&mut self, object: &Expr, index: &Expr, val: Value) {
        let idx_val = self.eval_expr_inner(index);
        // Phase-11 Tensor element store — `t[i, j] = v`. The index is a
        // tuple (parser desugar) or a bare Int for rank-1; the target's
        // `Arc<RwLock<...>>` storage aliases the binding's, so the write
        // lands regardless of how the receiver was reached.
        if let Value::Tensor { dims, data } = {
            // Resolve the receiver the same way as the container arms
            // below: identifier lookup or expression eval.
            match &object.kind {
                ExprKind::Identifier(name) => match self.env.get(name) {
                    Some(v) => v,
                    None => return,
                },
                _ => self.eval_expr_inner(object),
            }
        } {
            let Some(components) = method_call_tensor::index_components(&idx_val) else {
                self.record_runtime_error(
                    format!(
                        "tensor index must be integers (one per dim), got {}",
                        idx_val.variant_name()
                    ),
                    &index.span,
                );
                return;
            };
            match method_call_tensor::tensor_offset(&dims, &components) {
                Ok(off) => {
                    let mut guard = try_write_or_panic(&data, "<tensor>");
                    guard[off] = val;
                }
                Err(msg) => {
                    self.record_runtime_error(msg, &index.span);
                }
            }
            return;
        }
        let Value::Int(i) = idx_val else {
            return;
        };
        let i = i as usize;

        // Resolve the target Value. For a bare identifier, look it up
        // in the environment. For a nested index expression
        // (`rows[cur]` in `rows[cur][end] = val`), eval the outer
        // index — `Value::Array(rc)` clones the `Arc<RwLock<Vec<Value>>>`,
        // so a write through the returned rc IS visible at the original
        // `rows[cur]` slot. Mirrors codegen's
        // `compile_nested_vec_vec_index_store` shape: walk through to
        // the inner container's storage, then mutate at `i`.
        let target = match &object.kind {
            ExprKind::Identifier(name) => {
                let Some(v) = self.env.get(name) else {
                    return;
                };
                (v, name.clone())
            }
            ExprKind::Index { .. } => (self.eval_expr_inner(object), "<nested>".to_string()),
            // Field-access-rooted store (`h.items[i] = v`, incl. shared-
            // struct receivers): eval the field access — `Value::Array`
            // clones the `Arc<RwLock<Vec<Value>>>`, so the write through
            // the returned rc aliases the field's storage, same mechanism
            // as the nested-index arm above. Previously `_ => return`
            // swallowed this shape SILENTLY (the assignment no-op'd) —
            // surfaced by the kata-133-audit codegen fix's probe,
            // 2026-06-06.
            ExprKind::FieldAccess { .. } => (self.eval_expr_inner(object), "<field>".to_string()),
            _ => return,
        };
        let (target_value, label) = target;

        match target_value {
            Value::Array(rc) => {
                let mut guard = try_write_or_panic(&rc, &label);
                if i < guard.len() {
                    guard[i] = val;
                }
            }
            Value::Slice {
                storage,
                start,
                len,
                ..
            } => {
                let mut guard = try_write_or_panic(&storage, &label);
                if i < len {
                    guard[start + i] = val;
                }
            }
            _ => {}
        }
    }

    /// `Vec.filled(n, val)` — produce `n` clones of `val` per
    /// design.md:1631. Extracted to its own frame so the surrounding
    /// path-call match doesn't grow (debug-mode `eval_expr_inner`
    /// recursive callers are stack-budget-tight). Per-slot clone
    /// satisfies the spec's `T: Clone` requirement; negative length
    /// is a runtime error (Kāra has no usize).
    pub(crate) fn eval_vec_filled(&mut self, args: &[CallArg], span: &Span) -> Value {
        let Some(n_arg) = args.first() else {
            return self
                .record_runtime_error("Vec.filled expects 2 arguments (n, val), found 0", span);
        };
        let Some(val_arg) = args.get(1) else {
            return self
                .record_runtime_error("Vec.filled expects 2 arguments (n, val), found 1", span);
        };
        let n_val = self.eval_expr_inner(&n_arg.value);
        let val = self.eval_expr_inner(&val_arg.value);
        let Value::Int(n) = n_val else {
            return self.record_runtime_error("Vec.filled length must be i64", span);
        };
        if n < 0 {
            return self.record_runtime_error("Vec.filled length must be non-negative", span);
        }
        let len = n as usize;
        let mut items = Vec::with_capacity(len);
        for _ in 0..len {
            // `deep_clone_value` rather than `.clone()` so nested-
            // collection element types (e.g., `Vec[Vec[i64]]`) get
            // independent storage per slot — the derived `Value::Clone`
            // bumps the `Arc<RwLock<...>>` for `Value::Array`, which
            // would alias every entry to the same underlying Vec.
            items.push(deep_clone_value(&val));
        }
        Value::array_of(items)
    }

    /// `Vec.with_capacity(n: i64) -> Vec[T]` — empty Vec (len=0) with
    /// pre-allocated capacity n. Codegen relies on this for realloc-
    /// free push-N runs; here in the interpreter the underlying
    /// `Vec<Value>` honors the capacity hint, so the observable shape
    /// matches `Vec.new()` + reserve. Element type is erased at the
    /// Value layer (same as `Vec.new`).
    pub(crate) fn eval_vec_with_capacity(&mut self, args: &[CallArg], span: &Span) -> Value {
        let Some(n_arg) = args.first() else {
            return self.record_runtime_error(
                "Vec.with_capacity expects 1 argument (capacity), found 0",
                span,
            );
        };
        let n_val = self.eval_expr_inner(&n_arg.value);
        let Value::Int(n) = n_val else {
            return self.record_runtime_error("Vec.with_capacity capacity must be i64", span);
        };
        if n < 0 {
            return self
                .record_runtime_error("Vec.with_capacity capacity must be non-negative", span);
        }
        Value::array_of(Vec::with_capacity(n as usize))
    }

    /// `VecDeque[T]` mutation methods — `push_back` / `push_front` /
    /// `pop_back` / `pop_front`. Caller already verified the receiver
    /// is `Value::Array`. Extracted so its locals don't bloat
    /// `eval_method_call`'s stack frame.
    pub(crate) fn eval_vec_deque_method(
        &mut self,
        method: &str,
        obj: &Value,
        object: &Expr,
        args: &[CallArg],
    ) -> Value {
        let Value::Array(rc) = obj else {
            return Value::Unit;
        };
        let label = match &object.kind {
            ExprKind::Identifier(n) => n.clone(),
            _ => "<value>".to_string(),
        };
        match method {
            "push_back" => {
                let val = args
                    .first()
                    .map(|a| self.eval_expr_inner(&a.value))
                    .unwrap_or(Value::Unit);
                try_write_or_panic(rc, &label).push(val);
                Value::Unit
            }
            "push_front" => {
                let val = args
                    .first()
                    .map(|a| self.eval_expr_inner(&a.value))
                    .unwrap_or(Value::Unit);
                try_write_or_panic(rc, &label).insert(0, val);
                Value::Unit
            }
            "pop" | "pop_back" => {
                let popped = try_write_or_panic(rc, &label).pop();
                option_value_from(popped)
            }
            "pop_front" => {
                let mut guard = try_write_or_panic(rc, &label);
                let popped = if guard.is_empty() {
                    None
                } else {
                    Some(guard.remove(0))
                };
                option_value_from(popped)
            }
            _ => Value::Unit,
        }
    }
}

#[cfg(test)]
mod cleanup_tests {
    use super::exec::{cancelled_sentinel, ControlFlow, ExitPath};
    use super::value::{EnumData, Value};

    #[test]
    fn cancelled_cf_classifies_with_sentinel_payload() {
        // Sub-step 4 wiring: ControlFlow::Cancelled must map to
        // ExitPath::Cancelled with the Cancelled-sentinel value,
        // so errdefer(e) binds e to the sentinel during the
        // errdefer phase. Deterministic — does not rely on threading.
        let path = ExitPath::classify(&ControlFlow::Cancelled);
        match path {
            ExitPath::Cancelled(Value::EnumVariant {
                enum_name,
                variant,
                data,
            }) => {
                assert_eq!(enum_name, "Cancelled");
                assert_eq!(variant, "Cancelled");
                assert!(matches!(data, EnumData::Unit));
            }
            other => panic!("expected Cancelled(EnumVariant), got {:?}", other),
        }
    }

    #[test]
    fn cancelled_path_is_an_error_path() {
        // Drives the errdefer phase in run_cleanup. Sub-step 4 sets
        // is_error()=true so the errdefer drain executes.
        assert!(ExitPath::Cancelled(cancelled_sentinel()).is_error());
    }
}
