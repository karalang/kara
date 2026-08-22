//! Value model: the `Value` enum and its impls, the supporting carrier
//! types (`EnumData`, `IteratorSource`, `IteratorStep`, `FieldCell`,
//! `SharedStructInner`, `OrdValue`), the runtime error / test outcome
//! types (`ErrorTraceFrame`, `RuntimeError`, `TestOutcome`), and the
//! free helpers `try_write_or_panic` / `primitive_const_to_value`.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard};

use crate::ast::*;
use crate::hasher_kind::HasherKind;
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
    /// Computed / renamed columns (slice 7 — the expression-projection
    /// leg). Each entry needs an output name (a bare `col(..)` keeps its
    /// own; anything else must be `.alias_(..)`ed); results REPLACE a
    /// same-named column or APPEND. Entries all see the step's INPUT
    /// frame, never each other (the Polars parallel semantics).
    WithColumns(Vec<Arc<LazyExprIR>>),
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
    /// Numeric arithmetic (slice 7) — `col("a").mul(2)`. NULL on either
    /// side makes the result NULL; i64 pairs stay i64 (loud on division
    /// by zero / overflow), any f64 widens (IEEE thereafter).
    Arith {
        op: LazyArithOp,
        lhs: Box<LazyExprIR>,
        rhs: Box<LazyExprIR>,
    },
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

/// The slice-7 arithmetic vocabulary (`add`/`sub`/`mul`/`div` builder
/// methods — the bare-operator spelling waits on the queued operator-
/// overload decision, same as the comparisons).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LazyArithOp {
    Add,
    Sub,
    Mul,
    Div,
}

impl LazyArithOp {
    pub fn symbol(&self) -> &'static str {
        match self {
            LazyArithOp::Add => "+",
            LazyArithOp::Sub => "-",
            LazyArithOp::Mul => "*",
            LazyArithOp::Div => "/",
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
            LazyExprIR::Arith { op, lhs, rhs } => write!(f, "({lhs} {} {rhs})", op.symbol()),
            LazyExprIR::And(a, b) => write!(f, "({a} and {b})"),
            LazyExprIR::Or(a, b) => write!(f, "({a} or {b})"),
            LazyExprIR::Not(x) => write!(f, "(not {x})"),
            LazyExprIR::Desc(x) => write!(f, "{x} desc"),
            LazyExprIR::Agg { op, arg } => write!(f, "{}({arg})", op.name()),
            LazyExprIR::Alias { name, expr } => write!(f, "{expr} as {name}"),
        }
    }
}

/// The float width an interpreter `Tensor`'s elements represent. The
/// interpreter stores every float as f64, so an `f32` tensor must round after
/// each write / element-wise op to agree with codegen's packed f32 buffer.
/// Non-float and f64 elements use `F64`, which rounds nothing. B-2026-08-05-31.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TensorElemWidth {
    F32,
    F64,
}

impl TensorElemWidth {
    /// Round `v` to this width. A no-op for `F64`.
    pub fn round(self, v: f64) -> f64 {
        match self {
            TensorElemWidth::F32 => v as f32 as f64,
            TensorElemWidth::F64 => v,
        }
    }
}

/// Narrow the i128 integer carrier to `i64` for a consumer that is not
/// 128-bit-ready yet.
///
/// STAGE-1 MARKER (B-2026-08-19-8). `Value::Int` carries i128, but 128-bit is
/// still REJECTED at type-check (B-2026-08-19-6) until codegen's own i64
/// carrier and the lexer catch up — so every value reaching an i64-only
/// consumer today genuinely fits, and the `debug_assert` proves that on every
/// test run rather than assuming it.
///
/// Each call site is also a place stage 3/5 must revisit once values can
/// exceed i64: `grep -rn narrow_to_i64 src/` is that worklist. Written as a
/// named helper rather than a bare `as i64` precisely so the worklist exists —
/// a plain cast would leave 100+ silent truncation points indistinguishable
/// from ordinary code, which is the mistake B-2026-08-19-6 was made of.
///
/// A HARD check, not a `debug_assert`. The cheaper form would let a release
/// build truncate silently if the staging reasoning above is ever wrong, and
/// silent truncation of an integer is the exact defect this whole line of work
/// exists to remove. One `try_from` against tree-walk dispatch cost is not
/// worth trading that for; the stage-1 benchmark is what says whether that
/// judgement holds.
#[inline]
pub fn narrow_to_i64(n: i128) -> i64 {
    match i64::try_from(n) {
        Ok(v) => v,
        Err(_) => panic!(
            "internal error: 128-bit value {n} reached an i64-only consumer. \
             The `Value::Int` carrier is i128 but this consumer is not 128-bit \
             ready yet (B-2026-08-19-8 stage 3/5)."
        ),
    }
}

/// Shared buffer behind a `Channel[T]`'s `Sender`/`Receiver` pair.
///
/// Carries the queue AND the bound so that `Channel.bounded(cap)` is a real
/// bound in the interpreter rather than a name that behaves like
/// `Channel.new()` (B-2026-08-22-16). Holding the capacity INSIDE the `Arc`
/// (rather than as a second variant field) keeps every existing
/// `Value::Sender(_)` pattern and the `Arc::ptr_eq` identity test working
/// unchanged — only the three sites that actually lock the queue moved.
#[derive(Debug)]
pub struct ChannelBuf {
    pub queue: Mutex<VecDeque<Value>>,
    /// 0 = unbounded (`Channel.new()`); positive = the `Channel.bounded(cap)`
    /// bound. `requires cap > 0` is enforced in the typechecker, so 0 means
    /// "unbounded" without ambiguity.
    pub capacity: usize,
    /// Live endpoint counts, PER END (B-2026-08-22-24).
    ///
    /// `Arc::strong_count` cannot answer "is a receiver still alive" — both
    /// ends hold the same `Arc`, so one count cannot tell a live sender from a
    /// live receiver. These are maintained by [`SenderHandle`] /
    /// [`ReceiverHandle`], whose `Clone` and `Drop` are the only things that
    /// move them, which is what makes `receivers == 0` mean exactly "every
    /// receiver went away".
    pub senders: AtomicUsize,
    pub receivers: AtomicUsize,
}

