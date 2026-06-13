//! Single source of truth for the prelude — names that are in scope in
//! every Kāra source file without an explicit `import`.
//!
//! ## CR-24 slice 8: auto-injection mechanism
//!
//! Per `docs/design.md § Module System › Prelude`, the long-term design
//! puts stdlib types and traits in real Kāra source under `runtime/stdlib/`
//! and auto-imports them via a synthetic `import std.prelude.*;` at the top
//! of every user module. CR-24 lands only the *mechanism*:
//!
//!   1. The prelude lives at the canonical module path [`PRELUDE_PATH_SEGMENTS`]
//!      (`std.prelude`) in the program tree.
//!   2. A synthetic [`Module`] with stub [`Item`]s for every prelude name is
//!      injected into the [`ProgramTree`] by [`build_program_tree`], so
//!      cross-module resolution recognises `import std.prelude.X;` without
//!      `E0224 UnknownModule`.
//!   3. The same names are still registered directly in the resolver's global
//!      scope and the typechecker's type environment — `register_builtin_types`
//!      remains the *placeholder* implementation that backs the synthetic
//!      module's stub items. Wildcard imports (`import a.b.*;`) are deferred
//!      from CR-24, so we can't actually splat the synthetic module's
//!      contents into every file via the import machinery yet — direct
//!      registration provides the equivalent name visibility today.
//!
//! Real stdlib materialisation (replacing `register_builtin_types` with
//! `runtime/stdlib/*.kara` source baked into the compiler) is a follow-up CR
//! tracked in `docs/implementation_checklist/`.
//!
//! [`Module`]: crate::module::Module
//! [`ProgramTree`]: crate::module::ProgramTree
//! [`build_program_tree`]: crate::module::build_program_tree
//! [`Item`]: crate::ast::Item

use crate::ast::{
    Block, Deprecation, Function, GenericParam, GenericParams, ImportItem, Item, Program,
    StructDef, TraitDef, TypeKind, Unstable, Variance, Visibility,
};
use crate::token::Span;
use std::collections::HashMap;
use std::sync::LazyLock;

/// Canonical path of the synthetic prelude module: `std.prelude`. Stored as
/// `&'static str` segments here; callers that need an owned `Vec<String>`
/// (e.g. for [`crate::module::ModuleGraph::lookup`]) build one via
/// [`prelude_path`].
pub const PRELUDE_PATH_SEGMENTS: &[&str] = &["std", "prelude"];

/// Owned `Vec<String>` form of [`PRELUDE_PATH_SEGMENTS`].
pub fn prelude_path() -> Vec<String> {
    PRELUDE_PATH_SEGMENTS
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Primitive type names that the lexer / parser already accept as keywords
/// or identifier-keywords. Registered in scope 0 so unqualified use resolves
/// without an import. Several pieces of the compiler still inline narrower
/// subsets for their own purposes (numeric widths, etc.); this list is the
/// canonical surface every module sees.
pub const PRELUDE_PRIMITIVES: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "u8", "u16", "u32", "u64", "u128", "usize", "f32", "f64",
    "bool", "char", "String",
];

/// Stdlib type names visible without import. These are the placeholder
/// targets that `register_builtin_types` (typechecker.rs) backs with real
/// type-environment entries today; the follow-up stdlib-materialisation CR
/// will replace the shim with parsed Kāra source.
pub const PRELUDE_TYPES: &[&str] = &[
    "Option",
    "Result",
    "Vec",
    "VecDeque",
    "Array",
    "Vector",
    "Slice",
    "Map",
    "Set",
    "Entry",
    "Never",
    "StringSlice",
    "F32",
    "F64",
    "Atomic",
    "Mutex",
    "Ordering",
    "MemoryOrdering",
    "IoError",
    "VarError",
    "AllocError",
    "Utf8Error",
    // Phase-8 entry-point contract Slice B (2026-06-13): `ExitCode`
    // newtype — `distinct type ExitCode = i32`, one of the three legal
    // `main()` return types (design.md § Entry Point). Scope-0 so
    // `main() -> ExitCode` / `ExitCode.SUCCESS` / `ExitCode.from(c)`
    // resolve without an import. See `runtime/stdlib/exitcode.kara`.
    "ExitCode",
    "SortedSet",
    "Channel",
    "Sender",
    "Receiver",
    "Stats",
    "Regex",
    "RegexError",
    "Match",
    "Client",
    "Response",
    "HttpError",
    // Phase-8 line 24 — chained-builder request descriptor for
    // `c.request(...).header(...).body(...).timeout(...).send()`.
    // Opaque `{ handle: i64 }` wrapping a runtime-side `HTTP_BUILDERS`
    // entry; see `runtime/stdlib/http.kara`.
    "RequestBuilder",
    // Slice B (2026-05-09): minimal `std.http` server surface.
    // `Server` hosts the `serve_static` entry that v1's smoke test uses;
    // `Request` is the forward-compat opaque marker for the future
    // handler-dispatch path.
    "Server",
    "Request",
    // Phase 6 line 17 — `TcpListener` + `TcpStream` stdlib types,
    // composing through the `karac_park_on_fd` parking primitive.
    // Surface: `TcpListener.bind` / `.accept` (slice 8), and
    // `TcpStream.read` / `.write` (slice 9). `WebSocket` shares the
    // same single-i32-field layout and ships in slice 9e.1
    // (`send_text` / `recv_text` framing protocol).
    "TcpListener",
    "TcpStream",
    // `TcpError` — the `Result[_, TcpError]` error type for `bind` /
    // `accept` / `connect` / `read` / `write`. Scope-0 like its
    // `TlsError` mirror (below) so user code can pattern-match its
    // variants (`Interrupted` / `Other` / `AddrInUse` /
    // `ConnectionRefused` / `PermissionDenied`) without an explicit
    // import. (Added phase-8 line 74 — the named construction-cause
    // variants made naming `TcpError.<variant>` in a `match` the common
    // case; the earlier read/write surface only ever used `Err(_)`, so
    // the missing scope-0 registration went unnoticed.)
    "TcpError",
    "WebSocket",
    // Phase 6 line 236 slice 2 — TLS / HTTPS server-side surface.
    // `TlsListener` mirrors `TcpListener` (struct value carrying the
    // bound listener fd + an opaque `*mut TlsConfig` pointer);
    // `TlsStream` mirrors `TcpStream` (single i32 fd for the
    // post-handshake connection — TLS session state lives in the
    // runtime-side registry). `TlsConfig` is an empty marker type
    // — users never construct one directly; it exists so
    // `*mut TlsConfig` is a nameable type for the `TlsListener.config`
    // field. See `runtime/stdlib/tls.kara`.
    "TlsConfig",
    "TlsListener",
    "TlsStream",
    // Phase-8 line 24 (2026-05-29) — `TlsError` is the
    // `Result[i64, TlsError]` error type for `TlsStream.read /
    // .write / .write_all`. Three variants (`Interrupted`, `Other`,
    // `Protocol`); same shape mirror of `TcpError` plus the
    // rustls-protocol-fault catch-all. Scope-0 like the other
    // structured-error siblings (`IoError`, `HttpError`, `CliError`,
    // …) so user code can pattern-match without an explicit import.
    "TlsError",
    "Base64",
    "Hex",
    "Url",
    "DecodeError",
    // Phase 8 `File` handle slice F1 (2026-05-26): stateful file I/O.
    // Constructors `File.open` / `.create` / `.append` return
    // `Result[File, IoError]`; methods `read` / `write` / `flush`
    // operate on a live OS file descriptor. See `phase-8-stdlib-floor.md`
    // "File handle type" entry for the v1 surface + sub-task plan.
    "File",
    // Phase 8 `BufReader[R]` — buffered reader wrapper (Standard I/O
    // follow-up). `BufReader.new(reader)` / `.with_capacity(reader, cap)`
    // wrap a `File`; `read_line` / `read_to_string` / `read` amortize
    // syscall overhead. See `runtime/stdlib/bufreader.kara`.
    "BufReader",
    // `LinesIter` — line iterator returned by `BufReader.lines()`; drained
    // by a `for` loop yielding `Result[String, IoError]` per line. Non-
    // generic at v1 (concrete element type; wrapped reader erased).
    "LinesIter",
    // Phase 8 `BufWriter[W]` — buffered writer wrapper, the Write-side peer
    // of `BufReader`. `BufWriter.new(writer)` / `.with_capacity(writer, cap)`
    // wrap a `File`; `write` / `flush` amortize syscall overhead, flushing
    // on capacity / explicit `flush()` / drop. See
    // `runtime/stdlib/bufwriter.kara`.
    "BufWriter",
    // Debugger Contract slice 5: `std.runtime` introspection surface.
    // `Runtime` is the empty-marker host for the three `#[compiler_builtin]`
    // dispatch methods; `ParBlockInfo` / `TaskInfo` / `WaitTarget` are the
    // v1 contract data shapes returned by `Runtime.list_par_blocks()` /
    // `Runtime.list_tasks()`. See `runtime/stdlib/runtime.kara`.
    "Runtime",
    "ParBlockInfo",
    "TaskInfo",
    "WaitTarget",
    // Slice F (`std.json`): `Json` enum + `JsonError` struct visible at
    // scope-0 so user code can write `Json.parse(s)`, `match j { Json.Null => ... }`,
    // and pattern-match on the `JsonError` fields without an explicit import.
    "Json",
    "JsonError",
    // `std.cli` (v66 graduation, 2026-05-11): builder-style argument parser.
    // `Parser` / `Arg` / `Args` / `CliError` are the user-facing surface;
    // `ArgEntry` / `FlagEntry` / `ParsedValue` are internal row types that
    // back the parser's per-arg / per-flag / per-value storage and need
    // scope-0 visibility because their literal constructions appear in the
    // baked `runtime/stdlib/cli.kara` source. See `deferred.md § std.cli`.
    "Parser",
    "Arg",
    "Args",
    "CliError",
    "ArgEntry",
    "FlagEntry",
    "ParsedValue",
    // C1 slice (2026-05-16): subcommand + auto --help / --version
    // surface. `Subcommand` is the per-row storage on
    // `Parser.subcommands`; `SubcommandResult` is the v1 flat result
    // the dispatched subcommand fills in (one level of depth — the
    // recursive nesting shape lands when a real user case appears).
    "Subcommand",
    "SubcommandResult",
    // `std.tracing` (v64 backend-platform lift, 2026-05-09): structured
    // logging + span context, OTel-export-ready. `Span` / `LogEvent` /
    // `SpanField` are the user-visible data shapes; `NoOpExporter` is
    // the default (drop-everything) exporter and `StdoutExporter` is the
    // emission surface (renders spans/events to stdout) — both on the
    // `Exporter` trait, which user code can also implement. See
    // `runtime/stdlib/tracing.kara`.
    "Span",
    "LogEvent",
    "SpanField",
    "NoOpExporter",
    "StdoutExporter",
    // `Log` — ambient emission namespace (`Log.info("...")` etc.) over the
    // built-in `StdoutExporter`, no exporter value to thread.
    "Log",
    // `std.process` (v64 backend-platform lift): Command-builder /
    // Child handle / ExitStatus shapes. `EnvVar` is an internal row
    // type that backs `Command.cmd_env` and surfaces at scope-0
    // because its literal construction appears in the baked source.
    // See `runtime/stdlib/process.kara`.
    "Command",
    "Child",
    "ExitStatus",
    "EnvVar",
    // `Stdio` (stdin/stdout/stderr redirection setting for `Command`).
    "Stdio",
    // Captured-pipe handles surfaced by `Child.stdout()` / `.stderr()`
    // / `.stdin()` when the matching stream was spawned `Stdio.Piped`.
    "ChildStdout",
    "ChildStderr",
    "ChildStdin",
    // `Pool[T]` (v64 backend-platform lift): connection-pool
    // primitive. `Pool[T]` / `PooledConnection[T]` / `PoolError`
    // are the user-facing surface. See `runtime/stdlib/pool.kara`.
    "Pool",
    "PooledConnection",
    "PoolError",
    // `Tensor[T, Shape]` shape-typed N-D container (phase-11 numerical
    // stdlib, interpreter MVP). See `runtime/stdlib/tensor.kara`.
    "Tensor",
    // `Semaphore` application-layer backpressure primitive (phase-8 P1).
    // See `runtime/stdlib/semaphore.kara`.
    "Semaphore",
    "SemaphoreError",
    // `RateLimiter` token-bucket backpressure primitive (phase-8 P1).
    // See `runtime/stdlib/rate_limiter.kara`.
    "RateLimiter",
    // `BoundedChannel[T]` capacity-bounded backpressure queue (phase-8
    // P1). `OnFull` / `ChannelError` are its companion enums. See
    // `runtime/stdlib/bounded_channel.kara`.
    "BoundedChannel",
    "OnFull",
    "ChannelError",
    // Phase 6 line 186 slice 1 — `TaskGroup` / `TaskHandle[T]` from
    // `runtime/stdlib/task_group.kara`. `TaskGroup` is the
    // scope-local fan-out container per design.md § Explicit
    // Concurrency lines 9357–9366; `TaskHandle[T]` is the join
    // handle returned by every `spawn` call. The free-fn `spawn`
    // counterpart lives in `PRELUDE_FUNCTIONS` below.
    "TaskGroup",
    "TaskHandle",
    // `CStr` — the borrowed C-string type produced by `c"..."` literals
    // (design.md § C-String Literals). Scope-0 so the `let s: ref CStr =
    // c"..."` annotation form resolves; the type has no constructible
    // surface (stub struct, no public fields) — values only arise from
    // the literal form at v1. Method surface (`as_ptr` / `len` /
    // `is_empty` / `as_bytes`) dispatches through the typechecker's
    // `infer_cstr_method` arm, not an impl block. The owning `CString`
    // joins this list when its Phase-8 slice lands.
    "CStr",
];

