//! Value model: the `Value` enum and its impls, the supporting carrier
//! types (`EnumData`, `IteratorSource`, `IteratorStep`, `FieldCell`,
//! `SharedStructInner`, `OrdValue`), the runtime error / test outcome
//! types (`ErrorTraceFrame`, `RuntimeError`, `TestOutcome`), and the
//! free helpers `try_write_or_panic` / `primitive_const_to_value`.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard};

use crate::ast::*;
use crate::token::Span;

use super::helpers::value_compare;

// ── Error Return Trace ─────────────────────────────────────────

pub(crate) const ERROR_TRACE_MAX_DEPTH: usize = 64;

#[derive(Debug, Clone)]
pub struct ErrorTraceFrame {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub expr: String,
}

/// A user-triggered runtime error raised during interpretation (division by
/// zero, integer overflow, index out of bounds, unwrap of None/Err, etc.).
/// Distinct from compiler-invariant panics — those stay as `unreachable!`
/// because they indicate a bug in an earlier phase, not in user code.
#[derive(Debug, Clone)]
pub struct RuntimeError {
    pub message: String,
    pub span: Span,
    /// `assert_eq` / `assert_ne` failures populate these with the formatted
    /// left and right values so the test runner can surface them in
    /// structured `test_fail` events. `None` for any other runtime error.
    pub left: Option<String>,
    pub right: Option<String>,
}

/// Outcome of a single test invocation, produced by
/// [`Interpreter::run_test_function`]. The runner translates this into a
/// `test_pass` or `test_fail` JSONL event.
#[derive(Debug, Clone)]
pub struct TestOutcome {
    pub passed: bool,
    pub message: Option<String>,
    pub span: Option<Span>,
    pub left: Option<String>,
    pub right: Option<String>,
}

// ── Runtime Values ──────────────────────────────────────────────

/// One recorded step of a `LazyFrame` logical plan (phase-11
/// `LazyDataFrame` Option A, slice 1). Applied in list order over the
/// source scan; `collect()` / `explain()` fold the list into the
/// optimized single-scan form (outermost projection + minimum limit).
#[derive(Debug, Clone)]
pub enum LazyOp {
    /// Project to the named columns, in the given order.
    Select(Vec<String>),
    /// Keep at most the first `n` rows (already clamped to `>= 0`).
    Limit(i64),
    /// Keep only rows where the predicate expression evaluates true
    /// (slice 2). Column refs validate at collect against the columns
    /// visible at this step; a NULL slot fails any comparison.
    Filter(Arc<LazyExprIR>),
    /// Stable multi-key sort (slice 3). Each key is an expression,
    /// optionally `Desc`-wrapped for descending; NULL keys sort last.
    Sort(Vec<Arc<LazyExprIR>>),
    /// Group-by + aggregate (slice 4): first-occurrence group order;
    /// output schema = key columns then one column per aggregate.
    GroupBy {
        keys: Vec<Arc<LazyExprIR>>,
        aggs: Vec<Arc<LazyExprIR>>,
    },
    /// Inner join (slice 5) — the right side is a whole nested sub-plan
    /// (the plan tree's second child; the left spine stays the linear op
    /// list). `on` keys must exist on both sides; non-key right columns
    /// that collide with left names take a `_right` suffix.
    Join {
        right_source: Arc<RwLock<Vec<(String, Value)>>>,
        right_ops: Arc<Vec<LazyOp>>,
        on: Vec<String>,
    },
}

/// A lazy scalar expression tree (phase-11 LazyDataFrame slice 2) — the
/// planner's pushdown unit. Built by `LazyExpr.col(..)` + the comparison /
/// boolean builder methods; inspectable DATA, unlike a closure. Rendered
/// by `explain()`; evaluated per row at `collect()`.
#[derive(Debug, Clone, PartialEq)]
pub enum LazyExprIR {
    /// A column reference, resolved at the plan step where the enclosing
    /// expression applies.
    Col(String),
    LitInt(i64),
    LitFloat(f64),
    LitStr(String),
    LitBool(bool),
    /// A comparison — bool-valued; NULL on either side makes it FALSE
    /// (documented simple semantics, not full SQL three-valued logic).
    Cmp {
        op: LazyCmpOp,
        lhs: Box<LazyExprIR>,
        rhs: Box<LazyExprIR>,
    },
    And(Box<LazyExprIR>, Box<LazyExprIR>),
    Or(Box<LazyExprIR>, Box<LazyExprIR>),
    Not(Box<LazyExprIR>),
    /// Descending sort-key marker (`col("cnt").desc()`) — only
    /// meaningful as a `LazyFrame.sort` key; an error anywhere else.
    Desc(Box<LazyExprIR>),
    /// An aggregate over a group (slice 4) — only meaningful inside
    /// `LazyGroupBy.agg(..)`; an error in filter / sort position.
    Agg {
        op: LazyAggOp,
        arg: Box<LazyExprIR>,
    },
    /// Output-column name override for an aggregate (`.alias("cnt")`).
    Alias {
        name: String,
        expr: Box<LazyExprIR>,
    },
}

/// The slice-4 aggregate vocabulary. `Count` counts NON-NULL values;
/// `Sum`/`Mean` skip nulls (all-null group → NULL result, count → 0);
/// `Min`/`Max` order numbers and Strings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LazyAggOp {
    Count,
    Sum,
    Mean,
    Min,
    Max,
}