impl ChannelBuf {
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(ChannelBuf {
            queue: Mutex::new(VecDeque::new()),
            capacity,
            senders: AtomicUsize::new(0),
            receivers: AtomicUsize::new(0),
        })
    }

    /// True when no `Receiver` handle is left alive — the condition
    /// design.md's `send` ("panics if all receivers are dropped") and
    /// `try_send` (`SendError.Closed`) are specified against.
    pub fn receivers_gone(&self) -> bool {
        self.receivers.load(Ordering::Relaxed) == 0
    }
}

/// An owning `Sender` endpoint. The COUNT is the point: `Value` is freely
/// `Clone`d as it moves through a tree-walk evaluator, so the only way to
/// know how many endpoints are live is to make cloning and dropping the
/// handle the events that maintain it. A bare `Arc<ChannelBuf>` cannot,
/// which is why the endpoints stopped being one.
#[derive(Debug)]
pub struct SenderHandle(Arc<ChannelBuf>);

/// The `Receiver` twin of [`SenderHandle`]. Kept as two types rather than one
/// generic handle so a miscount is a type error rather than a wrong constant.
#[derive(Debug)]
pub struct ReceiverHandle(Arc<ChannelBuf>);

macro_rules! channel_endpoint_handle {
    ($handle:ident, $field:ident) => {
        impl $handle {
            pub fn new(buf: Arc<ChannelBuf>) -> Self {
                buf.$field.fetch_add(1, Ordering::Relaxed);
                $handle(buf)
            }
            /// The shared buffer. Read-only on purpose: handing out an owned
            /// `Arc` would let a caller resurrect an endpoint without the
            /// count moving.
            pub fn buf(&self) -> &Arc<ChannelBuf> {
                &self.0
            }
        }
        impl Clone for $handle {
            fn clone(&self) -> Self {
                $handle::new(Arc::clone(&self.0))
            }
        }
        impl Drop for $handle {
            fn drop(&mut self) {
                self.0.$field.fetch_sub(1, Ordering::Relaxed);
            }
        }
    };
}

channel_endpoint_handle!(SenderHandle, senders);
channel_endpoint_handle!(ReceiverHandle, receivers);