/// Operator and conversion trait names visible without import. Lets
/// `impl Add for Foo` and `where T: Ord` resolve out of the box.
pub const PRELUDE_TRAITS: &[&str] = &[
    "From",
    "Into",
    "TryFrom",
    "TryInto",
    "Add",
    "Sub",
    "Mul",
    "Div",
    "Rem",
    "Neg",
    "Eq",
    // CR-202 slice 5a: `PartialEq` is now a real registered trait
    // (via baked `runtime/stdlib/partial_eq.kara`) rather than a
    // side-set name consulted only through `derived_traits`. Listing
    // it here makes it visible at scope-0 so user code can write
    // `impl PartialEq for ...` and reference the bound in
    // `where T: PartialEq`.
    "PartialEq",
    // CR-202 slice 5c: `PartialOrd` joins as the partial-ordering
    // counterpart to PartialEq.
    "PartialOrd",
    // `std.tracing` exporter trait — see `runtime/stdlib/tracing.kara`.
    "Exporter",
    "Ord",
    // CR-202 slice 5e.
    "Hash",
    "BitAnd",
    "BitOr",
    "BitXor",
    "Shl",
    "Shr",
    "Not",
    // Phase 7 user-`impl Drop` dispatch — Prereq.1. Bakes the `Drop`
    // trait visible at scope-0 so user code can write
    // `impl Drop for X { fn drop(mut ref self) { ... } }` without
    // an explicit import. Signature validation lives in
    // `typechecker/env_build.rs` (`E_DROP_SIGNATURE_INVALID`).
    "Drop",
    "Index",
    "IndexMut",
    "Display",
    // CR-202 slice 5g.
    "Debug",
    "Iterator",
    "IntoIterator",
    // Slice F (`std.json`): `ToJson` / `FromJson` are user-impl-only in
    // v1 (no derived form); making them prelude-visible lets user types
    // declare `impl ToJson for MyType` without an explicit import.
    "ToJson",
    "FromJson",
    // Phase 6 line 218 slice 2 — `ScopeLocal` sealed marker trait
    // (design.md § ScopeLocal). Used by the typechecker walker
    // `check_scope_local_escape` to identify types that must not
    // escape their creating scope. Currently the only implementer
    // shipped in stdlib is `TaskHandle[T]` (see
    // `runtime/stdlib/task_group.kara`); future scope-bound handles
    // (RAII guards, scope-bound iterators) implement it the same way.
    "ScopeLocal",
];

/// Enum variant names from prelude enums (`Option`, `Result`, `Ordering`,
/// `MemoryOrdering`) surfaced unqualified per design.md § Prelude.
pub const PRELUDE_VARIANTS: &[&str] = &[
    "Some", "None", "Ok", "Err",
    // Ordering — comparison ordering, returned by Ord.cmp
    "Less", "Equal", "Greater",
    // MemoryOrdering — atomic memory ordering, used by Atomic[T] operations
    "Relaxed", "Acquire", "Release", "AcqRel", "SeqCst",
    // Entry[K, V] — Map.entry(k) returns one of these
    "Occupied", "Vacant",
];

/// Ambient program-rooted effect resources — resources whose provider is
/// installed at program start and lives for the program's lifetime. See
/// `docs/design.md § Provider-Rooted Resources` ("Scope of the rule") and
/// § Nondeterminism as an Explicit Resource. User code can reference these
/// without declaring `effect resource Clock;` manually; the interpreter
/// installs a default provider in the base frame so `Clock.now()` etc.
/// resolve deterministically outside any `with_provider` scope.
///
/// The list is intentionally conservative — each name listed here has at
/// least one built-in method implemented by the interpreter. Additional
/// primitives (`FileSystem`, `Network`, `Heap`, `Stdin`, `Env`) are
/// registered incrementally as their method surfaces land.
pub const PRELUDE_EFFECT_RESOURCES: &[&str] = &[
    "Clock",
    "RandomSource",
    "Env",
    "Stdin",
    "Stdout",
    "Stderr",
    "FileSystem",
    // Slice B follow-up (2026-05-09): `Network` registered alongside
    // the `Server.serve(addr, handler)` declaration in
    // `runtime/stdlib/http.kara`. v1 unifies sends and receives under
    // one resource; surfaced here so user code can write
    // `with sends(Network) receives(Network)` without an explicit
    // `effect resource Network;` declaration.
    "Network",
    // `std.process` (v64 backend-platform lift): every interaction
    // with the OS process table — `Command.spawn` / `Child.wait` /
    // `Child.try_wait` / `Child.kill` — carries `sends(ProcessTable)`.
    // Declared as `effect resource ProcessTable;` in
    // `runtime/stdlib/process.kara`; surfaced here for scope-0
    // visibility so user wrappers can write
    // `with sends(ProcessTable)` without redeclaring it.
    "ProcessTable",
];

/// Canonical method order per ambient resource, used by codegen to index
/// a synthesized vtable when a `with_provider[R]` override is pushed onto
/// the runtime provider stack (`src/codegen/provider.rs`). The slot index
/// of a method here is its vtable slot; both the override-vtable emission
/// (at the `with_provider` site) and the call-site runtime dispatch read
/// this table, so they stay in lockstep.
///
/// Covers every ambient method that has a codegen FFI default *and* can be
/// overridden via `with_provider[R]` at runtime. Each entry gets a vtable
/// slot at its position, a minted resource ID (`compile_program` mints one
/// per resource named here), and an override-vs-default runtime branch at
/// the call site (`compile_ambient_dispatch_branch`). The branch phi takes
/// the method's real return type — i64 (`Clock.now`, `RandomSource
/// .next_u64`), the unit-placeholder i64 (`Env.set`, `Stdout/Stderr.*`), a
/// `Vec` struct (`Env.args`), or a `Result` enum (`Env.var`, `Stdin.*`,
/// `FileSystem.*`) — so no slot-method's return shape is special-cased.
///
/// A method absent here has no vtable slot: `ambient_method_index` returns
/// `None`, the call falls straight to its FFI default, and an attempted
/// `with_provider` override of it is a loud codegen error (the
/// no-slot guard in `compile_ambient_resource_method`) rather than a silent
/// fall-through that would diverge from the interpreter. Add a method here
/// when it gains both an FFI lowering and override support.
///
/// Ambient methods are otherwise hardcoded in two places this must stay
/// aligned with: the interpreter's
/// `dispatch_builtin_resource_method_with_values` and codegen's
/// `compile_ambient_resource_method` / `compile_ambient_ffi`.
pub const AMBIENT_RESOURCE_METHODS: &[(&str, &[&str])] = &[
    ("Clock", &["now"]),
    ("Env", &["set", "var", "args"]),
    ("RandomSource", &["next_u64"]),
    ("Stdin", &["read_line", "read_to_string"]),
    ("Stdout", &["print", "println", "flush"]),
    ("Stderr", &["print", "println", "flush"]),
    ("FileSystem", &["read_to_string", "write"]),
];

// ── Baked stdlib source (CR-202 slice 3a) ───────────────────────────
//
// Real Kāra source for prelude types is authored under `runtime/stdlib/*.kara`
// and embedded into the compiler binary via `include_str!`. The pilot scope
// is `Option` only (slice 3); slice 4+ adds one file at a time, retiring the
// corresponding arm of `register_builtin_types` at each step.
//
// 3a is plumbing-only: this constant and [`STDLIB_PROGRAMS`] expose the
// parsed AST for downstream consumption, but no current pipeline code
// reads them. Slice 3c will splice the parsed `EnumDef` for Option into
// the synthetic prelude module's items list, replacing the stub
// `StructDef` that lives in this file today.

/// Primitive-type associated constant value. Stored as a single concrete
/// type per variant so the table can carry both signed and unsigned ranges
/// without lossy widening. The interpreter coerces to `Value::Int(i64)` /
/// `Value::Float(f64)` at consumption; the codegen emits the matching
/// LLVM constant width.
///
/// Const generics slice 2 (2026-05-11) added `Bool`, `Char`, and
/// `EnumVariant` so the const-expression evaluator can carry bool, char,
/// and fieldless-enum literal results. The `Copy` derive is dropped
/// (`EnumVariant`'s `String` payloads break Copy); callers `.clone()`
/// when needed. `I128`/`U128` are not in this slice — they require
/// `IntSize`/`UIntSize` extensions (see phase-5-diagnostics.md § Const
/// generics slice 2b).
#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    /// 128-bit signed integer (const generics slice 2b, 2026-05-11).
    /// Unlocked by the `IntSize::I128` extension landed alongside.
    /// AST `ExprKind::Integer(i64, _)` literals are bounded to i64
    /// at parse time, so today's source surface produces `I128`
    /// values that fit in i64; larger values land when the lexer /
    /// AST is widened to carry i128 literal bits directly.
    I128(i128),
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    /// 128-bit unsigned integer (const generics slice 2b).
    U128(u128),
    /// 64-bit only in v1 — when 32-bit targets land, swap to a target-
    /// conditional table.
    Usize(u64),
    F32(f32),
    F64(f64),
    /// `true` / `false` — produced by the const-expression evaluator
    /// for `ExprKind::Bool` and as the result of every comparison /
    /// logical operator. Permitted as a const-generic param type.
    Bool(bool),
    /// `'a'` — produced by the const-expression evaluator for
    /// `ExprKind::CharLit`. Permitted as a const-generic param type.
    Char(char),
    /// A fieldless-enum variant captured at compile time. `discriminant`
    /// is the variant's declared or auto-assigned tag; the typechecker's
    /// enum registry is the source of truth.
    EnumVariant {
        enum_name: String,
        variant_name: String,
        discriminant: i64,
    },
}