impl LazyAggOp {
    pub fn name(&self) -> &'static str {
        match self {
            LazyAggOp::Count => "count",
            LazyAggOp::Sum => "sum",
            LazyAggOp::Mean => "mean",
            LazyAggOp::Min => "min",
            LazyAggOp::Max => "max",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LazyCmpOp {
    Gt,
    Ge,
    Lt,
    Le,
    Eq,
    Ne,
}

impl LazyCmpOp {
    /// The rendering used by `explain()` — Kāra's own operator spellings.
    pub fn symbol(&self) -> &'static str {
        match self {
            LazyCmpOp::Gt => ">",
            LazyCmpOp::Ge => ">=",
            LazyCmpOp::Lt => "<",
            LazyCmpOp::Le => "<=",
            LazyCmpOp::Eq => "==",
            LazyCmpOp::Ne => "!=",
        }
    }
}

impl std::fmt::Display for LazyExprIR {
    /// Deterministic fully-parenthesized rendering for `explain()` —
    /// optimizer tests pin it byte-for-byte.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LazyExprIR::Col(n) => write!(f, "{n}"),
            LazyExprIR::LitInt(v) => write!(f, "{v}"),
            LazyExprIR::LitFloat(v) => write!(f, "{v}"),
            LazyExprIR::LitStr(s) => write!(f, "\"{s}\""),
            LazyExprIR::LitBool(b) => write!(f, "{b}"),
            LazyExprIR::Cmp { op, lhs, rhs } => write!(f, "({lhs} {} {rhs})", op.symbol()),
            LazyExprIR::And(a, b) => write!(f, "({a} and {b})"),
            LazyExprIR::Or(a, b) => write!(f, "({a} or {b})"),
            LazyExprIR::Not(x) => write!(f, "(not {x})"),
            LazyExprIR::Desc(x) => write!(f, "{x} desc"),
            LazyExprIR::Agg { op, arg } => write!(f, "{}({arg})", op.name()),
            LazyExprIR::Alias { name, expr } => write!(f, "{expr} as {name}"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    Char(char),
    String(String),
    /// `ref CStr` — the value of a `c"..."` literal (design.md § C-String
    /// Literals). Bytes exclude the trailing NUL (the terminator is a
    /// codegen-level artifact; `len()` reports the source byte count).
    /// `Arc` so aliasing a `ref CStr` binding is a refcount bump,
    /// mirroring the compiled form's thin-reference semantics (a rodata
    /// pointer). The tree-walk interpreter has no raw-pointer
    /// representation, so `as_ptr()` is rejected at eval time with a
    /// pointer at compiled mode (see `try_eval_seq_method`'s CStr arm).
    CStr(Arc<Vec<u8>>),
    /// `CString` — the owning C-string produced by `String.to_cstring()`
    /// (design.md § C-String Literals, "Owning `CString`"). Bytes exclude the
    /// trailing NUL, exactly like `CStr`; the ownership distinction that is
    /// real under `karac build` (heap buffer + `Drop`) is unobservable in the
    /// tree-walk interpreter, so the representation matches `CStr`. `as_ptr()`
    /// is likewise rejected at eval time (no raw-pointer representation).
    CString(Arc<Vec<u8>>),
    Unit,
    /// A `Type` pseudovalue — the comptime-only first-class type value
    /// (deferred.md § Comptime — Types as first-class values). Carries the
    /// canonical type name; the reflection API (`name()`, `fields()`,
    /// `variants()`, `is_struct()`, …) dispatches on it during comptime
    /// evaluation. A `TypeVal` only ever exists inside a `comptime` context
    /// — the typechecker rejects one flowing to runtime
    /// (`E_TYPE_VALUE_AT_RUNTIME`), and the comptime fold pass treats it as
    /// non-foldable, so it never reaches the runtime program.
    TypeVal(String),
    /// An `Expr` AST value — a comptime-only first-class fragment of code
    /// produced by the quasi-quote builder `ast.expr(s)` (substrate 3,
    /// deferred.md § Comptime — AST builder API). When a `comptime { ... }`
    /// block yields an `AstExpr`, the fold pass splices the contained
    /// expression at the comptime site (code generation) rather than folding a
    /// constant. Comptime-only: like `TypeVal`, it never reaches the runtime
    /// program as a value.
    AstExpr(Box<crate::ast::Expr>),
    /// An `Item` AST value — a comptime-only first-class fragment of code
    /// produced by the item builder `ast.item(s)` (substrate 4, deferred.md §
    /// Comptime — Code generation and derive desugaring). A `#[derive(X)]`
    /// expands to a call to `derive_x(comptime T: Type) -> Vec[Item]`; the
    /// returned `AstItem`s are spliced into the module after the derive site.
    /// Comptime-only: like `TypeVal` / `AstExpr`, it never reaches the runtime
    /// program as a value.
    AstItem(Box<crate::ast::Item>),
    Tuple(Vec<Value>),
    /// Sequence storage shared between the source binding and any live
    /// slice views. `Arc<RwLock<...>>` is universal — every Array
    /// allocation carries the shared cell whether or not it ever gets
    /// sliced, because retroactive upgrade when slice creation finds the
    /// source in another binding / struct field is significantly more
    /// complex. Tree-walk perf is irrelevant for v1; the extra
    /// `Arc::clone` + `RwLock::read/write` per op is the design's
    /// accepted cost. (`Arc<RwLock<>>` rather than the slice-plan-
    /// suggested `Rc<RefCell<>>` so `Value: Send + Sync` — the
    /// par-block branch evaluator uses `thread::scope` and shares
    /// captured Values across worker threads.) See Phase-5 § Slice
    /// borrow-tracking parity § sub-item 3 "Aliased interpreter
    /// representation".
    Array(Arc<RwLock<Vec<Value>>>),
    /// `Vector[T, N]` — the portable-SIMD lane vector (design.md § Portable
    /// SIMD). Plain `Vec<Value>` of exactly `N` numeric lanes with **value
    /// (Copy) semantics** — distinct from `Value::Array`'s shared
    /// `Arc<RwLock<...>>` reference semantics. Element-wise arithmetic
    /// produces a fresh `Vector`; lane read `v[i]` returns a lane by value.
    /// The interpreter validates Vector *semantics*; codegen validates its
    /// `<N x T>` memory representation (design.md "Interpreter parity scope").
    /// Phase-7 line 289 slice 1b.
    Vector(Vec<Value>),
    /// `Slice[T]` / `mut Slice[T]` runtime value — a window into shared
    /// storage. Created at `.as_slice()` / `.as_slice_mut()` /
    /// range-indexing / call-arg coercion sites; cloned by sharing the
    /// `Arc<RwLock<...>>` storage. Index reads / writes go through the
    /// same `try_write_or_panic` helper as direct array writes, so the
    /// runtime guard fires on aliased writes the borrow checker would
    /// otherwise reject.
    Slice {
        storage: Arc<RwLock<Vec<Value>>>,
        start: usize,
        len: usize,
        mutable: bool,
    },
    /// `Tensor[T, Shape]` — N-D dense container (phase-11 numerical
    /// stdlib, interpreter MVP). `dims` is the runtime dim list (rank =
    /// dims.len()); `data` is C-order (row-major) element storage in the
    /// same universal `Arc<RwLock<...>>` shared-cell shape as
    /// `Value::Array` — par-block branch evaluators share captured
    /// Values across real OS threads, so interior mutability must be
    /// Arc-shareable (see the Array doc comment above).
    Tensor {
        dims: Arc<Vec<i64>>,
        data: Arc<RwLock<Vec<Value>>>,
    },
    /// `Column[T]` — nullable 1-D column (phase-11 data-science stdlib,
    /// Arrow commitment; interpreter MVP). `data` holds one `Value` per
    /// slot in append order; `valid` is the parallel validity bitmap
    /// (one `bool` per slot — `false` = SQL null). The two Vecs are kept
    /// the same length (the Arrow invariant): `push_null` appends a
    /// `Value::Unit` placeholder to `data` (never observed — `is_null` /
    /// indexing gate on `valid`). Both ride the same universal
    /// `Arc<RwLock<...>>` shared-cell shape as `Value::Array` / `Tensor`
    /// so par-block capture stays sound. The codegen slice will lower
    /// this to the real Arrow `{ data, null_bitmap, len, capacity }`
    /// buffer layout (design.md § Memory Layout Commitments); the
    /// interpreter only needs the logical semantics.
    Column {
        data: Arc<RwLock<Vec<Value>>>,
        valid: Arc<RwLock<Vec<bool>>>,
    },
    /// `DataFrame` — schema-bearing table of named columns (phase-11
    /// data-science stdlib, Arrow commitment; interpreter MVP). An
    /// insertion-ordered list of `(name, Value::Column)` pairs — the
    /// order IS the Arrow schema order, and a linear scan resolves a
    /// name lookup at MVP scale. Each entry's `Value` is a
    /// `Value::Column` whose `Arc<RwLock<...>>` cells the frame shares
    /// (so `column(name)` hands back a view, par-block capture stays
    /// sound, and the frame is a thin shared owner). Every column is
    /// kept the same length (the row count / `height`) — the Arrow
    /// equal-length invariant, enforced at `insert`. The codegen slice
    /// will lower this to the real Arrow schema + a uniform `AnyColumn`
    /// store; the interpreter only needs the logical semantics.
    DataFrame {
        columns: Arc<RwLock<Vec<(String, Value)>>>,
    },
    /// `LazyFrame` — a deferred query plan over a DataFrame (phase-11
    /// `LazyDataFrame` Option A, slice 1). `source` is a live VIEW of the
    /// source frame's column list (the same `Arc` the eager frame holds);
    /// `ops` is the recorded logical plan in application order. Builder
    /// methods (`select` / `limit`) clone the op list and push one step —
    /// cheap at MVP scale, and a linear single-source pipeline is exactly
    /// slice-1's plan shape (`join` turns it into a tree in a later
    /// slice). Nothing executes until `collect()`; `explain()` renders
    /// the plan. See `runtime/stdlib/dataframe.kara § LazyFrame`.
    LazyFrame {
        source: Arc<RwLock<Vec<(String, Value)>>>,
        ops: Arc<Vec<LazyOp>>,
    },
    /// A lazy expression handle (`LazyExpr` surface value) — an immutable
    /// shared expression tree; builder methods wrap it in new nodes.
    LazyExpr(Arc<LazyExprIR>),
    /// The `group_by(keys)` → `agg(aggs)` intermediate (slice 4):
    /// the plan so far plus the pending grouping keys.
    LazyGroupBy {
        source: Arc<RwLock<Vec<(String, Value)>>>,
        ops: Arc<Vec<LazyOp>>,
        keys: Vec<Arc<LazyExprIR>>,
    },
    Map(Vec<(Value, Value)>),
    Struct {
        name: String,
        fields: HashMap<String, Value>,
    },
    /// A `shared struct` allocation — RC-backed, multi-holder, with
    /// per-field interior mutability for `mut` fields per design.md
    /// § Part 5: Shared Types. Aliasing a binding clones the `Arc`
    /// (refcount bump); mutations through any holder are visible to
    /// all holders. Immutable fields are stored once at construction;
    /// `mut` fields each carry their own borrow flag (RwLock here as
    /// a semantic stand-in — the codegen lowers to a 1-byte flag per
    /// design.md § cost notes).
    SharedStruct(Arc<SharedStructInner>),
    EnumVariant {
        enum_name: String,
        variant: String,
        data: EnumData,
    },
    Function {
        name: String,
        param_patterns: Vec<Pattern>,
        /// Default value expressions, aligned with `param_patterns`.
        /// `None` means the parameter has no default; `Some(expr)` is
        /// evaluated at call time when the caller omits the argument.
        param_defaults: Vec<Option<crate::ast::Expr>>,
        body: Block,
        /// Captured environment for closures
        closure_env: Option<HashMap<String, Value>>,
    },
    /// F32 total-order wrapper: NaN sorts last, implements Eq/Ord/Hash
    TotalFloat32(f32),
    /// F64 total-order wrapper: NaN sorts last, implements Eq/Ord/Hash
    TotalFloat64(f64),
    /// Atomic[T] runtime value. `Arc<Mutex<...>>` (not `Box`) so a par
    /// struct's `Atomic` field is genuinely *shared* across `par {}`
    /// branches — `eval_par_block` clones each branch's env values, and an
    /// `Arc` clone shares the same cell, matching codegen's reference
    /// semantics. The `Mutex` makes each `fetch_*` / `swap` / `compare_exchange`
    /// a real read-modify-write under lock, so concurrent branches don't race
    /// on a non-atomic cell (the prior `Box<Value>` raced: torn reads
    /// surfaced as `method '…' not found on type 'unknown'` panics and lost
    /// updates). An owned, un-aliased `Atomic` is never observed through two
    /// live handles single-threaded, so share-on-clone is unobservable
    /// outside the par case it fixes. Same rationale applies to `Mutex`.
    Atomic(Arc<Mutex<Value>>),
    /// Mutex[T] runtime value. `Arc<Mutex<...>>` (not `Box`) for the same
    /// reason as `Atomic` above: a par struct's `Mutex` field is genuinely
    /// shared across `par {}` branches (which run on real OS threads), and a
    /// `lock` block holds the *real* lock for the duration of its body, so
    /// concurrent branches serialise instead of racing on a single-threaded
    /// cell (the prior `Box<Value>` raced — a par-struct `Mutex` counter
    /// produced empty output / lost updates under `karac run`). A `lock` block
    /// binds the inner value as a mutable alias and writes it back into the
    /// guarded cell on exit. Re-locking the *same* mutex inside its own block
    /// deadlocks, matching codegen's real spinlock (std `Mutex` is not
    /// re-entrant). See [`eval_expr`]'s `ExprKind::Lock` arm.
    Mutex(Arc<Mutex<Value>>),
    /// `TaskGroup` scope-local fan-out container (design.md § Structured
    /// Concurrency / TaskGroup). The tree-walk interpreter runs each
    /// spawned child **eagerly and synchronously** at its `.spawn(closure)`
    /// call site (see `eval_taskgroup_spawn`), because the dynamic
    /// spawn/join shape has no lexical scope the interpreter could hang a
    /// `std::thread::scope` off of the way `par {}` does. So the group
    /// carries no live task state — it is a marker the method-dispatch path
    /// recognises to route `.spawn` / `.cancel`, and one that scope-exit
    /// drop treats as a no-op (every child has already run to completion).
    /// Codegen lowers the genuinely-parallel version against
    /// `karac_runtime_taskgroup_*`; the eager model produces identical
    /// output for the order-independent fan-out/join programs the
    /// typechecker's `ScopeLocal` rules permit, keeping `karac run` and
    /// `karac build` in agreement (B-2026-06-30-8).
    TaskGroup,
    /// `TaskHandle[T]` join handle returned by `spawn(closure)` /
    /// `tg.spawn(closure)`. In the interpreter's eager model the child has
    /// already run by the time the handle exists, so the handle simply
    /// carries the computed result value; `.join()` returns it. The
    /// `ScopeLocal` marker (typechecker-enforced) keeps the handle from
    /// escaping its spawning scope, so an owned boxed result needs no
    /// cross-thread sharing.
    TaskHandle(Box<Value>),
    /// SortedSet[T: Ord] — B-tree–backed ordered set keyed by OrdValue.
    /// BTreeMap provides O(log n) insert/remove/contains with iteration in
    /// ascending key order. The () value makes it a set (not a map).
    SortedSet(BTreeMap<OrdValue, ()>),
    /// SortedMap[K: Ord, V] — B-tree–backed ordered map (B3). The key→value
    /// sibling of `SortedSet`: keys are `OrdValue` (sorted via `value_compare`)
    /// and each maps to an arbitrary `Value`. Iteration / `keys` / `values` /
    /// `entries` yield in ascending key order, and the ordered queries
    /// (`min` / `max` / `range` / `floor` / `ceiling`) ride the B-tree cursor.
    SortedMap(BTreeMap<OrdValue, Value>),
    /// Set[T: Hash + Eq] — hash set backed by a Vec for interpreter simplicity.
    /// O(n) lookup is fine for testing; the typechecker enforces Hash + Eq.
    Set(Vec<Value>),
    /// Iterator value produced by `.iter()` / `.into_iter()` on a
    /// collection or by adaptor calls. `source` produces raw items
    /// (eager snapshot, chained sequence, or zipped pair); `steps` is
    /// the lazy adaptor chain applied per `next()` pull. The
    /// `IteratorSource` and `IteratorStep` enums grow as adaptors land.
    /// Tracked in `wip-list2.md` § Iterator trait — full adaptor surface.
    Iterator {
        source: IteratorSource,
        steps: Vec<IteratorStep>,
    },
    /// Sender[T] end of a Channel[T]. Wraps a shared queue so that cloning a
    /// Sender creates an additional producer that shares the same buffer.
    Sender(Arc<Mutex<VecDeque<Value>>>),
    /// Receiver[T] end of a Channel[T]. `recv()` blocks until an item is
    /// available; `try_recv()` returns immediately as `Option[T]`. In the
    /// single-threaded tree-walk interpreter the test pattern is always
    /// send-before-recv, so the queue already has items when recv fires.
    Receiver(Arc<Mutex<VecDeque<Value>>>),
    /// File handle wrapping a live OS file descriptor. The `Arc<Mutex<...>>`
    /// layout keeps `Value` clone-friendly without requiring `Clone` on
    /// `std::fs::File` (which is intentionally non-Clone — cloning a file
    /// handle is a `dup(2)` syscall, not a free op). Drop on the last
    /// Arc closes the underlying fd via `std::fs::File`'s own Drop impl.
    /// Constructed via `File.open` / `File.create` / `File.append`;
    /// methods `.read` / `.write` / `.flush` thread through the mutex.
    File(Arc<Mutex<std::fs::File>>),
    /// `BufReader[R]` buffered reader wrapping a `File`. Holds an owned
    /// `std::io::BufReader<std::fs::File>` (constructed over a `dup(2)`
    /// clone of the wrapped file's fd, so the BufReader owns its reader
    /// while the original `File` value stays usable). The `Arc<Mutex<…>>`
    /// keeps `Value` clone-friendly without requiring `Clone` on the
    /// inner reader; Drop on the last Arc closes the cloned fd. Phase 8
    /// `BufReader[R]` slice. Constructed via `BufReader.new` /
    /// `BufReader.with_capacity`; methods `read_line` / `read_to_string`
    /// / `read` thread through the mutex.
    BufReader(Arc<Mutex<std::io::BufReader<std::fs::File>>>),
    /// `LinesIter` — the line iterator returned by `BufReader.lines()`.
    /// Shares the wrapped reader's `Arc<Mutex<std::io::BufReader<…>>>` with
    /// the originating `BufReader` (Rust's `lines()` consumes the reader;
    /// the interpreter Arc-shares it instead, so draining the iterator
    /// advances — and leaves at EOF — the shared BufReader). The for-loop
    /// drains it one line at a time, yielding `Result[String, IoError]` per
    /// line. Phase 8 `BufReader[R]` `lines()` slice.
    LinesIter(Arc<Mutex<std::io::BufReader<std::fs::File>>>),
    /// `StdinLines` — the lazy line iterator returned by `Stdin.lines()`
    /// (phase-8 `Stdin.lines()` slice). Carries no reader handle: stdin is
    /// ambient (`std::io::stdin()`), so — unlike `LinesIter`, which Arc-shares a
    /// File-backed `BufReader` — this is a stateless marker. The for-loop drains
    /// it by reading `std::io::stdin().read_line` until EOF, yielding
    /// `Result[String, IoError]` per line (same Item shape as `LinesIter`).
    StdinLines,
    /// `BufWriter[W]` buffered writer wrapping a `File` — the Write-side
    /// peer of `BufReader`. Holds an owned
    /// `std::io::BufWriter<std::fs::File>` (constructed over a `dup(2)`
    /// clone of the wrapped file's fd, so the BufWriter owns its writer
    /// while the original `File` value stays usable). The `Arc<Mutex<…>>`
    /// keeps `Value` clone-friendly without requiring `Clone` on the inner
    /// writer; Drop on the last Arc runs `std::io::BufWriter`'s own Drop,
    /// flushing any buffered bytes through the cloned fd before it closes.
    /// Phase 8 `BufWriter[W]` slice. Constructed via `BufWriter.new` /
    /// `BufWriter.with_capacity`; methods `write` / `flush` thread through
    /// the mutex.
    BufWriter(Arc<Mutex<std::io::BufWriter<std::fs::File>>>),
    /// Aliasing slot used to back a `mut ref |...|` closure capture.
    /// Lives only inside an `Env` scope or a closure's captured-env map;
    /// never reaches user expressions because every path that reads a
    /// value goes through `Env::get`, which auto-derefs. Writes via
    /// `Env::set` propagate through the cell so mutations made inside one
    /// closure invocation are visible to the outer binding and to
    /// subsequent invocations. `Arc<Mutex<...>>` rather than
    /// `Rc<RefCell<...>>` so `par {}` can clone branch envs across thread
    /// boundaries (single-threaded mutation in practice — `par` branches
    /// run in independent envs).
    SharedCell(Arc<Mutex<Value>>),
    /// `Entry[K, V]` view returned by `Map.entry(k)` for in-place insert-or-
    /// modify. Spec at design.md § Entry[K, V].
    ///
    /// `map_var` names the original Map binding so `or_insert`,
    /// `or_insert_with`, and `and_modify` can write the mutation back via
    /// `env.set` — the interpreter's idiomatic mut-ref-self path. `None`
    /// when the entry was produced from a non-identifier receiver (rare;
    /// the chain still evaluates but mutations are dropped).
    ///
    /// `slot_idx` is the index of the `(key, value)` pair in the map's Vec
    /// when `Some` (Occupied); `None` means Vacant. The interpreter never
    /// hands a stale slot_idx to chain consumers — each method that mutates
    /// the map (or_insert / or_insert_with) refreshes the index before
    /// returning a fresh `Entry`.
    Entry {
        map_var: Option<String>,
        key: Box<Value>,
        slot_idx: Option<usize>,
    },
    /// A live `mut ref V` into a `Map` value slot, returned by
    /// `Entry.or_insert` / `or_insert_with`. Unlike `Entry` (a transient
    /// cursor), this is a genuine place-reference: `or_insert` guarantees
    /// the slot exists, then hands back this ref so write-through mutations
    /// reach the map. `Env::get` resolves it to the live slot value
    /// (auto-deref) and `Env::set` writes through to the slot — the same
    /// choke-point treatment as [`Value::SharedCell`], so `*r += 1`,
    /// `r += 1`, `*r = v`, and `.push(x)` (Arc-shared element storage) all
    /// land in the map. `map_var` names the Map binding; `key` selects the
    /// slot. Map *slots* never hold a `MapSlotRef` (it only ever lives in a
    /// local binding or as a chain-temporary), so map reads stay pristine.
    MapSlotRef {
        map_var: String,
        key: Box<Value>,
    },
    /// A `mut ref T` into a `Vec`/`Array` element, produced by
    /// `for x in xs.iter_mut()` (B-2026-07-14-10). Holds the element storage's
    /// shared handle directly (the same `Arc<RwLock<Vec<Value>>>` the `Array`
    /// binding shares), so `*x` reads the live element and `*x = v` / `*x += 1`
    /// write through to it — `Env::get`/`Env::set` treat it as an auto-deref /
    /// write-through slot exactly like `MapSlotRef`. `index` selects the
    /// element. Never stored inside a collection; it only ever lives in the
    /// per-iteration loop binding.
    VecSlotRef {
        storage: Arc<RwLock<Vec<Value>>>,
        index: usize,
    },
}

/// One mutable field on a `shared struct` instance. The spec
/// (design.md § Part 5: Shared Types) requires per-field borrow
/// tracking: reads are shared (multiple simultaneous readers OK),
/// writes are exclusive — if any other borrow (read or write) is
/// active when a write begins, the runtime panics. Tracking is
/// per field so mutating `node.left` does not conflict with reading
/// `node.right`. `RwLock::try_read` / `try_write` mirror these
/// semantics directly. Codegen lowers this to a 1-byte borrow flag
/// per the cost notes; the interpreter uses `RwLock<Value>` as a
/// semantic stand-in.
#[derive(Debug)]
pub struct FieldCell {
    pub value: RwLock<Value>,
}

impl FieldCell {
    pub fn new(v: Value) -> Self {
        FieldCell {
            value: RwLock::new(v),
        }
    }
}

/// Allocation backing a `shared struct` instance. Multiple holders
/// (each a `Value::SharedStruct(Arc::clone(...))`) share one inner;
/// mutation through any holder is visible to all. Aliasing is by
/// `Arc` clone — `let b = a` bumps the refcount, no deep copy.
///
/// Weak fields (declared `weak T` or `mut weak T`) live in dedicated
/// `weak_*_fields` maps backed by `std::sync::Weak<SharedStructInner>`
/// per design.md § Shared Types — Weak references. They never surface
/// to user code as a "raw weak" — field reads auto-upgrade and yield
/// `Option[T]`; writes accept a strong reference and downgrade.
#[derive(Debug)]
pub struct SharedStructInner {
    pub name: String,
    /// Fields without `mut` — fixed at construction, never replaced.
    pub immutable_fields: HashMap<String, Value>,
    /// Fields declared `mut` — each carries its own borrow flag.
    pub mut_fields: HashMap<String, FieldCell>,
    /// Fields declared `weak T` (no `mut`) — set at construction,
    /// not reassignable. `std::sync::Weak` mirrors the spec's storage
    /// model: assignment downgrades a strong reference; reads upgrade
    /// to `Option[T]`. Empty in v1 codegen — interpreter only.
    pub weak_immutable_fields: HashMap<String, std::sync::Weak<SharedStructInner>>,
    /// Fields declared `mut weak T` — set at construction or later
    /// via field assignment. The `RwLock` only guards the `Weak`
    /// handle itself (assignment vs concurrent read of the slot);
    /// upgrade to `Arc` is atomic via `Weak::upgrade`.
    pub weak_mut_fields: HashMap<String, RwLock<std::sync::Weak<SharedStructInner>>>,
}

/// Newtype wrapping [`Value`] that implements [`Ord`] via [`value_compare`]
/// so `Value` elements can key a `BTreeMap` without `Value` itself needing
/// to implement `Ord` globally (NaN semantics on floats make global Ord
/// unsound). Used exclusively by `Value::SortedSet`.
#[derive(Debug, Clone)]
pub struct OrdValue(pub Value);

impl PartialEq for OrdValue {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl Eq for OrdValue {}
impl PartialOrd for OrdValue {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for OrdValue {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        value_compare(&self.0, &other.0)
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Char(a), Value::Char(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Unit, Value::Unit) => true,
            (Value::Tuple(a), Value::Tuple(b)) => a == b,
            (Value::Array(a), Value::Array(b)) => {
                Arc::ptr_eq(a, b) || *a.read().unwrap() == *b.read().unwrap()
            }
            (
                Value::Slice {
                    storage: sa,
                    start: ssa,
                    len: la,
                    ..
                },
                Value::Slice {
                    storage: sb,
                    start: ssb,
                    len: lb,
                    ..
                },
            ) => {
                if la != lb {
                    return false;
                }
                let va = sa.read().unwrap();
                let vb = sb.read().unwrap();
                va[*ssa..*ssa + *la] == vb[*ssb..*ssb + *lb]
            }
            (
                Value::EnumVariant {
                    enum_name: a1,
                    variant: a2,
                    data: a3,
                },
                Value::EnumVariant {
                    enum_name: b1,
                    variant: b2,
                    data: b3,
                },
            ) => a1 == b1 && a2 == b2 && a3 == b3,
            (
                Value::Struct {
                    name: a1,
                    fields: a2,
                },
                Value::Struct {
                    name: b1,
                    fields: b2,
                },
            ) => a1 == b1 && a2 == b2,
            // `shared struct` equality is structural per design.md
            // § Equality Semantics — the `Eq` impl is dispatched
            // regardless of representation. `Arc::ptr_eq` is the
            // fast path for identical allocations (always equal).
            (Value::SharedStruct(a), Value::SharedStruct(b)) => {
                if Arc::ptr_eq(a, b) {
                    return true;
                }
                if a.name != b.name {
                    return false;
                }
                if a.immutable_fields != b.immutable_fields {
                    return false;
                }
                if a.mut_fields.len() != b.mut_fields.len() {
                    return false;
                }
                let mut_eq = a.mut_fields.iter().all(|(k, fa)| {
                    b.mut_fields
                        .get(k)
                        .map(|fb| {
                            let va = fa.value.try_read().ok();
                            let vb = fb.value.try_read().ok();
                            match (va, vb) {
                                (Some(x), Some(y)) => *x == *y,
                                _ => false,
                            }
                        })
                        .unwrap_or(false)
                });
                if !mut_eq {
                    return false;
                }
                // Weak fields: compare by referent identity (Arc::ptr_eq
                // on upgraded handles). Two dangling weaks are equal;
                // a dangling weak is not equal to a live weak.
                if a.weak_immutable_fields.len() != b.weak_immutable_fields.len()
                    || a.weak_mut_fields.len() != b.weak_mut_fields.len()
                {
                    return false;
                }
                let weak_imm_eq = a.weak_immutable_fields.iter().all(|(k, wa)| {
                    b.weak_immutable_fields
                        .get(k)
                        .map(|wb| weak_referent_eq(wa, wb))
                        .unwrap_or(false)
                });
                if !weak_imm_eq {
                    return false;
                }
                a.weak_mut_fields.iter().all(|(k, sa)| {
                    b.weak_mut_fields
                        .get(k)
                        .map(|sb| {
                            let wa = sa.try_read().ok();
                            let wb = sb.try_read().ok();
                            match (wa, wb) {
                                (Some(x), Some(y)) => weak_referent_eq(&x, &y),
                                _ => false,
                            }
                        })
                        .unwrap_or(false)
                })
            }
            // TotalFloat uses total ordering: NaN == NaN, -0.0 < +0.0
            (Value::TotalFloat32(a), Value::TotalFloat32(b)) => a.total_cmp(b).is_eq(),
            (Value::TotalFloat64(a), Value::TotalFloat64(b)) => a.total_cmp(b).is_eq(),
            (Value::Atomic(a), Value::Atomic(b)) => {
                // Snapshot each under its own lock (released before the next)
                // so comparing an atomic to itself can't self-deadlock.
                let av = a.lock().unwrap().clone();
                let bv = b.lock().unwrap().clone();
                av == bv
            }
            (Value::Mutex(a), Value::Mutex(b)) => {
                // Snapshot each under its own lock (released before the next)
                // so comparing a mutex to itself can't self-deadlock.
                let av = a.lock().unwrap().clone();
                let bv = b.lock().unwrap().clone();
                av == bv
            }
            (Value::Map(a), Value::Map(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .all(|(k, v)| b.iter().any(|(bk, bv)| bk == k && bv == v))
            }
            (Value::SortedSet(a), Value::SortedSet(b)) => {
                a.len() == b.len() && a.keys().zip(b.keys()).all(|(x, y)| x == y)
            }
            (Value::SortedMap(a), Value::SortedMap(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .zip(b.iter())
                        .all(|((ak, av), (bk, bv))| ak == bk && av == bv)
            }
            (Value::Set(a), Value::Set(b)) => a.len() == b.len() && a.iter().all(|x| b.contains(x)),
            // Channel ends compare by pointer identity — two Senders are equal
            // only when they wrap the exact same Arc allocation.
            (Value::Sender(a), Value::Sender(b)) => Arc::ptr_eq(a, b),
            (Value::Receiver(a), Value::Receiver(b)) => Arc::ptr_eq(a, b),
            (Value::Function { .. }, Value::Function { .. }) => false,
            // Iterators have no meaningful equality — like closures, two
            // iterator values aren't compared structurally.
            (Value::Iterator { .. }, Value::Iterator { .. }) => false,
            // Entry values aren't compared structurally either — they're
            // chain-locals returned only from Map.entry(k).
            (Value::Entry { .. }, Value::Entry { .. }) => false,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum EnumData {
    Unit,
    Tuple(Vec<Value>),
    Struct(HashMap<String, Value>),
}

/// The raw-item supplier behind a `Value::Iterator`. Eager handles the
/// usual `coll.iter()` snapshot path; Chain and Zip support the
/// multi-source combinators landed in `wip-list2.md` subtask 7. Pulling
/// from an iterator goes: `pull_source` (this enum) → apply each
/// `IteratorStep` in `steps` → yield (or reject and retry).
#[derive(Debug, Clone, PartialEq)]
pub enum IteratorSource {
    /// Pre-extracted items walked by cursor — Vec/Set/SortedSet/Map/
    /// Array.iter() use this. Map yields `(K, V)` tuples, SortedSet
    /// flattens to ascending order.
    Eager { items: Vec<Value>, cursor: usize },
    /// Sequential concatenation — drive each part fully (through its
    /// own step chain) before moving to the next. Each part is itself
    /// a `Value::Iterator`; `current` is the part being drained.
    Chain { parts: Vec<Value>, current: usize },
    /// Synchronous pair — pull from `left` and `right` in lockstep,
    /// yield `(a, b)` tuples until either side ends. Each side is a
    /// `Value::Iterator`.
    Zip { left: Box<Value>, right: Box<Value> },
    /// `.flat_map(f)` — pull an outer item, apply `f` to get an inner
    /// iterator, drain the inner before pulling the next outer. The
    /// closure is `Fn(T) -> Iterator[U]`. `current_inner` holds the
    /// in-flight inner iterator across multiple `next()` pulls; `None`
    /// means we need to advance the outer on the next pull. `f` is
    /// boxed because `Value::Iterator` embeds this enum inline; the
    /// closure (`Value::Function`) lives in `f`, so without indirection
    /// `Value`'s size would recurse through itself.
    FlatMap {
        outer: Box<Value>,
        f: Box<Value>,
        current_inner: Option<Box<Value>>,
    },
    /// `.cycle()` — restart on exhaustion. `template` is the snapshot
    /// taken at construction (cloned again on each restart);
    /// `current` is the in-flight clone being drained. `exhausted`
    /// flips to true when the template itself is empty (so we don't
    /// loop forever resetting an empty source). Each cycle through
    /// the template re-runs adaptor closures held in template's own
    /// `steps`, with their stateful counters reset to construction
    /// state.
    Cycle {
        template: Box<Value>,
        current: Box<Value>,
        exhausted: bool,
    },
    /// `.chunks(n)` — non-overlapping groups of up to `n` consecutive
    /// items. Each pull collects the next `n` items into a fresh
    /// `Vec[T]` (`allocates(Heap)`); the trailing group may be
    /// shorter than `n` if the source length isn't a multiple. `n`
    /// is clamped to `n.max(1)` at the dispatch site. `exhausted`
    /// flips sticky-true once the inner exhausts AND the trailing
    /// group has been emitted.
    Chunks {
        inner: Box<Value>,
        n: usize,
        exhausted: bool,
    },
    /// `.windows(n)` — sliding view of size `n` over the source,
    /// advancing one item per pull. Each pull yields a fresh
    /// `Vec[T]` clone of the buffer (`allocates(Heap)`). The first
    /// pull primes the buffer by collecting `n` items; subsequent
    /// pulls drop the front and push one new item. If the source
    /// has fewer than `n` items, the iterator yields nothing
    /// (matches Rust's `[T].windows(n)` semantics). `primed` is
    /// false on the first pull.
    Windows {
        inner: Box<Value>,
        n: usize,
        buffer: Vec<Value>,
        primed: bool,
        exhausted: bool,
    },
    /// `.chunk_by(key_fn)` — buffering adaptor that groups consecutive
    /// elements where `key_fn(item)` produces equal keys. Each pull
    /// yields one `Vec[T]` group; allocates a fresh Vec per group
    /// (effect-checker carries `allocates(Heap)` for
    /// `Iterator.chunk_by`). Modeled as a Source rather than a Step
    /// because one outer pull can consume many inner items, and the
    /// boundary between groups requires a one-item lookahead — when
    /// the key changes, the trailing item that triggered the change
    /// becomes the seed of the NEXT group, so we stash it in
    /// `pending_item` (with its already-computed `pending_key` so we
    /// don't re-fire the closure) until the following pull.
    /// `exhausted` flips after the inner returns None and the final
    /// in-flight group has been drained. `key_fn` is boxed for the
    /// same reason FlatMap's `f` is — without indirection
    /// `Value::Iterator → IteratorSource::ChunkBy → Value::Function`
    /// would make `Value`'s size cycle through itself.
    ChunkBy {
        inner: Box<Value>,
        key_fn: Box<Value>,
        pending_item: Option<Box<Value>>,
        pending_key: Option<Box<Value>>,
        exhausted: bool,
    },
    /// `.peekable()` — single-element lookahead buffer. `inner` is the
    /// underlying iterator (with all its own steps); `buffered` holds
    /// the next element if `peek()` has been called and not yet
    /// consumed by `next()`. Pulls drain from the buffer first; when
    /// empty, fall through to `iterator_step(inner)`. The wrapping
    /// `Value::Iterator`'s `steps` is always empty in well-typed
    /// programs because adaptors after `.peekable()` return
    /// `Iterator[U]` (not `Peekable[U]`), so `peek()` becomes
    /// type-unavailable downstream — meaning peek and next agree on
    /// the item type without needing to walk steps.
    Peekable {
        inner: Box<Value>,
        buffered: Option<Box<Value>>,
    },
}

/// One step in a `Value::Iterator`'s lazy adaptor chain. Each step is a
/// transform applied per `next()` pull. Some steps carry mutable state
/// (positional counters for `enumerate` / `take` / `skip`); the per-call
/// state is mutated on the cloned chain inside `iterator_step` and the
/// updated chain is written back to the iterator value before return.
#[derive(Debug, Clone, PartialEq)]
pub enum IteratorStep {
    /// `.map(f)` — apply `f` to each item before yielding.
    /// The Value is a `Value::Function` (closure).
    Map(Value),
    /// `.filter(pred)` — yield only items where `pred(item)` is `true`.
    /// The Value is a `Value::Function` (closure returning `bool`).
    Filter(Value),
    /// `.filter_map(f)` — apply `f: Fn(T) -> Option[U]` to each item;
    /// yield the payload of each `Some`, drop each `None` (map+filter
    /// fusion). The Value is a `Value::Function` (closure returning
    /// `Option[U]`).
    FilterMap(Value),
    /// `.enumerate()` — wrap each item into `(idx, item)`. The `usize`
    /// is the index of the *next* yielded item (incremented after wrap).
    Enumerate(usize),
    /// `.take(n)` — yield at most `n` items. The `usize` is the number
    /// of items remaining to yield; once it hits 0, the step signals
    /// "stop" and the iterator's cursor is advanced past end.
    Take(usize),
    /// `.skip(n)` — drop the first `n` items the step sees. The `usize`
    /// is the number of items still to skip; while > 0, the step
    /// rejects the item and decrements.
    Skip(usize),
    /// `.take_while(pred)` — yield items while `pred(item)` returns
    /// true; on the first false, signal stop (drain the source) and
    /// remain stopped on every subsequent pull. The `bool` flag tracks
    /// whether we've already seen the trip element so future pulls go
    /// straight to "stop" without re-firing the predicate.
    TakeWhile { pred: Value, done: bool },
    /// `.skip_while(pred)` — drop items while `pred(item)` returns
    /// true; on the first false, yield that element AND every
    /// subsequent element unconditionally. The `bool` flag flips once
    /// the predicate fails so future pulls bypass it entirely.
    SkipWhile { pred: Value, done: bool },
    /// `.step_by(n)` — yield every n-th item (n ≥ 1). The first item
    /// is always yielded; `remaining_skip` tracks how many items to
    /// reject before the next yield. Construction guarantees n ≥ 1
    /// (clamped at the dispatch site); n = 0 would underflow on the
    /// post-yield reset.
    StepBy { n: usize, remaining_skip: usize },
    /// `.inspect(f)` — invoke `f` on each item for its side effects,
    /// then pass the item through unchanged. The closure's return
    /// value is discarded.
    Inspect(Value),
    /// `.scan(init, f)` — thread mutable state through the iterator.
    /// `f` has signature `Fn(A, T) -> Option<(A, U)>`: returns
    /// `Some((new_state, yielded))` to advance and yield, or `None`
    /// to stop. The `done` flag flips sticky-true after the first
    /// `None` so subsequent pulls short-circuit without re-firing
    /// the closure. Note: this departs from Rust's
    /// `Fn(&mut St, T) -> Option<B>` because tree-walk closures
    /// snapshot captures and there's no `mut ref` parameter mode at
    /// the value layer; threading state via the return tuple is
    /// the simplest fix and matches the existing fold pattern
    /// (closure returns the new accumulator).
    Scan { f: Value, state: Value, done: bool },
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Int(v) => write!(f, "{}", v),
            Value::Float(v) => write!(f, "{}", v),
            Value::Bool(v) => write!(f, "{}", v),
            Value::Char(v) => write!(f, "{}", v),
            Value::String(v) => write!(f, "{}", v),
            // Lossy UTF-8 render — `CStr` carries raw bytes, and Display
            // here is a debug courtesy (the type doesn't coerce to String
            // at the language level; f-string interpolation rejects it at
            // typecheck via `type_supports_display`).
            Value::CStr(bytes) => write!(f, "{}", String::from_utf8_lossy(bytes)),
            // Same lossy-UTF-8 debug courtesy as `CStr`; `CString` likewise
            // does not coerce to String at the language level.
            Value::CString(bytes) => write!(f, "{}", String::from_utf8_lossy(bytes)),
            Value::Unit => write!(f, "()"),
            // A `Type` pseudovalue renders as its canonical name — a
            // debug courtesy; comptime code reads it via `.name()`.
            Value::TypeVal(name) => write!(f, "{}", name),
            // An `Expr` AST value — debug courtesy only; it is spliced as
            // code, not displayed.
            Value::AstExpr(_) => write!(f, "<ast expr>"),
            // An `Item` AST value — debug courtesy only; it is spliced as
            // code, not displayed.
            Value::AstItem(_) => write!(f, "<ast item>"),
            // Debug-courtesy render: shape only (element dumps for large
            // tensors would flood output; `t[i, j]` reads individual
            // elements).
            Value::Tensor { dims, .. } => {
                let rendered: Vec<String> = dims.iter().map(|d| d.to_string()).collect();
                write!(f, "Tensor[{}]", rendered.join(", "))
            }
            // Summary form (like Tensor) — element dump would flood output;
            // `c[i]` / `iter` read individual slots.
            Value::Column { valid, .. } => {
                write!(f, "Column[len={}]", valid.read().unwrap().len())
            }
            // Summary form — column names + shape; element dump would
            // flood output.
            Value::DataFrame { columns } => {
                let cols = columns.read().unwrap();
                let names: Vec<&str> = cols.iter().map(|(n, _)| n.as_str()).collect();
                let height = cols.first().map_or(0, |(_, c)| match c {
                    Value::Column { valid, .. } => valid.read().unwrap().len(),
                    _ => 0,
                });
                write!(
                    f,
                    "DataFrame[{} x {}: {}]",
                    cols.len(),
                    height,
                    names.join(", ")
                )
            }
            // Summary form — plan step count; `explain()` renders the plan.
            Value::LazyFrame { ops, .. } => {
                write!(f, "LazyFrame[{} step(s)]", ops.len())
            }
            // The expression tree itself — small by construction.
            Value::LazyExpr(ir) => write!(f, "LazyExpr[{ir}]"),
            // Pending grouping — key count only.
            Value::LazyGroupBy { keys, .. } => {
                write!(f, "LazyGroupBy[{} key(s)]", keys.len())
            }
            Value::Tuple(vals) => {
                write!(f, "(")?;
                for (i, v) in vals.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, ")")
            }
            Value::Array(rc) => {
                let vals = rc.read().unwrap();
                write!(f, "[")?;
                for (i, v) in vals.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, "]")
            }
            Value::Vector(lanes) => {
                write!(f, "Vector(")?;
                for (i, v) in lanes.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, ")")
            }
            Value::Slice {
                storage,
                start,
                len,
                ..
            } => {
                let vals = storage.read().unwrap();
                write!(f, "[")?;
                for (i, v) in vals[*start..*start + *len].iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, "]")
            }
            Value::Map(entries) => {
                write!(f, "{{")?;
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}: {}", k, v)?;
                }
                write!(f, "}}")
            }
            Value::Struct { name, fields } => {
                write!(f, "{} {{ ", name)?;
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}: {}", k, v)?;
                }
                write!(f, " }}")
            }
            Value::SharedStruct(inner) => {
                write!(f, "{} {{ ", inner.name)?;
                let mut first = true;
                for (k, v) in &inner.immutable_fields {
                    if !first {
                        write!(f, ", ")?;
                    }
                    first = false;
                    write!(f, "{}: {}", k, v)?;
                }
                for (k, cell) in &inner.mut_fields {
                    if !first {
                        write!(f, ", ")?;
                    }
                    first = false;
                    let v = cell.value.try_read().expect(
                        "shared struct field write-locked during Display — unreachable in single-task interpreter",
                    );
                    write!(f, "{}: {}", k, *v)?;
                }
                for (k, weak) in &inner.weak_immutable_fields {
                    if !first {
                        write!(f, ", ")?;
                    }
                    first = false;
                    write!(f, "{}: {}", k, upgrade_weak_to_option(weak))?;
                }
                for (k, slot) in &inner.weak_mut_fields {
                    if !first {
                        write!(f, ", ")?;
                    }
                    first = false;
                    let weak = slot.try_read().expect(
                        "shared struct weak field write-locked during Display — unreachable in single-task interpreter",
                    );
                    write!(f, "{}: {}", k, upgrade_weak_to_option(&weak))?;
                }
                write!(f, " }}")
            }
            Value::EnumVariant { variant, data, .. } => match data {
                EnumData::Unit => write!(f, "{}", variant),
                EnumData::Tuple(vals) => {
                    write!(f, "{}(", variant)?;
                    for (i, v) in vals.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", v)?;
                    }
                    write!(f, ")")
                }
                EnumData::Struct(fields) => {
                    write!(f, "{} {{ ", variant)?;
                    for (i, (k, v)) in fields.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}: {}", k, v)?;
                    }
                    write!(f, " }}")
                }
            },
            Value::Function { name, .. } => write!(f, "<fn {}>", name),
            Value::TotalFloat32(v) => write!(f, "F32({})", v),
            Value::TotalFloat64(v) => write!(f, "F64({})", v),
            Value::Atomic(v) => write!(f, "Atomic({})", v.lock().unwrap()),
            Value::Mutex(v) => write!(f, "Mutex({})", v.lock().unwrap()),
            Value::TaskGroup => write!(f, "TaskGroup"),
            Value::TaskHandle(v) => write!(f, "TaskHandle({})", v),
            Value::SortedSet(set) => {
                write!(f, "SortedSet{{")?;
                for (i, k) in set.keys().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", k.0)?;
                }
                write!(f, "}}")
            }
            Value::SortedMap(map) => {
                write!(f, "SortedMap{{")?;
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}: {}", k.0, v)?;
                }
                write!(f, "}}")
            }
            Value::Set(elems) => {
                write!(f, "Set{{")?;
                for (i, v) in elems.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, "}}")
            }
            Value::Sender(_) => write!(f, "<Sender>"),
            Value::Receiver(_) => write!(f, "<Receiver>"),
            Value::Iterator { source, .. } => match source {
                IteratorSource::Eager { items, cursor } => {
                    write!(f, "<iter {}/{}>", cursor, items.len())
                }
                IteratorSource::Chain { parts, current } => {
                    write!(f, "<iter chain {}/{}>", current, parts.len())
                }
                IteratorSource::Zip { .. } => write!(f, "<iter zip>"),
                IteratorSource::FlatMap { .. } => write!(f, "<iter flat_map>"),
                IteratorSource::Cycle { .. } => write!(f, "<iter cycle>"),
                IteratorSource::Peekable { .. } => write!(f, "<iter peekable>"),
                IteratorSource::Chunks { .. } => write!(f, "<iter chunks>"),
                IteratorSource::Windows { .. } => write!(f, "<iter windows>"),
                IteratorSource::ChunkBy { .. } => write!(f, "<iter chunk_by>"),
            },
            Value::SharedCell(cell) => write!(f, "{}", cell.lock().unwrap()),
            Value::Entry {
                map_var,
                key,
                slot_idx,
            } => {
                let occ = if slot_idx.is_some() {
                    "Occupied"
                } else {
                    "Vacant"
                };
                let mv = map_var.as_deref().unwrap_or("?");
                write!(f, "<{} entry for {} in {}>", occ, key, mv)
            }
            // A place-ref is auto-deref'd by `Env::get` before reaching any
            // value context, so this is defensive only.
            Value::MapSlotRef { map_var, key } => {
                write!(f, "<slot ref for {} in {}>", key, map_var)
            }
            Value::VecSlotRef { index, .. } => {
                // Display renders the referenced ELEMENT, not the ref wrapper —
                // an `iter_mut` binding printed bare (`println(x)`) should show
                // the element. Callers that print `*x` already auto-deref via
                // `Env::get`; this bare-ref path is a debug courtesy.
                write!(f, "<vec slot ref @{}>", index)
            }
            Value::File(_) => write!(f, "<File>"),
            Value::BufReader(_) => write!(f, "<BufReader>"),
            Value::BufWriter(_) => write!(f, "<BufWriter>"),
            Value::LinesIter(_) => write!(f, "<LinesIter>"),
            Value::StdinLines => write!(f, "<StdinLines>"),
        }
    }
}