#[derive(Debug, Clone)]
pub enum Value {
    /// Every integer width's runtime carrier (B-2026-08-19-8 stage 1).
    ///
    /// WIDENED FROM `i64` deliberately, and the alternative is worth recording:
    /// a parallel `Int128(i128)` variant produced only TWO compiler errors
    /// against 480 `Value::Int` sites, because all but two of those matches
    /// carry a catch-all — so a 128-bit value would have fallen through ~478
    /// arms in silence. That is the exact shape of B-2026-08-19-6, where
    /// 128-bit was admitted to the type system with nothing enforcing that the
    /// value paths kept up. Widening costs more edits and has no silent path:
    /// every site that cares about the width is a compile error.
    ///
    /// Carrying i128 does NOT grow `Value` — it is already 184 bytes because of
    /// the `String` variant.
    ///
    /// The carrier is wider than any width it currently serves: 128-bit is
    /// still REJECTED at type-check (B-2026-08-19-6) until the codegen carrier
    /// and the lexer catch up, so nothing puts an out-of-i64-range value in
    /// here yet. That is intentional staging, not an oversight.
    Int(i128),
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
        /// B-2026-08-05-31 — the declared element WIDTH, so f32 tensors round
        /// to f32 precision. `Value` has no f32 carrier (every float is f64),
        /// which made `Tensor[f32]` arithmetic disagree with AOT's packed f32
        /// buffer: `0.1 * 3` printed 0.30000000000000004 here and
        /// 0.30000001192092896 from `karac build`. Carrying the width lets the
        /// element-wise ops and element writes round through `as f32`, matching
        /// codegen. `F64` is the default and preserves prior behaviour for every
        /// other element type.
        elem: TensorElemWidth,
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
    /// B-2026-08-21-8 — `Arc<RwLock<MapData>>`, the same shape [`Value::Array`]
    /// uses, and for the same two reasons.
    ///
    /// This WAS a bare `Vec<(Value, Value)>`, which made every map operation
    /// quadratic through a path that has nothing to do with maps: `Env::get`
    /// ends in `other.clone()`, so materializing a method's receiver
    /// deep-copied the whole map before the method ran. Insert-only loops were
    /// quadratic on their own — measured 3.3x/3.8x per doubling — while the
    /// identical `Vec.push` loop stayed linear precisely because `Array` is
    /// already behind an `Arc`. An index alone would not have touched that
    /// cost; the representation is the fix.
    ///
    /// The derived `Clone` therefore SHARES storage, exactly as `Array`'s
    /// does, and value semantics come from `deep_clone_value` at binding sites
    /// — which has had a `Map` arm all along.
    Map(Arc<RwLock<MapData>>),
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
    /// A `weak T` handle held somewhere that is NOT a struct field —
    /// today, a container element (`Vec[weak N]`). B-2026-08-08-14.
    ///
    /// A `weak` FIELD needs no variant: it lives in `SharedStructInner`'s
    /// dedicated `weak_immutable_fields` / `weak_mut_fields` maps and is
    /// upgraded at the field-read site. A container element has no such home,
    /// so before this the push stored an ordinary strong
    /// [`Value::SharedStruct`] — which made a cycle through the container
    /// uncollectable in the interpreter AND made the element read hand a bare
    /// struct to a `match` expecting `Option[T]`.
    ///
    /// Never observed directly by user code: every read site upgrades it
    /// through [`upgrade_weak_to_option`] first, exactly as a weak field read
    /// does, so what a program sees is always an `Option[T]`.
    WeakRef(std::sync::Weak<SharedStructInner>),
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
    /// F16 total-order wrapper (16-bit IEEE half). Stored promoted to `f64`
    /// (the tree-walk interpreter has no native `f16` — same f64-promotion
    /// posture as the `f16` primitive; the compiled path is exact half
    /// precision). NaN sorts last, implements Eq/Ord/Hash.
    TotalFloat16(f64),
    /// Bf16 total-order wrapper (bfloat16). Stored promoted to `f64` (same
    /// posture as `TotalFloat16`). NaN sorts last, implements Eq/Ord/Hash.
    TotalBFloat16(f64),
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
    /// B-2026-08-21-8 — `Arc<RwLock<SetData>>`, the `Map` sibling. Same
    /// representation change, same reasons: the receiver clone was O(n) and
    /// `contains` was a linear scan.
    Set(Arc<RwLock<SetData>>),
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
    Sender(SenderHandle),
    /// Receiver[T] end of a Channel[T]. `recv()` blocks until an item is
    /// available; `try_recv()` returns immediately as `Option[T]`. In the
    /// single-threaded tree-walk interpreter the test pattern is always
    /// send-before-recv, so the queue already has items when recv fires.
    Receiver(ReceiverHandle),
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

// ── Map / Set storage (B-2026-08-21-8) ─────────────────────────

/// Hash a [`Value`] for the `Map` / `Set` index.
///
/// The ONE obligation is `a == b` ⟹ `hash_value(a) == hash_value(b)`. The
/// converse is not required and not attempted: [`MapData`] re-checks every
/// candidate with `==` before accepting it, so a collision costs a comparison,
/// never a wrong answer.
///
/// That asymmetry is what makes this safe to write conservatively, and the
/// arms below lean on it hard. Anything whose equality is subtle — every float
/// carrier, `SharedStruct`, channel ends, closures — hashes to its
/// DISCRIMINANT ALONE, so all such keys share one bucket and the lookup
/// degrades to a linear scan over them. That is precisely the behaviour this
/// bug is about removing, which is the point: the fallback is exactly today's
/// association-list semantics, so a key type this function under-hashes is
/// slow, never incorrect. Only OVER-hashing — splitting two equal values into
/// different buckets — could lose an entry, so no arm hashes anything the
/// matching arm of `PartialEq for Value` does not compare.
///
/// The float carriers are the sharpest case and the reason the rule is stated
/// this way: `Value::Float`'s equality is IEEE (`NaN != NaN`, `0.0 == -0.0`),
/// which no bit-pattern hash can track. They are unreachable as keys anyway —
/// the typechecker rejects `Map[f64, _]` with "key type does not implement
/// `Hash`" — so this costs nothing today and stays correct if that ever changes.
///
/// `Struct` fields and `EnumData::Struct` payloads live in a `HashMap`, whose
/// iteration order is not stable, so their per-field hashes are combined with
/// XOR — commutative, hence order-independent.
/// The interpreter's `Value` hash under an explicitly chosen hasher — the `Map[K, V, H]` /
/// `Set[T, H]` selector (B-2026-08-21-6). A container hashes every key through
/// the kind it was BUILT with, so its index and its observable order can never
/// disagree about which permutation is in force.
///
/// The two arms are the same walk over the same `Value` tree; only the leaf
/// hasher differs, so a key that is `Eq` to another still hashes equal under
/// either — the consistency contract holds per-container, which is the only
/// place it has to.
pub(crate) fn hash_value_with(kind: &HasherKind, v: &Value) -> u64 {
    match kind {
        HasherKind::SipHash13 => hash_value_generic::<karac_hash::KaraHasher>(v),
        HasherKind::Fx => hash_value_generic::<karac_hash::FxHasher>(v),
        // A user hasher is not a leaf permutation at all — it is user code, and
        // it consumes BYTES rather than a `Value` tree. See
        // `interpreter::user_hasher` for the flattening and for how an
        // interpreter is found this deep inside a container (B-2026-08-22-6).
        HasherKind::User(builder) => super::user_hasher::hash_value(builder, v),
    }
}

fn hash_value_generic<H: std::hash::Hasher + Default>(v: &Value) -> u64 {
    use std::hash::Hash;

    fn field_map_hash<H: std::hash::Hasher + Default>(fields: &HashMap<String, Value>) -> u64 {
        fields.iter().fold(0u64, |acc, (name, val)| {
            let mut h = H::default();
            name.hash(&mut h);
            hash_value_generic::<H>(val).hash(&mut h);
            acc ^ h.finish()
        })
    }

    let mut h = H::default();
    std::mem::discriminant(v).hash(&mut h);
    match v {
        Value::Int(i) => i.hash(&mut h),
        Value::String(s) => s.hash(&mut h),
        Value::Char(c) => c.hash(&mut h),
        Value::Bool(b) => b.hash(&mut h),
        Value::Unit => {}
        Value::CStr(bytes) | Value::CString(bytes) => bytes.hash(&mut h),
        Value::Tuple(items) => {
            for item in items {
                hash_value_generic::<H>(item).hash(&mut h);
            }
        }
        // Equality is CONTENTS-based (`Arc::ptr_eq` is only its fast path), so
        // the hash must walk the contents too — hashing the pointer would split
        // two equal arrays into different buckets.
        Value::Array(rc) => {
            for item in rc.read().unwrap().iter() {
                hash_value_generic::<H>(item).hash(&mut h);
            }
        }
        Value::Slice {
            storage,
            start,
            len,
            ..
        } => {
            let items = storage.read().unwrap();
            for item in &items[*start..*start + *len] {
                hash_value_generic::<H>(item).hash(&mut h);
            }
        }
        Value::EnumVariant {
            enum_name,
            variant,
            data,
        } => {
            enum_name.hash(&mut h);
            variant.hash(&mut h);
            match data {
                EnumData::Unit => {}
                EnumData::Tuple(items) => {
                    for item in items {
                        hash_value_generic::<H>(item).hash(&mut h);
                    }
                }
                EnumData::Struct(fields) => field_map_hash::<H>(fields).hash(&mut h),
            }
        }
        Value::Struct { name, fields } => {
            name.hash(&mut h);
            field_map_hash::<H>(fields).hash(&mut h);
        }
        // Discriminant only — see the doc comment. Correct, deliberately coarse.
        _ => {}
    }
    h.finish()
}

/// The interpreter's `Map` storage: insertion-ordered entries plus a hash
/// index over them (B-2026-08-21-8).
///
/// WHY BOTH. `entries` is the observable half — design.md § Map leaves
/// iteration order unspecified, but the interpreter has always iterated in
/// insertion order and there is no reason for this change to disturb that, so
/// every read path walks the `Vec` exactly as it did when this type WAS a
/// `Vec<(Value, Value)>`. `index` is the half that makes lookup stop being a
/// linear scan: it maps a key's hash to the positions in `entries` that might
/// hold it, and the caller confirms with `==`.
///
/// The positions are the reason `remove` reindexes: `Vec::remove` shifts every
/// later entry down one, invalidating stored positions. That rebuild is O(n),
/// but so is the shift it follows, so removal's complexity is unchanged.
#[derive(Debug, Default)]
pub struct MapData {
    entries: Vec<(Value, Value)>,
    index: HashMap<u64, Vec<usize>>,
    /// Which hash this map was BUILT with — the `Map[K, V, H]` selector
    /// (B-2026-08-21-6). Defaults to the spec's `SipHash13BuildHasher`, so a
    /// map created anywhere no type annotation is in sight lands on the
    /// DoS-resistant hasher rather than inheriting the fast one by accident.
    ///
    /// Carried by the value rather than looked up per operation because the
    /// hasher stops being visible in the type: `take_hasher_type_arg` strips
    /// it, so `Map[String, i64, FxBuildHasher]` and `Map[String, i64]` are one
    /// type and a map keeps its own hasher wherever it is passed. That mirrors
    /// codegen, where the chosen hash function is a field of the control block
    /// `karac_map_new` returns.
    hasher: HasherKind,
}

impl Clone for MapData {
    /// Clones the entries and REBUILDS the index rather than cloning it. The
    /// index is derived state; rebuilding is the same O(n) the entry clone
    /// already costs and removes any way for the two halves to disagree.
    ///
    /// The clone keeps the ORIGINAL's hasher: a copy of an `FxBuildHasher` map
    /// is still one, so its iteration order is the same as its source's.
    fn clone(&self) -> Self {
        Self::from_entries_with_hasher(self.hasher.clone(), self.entries.clone())
    }
}

impl MapData {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_entries(entries: Vec<(Value, Value)>) -> Self {
        Self::from_entries_with_hasher(HasherKind::default(), entries)
    }