/// Primitive-type associated constants — `i64.MAX` / `f64.INFINITY` /
/// `usize.MAX` etc. The same table feeds the typechecker (returns the
/// correct numeric type for `let x = i64.MAX;`), the interpreter
/// (returns `Value::Int` / `Value::Float` at runtime), and the codegen
/// (emits the matching LLVM constant). Both `i64.MAX` and the
/// theoretical `i64::MAX` syntactic form would dispatch through the
/// same lookup, but only the `.` form parses today (probe 2026-05-10
/// confirmed `::` produces a parser error).
pub static PRIMITIVE_CONSTS: &[(&str, &str, ConstValue)] = &[
    ("i8", "MAX", ConstValue::I8(i8::MAX)),
    ("i8", "MIN", ConstValue::I8(i8::MIN)),
    ("i16", "MAX", ConstValue::I16(i16::MAX)),
    ("i16", "MIN", ConstValue::I16(i16::MIN)),
    ("i32", "MAX", ConstValue::I32(i32::MAX)),
    ("i32", "MIN", ConstValue::I32(i32::MIN)),
    ("i64", "MAX", ConstValue::I64(i64::MAX)),
    ("i64", "MIN", ConstValue::I64(i64::MIN)),
    ("u8", "MAX", ConstValue::U8(u8::MAX)),
    ("u16", "MAX", ConstValue::U16(u16::MAX)),
    ("u32", "MAX", ConstValue::U32(u32::MAX)),
    ("u64", "MAX", ConstValue::U64(u64::MAX)),
    ("usize", "MAX", ConstValue::Usize(u64::MAX)),
    ("f32", "INFINITY", ConstValue::F32(f32::INFINITY)),
    ("f32", "NEG_INFINITY", ConstValue::F32(f32::NEG_INFINITY)),
    ("f32", "MAX", ConstValue::F32(f32::MAX)),
    ("f32", "MIN", ConstValue::F32(f32::MIN)),
    ("f32", "MIN_POSITIVE", ConstValue::F32(f32::MIN_POSITIVE)),
    ("f32", "NAN", ConstValue::F32(f32::NAN)),
    ("f32", "EPSILON", ConstValue::F32(f32::EPSILON)),
    ("f64", "INFINITY", ConstValue::F64(f64::INFINITY)),
    ("f64", "NEG_INFINITY", ConstValue::F64(f64::NEG_INFINITY)),
    ("f64", "MAX", ConstValue::F64(f64::MAX)),
    ("f64", "MIN", ConstValue::F64(f64::MIN)),
    ("f64", "MIN_POSITIVE", ConstValue::F64(f64::MIN_POSITIVE)),
    ("f64", "NAN", ConstValue::F64(f64::NAN)),
    ("f64", "EPSILON", ConstValue::F64(f64::EPSILON)),
];

/// Look up a primitive-type associated constant by `(type_name, const_name)`.
/// Returns `None` when no entry exists — callers fall through to whatever
/// default the surrounding dispatch site uses (typechecker silent
/// `Type::Error`; interpreter / codegen panic with a "should be caught by
/// resolver / typechecker" message under the existing field-access
/// fallback).
pub fn lookup_primitive_const(type_name: &str, const_name: &str) -> Option<&'static ConstValue> {
    PRIMITIVE_CONSTS
        .iter()
        .find(|(t, c, _)| *t == type_name && *c == const_name)
        .map(|(_, _, v)| v)
}

/// `ExitCode.SUCCESS` / `ExitCode.FAILURE` paren-free associated
/// constants (Phase-8 entry-point contract Slice B). Returns the raw
/// `i32` exit code — `0` for `SUCCESS`, `1` for `FAILURE` — or `None`
/// for any other field. Deliberately separate from `PRIMITIVE_CONSTS`:
/// these resolve to the `ExitCode` *distinct type*, not a bare `i32`,
/// so they cannot flow through `primitive_const_type` (which would
/// mistype `main() -> ExitCode { ExitCode.SUCCESS }`). The field-access
/// intercepts in the typechecker / interpreter / codegen each consult
/// this and then wrap the result in the type / value / LLVM constant
/// appropriate to that phase. The literal `1` (not the platform
/// `EXIT_FAILURE`) matches the `Err`-exit code in design.md § Entry
/// Point for byte-reproducible behavior across platforms.
pub fn lookup_exitcode_const(type_name: &str, const_name: &str) -> Option<i32> {
    if type_name != "ExitCode" {
        return None;
    }
    match const_name {
        "SUCCESS" => Some(0),
        "FAILURE" => Some(1),
        _ => None,
    }
}

/// Embedded stdlib sources, keyed by their on-disk basename (relative to
/// `runtime/stdlib/`). Sources are baked at compile time via `include_str!`
/// so the resulting binary is self-contained.
pub const STDLIB_SOURCES: &[(&str, &str)] = &[
    ("option.kara", include_str!("../runtime/stdlib/option.kara")),
    ("result.kara", include_str!("../runtime/stdlib/result.kara")),
    ("vec.kara", include_str!("../runtime/stdlib/vec.kara")),
    (
        "vec_deque.kara",
        include_str!("../runtime/stdlib/vec_deque.kara"),
    ),
    ("map.kara", include_str!("../runtime/stdlib/map.kara")),
    (
        "sorted_set.kara",
        include_str!("../runtime/stdlib/sorted_set.kara"),
    ),
    (
        "channel.kara",
        include_str!("../runtime/stdlib/channel.kara"),
    ),
    ("sender.kara", include_str!("../runtime/stdlib/sender.kara")),
    (
        "receiver.kara",
        include_str!("../runtime/stdlib/receiver.kara"),
    ),
    ("set.kara", include_str!("../runtime/stdlib/set.kara")),
    (
        "peekable.kara",
        include_str!("../runtime/stdlib/peekable.kara"),
    ),
    ("atomic.kara", include_str!("../runtime/stdlib/atomic.kara")),
    ("mutex.kara", include_str!("../runtime/stdlib/mutex.kara")),
    ("f32.kara", include_str!("../runtime/stdlib/f32.kara")),
    ("f64.kara", include_str!("../runtime/stdlib/f64.kara")),
    ("stats.kara", include_str!("../runtime/stdlib/stats.kara")),
    ("regex.kara", include_str!("../runtime/stdlib/regex.kara")),
    ("http.kara", include_str!("../runtime/stdlib/http.kara")),
    // Phase 6 line 17 — `TcpListener` stdlib type composing through
    // the `karac_park_on_fd` leaf parking primitive (Slice 6 + 7).
    ("tcp.kara", include_str!("../runtime/stdlib/tcp.kara")),
    // Phase 6 line 17 slice 9e.1 — `WebSocket` stdlib type with
    // RFC 6455 text-frame send/recv. Depends on `tcp.kara` for the
    // `TcpError` enum reused as the structured-error type.
    ("ws.kara", include_str!("../runtime/stdlib/ws.kara")),
    // Phase 6 line 236 slice 2 — `TlsListener` / `TlsStream` /
    // `TlsConfig` stdlib surface for server-side TLS. Composes
    // through `runtime/src/tls.rs` (slice 1's rustls FFI). Reuses
    // `TcpError` from `tcp.kara` for read / write error variants.
    ("tls.kara", include_str!("../runtime/stdlib/tls.kara")),
    // Phase 6 line 186 slice 1 — `TaskGroup` / `TaskHandle[T]` /
    // free-fn `spawn`. Typechecker-only landing at v1 (codegen
    // ships with slice 4 of the same tracker entry).
    (
        "task_group.kara",
        include_str!("../runtime/stdlib/task_group.kara"),
    ),
    (
        "encoding.kara",
        include_str!("../runtime/stdlib/encoding.kara"),
    ),
    (
        "ordering.kara",
        include_str!("../runtime/stdlib/ordering.kara"),
    ),
    (
        "memory_ordering.kara",
        include_str!("../runtime/stdlib/memory_ordering.kara"),
    ),
    ("entry.kara", include_str!("../runtime/stdlib/entry.kara")),
    (
        "io_error.kara",
        include_str!("../runtime/stdlib/io_error.kara"),
    ),
    (
        "var_error.kara",
        include_str!("../runtime/stdlib/var_error.kara"),
    ),
    (
        "alloc_error.kara",
        include_str!("../runtime/stdlib/alloc_error.kara"),
    ),
    (
        "utf8_error.kara",
        include_str!("../runtime/stdlib/utf8_error.kara"),
    ),
    // Phase-8 entry-point contract Slice B: `ExitCode` newtype, one of
    // the three legal `main()` return types. `distinct type = i32`;
    // `SUCCESS` / `FAILURE` are intercepted associated constants (see
    // `lookup_exitcode_const`). No deps — order-independent.
    (
        "exitcode.kara",
        include_str!("../runtime/stdlib/exitcode.kara"),
    ),
    ("index.kara", include_str!("../runtime/stdlib/index.kara")),
    ("from.kara", include_str!("../runtime/stdlib/from.kara")),
    ("into.kara", include_str!("../runtime/stdlib/into.kara")),
    (
        "try_from.kara",
        include_str!("../runtime/stdlib/try_from.kara"),
    ),
    (
        "try_into.kara",
        include_str!("../runtime/stdlib/try_into.kara"),
    ),
    (
        "iterator.kara",
        include_str!("../runtime/stdlib/iterator.kara"),
    ),
    (
        "into_iterator.kara",
        include_str!("../runtime/stdlib/into_iterator.kara"),
    ),
    ("not.kara", include_str!("../runtime/stdlib/not.kara")),
    // Phase 7 user-`impl Drop` dispatch — Prereq.1. Bakes the `Drop`
    // trait so user code can write `impl Drop for X { fn drop(mut ref
    // self) { ... } }` without an inline trait declaration. See
    // `runtime/stdlib/drop.kara` for the rationale + signature rule.
    ("drop.kara", include_str!("../runtime/stdlib/drop.kara")),
    (
        "partial_eq.kara",
        include_str!("../runtime/stdlib/partial_eq.kara"),
    ),
    ("eq.kara", include_str!("../runtime/stdlib/eq.kara")),
    (
        "partial_ord.kara",
        include_str!("../runtime/stdlib/partial_ord.kara"),
    ),
    ("ord.kara", include_str!("../runtime/stdlib/ord.kara")),
    ("hash.kara", include_str!("../runtime/stdlib/hash.kara")),
    (
        "display.kara",
        include_str!("../runtime/stdlib/display.kara"),
    ),
    ("debug.kara", include_str!("../runtime/stdlib/debug.kara")),
    ("add.kara", include_str!("../runtime/stdlib/add.kara")),
    ("sub.kara", include_str!("../runtime/stdlib/sub.kara")),
    ("mul.kara", include_str!("../runtime/stdlib/mul.kara")),
    ("div.kara", include_str!("../runtime/stdlib/div.kara")),
    ("rem.kara", include_str!("../runtime/stdlib/rem.kara")),
    ("neg.kara", include_str!("../runtime/stdlib/neg.kara")),
    ("bitand.kara", include_str!("../runtime/stdlib/bitand.kara")),
    ("bitor.kara", include_str!("../runtime/stdlib/bitor.kara")),
    ("bitxor.kara", include_str!("../runtime/stdlib/bitxor.kara")),
    ("shl.kara", include_str!("../runtime/stdlib/shl.kara")),
    ("shr.kara", include_str!("../runtime/stdlib/shr.kara")),
    ("io.kara", include_str!("../runtime/stdlib/io.kara")),
    (
        "runtime.kara",
        include_str!("../runtime/stdlib/runtime.kara"),
    ),
    // Slice F (`std.json`).
    ("json.kara", include_str!("../runtime/stdlib/json.kara")),
    // `std.cli` builder-style argument parser (v66 graduation).
    ("cli.kara", include_str!("../runtime/stdlib/cli.kara")),
    // `std.tracing` structured logging + spans (v64 backend-platform lift).
    (
        "tracing.kara",
        include_str!("../runtime/stdlib/tracing.kara"),
    ),
    // `std.process` Command / Child / ExitStatus + ProcessTable resource
    // (v64 backend-platform lift). Surface only — OS-touching methods
    // return placeholder Err pending a follow-up intrinsic slice.
    (
        "process.kara",
        include_str!("../runtime/stdlib/process.kara"),
    ),
    // `Pool[T]` connection-pool primitive (v64 backend-platform lift).
    // Surface only — acquire returns placeholder Err pending the
    // follow-up bounded-waiters intrinsic.
    ("pool.kara", include_str!("../runtime/stdlib/pool.kara")),
    // `Semaphore` application-layer backpressure primitive (phase-8 P1).
    // Surface + collapsed single-threaded intrinsic (immediate-serve-or-
    // timeout); the parking-with-timeout backend lands with the event loop.
    (
        "semaphore.kara",
        include_str!("../runtime/stdlib/semaphore.kara"),
    ),
    // `RateLimiter` token-bucket backpressure primitive (phase-8 P1).
    // Synchronous `try_acquire`; the async waiting `acquire` lands with
    // the event loop.
    (
        "rate_limiter.kara",
        include_str!("../runtime/stdlib/rate_limiter.kara"),
    ),
    // `BoundedChannel[T]` capacity-bounded backpressure queue (phase-8
    // P1). FailFast send + non-blocking recv in v1; Block's park lands
    // with the event loop.
    (
        "bounded_channel.kara",
        include_str!("../runtime/stdlib/bounded_channel.kara"),
    ),
    // `BufReader[R]` buffered reader wrapper (phase-8 Standard I/O
    // follow-up). Wraps a `File` reader (concretely, at v1) with an
    // internal buffer so per-call syscall overhead amortizes. Surface +
    // collapsed interpreter intrinsic; see `runtime/stdlib/bufreader.kara`.
    (
        "bufreader.kara",
        include_str!("../runtime/stdlib/bufreader.kara"),
    ),
    // `BufWriter[W]` buffered writer wrapper (phase-8 Standard I/O
    // follow-up). The Write-side peer of `BufReader`: wraps a `File`
    // writer (concretely, at v1) with an internal buffer so per-call
    // syscall overhead amortizes. Surface + collapsed interpreter
    // intrinsic; see `runtime/stdlib/bufwriter.kara`.
    (
        "bufwriter.kara",
        include_str!("../runtime/stdlib/bufwriter.kara"),
    ),
    // Compile-time layout introspection — `size_of[T]()` / `align_of[T]()`
    // (the `offset_of[T](field)` arm is a parser special-form, not a
    // stdlib function — see `runtime/stdlib/intrinsics.kara`).
    (
        "intrinsics.kara",
        include_str!("../runtime/stdlib/intrinsics.kara"),
    ),
    // `Tensor[T, Shape]` shape-typed N-D container (phase-11 numerical
    // stdlib). Interpreter MVP: zeros/ones/full + shape/rank + tuple
    // indexing; see `runtime/stdlib/tensor.kara`.
    ("tensor.kara", include_str!("../runtime/stdlib/tensor.kara")),
    // `std.time` — native async `sleep_ms` (the leaf `suspends` timer
    // primitive; auto-par divergence slice A2a-2.2). See
    // `runtime/stdlib/time.kara`.
    ("time.kara", include_str!("../runtime/stdlib/time.kara")),
];