/// Slice 3 runtime guard — write-lock the shared array storage,
/// panicking with an aliased-write message if another reader or writer
/// is currently holding it. Centralized at every mutating array / slice
/// site (push / pop / insert / remove / set_element / index-assignment)
/// so the `panic_on_aliased_write` rule has one structural enforcement
/// point. The `source_label` is best-effort context — derived from the
/// active expression's place-expression root when available, else
/// `"<value>"`.
pub(crate) fn try_write_or_panic<'a>(
    storage: &'a Arc<RwLock<Vec<Value>>>,
    source_label: &str,
) -> std::sync::RwLockWriteGuard<'a, Vec<Value>> {
    storage.try_write().unwrap_or_else(|_| {
        panic!(
            "aliased write detected: {} mutated while a borrow is live",
            source_label
        )
    })
}

/// Coerce a primitive-type associated constant to the type-erased
/// runtime value the interpreter uses. Signed and unsigned integer
/// constants share `Value::Int(i64)`; both float widths share
/// `Value::Float(f64)`. The codegen path uses the same `ConstValue`
/// table but emits the correct LLVM constant width per variant.
pub(crate) fn primitive_const_to_value(cv: &crate::prelude::ConstValue) -> Value {
    use crate::prelude::ConstValue::*;
    match cv {
        I8(v) => Value::Int(*v as i64),
        I16(v) => Value::Int(*v as i64),
        I32(v) => Value::Int(*v as i64),
        I64(v) => Value::Int(*v),
        // Const generics slice 2b: i128 / u128 coercion to Value::Int(i64)
        // is lossy — values that overflow i64 are silently truncated.
        // The slice 2 plan's hard-stop fallback acknowledged this:
        // i128 const-args evaluate cleanly at the typechecker (compile-
        // time fold) but the interpreter's runtime Value can't hold
        // 128-bit values. A future Value::Int128 widening replaces this
        // truncation; today the only path that reaches here is the
        // primitive-table coercion for `i128.MAX` / `i128.MIN` style
        // associated constants — none are defined in PRIMITIVE_CONSTS
        // for the 128-bit widths.
        I128(v) => Value::Int(*v as i64),
        U8(v) => Value::Int(*v as i64),
        U16(v) => Value::Int(*v as i64),
        U32(v) => Value::Int(*v as i64),
        U64(v) => Value::Int(*v as i64),
        U128(v) => Value::Int(*v as i64),
        Usize(v) => Value::Int(*v as i64),
        F32(v) => Value::Float(*v as f64),
        F64(v) => Value::Float(*v),
        Bool(b) => Value::Bool(*b),
        Char(c) => Value::Char(*c),
        // Fieldless-enum constants surface as a unit variant; the
        // interpreter's enum-variant representation carries the parent
        // enum + variant name as strings.
        EnumVariant {
            enum_name,
            variant_name,
            ..
        } => Value::EnumVariant {
            enum_name: enum_name.clone(),
            variant: variant_name.clone(),
            data: EnumData::Unit,
        },
    }
}