    pub fn from_entries_with_hasher(hasher: HasherKind, entries: Vec<(Value, Value)>) -> Self {
        let mut index: HashMap<u64, Vec<usize>> = HashMap::with_capacity(entries.len());
        for (i, (k, _)) in entries.iter().enumerate() {
            index
                .entry(hash_value_with(&hasher, k))
                .or_default()
                .push(i);
        }
        Self {
            entries,
            index,
            hasher,
        }
    }

    /// The hasher this map was built with.
    pub fn hasher(&self) -> HasherKind {
        self.hasher.clone()
    }

    /// Switch the hasher and rebuild the index under it. Called once, on a
    /// FRESH map, when the construction site's type annotation names a hasher
    /// (`Map.new()` in `eval_call`); the reindex makes it correct even if it
    /// were ever called on a populated map.
    pub fn set_hasher(&mut self, hasher: HasherKind) {
        if self.hasher == hasher {
            return;
        }
        self.hasher = hasher;
        self.reindex();
    }

    fn hash_key(&self, key: &Value) -> u64 {
        hash_value_with(&self.hasher, key)
    }

    pub fn entries(&self) -> &[(Value, Value)] {
        &self.entries
    }

    pub fn into_entries(self) -> Vec<(Value, Value)> {
        self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, (Value, Value)> {
        self.entries.iter()
    }

    /// The entries in OBSERVABLE order — what `keys()` / `values()` /
    /// `entries()` / a `for` loop / `Display` walk (B-2026-08-21-6).
    ///
    /// design.md § Map: "Iteration order is unspecified and varies across
    /// process runs — the default hasher is DoS-resistant and seeded fresh per
    /// process ... so bucket placement and therefore iteration order differ
    /// from one run to the next." Ordering by the SEEDED hash is how the
    /// interpreter delivers that; the compiled backends get it for free,
    /// because `karac_map_*` is an open-addressed table and its walk already
    /// follows bucket placement.
    ///
    /// STORAGE stays insertion-ordered on purpose. `entries` is indexed
    /// positionally by the `Entry` chain's `slot_idx` and by `position_of`, and
    /// reordering it would invalidate every stored position — the same hazard
    /// that makes `remove` reindex. Only the observable walk is permuted, so
    /// the two concerns stay separate.
    ///
    /// Ties break on insertion index, so the order is a deterministic function
    /// of (seed, contents) rather than of hash-map internals — two runs with
    /// the same pinned `KARAC_HASH_SEED` agree exactly, which is what lets the
    /// test suites and the kata A/B harness compare output at all.
    pub fn iter_observable(&self) -> impl Iterator<Item = &(Value, Value)> {
        let mut order: Vec<usize> = (0..self.entries.len()).collect();
        order.sort_by_key(|&i| (self.hash_key(&self.entries[i].0), i));
        order.into_iter().map(move |i| &self.entries[i])
    }

    /// Position of `key` in `entries`, or `None`. The hash narrows the search
    /// to one bucket; `==` decides, so a collision is a slower answer and never
    /// a wrong one.
    pub fn position_of(&self, key: &Value) -> Option<usize> {
        let bucket = self.index.get(&self.hash_key(key))?;
        bucket.iter().copied().find(|&i| self.entries[i].0 == *key)
    }

    pub fn contains_key(&self, key: &Value) -> bool {
        self.position_of(key).is_some()
    }

    pub fn get(&self, key: &Value) -> Option<&Value> {
        self.position_of(key).map(|i| &self.entries[i].1)
    }

    pub fn get_mut(&mut self, key: &Value) -> Option<&mut Value> {
        self.position_of(key).map(|i| &mut self.entries[i].1)
    }

    /// Positional read of a key / value, for the `Entry` chain's `slot_idx`.
    pub fn key_at(&self, i: usize) -> Option<&Value> {
        self.entries.get(i).map(|(k, _)| k)
    }

    pub fn value_at(&self, i: usize) -> Option<&Value> {
        self.entries.get(i).map(|(_, v)| v)
    }

    /// Positional write of a VALUE. Deliberately hands out no path to the key:
    /// the index maps key hashes to positions, so mutating a key in place
    /// would strand its entry where no lookup can find it, while changing a
    /// value cannot invalidate anything.
    pub fn value_at_mut(&mut self, i: usize) -> Option<&mut Value> {
        self.entries.get_mut(i).map(|(_, v)| v)
    }

    /// Insert or overwrite, returning the previous value. An overwrite keeps
    /// the key's original POSITION, so insertion order is a property of first
    /// insertion — matching the `Vec`-scan code this replaces, which found the
    /// existing pair and assigned through it.
    pub fn insert(&mut self, key: Value, value: Value) -> Option<Value> {
        match self.position_of(&key) {
            Some(i) => Some(std::mem::replace(&mut self.entries[i].1, value)),
            None => {
                self.index
                    .entry(self.hash_key(&key))
                    .or_default()
                    .push(self.entries.len());
                self.entries.push((key, value));
                None
            }
        }
    }

    pub fn remove(&mut self, key: &Value) -> Option<(Value, Value)> {
        let i = self.position_of(key)?;
        let pair = self.entries.remove(i);
        self.reindex();
        Some(pair)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.index.clear();
    }

    pub fn retain(&mut self, mut f: impl FnMut(&Value, &mut Value) -> bool) {
        self.entries.retain_mut(|(k, v)| f(k, v));
        self.reindex();
    }

    /// Rebuild `index` from `entries`. Required after anything that moves an
    /// entry to a different position.
    fn reindex(&mut self) {
        let hasher = self.hasher.clone();
        self.index.clear();
        for (i, (k, _)) in self.entries.iter().enumerate() {
            self.index
                .entry(hash_value_with(&hasher, k))
                .or_default()
                .push(i);
        }
    }
}

/// The interpreter's `Set` storage — [`MapData`]'s sibling, same design and
/// same reasons (B-2026-08-21-8): insertion-ordered `items` for the observable
/// half, a hash index over them so `contains` / `insert` / `remove` stop being
/// linear scans, and `==` confirming every candidate the hash suggests.
#[derive(Debug, Default)]
pub struct SetData {
    items: Vec<Value>,
    index: HashMap<u64, Vec<usize>>,
    /// The `Set[T, H]` selector — see [`MapData::hasher`] for the whole
    /// rationale; this is the same field for the same reason.
    hasher: HasherKind,
}

impl Clone for SetData {
    fn clone(&self) -> Self {
        Self::from_items_with_hasher(self.hasher.clone(), self.items.clone())
    }
}

impl SetData {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_items(items: Vec<Value>) -> Self {
        Self::from_items_with_hasher(HasherKind::default(), items)
    }