/// Phase-10 (`std.web`): baked stdlib modules that are GATED — real Kāra
/// source compiled into the binary like [`STDLIB_SOURCES`], but **not**
/// part of the prelude. Nothing here reaches the resolver's scope-0, the
/// `PRELUDE_*` name lists, or the typechecker's `register_baked_stdlib`
/// walk; the only path into user scope is an explicit
/// `import std.web.{Display, ...};` resolved against the synthetic
/// modules [`build_program_tree`] splices in from
/// [`synthetic_gated_modules`]. This is the design.md § "Web / Host
/// Effect Vocabulary" module-gating rule: native-only compilations must
/// never see these resource names, so server-only programs' effect
/// inference stays free of web-host noise.
///
/// Each entry is `(module path segments, source)` — unlike
/// `STDLIB_SOURCES`, the module path is explicit because these files
/// define real (non-prelude) module identities.
///
/// [`build_program_tree`]: crate::module::build_program_tree
pub const GATED_STDLIB_SOURCES: &[(&[&str], &str)] = &[
    (&["std", "web"], include_str!("../runtime/stdlib/web.kara")),
    (
        &["std", "web", "net"],
        include_str!("../runtime/stdlib/web_net.kara"),
    ),
    (
        &["std", "web", "time"],
        include_str!("../runtime/stdlib/web_time.kara"),
    ),
    (
        &["std", "wasi"],
        include_str!("../runtime/stdlib/wasi.kara"),
    ),
];

/// Parsed AST of every entry in [`STDLIB_SOURCES`]. Parsed lazily on first
/// access and cached for the lifetime of the process. The vector preserves
/// the source order from `STDLIB_SOURCES`, so callers that need
/// deterministic load order (e.g. resolve trait/struct dependencies) get
/// it for free.
///
/// Panics if any baked source fails to parse — a parse failure indicates
/// a bug in the stdlib source itself, not in user code, and there is no
/// recoverable path. The error message names the offending file so the
/// fix is obvious.
pub static STDLIB_PROGRAMS: LazyLock<Vec<(&'static str, Program)>> = LazyLock::new(|| {
    let mut out = Vec::with_capacity(STDLIB_SOURCES.len());
    for &(name, src) in STDLIB_SOURCES {
        let parsed = crate::parse(src);
        if !parsed.errors.is_empty() {
            let msgs = parsed
                .errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("\n  ");
            panic!(
                "baked stdlib source `{}` failed to parse:\n  {}",
                name, msgs
            );
        }
        out.push((name, parsed.program));
    }
    out
});

/// Per-stdlib-type variance declarations (design.md § Variance), keyed
/// by type name → per-parameter variance vector in declaration order.
/// Derived from the explicit `+T` / `-T` / `=T` markers on baked stdlib
/// struct/enum declarations ([`STDLIB_PROGRAMS`] + [`GATED_STDLIB_PROGRAMS`]),
/// plus hardcoded entries for parametric pseudo-types with no syntactic
/// declaration surface (`Iterator`, `Array` — see
/// `register_compiler_intrinsic_env`).
///
/// Consulted by the typechecker's `types_compatible` Named-type arm to
/// gate generic-argument subtyping per slot: `+T` slots accept
/// refinement-to-base widening, `=T` slots demand mutual compatibility,
/// `-T` slots flip the direction. Types absent from this table — every
/// user-defined type — are invariant in all parameters (user-side
/// variance declarations are rejected with `E_VARIANCE_USER_DECL_NOT_YET`
/// at v1).
///
/// Known narrowing: the lookup is name-keyed, so a user type that
/// *shadows* a covariant stdlib name (`struct Option[T] {...}` — the
/// resolver allows prelude shadowing) inherits the stdlib entry's
/// variance at compatibility-check time. Shadowing a prelude type AND
/// relying on generic-arg invariance with refinement arguments is a
/// corner the name-keyed table accepts at v1; the fix (def-site-keyed
/// lookup) requires threading decl identity into `Type::Named`.
pub static STDLIB_VARIANCE: LazyLock<HashMap<String, Vec<Variance>>> = LazyLock::new(|| {
    let mut table: HashMap<String, Vec<Variance>> = HashMap::new();
    // `Iterator` is a trait + a compiler-intrinsic parametric
    // pseudo-struct (runtime/stdlib/iterator.kara documents the split);
    // there is no syntactic generic-param list to mark. Audit:
    // `Iterator[+T]` — produces `T`s, never consumes one.
    table.insert("Iterator".to_string(), vec![Variance::Covariant]);
    // `Array[=T, const N]` — compiler-intrinsic; invariant in T
    // (mutable through `mut ref`).
    table.insert("Array".to_string(), vec![Variance::Invariant]);
    let programs = STDLIB_PROGRAMS
        .iter()
        .map(|(_, p)| p)
        .chain(GATED_STDLIB_PROGRAMS.iter().map(|(_, p)| p));
    for program in programs {
        for item in &program.items {
            let (name, generics) = match item {
                Item::StructDef(s) => (&s.name, &s.generic_params),
                Item::EnumDef(e) => (&e.name, &e.generic_params),
                _ => continue,
            };
            let Some(gp) = generics else { continue };
            let variances: Vec<Variance> = gp
                .params
                .iter()
                .filter(|p| !p.is_const && !p.is_variadic_shape)
                .map(|p| p.variance)
                .collect();
            if !variances.is_empty() {
                table.insert(name.clone(), variances);
            }
        }
    }
    table
});

/// Per-slot variance lookup for a named type. `None` for types with no
/// stdlib variance declaration — callers treat every slot as invariant.
pub fn stdlib_variance(name: &str) -> Option<&'static [Variance]> {
    STDLIB_VARIANCE.get(name).map(|v| v.as_slice())
}

/// Parsed AST of every entry in [`GATED_STDLIB_SOURCES`]. Same contract as
/// [`STDLIB_PROGRAMS`] (lazy, cached, panics on parse failure — a broken
/// baked source is a compiler bug, not user error), keyed by module path
/// instead of file name.
pub static GATED_STDLIB_PROGRAMS: LazyLock<Vec<(Vec<String>, Program)>> = LazyLock::new(|| {
    let mut out = Vec::with_capacity(GATED_STDLIB_SOURCES.len());
    for &(path, src) in GATED_STDLIB_SOURCES {
        let parsed = crate::parse(src);
        if !parsed.errors.is_empty() {
            let msgs = parsed
                .errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("\n  ");
            panic!(
                "gated baked stdlib module `{}` failed to parse:\n  {}",
                path.join("."),
                msgs
            );
        }
        let path: Vec<String> = path.iter().map(|s| s.to_string()).collect();
        out.push((path, parsed.program));
    }
    out
});

/// Synthetic-module payloads for the gated stdlib: `(module path, items)`
/// per [`GATED_STDLIB_SOURCES`] entry, with `stdlib_origin = true` flipped
/// on item kinds that carry the flag (same resolver gate bypass
/// [`synthetic_prelude_items`]'s baked splice uses). `build_program_tree`
/// appends one `is_synthetic` module per entry so
/// `import std.web.{Display, ...};` resolves — and nothing else does:
/// these names have no scope-0 registration, which is the entire gating
/// mechanism.
pub fn synthetic_gated_modules() -> Vec<(Vec<String>, Vec<Item>)> {
    GATED_STDLIB_PROGRAMS
        .iter()
        .map(|(path, program)| {
            let items = program
                .items
                .iter()
                .map(|item| {
                    let mut cloned = item.clone();
                    match &mut cloned {
                        Item::Function(f) => f.stdlib_origin = true,
                        Item::StructDef(s) => s.stdlib_origin = true,
                        Item::EnumDef(e) => e.stdlib_origin = true,
                        Item::TraitDef(t) => t.stdlib_origin = true,
                        // `EffectResource` and friends carry no
                        // `stdlib_origin`; nothing to flip.
                        _ => {}
                    }
                    cloned
                })
                .collect();
            (path.clone(), items)
        })
        .collect()
}