impl Value {
    /// Slice 3 helper — wrap a fresh `Vec<Value>` in the shared
    /// `Arc<RwLock<>>` storage used for `Value::Array`. Every Array
    /// allocation goes through this so the rep upgrade stays uniform.
    pub fn array_of(items: Vec<Value>) -> Value {
        Value::Array(Arc::new(RwLock::new(items)))
    }

    /// If this value is `Result::Err(e)`, return `e` (the single payload).
    /// Used by the `karac run` entry-point handler to implement design.md
    /// § Entry Point: a `main() -> Result[(), E]` that returns `Err(e)` prints
    /// `Error: {e}` to stderr and exits 1 — matching the AOT codegen
    /// adaptation (B-2026-06-12-9). `None` for `Ok`, any other variant, or a
    /// non-enum value (so a plain `fn main()` returning `Unit` is unaffected).
    pub fn as_result_err_payload(&self) -> Option<&Value> {
        match self {
            Value::EnumVariant {
                enum_name,
                variant,
                data: EnumData::Tuple(vs),
            } if enum_name == "Result" && variant == "Err" => vs.first(),
            _ => None,
        }
    }

    /// Slice 3 helper — borrow the inner `Vec<Value>` for read-only access.
    /// Returns `None` for non-array values so callers can fall through to
    /// other arms cleanly. The guard is held for the lifetime of the
    /// returned `RwLockReadGuard`, so callers should keep it scoped.
    pub fn as_array_borrow(&self) -> Option<RwLockReadGuard<'_, Vec<Value>>> {
        match self {
            Value::Array(rc) => Some(rc.read().unwrap()),
            _ => None,
        }
    }

    /// Static name of this Value's enum discriminant. Used by interpreter
    /// invariant-violation panics so the message names the actual variant
    /// received instead of a vague "type mismatch", letting a debugger
    /// start at the right layer — an interpreter codepath that produced
    /// the wrong variant (e.g. a `Cast` arm that no-ops) or, less often,
    /// a real typechecker miss.
    pub fn variant_name(&self) -> &'static str {
        match self {
            Value::Int(_) => "Int",
            Value::Float(_) => "Float",
            Value::Bool(_) => "Bool",
            Value::Char(_) => "Char",
            Value::String(_) => "String",
            Value::CStr(_) => "CStr",
            Value::CString(_) => "CString",
            Value::Unit => "Unit",
            Value::TypeVal(_) => "TypeVal",
            Value::AstExpr(_) => "AstExpr",
            Value::AstItem(_) => "AstItem",
            Value::Tensor { .. } => "Tensor",
            Value::Column { .. } => "Column",
            Value::DataFrame { .. } => "DataFrame",
            Value::LazyFrame { .. } => "LazyFrame",
            Value::LazyExpr(_) => "LazyExpr",
            Value::LazyGroupBy { .. } => "LazyGroupBy",
            Value::Tuple(_) => "Tuple",
            Value::Array(_) => "Array",
            Value::Vector(_) => "Vector",
            Value::Slice { .. } => "Slice",
            Value::Map(_) => "Map",
            Value::Struct { .. } => "Struct",
            Value::SharedStruct(_) => "SharedStruct",
            Value::EnumVariant { .. } => "EnumVariant",
            Value::Function { .. } => "Function",
            Value::TotalFloat32(_) => "TotalFloat32",
            Value::TotalFloat64(_) => "TotalFloat64",
            Value::Atomic(_) => "Atomic",
            Value::Mutex(_) => "Mutex",
            Value::TaskGroup => "TaskGroup",
            Value::TaskHandle(_) => "TaskHandle",
            Value::SortedSet(_) => "SortedSet",
            Value::SortedMap(_) => "SortedMap",
            Value::Set(_) => "Set",
            Value::Iterator { .. } => "Iterator",
            Value::Sender(_) => "Sender",
            Value::Receiver(_) => "Receiver",
            Value::SharedCell(_) => "SharedCell",
            Value::Entry { .. } => "Entry",
            Value::MapSlotRef { .. } => "MapSlotRef",
            Value::VecSlotRef { .. } => "VecSlotRef",
            Value::File(_) => "File",
            Value::BufReader(_) => "BufReader",
            Value::BufWriter(_) => "BufWriter",
            Value::LinesIter(_) => "LinesIter",
            Value::StdinLines => "StdinLines",
        }
    }

    /// Format for programmer-facing debug output.
    /// Strings are quoted, chars are single-quoted; compound values recurse.
    pub fn debug_fmt(&self) -> String {
        match self {
            Value::String(v) => format!("{:?}", v),
            Value::Char(v) => format!("{:?}", v),
            Value::Tuple(vals) => {
                let inner: Vec<String> = vals.iter().map(|v| v.debug_fmt()).collect();
                format!("({})", inner.join(", "))
            }
            Value::Array(rc) => {
                let vals = rc.read().unwrap();
                let inner: Vec<String> = vals.iter().map(|v| v.debug_fmt()).collect();
                format!("[{}]", inner.join(", "))
            }
            Value::Slice {
                storage,
                start,
                len,
                ..
            } => {
                let vals = storage.read().unwrap();
                let inner: Vec<String> = vals[*start..*start + *len]
                    .iter()
                    .map(|v| v.debug_fmt())
                    .collect();
                format!("[{}]", inner.join(", "))
            }
            Value::Map(entries) => {
                let pairs: Vec<String> = entries
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k.debug_fmt(), v.debug_fmt()))
                    .collect();
                format!("{{{}}}", pairs.join(", "))
            }
            Value::Struct { name, fields } => {
                let field_strs: Vec<String> = fields
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k, v.debug_fmt()))
                    .collect();
                format!("{} {{ {} }}", name, field_strs.join(", "))
            }
            Value::SharedStruct(inner) => {
                let mut parts: Vec<String> = inner
                    .immutable_fields
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k, v.debug_fmt()))
                    .collect();
                for (k, cell) in &inner.mut_fields {
                    let v = cell.value.try_read().expect(
                        "shared struct field write-locked during debug_fmt — unreachable in single-task interpreter",
                    );
                    parts.push(format!("{}: {}", k, v.debug_fmt()));
                }
                for (k, weak) in &inner.weak_immutable_fields {
                    parts.push(format!(
                        "{}: {}",
                        k,
                        upgrade_weak_to_option(weak).debug_fmt()
                    ));
                }
                for (k, slot) in &inner.weak_mut_fields {
                    let weak = slot.try_read().expect(
                        "shared struct weak field write-locked during debug_fmt — unreachable in single-task interpreter",
                    );
                    parts.push(format!(
                        "{}: {}",
                        k,
                        upgrade_weak_to_option(&weak).debug_fmt()
                    ));
                }
                format!("{} {{ {} }}", inner.name, parts.join(", "))
            }
            Value::EnumVariant { variant, data, .. } => match data {
                EnumData::Unit => variant.clone(),
                EnumData::Tuple(vals) => {
                    let inner: Vec<String> = vals.iter().map(|v| v.debug_fmt()).collect();
                    format!("{}({})", variant, inner.join(", "))
                }
                EnumData::Struct(fields) => {
                    let field_strs: Vec<String> = fields
                        .iter()
                        .map(|(k, v)| format!("{}: {}", k, v.debug_fmt()))
                        .collect();
                    format!("{} {{ {} }}", variant, field_strs.join(", "))
                }
            },
            Value::SortedSet(set) => {
                let inner: Vec<String> = set.keys().map(|k| k.0.debug_fmt()).collect();
                format!("SortedSet{{{}}}", inner.join(", "))
            }
            Value::SortedMap(map) => {
                let inner: Vec<String> = map
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k.0.debug_fmt(), v.debug_fmt()))
                    .collect();
                format!("SortedMap{{{}}}", inner.join(", "))
            }
            Value::Set(elems) => {
                let inner: Vec<String> = elems.iter().map(|v| v.debug_fmt()).collect();
                format!("Set{{{}}}", inner.join(", "))
            }
            Value::Sender(_) => "<Sender>".to_string(),
            Value::Receiver(_) => "<Receiver>".to_string(),
            other => format!("{}", other),
        }
    }
}