    pub fn from_items_with_hasher(hasher: HasherKind, items: Vec<Value>) -> Self {
        let mut index: HashMap<u64, Vec<usize>> = HashMap::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            index
                .entry(hash_value_with(&hasher, item))
                .or_default()
                .push(i);
        }
        Self {
            items,
            index,
            hasher,
        }
    }

    /// The hasher this set was built with.
    pub fn hasher(&self) -> HasherKind {
        self.hasher.clone()
    }

    /// The items in OBSERVABLE order — [`MapData::iter_observable`]'s twin,
    /// same contract and same reasons. `items` stays insertion-ordered because
    /// the index addresses it positionally; only the walk is permuted.
    pub fn iter_observable(&self) -> impl Iterator<Item = &Value> {
        let mut order: Vec<usize> = (0..self.items.len()).collect();
        order.sort_by_key(|&i| (self.hash_item(&self.items[i]), i));
        order.into_iter().map(move |i| &self.items[i])
    }

    /// Switch the hasher and rebuild the index — [`MapData::set_hasher`]'s twin.
    pub fn set_hasher(&mut self, hasher: HasherKind) {
        if self.hasher == hasher {
            return;
        }
        self.hasher = hasher;
        self.reindex();
    }

    fn hash_item(&self, item: &Value) -> u64 {
        hash_value_with(&self.hasher, item)
    }

    /// Build from items that may contain DUPLICATES, keeping the first
    /// occurrence of each — the set-ness a raw `Vec` of elements does not
    /// carry on its own.
    pub fn from_items_deduped(items: Vec<Value>) -> Self {
        let mut set = Self::new();
        for item in items {
            set.insert(item);
        }
        set
    }

    pub fn items(&self) -> &[Value] {
        &self.items
    }