/// Resolve one import declaration against the gated baked stdlib: if
/// `path` names a [`GATED_STDLIB_SOURCES`] module, return a real `Item`
/// clone (stdlib_origin = true, alias applied) for every brace-listed
/// name that module defines. Returns `None` when `path` is not a gated
/// module; names the module does not define are silently skipped (the
/// caller's normal unknown-item handling owns that diagnostic).
///
/// Two pipelines splice these clones into the program they compile:
///
///  - **Single-file** (`Pipeline::resolve`): there is no `ProgramTree`,
///    so without expansion a gated import binds blindly and the first
///    *use* ICEs in the interpreter ("variable 'fetch' not found") or
///    falls over in codegen. Expansion replaces the import binding with
///    the real declarations.
///  - **Project codegen** (`run_multi_file_codegen`): synthetic modules
///    are skipped when concatenating the super-program, so an imported
///    `fetch` resolves and typechecks per-module (the typechecker
///    chases the tree) but its body never reaches codegen. The
///    concatenation appends the expansion of every gated import found
///    in user modules.
///
/// Alias rename (`import std.web.Display as Screen;`) clones the item
/// under the alias. For effect resources this matches project-mode
/// semantics today: effect sets identify resources by the *clause
/// string*, so `writes(Screen)` is the string "Screen" in either mode.
pub fn gated_items_for_import(path: &[String], items: &[ImportItem]) -> Option<Vec<Item>> {
    let (_, program) = GATED_STDLIB_PROGRAMS
        .iter()
        .find(|(p, _)| p.as_slice() == path)?;
    let mut out = Vec::new();
    // Original (pre-alias) names of struct/enum items spliced un-aliased,
    // so their `impl` blocks can be auto-spliced below.
    let mut spliced_type_names: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for ii in items {
        let found = program.items.iter().find(|item| match item {
            Item::Function(f) => f.name == ii.name,
            Item::StructDef(s) => s.name == ii.name,
            Item::EnumDef(e) => e.name == ii.name,
            Item::TraitDef(t) => t.name == ii.name,
            Item::EffectResource(r) => r.name == ii.name,
            _ => false,
        });
        let Some(found) = found else { continue };
        if ii.alias.is_none() {
            match found {
                Item::StructDef(s) => {
                    spliced_type_names.insert(s.name.clone());
                }
                Item::EnumDef(e) => {
                    spliced_type_names.insert(e.name.clone());
                }
                _ => {}
            }
        }
        let mut cloned = found.clone();
        let bound = ii.alias.as_ref().unwrap_or(&ii.name);
        match &mut cloned {
            Item::Function(f) => {
                f.stdlib_origin = true;
                f.name = bound.clone();
            }
            Item::StructDef(s) => {
                s.stdlib_origin = true;
                s.name = bound.clone();
            }
            Item::EnumDef(e) => {
                e.stdlib_origin = true;
                e.name = bound.clone();
            }
            Item::TraitDef(t) => {
                t.stdlib_origin = true;
                t.name = bound.clone();
            }
            Item::EffectResource(r) => {
                // Alias-renamed host resources keep canonical provenance
                // so the target gate keys its provided-resource table on
                // the real name — `import std.web.Display as Screen;`
                // must not let `writes(Screen)` evade the Display gate.
                if r.canonical_host_name.is_none() && *bound != r.name {
                    r.canonical_host_name = Some(r.name.clone());
                }
                r.name = bound.clone();
            }
            _ => {}
        }
        out.push(cloned);
    }

    // Auto-splice `impl` blocks for the struct/enum types spliced above.
    // A gated module's type (`std.web.time.Duration`) carries its methods
    // in an `impl Duration { ... }` block, which is not itself a
    // brace-listable name — without this, `Duration.ms(..)` / `.as_ms()`
    // resolve to "no associated function". Only un-aliased types are
    // covered (an `import ... as Alias` would need the impl's target
    // rewritten too — out of scope; no gated aliased-type-with-impl exists
    // at v1). Matches on the impl target's last path segment, mirroring
    // `codegen::helpers::impl_target_name`.
    if !spliced_type_names.is_empty() {
        for item in &program.items {
            if let Item::ImplBlock(imp) = item {
                let target = match &imp.target_type.kind {
                    crate::ast::TypeKind::Path(p) => p.segments.last().cloned(),
                    _ => None,
                };
                if target
                    .as_deref()
                    .is_some_and(|t| spliced_type_names.contains(t))
                {
                    out.push(item.clone());
                }
            }
        }
    }

    // Auto-splice the gated module's OWN effect-resource declarations that
    // the spliced functions reference. A gated stdlib producer like
    // `std.web.time.after` declares `with writes(Timer)`, but a user who
    // writes `import std.web.time.{after};` does not (and should not have
    // to) also import `Timer` — yet `after`'s effect clause must still
    // resolve. Pulling the referenced resource in from the same module
    // keeps it gated (a program importing nothing from this module never
    // sees it) while making the producer self-contained. Only *referenced*
    // resources are spliced — sibling resources (e.g. `Storage`/`Console`
    // when only a `Timer` producer is imported) stay out of scope, so the
    // gating test in `tests/module_graph.rs` is unaffected. Cross-import
    // duplicates (the resource imported explicitly *and* via a producer)
    // are deduped by the callers (`expand_gated_stdlib_imports` /
    // project-mode concatenation), which key on the resource name.
    let mut present: std::collections::HashSet<String> = out
        .iter()
        .filter_map(|it| match it {
            Item::EffectResource(r) => Some(r.name.clone()),
            _ => None,
        })
        .collect();
    let referenced: Vec<String> = out
        .iter()
        .filter_map(|it| match it {
            Item::Function(f) => f.effects.as_ref(),
            _ => None,
        })
        .flat_map(|effects| effects.items.iter())
        .filter_map(|ei| match ei {
            crate::ast::EffectItem::Verb(v) => Some(v),
            _ => None,
        })
        .flat_map(|v| v.resources.iter())
        .filter_map(|res| res.path.last().cloned())
        .collect();
    for name in referenced {
        if !present.insert(name.clone()) {
            continue;
        }
        if let Some(decl) = program
            .items
            .iter()
            .find(|it| matches!(it, Item::EffectResource(r) if r.name == name))
        {
            out.push(decl.clone());
        }
    }

    Some(out)
}

/// Single-file-mode gated-import expansion (see
/// [`gated_items_for_import`]). Rewrites `program` in place: every
/// import of a gated stdlib module is replaced by the real items it
/// names; import items the gated module does NOT define are left in
/// the import declaration so the resolver's blind-bind path keeps
/// owning that (pre-existing) behaviour.
pub fn expand_gated_stdlib_imports(program: &mut Program) {
    let mut appended: Vec<Item> = Vec::new();
    for item in &mut program.items {
        let Item::Import(imp) = item else { continue };
        let Some(expansion) = gated_items_for_import(&imp.path, &imp.items) else {
            continue;
        };
        // Drop exactly the import items that expanded; keep the rest.
        let expanded_names: Vec<&str> = expansion
            .iter()
            .map(|it| match it {
                Item::Function(f) => f.name.as_str(),
                Item::StructDef(s) => s.name.as_str(),
                Item::EnumDef(e) => e.name.as_str(),
                Item::TraitDef(t) => t.name.as_str(),
                Item::EffectResource(r) => r.name.as_str(),
                _ => "",
            })
            .collect();
        imp.items.retain(|ii| {
            let bound = ii.alias.as_ref().unwrap_or(&ii.name);
            !expanded_names.contains(&bound.as_str())
        });
        appended.extend(expansion);
    }
    // Imports left with zero items would confuse downstream passes —
    // remove them entirely.
    program.items.retain(|item| match item {
        Item::Import(imp) => !imp.items.is_empty(),
        _ => true,
    });
    // Dedup auto-spliced effect resources by name: a resource can arrive
    // both explicitly (`import std.web.Timer`) and via a producer's
    // referenced-resource auto-splice (`import std.web.time.after`); a
    // second `effect resource Timer;` in the program would be a duplicate
    // definition (`collect_effect_resource` errors). Keep the first.
    let mut seen_resources: std::collections::HashSet<String> = std::collections::HashSet::new();
    appended.retain(|item| match item {
        Item::EffectResource(r) => seen_resources.insert(r.name.clone()),
        _ => true,
    });
    program.items.extend(appended);
}

/// A baked-stdlib method's stability annotations: the `#[unstable]` payload
/// and the `#[deprecated]` payload, either or both of which may be present.
/// Value type of [`STDLIB_METHOD_STABILITY`].
pub type StabilityPayload = (Option<Unstable>, Option<Deprecation>);

/// Baked-stdlib method-level `#[unstable]` / `#[deprecated]` payloads,
/// keyed by `"TargetType.method"` (e.g. `"Server.serve_static"`).
///
/// **Why a separate table.** Baked stdlib impls live in [`STDLIB_PROGRAMS`]
/// and are walked by the typechecker directly (`register_baked_stdlib` →
/// `env_add_impl`); they never reach the resolver's `collect_impl` pass, so
/// their method-level stability attributes never land in the symbol table's
/// `unstables` / `deprecations` sidecars the way *user-authored* impl methods
/// do (`record_unstable_if_present` / `record_deprecation_if_present`). And
/// `ImplInfo.methods` (`env.impls`) stores only `FunctionSig`, which drops
/// the attributes. This table is the baked-stdlib mirror of those sidecars:
/// the method-aware use-site lint (`TypeChecker::check_method_stability`)
/// consults the symbol-table sidecar for user methods and *this* table for
/// baked-stdlib methods. Built once, lazily, from the parsed stdlib AST so
/// any future `#[unstable]` / `#[deprecated]` stdlib method tag is picked up
/// with no further wiring.
pub static STDLIB_METHOD_STABILITY: LazyLock<HashMap<String, StabilityPayload>> =
    LazyLock::new(|| {
        let mut out: HashMap<String, StabilityPayload> = HashMap::new();
        for (_, program) in STDLIB_PROGRAMS.iter() {
            for item in &program.items {
                let Item::ImplBlock(imp) = item else { continue };
                // Inherent + trait impls both contribute; the key is the target
                // type's nominal name (last path segment, so `impl[T] Vec[T]`
                // keys under `Vec`). Generic args don't participate in the key.
                let TypeKind::Path(path) = &imp.target_type.kind else {
                    continue;
                };
                let Some(type_name) = path.segments.last() else {
                    continue;
                };
                for impl_item in &imp.items {
                    let crate::ast::ImplItem::Method(method) = impl_item else {
                        continue;
                    };
                    if method.unstable.is_none() && method.deprecation.is_none() {
                        continue;
                    }
                    out.insert(
                        format!("{type_name}.{}", method.name),
                        (method.unstable.clone(), method.deprecation.clone()),
                    );
                }
            }
        }
        out
    });