/// Identity comparison between two `Weak<SharedStructInner>` handles
/// for use in `Value::SharedStruct` PartialEq. Two weaks are equal iff
/// they point at the same allocation (`Arc::ptr_eq` after upgrade) or
/// both are dangling. A dangling weak is never equal to a live one.
pub(crate) fn weak_referent_eq(
    a: &std::sync::Weak<SharedStructInner>,
    b: &std::sync::Weak<SharedStructInner>,
) -> bool {
    match (a.upgrade(), b.upgrade()) {
        (None, None) => true,
        (Some(x), Some(y)) => Arc::ptr_eq(&x, &y),
        _ => false,
    }
}

/// Upgrade a stored `Weak<SharedStructInner>` to a runtime `Option[T]`
/// per design.md § Shared Types — Weak references. Returns
/// `Some(SharedStruct)` when the referent is still alive (the upgrade
/// bumps the strong RC), or `None` if every strong holder has been
/// dropped. Used at every `weak`-field read site and any `.upgrade()`
/// dispatch.
pub(crate) fn upgrade_weak_to_option(weak: &std::sync::Weak<SharedStructInner>) -> Value {
    match weak.upgrade() {
        Some(arc) => Value::EnumVariant {
            enum_name: "Option".to_string(),
            variant: "Some".to_string(),
            data: EnumData::Tuple(vec![Value::SharedStruct(arc)]),
        },
        None => Value::EnumVariant {
            enum_name: "Option".to_string(),
            variant: "None".to_string(),
            data: EnumData::Unit,
        },
    }
}