    pub fn into_items(self) -> Vec<Value> {
        self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Value> {
        self.items.iter()
    }

    pub fn position_of(&self, item: &Value) -> Option<usize> {
        let bucket = self.index.get(&self.hash_item(item))?;
        bucket.iter().copied().find(|&i| self.items[i] == *item)
    }

    pub fn contains(&self, item: &Value) -> bool {
        self.position_of(item).is_some()
    }

    /// Insert, returning whether the set gained an element.
    pub fn insert(&mut self, item: Value) -> bool {
        if self.contains(&item) {
            return false;
        }
        self.index
            .entry(self.hash_item(&item))
            .or_default()
            .push(self.items.len());
        self.items.push(item);
        true
    }

    pub fn remove(&mut self, item: &Value) -> bool {
        match self.position_of(item) {
            Some(i) => {
                self.items.remove(i);
                self.reindex();
                true
            }
            None => false,
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.index.clear();
    }

    pub fn retain(&mut self, mut f: impl FnMut(&Value) -> bool) {
        self.items.retain(|v| f(v));
        self.reindex();
    }

    fn reindex(&mut self) {
        let hasher = self.hasher.clone();
        self.index.clear();
        for (i, item) in self.items.iter().enumerate() {
            self.index
                .entry(hash_value_with(&hasher, item))
                .or_default()
                .push(i);
        }
    }
}

impl FromIterator<Value> for SetData {
    /// DEDUPING, because every caller collecting into a set means set-ness.
    fn from_iter<I: IntoIterator<Item = Value>>(iter: I) -> Self {
        Self::from_items_deduped(iter.into_iter().collect())
    }
}

impl<'a> IntoIterator for &'a SetData {
    type Item = &'a Value;
    type IntoIter = std::slice::Iter<'a, Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

impl FromIterator<(Value, Value)> for MapData {
    fn from_iter<I: IntoIterator<Item = (Value, Value)>>(iter: I) -> Self {
        Self::from_entries(iter.into_iter().collect())
    }
}

impl<'a> IntoIterator for &'a MapData {
    type Item = &'a (Value, Value);
    type IntoIter = std::slice::Iter<'a, (Value, Value)>;
    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter()
    }
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
            (Value::TotalFloat16(a), Value::TotalFloat16(b)) => a.total_cmp(b).is_eq(),
            (Value::TotalBFloat16(a), Value::TotalBFloat16(b)) => a.total_cmp(b).is_eq(),
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
                if Arc::ptr_eq(a, b) {
                    return true;
                }
                let (a, b) = (a.read().unwrap(), b.read().unwrap());
                // Order-insensitive, as it has always been — but through the
                // index rather than a nested scan, so map equality is O(n)
                // instead of O(n^2) (B-2026-08-21-8).
                a.len() == b.len() && a.iter().all(|(k, v)| b.get(k) == Some(v))
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
            (Value::Set(a), Value::Set(b)) => {
                if Arc::ptr_eq(a, b) {
                    return true;
                }
                let (a, b) = (a.read().unwrap(), b.read().unwrap());
                // Indexed membership rather than a nested scan: O(n), was O(n^2).
                a.len() == b.len() && a.iter().all(|x| b.contains(x))
            }
            // Channel ends compare by pointer identity — two Senders are equal
            // only when they wrap the exact same Arc allocation.
            (Value::Sender(a), Value::Sender(b)) => Arc::ptr_eq(a.buf(), b.buf()),
            (Value::Receiver(a), Value::Receiver(b)) => Arc::ptr_eq(a.buf(), b.buf()),
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
                for (i, (k, v)) in entries.read().unwrap().iter_observable().enumerate() {
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
            // B-2026-08-08-14 — rendered through the upgrade, so a live
            // target prints as the struct it points at and a dead one prints
            // `None`. Displaying the handle itself would leak the
            // representation into user-visible output; every other read site
            // upgrades first, and so does this one.
            Value::WeakRef(w) => match w.upgrade() {
                Some(arc) => write!(f, "{}", Value::SharedStruct(arc)),
                None => write!(f, "None"),
            },
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
            // B-2026-08-11-8: render the INNER float, matching the bare
            // primitive (`1.5`) and codegen's `synth_display.rs` arm — NOT
            // `F64(1.5)`. Until this slice the Display gate rejected every
            // `F64` in `println` / f-strings, so the wrapper form was
            // unreachable from checked Kāra and only ever appeared in
            // typecheck-bypassing interpreter tests; making it printable
            // meant picking a rendering, and the wrapper is an ordering
            // contract, not a distinct textual form.
            Value::TotalFloat32(v) => write!(f, "{}", v),
            Value::TotalFloat64(v) => write!(f, "{}", v),
            Value::TotalFloat16(v) => write!(f, "{}", v),
            Value::TotalBFloat16(v) => write!(f, "{}", v),
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
                for (i, v) in elems.read().unwrap().iter_observable().enumerate() {
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
        I8(v) => Value::Int((*v as i64).into()),
        I16(v) => Value::Int((*v as i64).into()),
        I32(v) => Value::Int((*v as i64).into()),
        I64(v) => Value::Int((*v).into()),
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
        I128(v) => Value::Int((*v as i64).into()),
        U8(v) => Value::Int((*v as i64).into()),
        U16(v) => Value::Int((*v as i64).into()),
        U32(v) => Value::Int((*v as i64).into()),
        U64(v) => Value::Int((*v as i64).into()),
        U128(v) => Value::Int((*v as i64).into()),
        Usize(v) => Value::Int((*v as i64).into()),
        Isize(v) => Value::Int((*v).into()),
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

    /// Build a `Map` from insertion-ordered entries — the `Value::Map` twin of
    /// [`Value::array_of`], and the spelling every construction site uses so
    /// the index is built in exactly one place.
    pub fn map_of(entries: Vec<(Value, Value)>) -> Value {
        Value::Map(Arc::new(RwLock::new(MapData::from_entries(entries))))
    }

    /// An empty `Map`.
    pub fn empty_map() -> Value {
        Value::map_of(Vec::new())
    }

    /// Build a `Set` from insertion-ordered items, keeping the first
    /// occurrence of any duplicate.
    pub fn set_of(items: Vec<Value>) -> Value {
        Value::Set(Arc::new(RwLock::new(SetData::from_items_deduped(items))))
    }

    /// An empty `Set`.
    pub fn empty_set() -> Value {
        Value::set_of(Vec::new())
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
            Value::WeakRef(_) => "WeakRef",
            Value::EnumVariant { .. } => "EnumVariant",
            Value::Function { .. } => "Function",
            Value::TotalFloat32(_) => "TotalFloat32",
            Value::TotalFloat64(_) => "TotalFloat64",
            Value::TotalFloat16(_) => "TotalFloat16",
            Value::TotalBFloat16(_) => "TotalBFloat16",
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
                    .read()
                    .unwrap()
                    .iter_observable()
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
                let inner: Vec<String> = elems
                    .read()
                    .unwrap()
                    .iter_observable()
                    .map(|v| v.debug_fmt())
                    .collect();
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

#[cfg(test)]
mod map_data_tests {
    use super::*;

    /// The load-bearing invariant of the whole `Map` index (B-2026-08-21-8):
    /// `a == b` ⟹ `hash_value(a) == hash_value(b)`. Break it and a lookup
    /// silently misses an entry that is present, which is a wrong answer, not
    /// a slow one — so this is checked over every key shape the typechecker
    /// admits (`Map[K, V]` requires `K: Hash + Eq`: integers of any width,
    /// `String`, `char`, `bool`, tuples, `Vec`, `Array`, `Option`, and
    /// `#[derive(Hash, Eq)]` structs), plus the exotic carriers that reach the
    /// deliberately-coarse fallback.
    ///
    /// Pairs are built so that equal values are constructed INDEPENDENTLY —
    /// two separately-allocated arrays, two separately-built structs — since
    /// hashing a pointer instead of contents would pass a test that compared a
    /// value to a clone of itself.
    fn equal_pairs() -> Vec<(Value, Value)> {
        let mut fields_a = HashMap::new();
        fields_a.insert("x".to_string(), Value::Int(1));
        fields_a.insert("y".to_string(), Value::String("s".into()));
        let mut fields_b = HashMap::new();
        // Inserted in the opposite order: `fields` is a `HashMap`, so the hash
        // must not depend on iteration order.
        fields_b.insert("y".to_string(), Value::String("s".into()));
        fields_b.insert("x".to_string(), Value::Int(1));

        vec![
            (Value::Int(42), Value::Int(42)),
            (Value::Int(-7), Value::Int(-7)),
            (Value::String("hello".into()), Value::String("hello".into())),
            (Value::String(String::new()), Value::String(String::new())),
            (Value::Char('é'), Value::Char('é')),
            (Value::Bool(true), Value::Bool(true)),
            (Value::Unit, Value::Unit),
            (
                Value::Tuple(vec![Value::Int(1), Value::String("a".into())]),
                Value::Tuple(vec![Value::Int(1), Value::String("a".into())]),
            ),
            (
                Value::array_of(vec![Value::Int(1), Value::Int(2)]),
                Value::array_of(vec![Value::Int(1), Value::Int(2)]),
            ),
            (Value::array_of(vec![]), Value::array_of(vec![])),
            (
                Value::EnumVariant {
                    enum_name: "Option".into(),
                    variant: "Some".into(),
                    data: EnumData::Tuple(vec![Value::Int(3)]),
                },
                Value::EnumVariant {
                    enum_name: "Option".into(),
                    variant: "Some".into(),
                    data: EnumData::Tuple(vec![Value::Int(3)]),
                },
            ),
            (
                Value::EnumVariant {
                    enum_name: "Option".into(),
                    variant: "None".into(),
                    data: EnumData::Unit,
                },
                Value::EnumVariant {
                    enum_name: "Option".into(),
                    variant: "None".into(),
                    data: EnumData::Unit,
                },
            ),
            (
                Value::Struct {
                    name: "P".into(),
                    fields: fields_a,
                },
                Value::Struct {
                    name: "P".into(),
                    fields: fields_b,
                },
            ),
            // Nested: a tuple of containers, to catch a recursion that stops
            // one level down.
            (
                Value::Tuple(vec![Value::array_of(vec![Value::Int(9)])]),
                Value::Tuple(vec![Value::array_of(vec![Value::Int(9)])]),
            ),
            // The coarse-fallback carriers. Equal by `==`, so they must hash
            // equal — trivially true while they hash to the discriminant, and
            // this is what fails if someone later adds a bit-pattern arm.
            (Value::Float(1.5), Value::Float(1.5)),
            (Value::Float(0.0), Value::Float(-0.0)),
        ]
    }

    /// design.md § `Hash` and `Hasher`, "`Eq` consistency contract": `a == b`
    /// must feed identical bytes to the hasher. Checked under BOTH selectors,
    /// because a `Map[K, V, FxBuildHasher]` whose keys hashed inconsistently
    /// would miss lookups exactly as badly as the default one would.
    #[test]
    fn hash_value_agrees_with_equality_under_every_hasher() {
        for kind in &[HasherKind::SipHash13, HasherKind::Fx] {
            for (a, b) in equal_pairs() {
                assert!(
                    a == b,
                    "fixture is not actually an equal pair: {a:?} vs {b:?}"
                );
                assert_eq!(
                    hash_value_with(kind, &a),
                    hash_value_with(kind, &b),
                    "equal values hashed differently under {kind:?} — a Map lookup \
                     would miss: {a:?} vs {b:?}"
                );
            }
        }
    }

    /// A map built with the Fx selector must actually USE it: the two hashers
    /// disagree on these keys, so an `FxBuildHasher` map whose index was still
    /// keyed on the seeded hash would show the default's order here.
    ///
    /// THE KEY COUNT IS LOAD-BEARING (B-2026-08-22-22). This detects the
    /// regression by asserting the two hashers produce DIFFERENT iteration
    /// orders, and SipHash13 is seeded per process — so with few enough keys
    /// the seeded order coincides with Fx's now and then and the `assert_ne!`
    /// fires on correct code. It did, once, with six keys: both sides came
    /// back `["charlie", "yankee", "mike", "bravo", "alpha", "zulu"]`.
    ///
    /// Six keys admit 720 orderings; the observed rate was under 1 in 400
    /// runs of the compiled test binary (each a fresh process, i.e. a fresh
    /// seed), consistent with that. Sixteen keys admit ~2e13, which puts the
    /// coincidence far below every other source of flake in this suite. This
    /// is a probability argument, not a proof — the alternative, pinning the
    /// exact expected Fx walk as a constant, would be fully deterministic but
    /// would couple the test to the index's bucket layout, which is precisely
    /// what the order comparison was written to avoid.
    #[test]
    fn a_map_hashes_through_the_hasher_it_was_built_with() {
        let keys = [
            "zulu", "alpha", "mike", "bravo", "yankee", "charlie", "delta", "echo", "foxtrot",
            "golf", "hotel", "india", "juliett", "kilo", "lima", "november",
        ];
        let entries: Vec<(Value, Value)> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| (Value::String(k.to_string()), Value::Int(i as i128)))
            .collect();

        let sip = MapData::from_entries_with_hasher(HasherKind::SipHash13, entries.clone());
        let fx = MapData::from_entries_with_hasher(HasherKind::Fx, entries);
        assert_eq!(fx.hasher(), HasherKind::Fx);

        let order = |m: &MapData| -> Vec<String> {
            m.iter_observable()
                .map(|(k, _)| match k {
                    Value::String(s) => s.clone(),
                    other => panic!("unexpected key {other:?}"),
                })
                .collect()
        };
        assert_ne!(
            order(&sip),
            order(&fx),
            "both hashers produced the same order — the selector is not reaching the index"
        );

        // And the Fx order is REPRODUCIBLE, which is the property being opted
        // into: rebuilding the same map gives the same walk, in a process
        // whose seed is random.
        let fx_again = MapData::from_entries_with_hasher(
            HasherKind::Fx,
            keys.iter()
                .enumerate()
                .map(|(i, k)| (Value::String(k.to_string()), Value::Int(i as i128)))
                .collect(),
        );
        assert_eq!(order(&fx), order(&fx_again));

        // Lookup still works through the non-default index.
        for (i, k) in keys.iter().enumerate() {
            assert_eq!(
                fx.get(&Value::String(k.to_string())),
                Some(&Value::Int(i as i128)),
                "Fx map lost key {k}"
            );
        }
    }

    #[test]
    fn map_data_finds_every_key_it_stores() {
        // The index answering correctly for each key shape, through the real
        // insert/lookup path rather than through `hash_value` alone.
        let mut m = MapData::new();
        let keys: Vec<Value> = equal_pairs().into_iter().map(|(a, _)| a).collect();
        for (i, k) in keys.iter().enumerate() {
            m.insert(k.clone(), Value::Int(i as i128));
        }
        for (i, (a, b)) in equal_pairs().into_iter().enumerate() {
            // Looked up by the INDEPENDENTLY-built twin, not by the stored key.
            assert_eq!(
                m.get(&b),
                Some(&Value::Int(i as i128)),
                "lookup missed a stored key: {a:?}"
            );
        }
        assert_eq!(m.len(), keys.len());
    }

    #[test]
    fn map_data_preserves_insertion_order_across_overwrite_and_remove() {
        // The observable half. Iteration order is insertion order, an
        // overwrite keeps the original position, and a removal closes the gap
        // without disturbing the survivors' order.
        let mut m = MapData::new();
        for k in [30i128, 10, 20, 5] {
            m.insert(Value::Int(k), Value::Int(k * 2));
        }
        let order = |m: &MapData| -> Vec<i128> {
            m.iter()
                .map(|(k, _)| match k {
                    Value::Int(i) => *i,
                    other => panic!("unexpected key {other:?}"),
                })
                .collect()
        };
        assert_eq!(order(&m), vec![30, 10, 20, 5]);

        let prev = m.insert(Value::Int(10), Value::Int(999));
        assert_eq!(
            prev,
            Some(Value::Int(20)),
            "overwrite returns the old value"
        );
        assert_eq!(order(&m), vec![30, 10, 20, 5], "overwrite keeps position");
        assert_eq!(m.get(&Value::Int(10)), Some(&Value::Int(999)));

        m.remove(&Value::Int(30));
        assert_eq!(order(&m), vec![10, 20, 5]);
        // Every survivor still findable — this is what a stale index breaks.
        for k in [10i128, 20, 5] {
            assert!(m.contains_key(&Value::Int(k)), "lost key {k} after remove");
        }
        assert!(!m.contains_key(&Value::Int(30)));
    }

    #[test]
    fn map_data_handles_a_hash_collision_bucket() {
        // Two DIFFERENT keys that share a bucket by construction: both float
        // carriers hash to the discriminant alone. The `==` recheck is what
        // keeps them distinct, and this is the case that would silently
        // conflate them if `position_of` trusted the hash.
        let mut m = MapData::new();
        m.insert(Value::Float(1.0), Value::Int(1));
        m.insert(Value::Float(2.0), Value::Int(2));
        assert_eq!(m.len(), 2, "collision must not overwrite");
        assert_eq!(m.get(&Value::Float(1.0)), Some(&Value::Int(1)));
        assert_eq!(m.get(&Value::Float(2.0)), Some(&Value::Int(2)));
        assert_eq!(m.get(&Value::Float(3.0)), None);
    }
}