/// Compiler builtins / I/O functions visible without import. Implementations
/// stay compiler-side (`!` return type, source-location capture, release
/// elision) per `docs/design.md § Module System › Prelude` — only the names
/// live here.
pub const PRELUDE_FUNCTIONS: &[&str] = &[
    "todo",
    "unreachable",
    "dbg",
    "print",
    "println",
    "eprintln",
    "assert",
    "assert_eq",
    "assert_ne",
    // Scoped provider injection — see docs/design.md § Provider-Rooted
    // Resources. The parser accepts it as an ordinary identifier; the
    // interpreter intercepts the `with_provider[R](p, || body)` call shape
    // to push/pop a provider frame (see Interpreter::match_with_provider).
    "with_provider",
    // Compile-time layout introspection intrinsics — see
    // `runtime/stdlib/intrinsics.kara`. The typechecker intercepts every
    // call site (`infer_layout_query_intrinsic`) to validate the type
    // argument and emit `E_OPAQUE_TYPE_NO_KNOWN_SIZE` for opaque foreign
    // types; codegen intercepts (`compile_layout_query_intrinsic`) to
    // emit the LLVM size / ABI-alignment constant. The prelude entry
    // here is what makes the resolver accept the bare identifier — baked
    // stdlib bypasses the resolver per the comment in
    // `register_baked_stdlib`, so the resolver-side registration is
    // additive on top of the typechecker / codegen wiring.
    "size_of",
    "align_of",
    // `std.time::sleep_ms(ms: i64) with suspends` — native async sleep,
    // the leaf `suspends` timer primitive (auto-par divergence slice
    // A2a-2.2). See `runtime/stdlib/time.kara`. Codegen intercepts the
    // call (`compile_call`) and emits the park-on-timer state machine;
    // the prelude entry makes the resolver accept the bare identifier.
    "sleep_ms",
    // Phase 6 line 186 slice 1 — free-fn `spawn[T](f: Fn() -> T) ->
    // TaskHandle[T]`. Counterpart to `TaskGroup.spawn`; uses an
    // ambient process-wide scope rather than a user-controlled
    // `TaskGroup` scope. See `runtime/stdlib/task_group.kara`. The
    // ownership-side walker (`src/ownership/par_helpers.rs`) has
    // recognised bare-identifier `spawn` as a par-region boundary
    // since the Phase 7 codegen entry "OwnershipChecker Phase 2"
    // shipped 2026-05-18 — registering the name here promotes the
    // call from "boundary-detected unknown callee" to a real
    // stdlib item without changing the boundary-detection behavior.
    "spawn",
    // Phase 6 — `collect_all_vec[T, E](fs: Vec[Fn() -> Result[T, E]]) ->
    // Vec[Result[T, E]]`. Gather-all-errors homogeneous parallel
    // primitive (decl in `runtime/stdlib/task_group.kara`). Like
    // `spawn`, registering the name here promotes the bare-identifier
    // call to a real stdlib item; the call site is intercepted in the
    // interpreter (`eval_collect_all_vec`) and codegen rather than
    // running its `#[compiler_builtin]` placeholder body.
    "collect_all_vec",
    // Phase 6 — `collect_all(|| a, || b, …)`: the heterogeneous
    // fixed-arity (2..=8) gather. No stdlib decl (the return tuple shape
    // varies per arity); the typechecker synthesizes the tuple type
    // (`infer_collect_all`) and the interpreter / codegen intercept the
    // call shape. Registering the name here makes the resolver accept the
    // bare identifier.
    "collect_all",
    // Phase 8 line 153 (active-span propagation) — `std.tracing`.
    // `with_span(span, || body)` installs an ambient active span for the
    // body; the interpreter and codegen intercept the call shape (see
    // `Interpreter::match_with_span` / `match_with_span_call`) like
    // `with_provider`. `tracing_active_span()` reads the active span id
    // (0 = none); the `LogEvent` constructors call it to auto-stamp
    // events, and it's intercepted to the per-thread register rather than
    // running its `#[compiler_builtin]` placeholder body. Registering the
    // names here makes the resolver accept the bare identifiers.
    "with_span",
    "tracing_active_span",
    // Phase 8 line 156 (configurable ambient exporter) — the builtins the
    // rewritten `Log.*` / `Log.set_min_level` / `Log.reset` bodies lower
    // through. Codegen intercepts them to the `karac_tracing_*` process-
    // global config; the interpreter backs them with its tracing fields.
    // (`Log.set_exporter` is intercepted at its call site, not as a bare
    // builtin, so it isn't listed here.)
    "tracing_level_enabled",
    "tracing_emit_event",
    "tracing_set_min_level",
    "tracing_reset",
];

/// Synthetic span used for every stub item the prelude module emits. The
/// resolver / typechecker recognise span (line 0, column 0) as compiler-
/// generated and allow user definitions to shadow it without raising
/// `E0101 DuplicateDefinition`.
fn synthetic_span() -> Span {
    Span {
        line: 0,
        column: 0,
        offset: 0,
        length: 0,
    }
}

/// Build a stub [`Item`] sequence representing the prelude module's
/// publicly-visible surface. The bodies are intentionally empty — the real
/// shape lives in `register_builtin_types` (typechecker) and the resolver's
/// `register_primitives`. These stubs exist purely so cross-module resolution
/// (`module::canonical_origin`, `module::module_exposes_item`,
/// `resolver::module_top_level_names_for_id`) can see `std.prelude` exposing
/// the right names when user code writes `import std.prelude.X;`.
pub fn synthetic_prelude_items() -> Vec<Item> {
    let span = synthetic_span();
    let mut items: Vec<Item> = Vec::new();

    for &name in PRELUDE_TYPES {
        // Slice 3c: prelude type names that have a baked source declaration
        // splice in the real `Item` from `STDLIB_PROGRAMS` (with
        // `stdlib_origin = true` so the slice-3b resolver gate bypass
        // applies even though user-mode resolver sessions walk this
        // module). All other names continue to use the placeholder stub.
        // Slice 4 grows the baked surface one type at a time.
        if let Some(item) = baked_item_for(name) {
            items.push(item);
        } else {
            items.push(stub_struct(name, &span));
        }
    }
    for &name in PRELUDE_TRAITS {
        // Slice 5a extends the same bake-or-stub split to traits.
        if let Some(item) = baked_item_for(name) {
            items.push(item);
        } else {
            items.push(stub_trait(name, &span));
        }
    }
    for &name in PRELUDE_FUNCTIONS {
        items.push(stub_function(name, &span));
    }
    items
    // Note: prelude *variant* names (`Some`, `None`, `Ok`, `Err`, …) are not
    // exposed as top-level items here. They reach user code via the
    // resolver's scope-0 registration (`register_prelude_symbols`) instead —
    // mirroring Rust, where `use std::option::Some;` is not the path users
    // import variants through. Users that need to qualify a variant write
    // `Option.Some(x)` or import the enum and use `Some(x)` unqualified.
}

/// Look up a top-level item by name across every baked stdlib program.
/// Returns a clone with `stdlib_origin = true` flipped on the matching
/// item kind so the resolver's slice-3b gate bypass applies. Slice 3c
/// uses this from `synthetic_prelude_items`; slice 3d wires the same
/// helper into `register_builtin_types` so the typechecker registers
/// from baked source instead of the hardcoded shape.
fn baked_item_for(name: &str) -> Option<Item> {
    for (_, program) in STDLIB_PROGRAMS.iter() {
        for item in &program.items {
            let matches = match item {
                Item::Function(f) => f.name == name,
                Item::StructDef(s) => s.name == name,
                Item::EnumDef(e) => e.name == name,
                Item::TraitDef(t) => t.name == name,
                _ => false,
            };
            if !matches {
                continue;
            }
            let mut cloned = item.clone();
            match &mut cloned {
                Item::Function(f) => f.stdlib_origin = true,
                Item::StructDef(s) => s.stdlib_origin = true,
                Item::EnumDef(e) => e.stdlib_origin = true,
                Item::TraitDef(t) => t.stdlib_origin = true,
                _ => {}
            }
            return Some(cloned);
        }
    }
    None
}

fn stub_struct(name: &str, span: &Span) -> Item {
    Item::StructDef(StructDef {
        span: span.clone(),
        attributes: Vec::new(),
        doc_comment: None,
        is_pub: true,
        is_private: false,
        is_shared: false,
        is_par: false,
        // Synthetic prelude stub — no real source position for the
        // `struct` keyword. Carries the item's full span as a benign
        // placeholder; fix_diff edit emission only consults this when
        // a `ConcurrentPlainStruct` diagnostic resolves to the matching
        // StructDef, which never happens for prelude stubs (they don't
        // appear in user `par` blocks).
        struct_keyword_span: span.clone(),
        kind_keyword_span: None,
        no_rc: false,
        name: name.to_string(),
        generic_params: stub_generics(name, span),
        where_clause: None,
        fields: Vec::new(),
        invariants: Vec::new(),
        impl_invariants: Vec::new(),
        stdlib_origin: true,
        deprecation: None,
        unstable: None,
        is_non_exhaustive: false,
        lint_overrides: Vec::new(),
    })
}

/// Generic parameter list for the few prelude types whose generic arity is
/// commonly inspected. The stubs do not have to match the *real* generic
/// arity exactly — they exist only so resolver / typechecker queries that
/// read generic parameter count from the typechecker's `register_builtin_types`
/// env entries stay authoritative; the synthetic module shim never
/// participates in type inference.
fn stub_generics(name: &str, span: &Span) -> Option<GenericParams> {
    let params: &[&str] = match name {
        "Option" | "Vec" | "VecDeque" | "Slice" | "Array" | "Vector" | "Set" | "Atomic"
        | "Mutex" | "SortedSet" | "Channel" | "Sender" | "Receiver" | "BufReader" | "BufWriter" => {
            &["T"]
        }
        "Result" => &["T", "E"],
        "Map" | "Entry" => &["K", "V"],
        _ => return None,
    };
    Some(GenericParams {
        span: span.clone(),
        effect_params: Vec::new(),
        params: params
            .iter()
            .map(|p| GenericParam {
                span: span.clone(),
                name: (*p).to_string(),
                bounds: Vec::new(),
                is_const: false,
                const_type: None,
                variance: Variance::Invariant,
                variance_span: None,
                is_variadic_shape: false,
            })
            .collect(),
    })
}

fn stub_trait(name: &str, span: &Span) -> Item {
    Item::TraitDef(TraitDef {
        span: span.clone(),
        attributes: Vec::new(),
        doc_comment: None,
        is_pub: true,
        is_private: false,
        name: name.to_string(),
        generic_params: None,
        supertraits: Vec::new(),
        trait_effects: None,
        where_clause: None,
        items: Vec::new(),
        stdlib_origin: true,
        deprecation: None,
        unstable: None,
        lint_overrides: Vec::new(),
        on_unimplemented: None,
    })
}

fn stub_function(name: &str, span: &Span) -> Item {
    Item::Function(Function {
        span: span.clone(),
        attributes: Vec::new(),
        doc_comment: None,
        is_pub: true,
        is_private: false,
        is_unsafe: false,
        name: name.to_string(),
        generic_params: None,
        params: Vec::new(),
        self_param: None,
        return_type: None,
        effects: None,
        requires: Vec::new(),
        ensures: Vec::new(),
        where_clause: None,
        body: Block {
            stmts: Vec::new(),
            final_expr: None,
            span: span.clone(),
        },
        stdlib_origin: true,
        deprecation: None,
        unstable: None,
        is_track_caller: false,
        lint_overrides: Vec::new(),
        profile_compat: Vec::new(),
        abi: None,
    })
}

/// True iff `path` names the synthetic prelude module.
pub fn is_prelude_path(path: &[String]) -> bool {
    path.len() == PRELUDE_PATH_SEGMENTS.len()
        && path
            .iter()
            .zip(PRELUDE_PATH_SEGMENTS.iter())
            .all(|(a, b)| a == b)
}

/// Visibility every synthetic prelude item carries. Kept as a helper so
/// downstream call sites do not have to repeat the literal.
#[allow(dead_code)]
pub fn prelude_visibility() -> Visibility {
    Visibility::Pub
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{EnumDef, Item, VariantKind};

    /// Find the `EnumDef` for `name` among the items of the parsed stdlib
    /// program at the given index in `STDLIB_PROGRAMS`. Test helper.
    fn find_enum(idx: usize, name: &str) -> &'static EnumDef {
        let (_, program) = &STDLIB_PROGRAMS[idx];
        for item in &program.items {
            if let Item::EnumDef(e) = item {
                if e.name == name {
                    return e;
                }
            }
        }
        panic!(
            "expected enum `{}` in stdlib program at index {}",
            name, idx
        );
    }

    #[test]
    fn stdlib_sources_contains_option_kara() {
        let names: Vec<&str> = STDLIB_SOURCES.iter().map(|(n, _)| *n).collect();
        assert!(
            names.contains(&"option.kara"),
            "STDLIB_SOURCES should contain option.kara, got: {:?}",
            names
        );
    }

    #[test]
    fn stdlib_sources_contains_result_kara() {
        // CR-202 slice 4a: `Result` joins the baked surface.
        let names: Vec<&str> = STDLIB_SOURCES.iter().map(|(n, _)| *n).collect();
        assert!(
            names.contains(&"result.kara"),
            "STDLIB_SOURCES should contain result.kara, got: {:?}",
            names
        );
    }

    #[test]
    fn stdlib_sources_contains_vec_kara() {
        // CR-202 slice 4b: `Vec` joins the baked surface.
        let names: Vec<&str> = STDLIB_SOURCES.iter().map(|(n, _)| *n).collect();
        assert!(
            names.contains(&"vec.kara"),
            "STDLIB_SOURCES should contain vec.kara, got: {:?}",
            names
        );
    }

    #[test]
    fn stdlib_sources_contains_map_kara() {
        // CR-202 slice 6.1a: Map joins the baked surface.
        let names: Vec<&str> = STDLIB_SOURCES.iter().map(|(n, _)| *n).collect();
        assert!(
            names.contains(&"map.kara"),
            "STDLIB_SOURCES should contain map.kara, got: {:?}",
            names
        );
    }

    #[test]
    fn stdlib_sources_contains_partial_eq_kara() {
        // CR-202 slice 5a: first baked trait file.
        let names: Vec<&str> = STDLIB_SOURCES.iter().map(|(n, _)| *n).collect();
        assert!(
            names.contains(&"partial_eq.kara"),
            "STDLIB_SOURCES should contain partial_eq.kara, got: {:?}",
            names
        );
    }

    #[test]
    fn stdlib_sources_contains_eq_kara() {
        // CR-202 slice 5b: `Eq` joins the baked surface with `: PartialEq`.
        let names: Vec<&str> = STDLIB_SOURCES.iter().map(|(n, _)| *n).collect();
        assert!(
            names.contains(&"eq.kara"),
            "STDLIB_SOURCES should contain eq.kara, got: {:?}",
            names
        );
    }

    #[test]
    fn stdlib_sources_contains_partial_ord_kara() {
        // CR-202 slice 5c: `PartialOrd: PartialEq` joins the baked surface.
        let names: Vec<&str> = STDLIB_SOURCES.iter().map(|(n, _)| *n).collect();
        assert!(
            names.contains(&"partial_ord.kara"),
            "STDLIB_SOURCES should contain partial_ord.kara, got: {:?}",
            names
        );
    }

    #[test]
    fn stdlib_sources_contains_ord_kara() {
        // CR-202 slice 5d: `Ord: PartialOrd + Eq` joins the baked surface.
        let names: Vec<&str> = STDLIB_SOURCES.iter().map(|(n, _)| *n).collect();
        assert!(
            names.contains(&"ord.kara"),
            "STDLIB_SOURCES should contain ord.kara, got: {:?}",
            names
        );
    }

    #[test]
    fn stdlib_sources_contains_hash_kara() {
        // CR-202 slice 5e: `Hash` joins the baked surface (without the
        // `Hasher` bound — that lands when Hasher itself is baked).
        let names: Vec<&str> = STDLIB_SOURCES.iter().map(|(n, _)| *n).collect();
        assert!(
            names.contains(&"hash.kara"),
            "STDLIB_SOURCES should contain hash.kara, got: {:?}",
            names
        );
    }

    #[test]
    fn stdlib_sources_contains_display_and_debug_kara() {
        // CR-202 slices 5f/5g: Display moves to baked (replacing the
        // entry in register_stdlib_traits); Debug joins as a new entry.
        let names: Vec<&str> = STDLIB_SOURCES.iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"display.kara"));
        assert!(names.contains(&"debug.kara"));
    }

    #[test]
    fn stdlib_sources_have_nonempty_bodies() {
        for &(name, src) in STDLIB_SOURCES {
            assert!(
                !src.trim().is_empty(),
                "stdlib source `{}` should not be empty",
                name
            );
        }
    }

    #[test]
    fn stdlib_programs_parses_option_cleanly() {
        // Forces evaluation of the LazyLock; would panic with a parse-error
        // message if the bake source is malformed.
        let programs: &Vec<(&'static str, Program)> = &STDLIB_PROGRAMS;
        assert_eq!(
            programs.len(),
            STDLIB_SOURCES.len(),
            "STDLIB_PROGRAMS should have one entry per STDLIB_SOURCES entry"
        );
    }

    #[test]
    fn baked_option_has_some_and_none_variants() {
        let opt = find_enum(0, "Option");
        let variant_names: Vec<&str> = opt.variants.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(
            variant_names,
            vec!["Some", "None"],
            "baked Option should declare exactly Some(T), None"
        );
    }

    #[test]
    fn baked_option_has_one_generic_param_named_t() {
        let opt = find_enum(0, "Option");
        let params = opt
            .generic_params
            .as_ref()
            .expect("baked Option should declare a generic parameter list");
        let names: Vec<&str> = params.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["T"]);
    }

    #[test]
    fn baked_option_some_variant_carries_type_param() {
        let opt = find_enum(0, "Option");
        let some = opt
            .variants
            .iter()
            .find(|v| v.name == "Some")
            .expect("Some variant should exist");
        match &some.kind {
            VariantKind::Tuple(types) => {
                assert_eq!(types.len(), 1, "Some(T) should carry exactly one type");
            }
            other => panic!("expected Some to be Tuple-shaped, got {:?}", other),
        }
    }

    #[test]
    fn baked_option_none_variant_is_unit() {
        let opt = find_enum(0, "Option");
        let none = opt
            .variants
            .iter()
            .find(|v| v.name == "None")
            .expect("None variant should exist");
        assert!(
            matches!(none.kind, VariantKind::Unit),
            "None should be a Unit variant, got {:?}",
            none.kind
        );
    }

    // ── Slice 3d verification: synthetic_prelude_items splices baked Option ──

    fn find_prelude_item<'a>(items: &'a [Item], name: &str) -> Option<&'a Item> {
        items.iter().find(|i| match i {
            Item::Function(f) => f.name == name,
            Item::StructDef(s) => s.name == name,
            Item::EnumDef(e) => e.name == name,
            Item::TraitDef(t) => t.name == name,
            _ => false,
        })
    }

    #[test]
    fn synthetic_prelude_items_returns_baked_option_as_enum_def() {
        // Pre-3c the prelude module exposed Option as `Item::StructDef`
        // (a placeholder stub from `stub_struct`). After 3c the splice
        // should be the real `Item::EnumDef` parsed from
        // `runtime/stdlib/option.kara`.
        let items = synthetic_prelude_items();
        let opt = find_prelude_item(&items, "Option").expect("synthetic prelude exposes Option");
        assert!(
            matches!(opt, Item::EnumDef(_)),
            "Option should be spliced as EnumDef (baked), got {:?}",
            opt
        );
    }

    #[test]
    fn synthetic_prelude_items_returns_baked_result_as_enum_def() {
        // CR-202 slice 4a: same splice path, second file. Confirms the
        // multi-file `STDLIB_SOURCES` path resolves Result through
        // `baked_item_for` rather than falling back to the stub.
        let items = synthetic_prelude_items();
        let res = find_prelude_item(&items, "Result").expect("synthetic prelude exposes Result");
        assert!(
            matches!(res, Item::EnumDef(_)),
            "Result should be spliced as EnumDef (baked), got {:?}",
            res
        );
        let Item::EnumDef(e) = res else {
            unreachable!()
        };
        assert!(
            e.span.line > 0,
            "baked Result should carry a real source span"
        );
        assert!(
            e.stdlib_origin,
            "baked Result should be tagged stdlib_origin = true"
        );
    }

    #[test]
    fn synthetic_prelude_items_returns_baked_vec_as_struct_def() {
        // CR-202 slice 4b: Vec joins the baked surface as a struct.
        // Pre-4b Vec was a `stub_struct` with synthetic span; post-4b it
        // is the real `struct Vec[T] { }` from baked source.
        let items = synthetic_prelude_items();
        let v = find_prelude_item(&items, "Vec").expect("synthetic prelude exposes Vec");
        let Item::StructDef(s) = v else {
            panic!("Vec should be spliced as StructDef (baked), got {:?}", v);
        };
        assert!(s.span.line > 0, "baked Vec should carry a real source span");
        assert!(
            s.stdlib_origin,
            "baked Vec should be tagged stdlib_origin = true"
        );
        let params = s
            .generic_params
            .as_ref()
            .expect("baked Vec should declare a generic param list");
        assert_eq!(
            params
                .params
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec!["T"],
        );
    }

    #[test]
    fn synthetic_prelude_items_returns_baked_partial_eq_as_trait_def() {
        // CR-202 slice 5a: first baked trait. Pre-5a `PartialEq` did not
        // appear in `PRELUDE_TRAITS` and was therefore not exposed
        // through the synthetic prelude module at all. After 5a it is a
        // real `Item::TraitDef` from `runtime/stdlib/partial_eq.kara`.
        let items = synthetic_prelude_items();
        let pe =
            find_prelude_item(&items, "PartialEq").expect("synthetic prelude exposes PartialEq");
        let Item::TraitDef(t) = pe else {
            panic!(
                "PartialEq should be spliced as TraitDef (baked), got {:?}",
                pe
            );
        };
        assert!(
            t.span.line > 0,
            "baked PartialEq should carry a real source span"
        );
        assert!(
            t.stdlib_origin,
            "baked PartialEq should be tagged stdlib_origin = true"
        );
        // Should declare exactly one method (`eq`), no associated types.
        let method_names: Vec<&str> = t
            .items
            .iter()
            .filter_map(|i| match i {
                crate::ast::TraitItem::Method(m) => Some(m.name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(method_names, vec!["eq"]);
    }

    #[test]
    fn synthetic_prelude_items_returns_baked_eq_with_partial_eq_supertrait() {
        // CR-202 slice 5b: `Eq` is now `Eq: PartialEq` from baked source.
        // Pre-5b the hardcoded `register_stdlib_traits` array registered
        // `Eq` with no supertraits.
        let items = synthetic_prelude_items();
        let eq = find_prelude_item(&items, "Eq").expect("synthetic prelude exposes Eq");
        let Item::TraitDef(t) = eq else {
            panic!("Eq should be spliced as TraitDef (baked), got {:?}", eq);
        };
        assert!(t.span.line > 0, "baked Eq should carry a real source span");
        assert!(
            t.stdlib_origin,
            "baked Eq should be tagged stdlib_origin = true"
        );
        let supertrait_names: Vec<&str> = t
            .supertraits
            .iter()
            .map(|b| b.path.last().map(|s| s.as_str()).unwrap_or(""))
            .collect();
        assert_eq!(
            supertrait_names,
            vec!["PartialEq"],
            "baked Eq should declare `PartialEq` as its sole supertrait"
        );
        let method_count = t
            .items
            .iter()
            .filter(|i| matches!(i, crate::ast::TraitItem::Method(_)))
            .count();
        assert_eq!(method_count, 0, "Eq should declare no methods of its own");
    }

    #[test]
    fn baked_option_has_real_source_span_not_synthetic() {
        // The placeholder stubs use `synthetic_span()` (line 0, column 0,
        // offset 0). The baked source's span is set by the parser based
        // on the actual byte offset of the `enum Option` declaration in
        // `runtime/stdlib/option.kara`, so it has a non-zero line. This
        // is what 3d's diagnostic-span improvement rests on:
        // Option-related diagnostics now point at the real source rather
        // than the synthetic origin.
        let items = synthetic_prelude_items();
        let opt = find_prelude_item(&items, "Option").unwrap();
        let Item::EnumDef(e) = opt else {
            panic!("expected EnumDef");
        };
        assert!(
            e.span.line > 0,
            "baked Option's span should point at the real source (non-zero line), got line={}",
            e.span.line
        );
    }

    #[test]
    fn baked_option_carries_stdlib_origin_tag() {
        // The slice-3b gate bypass relies on `stdlib_origin = true` on
        // baked items. `baked_item_for` flips it after cloning; verify
        // the splice preserves the flag end to end.
        let items = synthetic_prelude_items();
        let opt = find_prelude_item(&items, "Option").unwrap();
        let Item::EnumDef(e) = opt else {
            panic!("expected EnumDef");
        };
        assert!(
            e.stdlib_origin,
            "baked Option should be tagged stdlib_origin = true"
        );
    }

    #[test]
    fn placeholder_stub_for_unbaked_prelude_type_keeps_synthetic_span() {
        // CR-202 contract: only types with a baked source file get the
        // real-source treatment; everything else continues to use
        // `stub_struct` with a synthetic span. This pins the
        // partial-migration property until the remaining types migrate.
        // `Slice` is a stable picker — it's a built-in primitive
        // (`Type::Slice` in `lower_path_type`) that the per-type slice
        // schedule explicitly defers, so its stub status holds across
        // slice-6.1's mechanical migrations.
        let items = synthetic_prelude_items();
        let slice_item = find_prelude_item(&items, "Slice").expect("Slice is still a prelude name");
        match slice_item {
            Item::StructDef(s) => {
                assert_eq!(
                    s.span.line, 0,
                    "non-baked prelude type should still use synthetic span"
                );
                assert!(
                    s.stdlib_origin,
                    "stubs are still tagged stdlib_origin = true \
                     (the synthetic prelude module IS stdlib origin)"
                );
            }
            other => panic!("Slice should still be a stub StructDef, got {:?}", other),
        }
    }

    // ── Slice 6.3: Stats methods migrated to impl-block ─────────────
    //
    // CR-202 slice 6.3 retires `env.functions["Stats.<method>"]` for
    // every Stats method in favour of
    // `impl Stats { #[compiler_builtin] fn ... }` in baked source.
    // This test pins the AST shape; the dispatch round-trip is covered
    // by `tests/typechecker.rs::test_stats_*_ok` and
    // `tests/interpreter.rs::test_stats_*`.

    /// Assert that the baked stdlib file `file_basename` declares an
    /// inherent (no trait_name) impl block on `target_type` whose methods
    /// include each name in `expected` and that every method carries the
    /// `#[compiler_builtin]` attribute. Test helper used to pin slice-6.3
    /// migrations across multiple types.
    fn assert_inherent_impl_compiler_builtin(
        file_basename: &str,
        target_type: &str,
        expected: &[&str],
    ) {
        let program = STDLIB_PROGRAMS
            .iter()
            .find(|(name, _)| *name == file_basename)
            .map(|(_, p)| p)
            .unwrap_or_else(|| panic!("{} should be in STDLIB_PROGRAMS", file_basename));
        let impls: Vec<_> = program
            .items
            .iter()
            .filter_map(|i| match i {
                Item::ImplBlock(b) => Some(b),
                _ => None,
            })
            .collect();
        let imp = impls
            .iter()
            .find(|b| {
                b.trait_name.is_none()
                    && match &b.target_type.kind {
                        crate::ast::TypeKind::Path(p) => {
                            p.segments.last().is_some_and(|s| s == target_type)
                        }
                        _ => false,
                    }
            })
            .unwrap_or_else(|| {
                panic!(
                    "{} should declare an inherent impl block on `{}`",
                    file_basename, target_type
                )
            });
        for name in expected {
            let method = imp
                .items
                .iter()
                .find_map(|item| match item {
                    crate::ast::ImplItem::Method(m) if m.name == *name => Some(m),
                    _ => None,
                })
                .unwrap_or_else(|| {
                    panic!(
                        "`impl {}` in {} should declare method `{}`",
                        target_type, file_basename, name
                    )
                });
            assert!(
                method
                    .attributes
                    .iter()
                    .any(|a| a.is_bare("compiler_builtin")),
                "{}.{} should carry #[compiler_builtin]",
                target_type,
                name
            );
        }
    }

    #[test]
    fn baked_stats_carries_inherent_impl_with_compiler_builtin_methods() {
        assert_inherent_impl_compiler_builtin(
            "stats.kara",
            "Stats",
            &[
                "sum", "prod", "mean", "variance", "stddev", "median", "min", "max",
            ],
        );
    }

    #[test]
    fn baked_regex_carries_inherent_impl_with_compiler_builtin_methods() {
        assert_inherent_impl_compiler_builtin(
            "regex.kara",
            "Regex",
            &["compile", "is_match", "find", "find_all", "replace_all"],
        );
    }

    #[test]
    fn baked_http_carries_inherent_impl_with_compiler_builtin_methods() {
        assert_inherent_impl_compiler_builtin(
            "http.kara",
            "Client",
            &["new", "get", "post", "request"],
        );
        // Phase-8 line 24 — chained-builder configuration + send.
        assert_inherent_impl_compiler_builtin(
            "http.kara",
            "RequestBuilder",
            &["header", "body", "timeout", "send"],
        );
        // Phase-8 line 32 — `text()` / `bytes()` return-type split
        // (text = String view, bytes = `Vec[u8]` raw-byte view).
        assert_inherent_impl_compiler_builtin(
            "http.kara",
            "Response",
            &["status", "body", "bytes", "header", "headers"],
        );
        assert_inherent_impl_compiler_builtin("http.kara", "HttpError", &["message"]);
        // Slice B (2026-05-09): server surface. `serve` is the Slice B
        // follow-up handler-dispatch entry (codegen + thin stdlib
        // declaration; runtime extern at `runtime/src/lib.rs:1879`).
        assert_inherent_impl_compiler_builtin(
            "http.kara",
            "Server",
            &["serve_static", "serve", "serve_tls"],
        );
        // HTTP handler ABI trampoline (2026-05-09): F3 method surface —
        // `Request.path()` + `Request.method()` + `Request.body()` +
        // `Request.header(name)` round-trip through the runtime externs
        // and copy bytes into a fresh owned String per call (F2 owned-
        // String contract; `header` wraps the result in `Option[String]`).
        // Phase-8 line 13: `headers()` + `query()` add full-map iteration,
        // each returning `Vec[(String, String)]`.
        assert_inherent_impl_compiler_builtin(
            "http.kara",
            "Request",
            &["path", "method", "body", "header", "headers", "query"],
        );
    }

    #[test]
    fn baked_encoding_carries_inherent_impl_with_compiler_builtin_methods() {
        assert_inherent_impl_compiler_builtin(
            "encoding.kara",
            "Base64",
            &["encode", "encode_url_safe", "decode"],
        );
        assert_inherent_impl_compiler_builtin(
            "encoding.kara",
            "Hex",
            &["encode", "encode_upper", "decode"],
        );
        assert_inherent_impl_compiler_builtin("encoding.kara", "Url", &["encode", "decode"]);
    }

    /// `drop_carries_soundness` audit gate (design.md § Drop >
    /// "Destructors are NOT soundness mechanisms"; phase-8 § Stdlib
    /// lint — `drop_carries_soundness` audit checklist). Every
    /// `impl Drop` in the baked + gated stdlib must (a) target a type
    /// on the audited allowlist below and (b) carry a
    /// `Drop-skip-sound:` line in the comment block directly above the
    /// impl, recording the answer to "if this Drop never runs, what
    /// becomes unsafe?" — which must be "nothing: only resources
    /// leak; no UB, no type-system violation". Adding a `Drop` impl to
    /// a new stdlib type fails this gate until the audit answer is
    /// written at the impl site and the type is appended here. The
    /// stdlib is baked into the compiler binary, so `cargo test` IS
    /// the stdlib build step (mirrors the variance hygiene gate in
    /// `src/typechecker/variance.rs`).
    #[test]
    fn baked_stdlib_drop_impls_are_audited_drop_skip_sound() {
        // Audited 2026-06-07. Per-type answers live in the
        // `Drop-skip-sound:` comment above each impl.
        const AUDITED: &[&str] = &[
            "BoundedChannel",
            "PooledConnection",
            "TaskGroup",
            "TcpListener",
            "TcpStream",
            "TlsListener",
            "TlsStream",
            "WebSocket",
        ];

        // (a) Parse-level: every stdlib `impl Drop for X` has an
        // audited `X`.
        let mut seen = std::collections::BTreeSet::new();
        let programs: Vec<(String, &crate::ast::Program)> = STDLIB_PROGRAMS
            .iter()
            .map(|(n, p)| (n.to_string(), p))
            .chain(
                GATED_STDLIB_PROGRAMS
                    .iter()
                    .map(|(path, p)| (path.join("."), p)),
            )
            .collect();
        for (name, program) in &programs {
            for item in &program.items {
                let Item::ImplBlock(imp) = item else { continue };
                let is_drop = imp
                    .trait_name
                    .as_ref()
                    .and_then(|p| p.segments.last())
                    .is_some_and(|s| s == "Drop");
                if !is_drop {
                    continue;
                }
                let target = match &imp.target_type.kind {
                    crate::ast::TypeKind::Path(p) => p.segments.last().cloned().unwrap_or_default(),
                    _ => String::new(),
                };
                assert!(
                    AUDITED.contains(&target.as_str()),
                    "{name}: `impl Drop for {target}` is not on the \
                     drop_carries_soundness audited allowlist — answer \
                     \"if this Drop never runs, what becomes unsafe?\" \
                     (must be \"nothing — only resources leak\"), record \
                     it in a `Drop-skip-sound:` comment directly above \
                     the impl, and append the type to AUDITED",
                );
                seen.insert(target);
            }
        }
        // Non-vacuity + pruning: the allowlist must be exactly the set
        // of Drop impls found — a stale entry means the impl was
        // removed (prune it); an empty `seen` means the parse-level
        // scan broke.
        let expected: std::collections::BTreeSet<String> =
            AUDITED.iter().map(|s| s.to_string()).collect();
        assert_eq!(
            seen, expected,
            "AUDITED allowlist is out of sync with the stdlib's actual \
             `impl Drop` set",
        );

        // (b) Source-level: the contiguous comment block directly
        // above each `impl … Drop for …` contains the marker.
        let sources = STDLIB_SOURCES
            .iter()
            .map(|(n, s)| (n.to_string(), *s))
            .chain(
                GATED_STDLIB_SOURCES
                    .iter()
                    .map(|(path, s)| (path.join("."), *s)),
            );
        for (name, src) in sources {
            let lines: Vec<&str> = src.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                let t = line.trim_start();
                if !(t.starts_with("impl") && t.contains(" Drop for ")) {
                    continue;
                }
                let audited = (0..i)
                    .rev()
                    .map(|j| lines[j].trim_start())
                    .take_while(|c| c.starts_with("//"))
                    .any(|c| c.contains("Drop-skip-sound:"));
                assert!(
                    audited,
                    "{name}:{}: `impl Drop` lacks a `Drop-skip-sound:` \
                     line in the comment block directly above it",
                    i + 1,
                );
            }
        }
    }
}
