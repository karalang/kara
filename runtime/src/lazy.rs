//! LazyFrame runtime engine — the codegen twin of the interpreter's
//! phase-11 LazyDataFrame surface (`src/interpreter/method_call_dataframe.rs`).
//!
//! Compiled code builds plans at RUNTIME through the `karac_lazy_*` FFI
//! (plan shape can depend on runtime values and loops, so a static
//! compile-time encoding was rejected). Two ABI facts anchor everything:
//!
//! * **Handles are refcounted, borrow-everywhere.** `LazyExpr` /
//!   `LazyFrame` / `LazyGroupBy` are POD one-field structs at the Kāra
//!   level, so the ownership checker treats them as copyable and a
//!   binding may be used many times (the interpreter clones its `Arc`'d
//!   IR on every builder call). Handles here are `Arc::into_raw`
//!   pointers; every constructor / builder / consumer BORROWS its
//!   arguments (internal `Arc` clone) and returns a fresh +1 handle.
//!   Codegen stores each produced handle in an alloca and releases it
//!   once at the scope where it was produced (the
//!   FreeFileHandle/FreeGpuBuffer cleanup pattern) — no retains, no move
//!   tracking. A raw word-copied binding (`let b = a`) is valid only
//!   while the producing scope lives; escaping one through a block
//!   expression is outside the v1 twin (see the tracker's KNOWN
//!   LIMITATION note).
//! * **`lazy()` deep-copies the source frame** into a Rust-native column
//!   store shared by `Arc` across derived plans. Compiled `DataFrame`
//!   control blocks are single-ownership (no refcount), so borrowing
//!   would dangle if the frame dropped before `collect` — the interpreter
//!   `Arc`-shares, this is the closest safe equivalent (refcounted
//!   sharing of the control blocks themselves is the P2 upgrade).
//!
//! Twin scope: the FULL LazyFrame op surface — `select` / `limit` /
//! `filter` / `sort` / `group_by`+`agg` / `join` / `with_columns` plan
//! ops, the full expression set (col/lit/cmp/and/or/not/arith plus the
//! `desc` sort marker, the `count`/`sum`/`mean`/`min`/`max` aggregates
//! and `alias_`), `explain` (byte-parity with the interpreter, including
//! the constant-folding + CSE passes and the compact JOIN sub-plan
//! rendering), and `collect`. The fold/render/eval logic here is a
//! lockstep port of the interpreter's — the two must stay in sync the
//! same way the CSV splitter twins do.

use std::sync::Arc;

// ── Expression nodes ────────────────────────────────────────────

/// Comparison op codes shared with codegen (`src/codegen/dataframe.rs`):
/// 0=gt 1=ge 2=lt 3=le 4=eq 5=ne.
const CMP_SYMBOLS: [&str; 6] = [">", ">=", "<", "<=", "==", "!="];
/// Arithmetic op codes shared with codegen: 0=add 1=sub 2=mul 3=div.
const ARITH_SYMBOLS: [&str; 4] = ["+", "-", "*", "/"];
/// Aggregate op codes shared with codegen: 0=count 1=sum 2=mean 3=min
/// 4=max — the names are the interpreter's `LazyAggOp::name()` strings
/// (explain + default output names are byte-pinned across backends).
const AGG_NAMES: [&str; 5] = ["count", "sum", "mean", "min", "max"];

#[derive(Debug, PartialEq)]
pub enum ExprNode {
    Col(String),
    LitInt(i64),
    LitFloat(f64),
    LitStr(String),
    LitBool(bool),
    Cmp {
        op: u64,
        lhs: Arc<ExprNode>,
        rhs: Arc<ExprNode>,
    },
    And(Arc<ExprNode>, Arc<ExprNode>),
    Or(Arc<ExprNode>, Arc<ExprNode>),
    Not(Arc<ExprNode>),
    Arith {
        op: u64,
        lhs: Arc<ExprNode>,
        rhs: Arc<ExprNode>,
    },
    /// Descending sort-key marker — only meaningful as a
    /// `LazyFrame.sort` key; an error anywhere else.
    Desc(Arc<ExprNode>),
    /// An aggregate over a group — only meaningful inside
    /// `LazyGroupBy.agg(..)`; an error in filter / sort position.
    Agg {
        op: u64,
        arg: Arc<ExprNode>,
    },
    /// Output-column name override (`.alias_("cnt")`).
    Alias {
        name: String,
        expr: Arc<ExprNode>,
    },
}

impl std::fmt::Display for ExprNode {
    /// Byte-parity with the interpreter's `LazyExprIR` Display — the
    /// explain oracle diffs the two backends.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExprNode::Col(n) => write!(f, "{n}"),
            ExprNode::LitInt(v) => write!(f, "{v}"),
            ExprNode::LitFloat(v) => write!(f, "{v}"),
            ExprNode::LitStr(s) => write!(f, "\"{s}\""),
            ExprNode::LitBool(b) => write!(f, "{b}"),
            ExprNode::Cmp { op, lhs, rhs } => {
                write!(f, "({lhs} {} {rhs})", CMP_SYMBOLS[*op as usize])
            }
            ExprNode::And(a, b) => write!(f, "({a} and {b})"),
            ExprNode::Or(a, b) => write!(f, "({a} or {b})"),
            ExprNode::Not(x) => write!(f, "(not {x})"),
            ExprNode::Arith { op, lhs, rhs } => {
                write!(f, "({lhs} {} {rhs})", ARITH_SYMBOLS[*op as usize])
            }
            ExprNode::Desc(x) => write!(f, "{x} desc"),
            ExprNode::Agg { op, arg } => write!(f, "{}({arg})", AGG_NAMES[*op as usize]),
            ExprNode::Alias { name, expr } => write!(f, "{expr} as {name}"),
        }
    }
}

/// Reconstitute an `Arc` from a handle WITHOUT consuming the caller's
/// count (the borrow side of the ABI — every argument position). The
/// `from_raw`/`into_raw` juggling is contained to the FFI shims here.
unsafe fn expr_borrow(h: *const ExprNode) -> Arc<ExprNode> {
    Arc::increment_strong_count(h);
    Arc::from_raw(h)
}

fn expr_new(n: ExprNode) -> *const ExprNode {
    Arc::into_raw(Arc::new(n))
}

/// Abort with a message — the twin's runtime-error posture. Compiled
/// `collect()` has no `Result` channel (the stub returns a bare
/// `DataFrame`), and the interpreter's equivalent is a recorded runtime
/// error that terminates the program; printing and aborting is the
/// compiled analogue (same convention as index-out-of-bounds).
fn lazy_abort(msg: &str) -> ! {
    // Compiled `println` goes through libc stdio (printf/fwrite), which
    // buffers when stdout is not a TTY — and `abort()` skips the exit
    // flush. Flush ALL libc output streams first (C99 `fflush(NULL)`)
    // so pre-error output — e.g. an INVALID PLAN `explain` printed
    // before the failing `collect` — survives, matching the
    // interpreter's output ordering.
    extern "C" {
        fn fflush(stream: *mut core::ffi::c_void) -> i32;
    }
    unsafe {
        fflush(core::ptr::null_mut());
    }
    eprintln!("runtime error: {msg}");
    std::process::abort();
}

unsafe fn str_from_raw(ptr: *const u8, len: usize) -> String {
    if ptr.is_null() {
        return String::new();
    }
    String::from_utf8_lossy(std::slice::from_raw_parts(ptr, len)).into_owned()
}

#[no_mangle]
/// # Safety
/// `ptr`/`len` must describe a readable UTF-8ish byte range.
pub unsafe extern "C" fn karac_lazy_expr_col(ptr: *const u8, len: usize) -> *const ExprNode {
    expr_new(ExprNode::Col(str_from_raw(ptr, len)))
}

#[no_mangle]
pub extern "C" fn karac_lazy_expr_lit_int(v: i64) -> *const ExprNode {
    expr_new(ExprNode::LitInt(v))
}

#[no_mangle]
pub extern "C" fn karac_lazy_expr_lit_float(v: f64) -> *const ExprNode {
    expr_new(ExprNode::LitFloat(v))
}

#[no_mangle]
/// # Safety
/// `ptr`/`len` must describe a readable byte range.
pub unsafe extern "C" fn karac_lazy_expr_lit_str(ptr: *const u8, len: usize) -> *const ExprNode {
    expr_new(ExprNode::LitStr(str_from_raw(ptr, len)))
}

#[no_mangle]
pub extern "C" fn karac_lazy_expr_lit_bool(v: u8) -> *const ExprNode {
    expr_new(ExprNode::LitBool(v != 0))
}

#[no_mangle]
/// # Safety
/// `lhs`/`rhs` must be live expr handles (borrowed).
pub unsafe extern "C" fn karac_lazy_expr_cmp(
    op: u64,
    lhs: *const ExprNode,
    rhs: *const ExprNode,
) -> *const ExprNode {
    if op >= 6 {
        lazy_abort("LazyExpr: invalid comparison op code (compiler bug)");
    }
    expr_new(ExprNode::Cmp {
        op,
        lhs: expr_borrow(lhs),
        rhs: expr_borrow(rhs),
    })
}

#[no_mangle]
/// # Safety
/// `a`/`b` must be live expr handles (borrowed). `op`: 0=and 1=or.
pub unsafe extern "C" fn karac_lazy_expr_bool(
    op: u64,
    a: *const ExprNode,
    b: *const ExprNode,
) -> *const ExprNode {
    let (a, b) = (expr_borrow(a), expr_borrow(b));
    expr_new(match op {
        0 => ExprNode::And(a, b),
        1 => ExprNode::Or(a, b),
        _ => lazy_abort("LazyExpr: invalid boolean op code (compiler bug)"),
    })
}

#[no_mangle]
/// # Safety
/// `x` must be a live expr handle (borrowed).
pub unsafe extern "C" fn karac_lazy_expr_not(x: *const ExprNode) -> *const ExprNode {
    expr_new(ExprNode::Not(expr_borrow(x)))
}

#[no_mangle]
/// Bump an expr handle's count — emitted ONLY before a `return` of a
/// LazyExpr-typed value (the escaping count that outlives the producing
/// scope's release; the caller registers the matching release).
///
/// # Safety
/// `x` must be a live expr handle.
pub unsafe extern "C" fn karac_lazy_expr_retain(x: *const ExprNode) {
    Arc::increment_strong_count(x);
}

#[no_mangle]
/// # Safety
/// `lhs`/`rhs` must be live expr handles (borrowed).
pub unsafe extern "C" fn karac_lazy_expr_arith(
    op: u64,
    lhs: *const ExprNode,
    rhs: *const ExprNode,
) -> *const ExprNode {
    if op >= 4 {
        lazy_abort("LazyExpr: invalid arithmetic op code (compiler bug)");
    }
    expr_new(ExprNode::Arith {
        op,
        lhs: expr_borrow(lhs),
        rhs: expr_borrow(rhs),
    })
}

#[no_mangle]
/// Descending sort-key marker builder (`col("cnt").desc()`).
///
/// # Safety
/// `x` must be a live expr handle (borrowed).
pub unsafe extern "C" fn karac_lazy_expr_desc(x: *const ExprNode) -> *const ExprNode {
    expr_new(ExprNode::Desc(expr_borrow(x)))
}

#[no_mangle]
/// Aggregate builder — `op`: 0=count 1=sum 2=mean 3=min 4=max.
///
/// # Safety
/// `arg` must be a live expr handle (borrowed).
pub unsafe extern "C" fn karac_lazy_expr_agg(op: i64, arg: *const ExprNode) -> *const ExprNode {
    if !(0..=4).contains(&op) {
        lazy_abort("LazyExpr: invalid aggregate op code (compiler bug)");
    }
    expr_new(ExprNode::Agg {
        op: op as u64,
        arg: expr_borrow(arg),
    })
}

#[no_mangle]
/// Output-name override builder (`.alias_("cnt")`).
///
/// # Safety
/// `expr` must be a live expr handle (borrowed); `name_ptr`/`name_len`
/// a readable byte range.
pub unsafe extern "C" fn karac_lazy_expr_alias(
    expr: *const ExprNode,
    name_ptr: *const u8,
    name_len: usize,
) -> *const ExprNode {
    expr_new(ExprNode::Alias {
        name: str_from_raw(name_ptr, name_len),
        expr: expr_borrow(expr),
    })
}

#[no_mangle]
/// # Safety
/// `x` must be a live expr handle; one count is released.
pub unsafe extern "C" fn karac_lazy_expr_release(x: *const ExprNode) {
    drop(Arc::from_raw(x));
}

// ── Frame + plan ────────────────────────────────────────────────

/// One deep-copied column in Rust-native form, widened for evaluation
/// (narrow ints → i64, f32 → f64 — exact roundtrips). The original
/// `(kind, elem_size)` ride along on `FrameCol` so `collect` re-emits
/// the source dtype bit-for-bit. Columns DERIVED mid-plan (group keys /
/// aggregates / `with_columns` results) carry the derived dtype:
/// i64→(1,8), f64→(3,8), bool→(0,1), String→(4,24) — `count` is i64,
/// `mean` is f64, `min`/`max`/`sum` keep the input column's runtime
/// type, group keys keep the source column's dtype.
#[derive(Debug, Clone)]
pub enum ColData {
    I64(Vec<i64>),
    F64(Vec<f64>),
    Bool(Vec<bool>),
    Str(Vec<String>),
}

#[derive(Debug, Clone)]
pub struct FrameCol {
    pub name: String,
    pub data: ColData,
    /// Validity per row — `false` = NULL slot.
    pub valid: Vec<bool>,
    /// The source column's element-class tag (0=bool 1=signed 2=unsigned
    /// 3=float 4=String — the control-block contract in file.rs).
    pub kind: i64,
    /// The source column's cell width in bytes.
    pub elem_size: i64,
}

/// The deep-copied source frame, `Arc`-shared by every plan derived
/// from one `lazy()` call. Row count is derived from the columns
/// (`cols_height`), exactly like the interpreter's `lazy_cols_height`.
#[derive(Debug)]
pub struct Frame {
    pub cols: Vec<FrameCol>,
}

/// One recorded plan step — the twin of the interpreter's `LazyOp`.
#[derive(Debug, Clone)]
pub enum PlanOp {
    Select(Vec<String>),
    Limit(i64),
    Filter(Arc<ExprNode>),
    /// Stable multi-key sort. Each key is an expression, optionally
    /// `Desc`-wrapped for descending; NULL keys sort last.
    Sort(Vec<Arc<ExprNode>>),
    /// Group-by + aggregate: first-occurrence group order; output
    /// schema = key columns then one column per aggregate.
    GroupBy {
        keys: Vec<Arc<ExprNode>>,
        aggs: Vec<Arc<ExprNode>>,
    },
    /// Inner join — the right side is a whole nested sub-plan (the plan
    /// tree's second child; the left spine stays the linear op list).
    Join {
        right: Arc<LazyPlan>,
        on: Vec<String>,
    },
    /// Computed / renamed columns. Each entry needs an output name (a
    /// bare `col(..)` keeps its own; anything else must be
    /// `.alias_(..)`ed); results REPLACE a same-named column or APPEND.
    /// Entries all see the step's INPUT frame, never each other.
    WithColumns(Vec<Arc<ExprNode>>),
}

/// A lazy plan: `Arc`-shared frame + immutable op list. Builders clone
/// the list (value semantics — the receiver plan stays usable, exactly
/// like the interpreter's `Value::LazyFrame`).
#[derive(Debug)]
pub struct LazyPlan {
    pub frame: Arc<Frame>,
    pub ops: Vec<PlanOp>,
}

/// The intermediate between `group_by(keys)` and `agg(aggs)` — carries
/// the pending keys (the interpreter's `Value::LazyGroupBy`). Its own
/// handle type with its own retain/release pair.
#[derive(Debug)]
pub struct LazyGb {
    pub plan: Arc<LazyPlan>,
    pub keys: Vec<Arc<ExprNode>>,
}

unsafe fn plan_borrow(h: *const LazyPlan) -> Arc<LazyPlan> {
    Arc::increment_strong_count(h);
    Arc::from_raw(h)
}

fn plan_new(p: LazyPlan) -> *const LazyPlan {
    Arc::into_raw(Arc::new(p))
}

unsafe fn gb_borrow(h: *const LazyGb) -> Arc<LazyGb> {
    Arc::increment_strong_count(h);
    Arc::from_raw(h)
}

/// Clone an op list (ExprNode / sub-plan arcs are shared, not
/// deep-copied).
fn clone_ops(ops: &[PlanOp]) -> Vec<PlanOp> {
    ops.to_vec()
}

/// Read `count` packed LazyExpr handle words off a `Vec[LazyExpr]` DATA
/// pointer (each element is the Kāra `{ handle_id: i64 }` POD struct —
/// an 8-byte handle word), borrowing every handle.
unsafe fn exprs_from_raw(data: *const u8, count: usize) -> Vec<Arc<ExprNode>> {
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let word = *(data.add(i * 8) as *const i64);
        out.push(expr_borrow(word as *const ExprNode));
    }
    out
}

/// Read `count` Strings off a `Vec[String]` DATA pointer (`count`
/// contiguous 24-byte Kāra String aggregates `{ptr+0, len+8, cap+16}`).
unsafe fn strings_from_raw(elems: *const u8, count: usize) -> Vec<String> {
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let cell = elems.add(i * 24);
        let sptr = *(cell as *const *const u8);
        let slen = *(cell.add(8) as *const i64) as usize;
        out.push(str_from_raw(sptr, slen));
    }
    out
}

#[no_mangle]
/// Bump a plan handle's count — the return-position sibling of
/// `karac_lazy_expr_retain`.
///
/// # Safety
/// `plan` must be a live plan handle.
pub unsafe extern "C" fn karac_lazy_retain(plan: *const LazyPlan) {
    Arc::increment_strong_count(plan);
}

#[no_mangle]
/// # Safety
/// `plan` must be a live plan handle; one count is released.
pub unsafe extern "C" fn karac_lazy_release(plan: *const LazyPlan) {
    drop(Arc::from_raw(plan));
}

#[no_mangle]
/// Bump a LazyGroupBy handle's count — return-position sibling of
/// `karac_lazy_retain` for the group-by intermediate.
///
/// # Safety
/// `gb` must be a live LazyGb handle.
pub unsafe extern "C" fn karac_lazy_gb_retain(gb: *const LazyGb) {
    Arc::increment_strong_count(gb);
}

#[no_mangle]
/// # Safety
/// `gb` must be a live LazyGb handle; one count is released.
pub unsafe extern "C" fn karac_lazy_gb_release(gb: *const LazyGb) {
    drop(Arc::from_raw(gb));
}

// ── Plan builders ───────────────────────────────────────────────

#[no_mangle]
/// # Safety
/// `plan` is borrowed. `elems`/`count` are the `Vec[String]` control
/// block's data pointer + len: `count` contiguous 24-byte Kāra String
/// aggregates `{ptr+0, len+8, cap+16}` (the layout `Vec[String]`
/// columns and `compile_dataframe_select` already walk).
pub unsafe extern "C" fn karac_lazy_select(
    plan: *const LazyPlan,
    elems: *const u8,
    count: usize,
) -> *const LazyPlan {
    let p = plan_borrow(plan);
    let cols = strings_from_raw(elems, count);
    let mut ops = clone_ops(&p.ops);
    ops.push(PlanOp::Select(cols));
    plan_new(LazyPlan {
        frame: Arc::clone(&p.frame),
        ops,
    })
}

#[no_mangle]
/// # Safety
/// `plan` is borrowed.
pub unsafe extern "C" fn karac_lazy_limit(plan: *const LazyPlan, n: i64) -> *const LazyPlan {
    let p = plan_borrow(plan);
    let mut ops = clone_ops(&p.ops);
    ops.push(PlanOp::Limit(n.max(0)));
    plan_new(LazyPlan {
        frame: Arc::clone(&p.frame),
        ops,
    })
}

#[no_mangle]
/// # Safety
/// `plan` and `pred` are borrowed.
pub unsafe extern "C" fn karac_lazy_filter(
    plan: *const LazyPlan,
    pred: *const ExprNode,
) -> *const LazyPlan {
    let p = plan_borrow(plan);
    let e = expr_borrow(pred);
    let mut ops = clone_ops(&p.ops);
    ops.push(PlanOp::Filter(e));
    plan_new(LazyPlan {
        frame: Arc::clone(&p.frame),
        ops,
    })
}

#[no_mangle]
/// # Safety
/// `plan` is borrowed. `keys`/`count` are a `Vec[LazyExpr]` DATA
/// pointer + len (packed 8-byte handle words — see `exprs_from_raw`).
pub unsafe extern "C" fn karac_lazy_sort(
    plan: *const LazyPlan,
    keys: *const u8,
    count: usize,
) -> *const LazyPlan {
    if count == 0 {
        lazy_abort("LazyFrame.sort needs at least one key");
    }
    let p = plan_borrow(plan);
    let keys = exprs_from_raw(keys, count);
    let mut ops = clone_ops(&p.ops);
    ops.push(PlanOp::Sort(keys));
    plan_new(LazyPlan {
        frame: Arc::clone(&p.frame),
        ops,
    })
}

#[no_mangle]
/// Start a grouping: returns a NEW `LazyGb` handle carrying the plan +
/// pending keys (released via `karac_lazy_gb_release`).
///
/// # Safety
/// `plan` is borrowed. `keys`/`count` are a `Vec[LazyExpr]` DATA
/// pointer + len.
pub unsafe extern "C" fn karac_lazy_group_by(
    plan: *const LazyPlan,
    keys: *const u8,
    count: usize,
) -> *const LazyGb {
    if count == 0 {
        lazy_abort("LazyFrame.group_by needs at least one key");
    }
    let p = plan_borrow(plan);
    let keys = exprs_from_raw(keys, count);
    Arc::into_raw(Arc::new(LazyGb { plan: p, keys }))
}

#[no_mangle]
/// Complete a grouping: a fresh plan with the `GroupBy` step appended.
///
/// # Safety
/// `gb` is borrowed. `aggs`/`count` are a `Vec[LazyExpr]` DATA
/// pointer + len.
pub unsafe extern "C" fn karac_lazy_agg(
    gb: *const LazyGb,
    aggs: *const u8,
    count: usize,
) -> *const LazyPlan {
    if count == 0 {
        lazy_abort("LazyGroupBy.agg needs at least one aggregate");
    }
    let g = gb_borrow(gb);
    let aggs = exprs_from_raw(aggs, count);
    let mut ops = clone_ops(&g.plan.ops);
    ops.push(PlanOp::GroupBy {
        keys: g.keys.clone(),
        aggs,
    });
    plan_new(LazyPlan {
        frame: Arc::clone(&g.plan.frame),
        ops,
    })
}

#[no_mangle]
/// # Safety
/// `plan` and `other` are borrowed plan handles (the right side is
/// stored as a nested sub-plan `Arc`). `on`/`count` are a `Vec[String]`
/// DATA pointer + len.
pub unsafe extern "C" fn karac_lazy_join(
    plan: *const LazyPlan,
    other: *const LazyPlan,
    on: *const u8,
    count: usize,
) -> *const LazyPlan {
    if count == 0 {
        lazy_abort("LazyFrame.join needs at least one key");
    }
    let p = plan_borrow(plan);
    let right = plan_borrow(other);
    let on = strings_from_raw(on, count);
    let mut ops = clone_ops(&p.ops);
    ops.push(PlanOp::Join { right, on });
    plan_new(LazyPlan {
        frame: Arc::clone(&p.frame),
        ops,
    })
}

#[no_mangle]
/// # Safety
/// `plan` is borrowed. `exprs`/`count` are a `Vec[LazyExpr]` DATA
/// pointer + len.
pub unsafe extern "C" fn karac_lazy_with_columns(
    plan: *const LazyPlan,
    exprs: *const u8,
    count: usize,
) -> *const LazyPlan {
    if count == 0 {
        lazy_abort("LazyFrame.with_columns needs at least one entry");
    }
    let p = plan_borrow(plan);
    let exprs = exprs_from_raw(exprs, count);
    let mut ops = clone_ops(&p.ops);
    ops.push(PlanOp::WithColumns(exprs));
    plan_new(LazyPlan {
        frame: Arc::clone(&p.frame),
        ops,
    })
}

// ── Constant folding + CSE (lockstep port of `fold_lazy_expr`) ──

fn flatten_bool_chain(e: Arc<ExprNode>, is_and: bool, out: &mut Vec<Arc<ExprNode>>) {
    match (&*e, is_and) {
        (ExprNode::And(a, b), true) | (ExprNode::Or(a, b), false) => {
            flatten_bool_chain(fold_expr(a), is_and, out);
            flatten_bool_chain(fold_expr(b), is_and, out);
        }
        _ => out.push(e),
    }
}

/// Bottom-up constant folding + CSE — must stay in lockstep with the
/// interpreter's `fold_lazy_expr` (explain output is byte-pinned across
/// backends). Type-mismatched / bool-ordered literal comparisons stay
/// unfolded so collect still errors loudly; NaN comparisons fold false.
/// `Desc`/`Agg`/`Alias` wrappers fold THROUGH (their inner expression
/// simplifies; the wrapper stays).
fn fold_expr(e: &Arc<ExprNode>) -> Arc<ExprNode> {
    match &**e {
        ExprNode::Cmp { op, lhs, rhs } => {
            let l = fold_expr(lhs);
            let r = fold_expr(rhs);
            let ord = match (&*l, &*r) {
                (ExprNode::LitInt(x), ExprNode::LitInt(y)) => Some(x.partial_cmp(y)),
                (ExprNode::LitFloat(x), ExprNode::LitFloat(y)) => Some(x.partial_cmp(y)),
                (ExprNode::LitInt(x), ExprNode::LitFloat(y)) => Some((*x as f64).partial_cmp(y)),
                (ExprNode::LitFloat(x), ExprNode::LitInt(y)) => Some(x.partial_cmp(&(*y as f64))),
                (ExprNode::LitStr(x), ExprNode::LitStr(y)) => Some(Some(x.cmp(y))),
                (ExprNode::LitBool(x), ExprNode::LitBool(y)) if *op >= 4 => Some(Some(x.cmp(y))),
                _ => None,
            };
            match ord {
                Some(ord) => Arc::new(ExprNode::LitBool(ord.is_some_and(|o| match op {
                    0 => o.is_gt(),
                    1 => o.is_ge(),
                    2 => o.is_lt(),
                    3 => o.is_le(),
                    4 => o.is_eq(),
                    _ => o.is_ne(),
                }))),
                None => Arc::new(ExprNode::Cmp {
                    op: *op,
                    lhs: l,
                    rhs: r,
                }),
            }
        }
        ExprNode::And(..) | ExprNode::Or(..) => {
            let is_and = matches!(&**e, ExprNode::And(..));
            let mut leaves = Vec::new();
            flatten_bool_chain(Arc::clone(e), is_and, &mut leaves);
            let mut out: Vec<Arc<ExprNode>> = Vec::new();
            for leaf in leaves {
                match &*leaf {
                    ExprNode::LitBool(b) => {
                        if *b == is_and {
                            continue; // neutral element
                        }
                        return Arc::new(ExprNode::LitBool(*b)); // dominator
                    }
                    _ => {
                        if !out.iter().any(|x| **x == *leaf) {
                            out.push(leaf); // CSE: first occurrence wins
                        }
                    }
                }
            }
            let mut it = out.into_iter();
            match it.next() {
                None => Arc::new(ExprNode::LitBool(is_and)),
                Some(first) => it.fold(first, |acc, x| {
                    Arc::new(if is_and {
                        ExprNode::And(acc, x)
                    } else {
                        ExprNode::Or(acc, x)
                    })
                }),
            }
        }
        ExprNode::Arith { op, lhs, rhs } => {
            let l = fold_expr(lhs);
            let r = fold_expr(rhs);
            let folded = match (&*l, &*r) {
                (ExprNode::LitInt(x), ExprNode::LitInt(y)) => match op {
                    0 => x.checked_add(*y).map(ExprNode::LitInt),
                    1 => x.checked_sub(*y).map(ExprNode::LitInt),
                    2 => x.checked_mul(*y).map(ExprNode::LitInt),
                    _ => {
                        if *y == 0 {
                            None // stays unfolded — loud at eval
                        } else {
                            x.checked_div(*y).map(ExprNode::LitInt)
                        }
                    }
                },
                (
                    ExprNode::LitInt(_) | ExprNode::LitFloat(_),
                    ExprNode::LitInt(_) | ExprNode::LitFloat(_),
                ) => {
                    let as_f = |n: &ExprNode| match n {
                        ExprNode::LitInt(v) => *v as f64,
                        ExprNode::LitFloat(v) => *v,
                        _ => unreachable!(),
                    };
                    let (x, y) = (as_f(&l), as_f(&r));
                    Some(ExprNode::LitFloat(match op {
                        0 => x + y,
                        1 => x - y,
                        2 => x * y,
                        _ => x / y,
                    }))
                }
                _ => None,
            };
            match folded {
                Some(n) => Arc::new(n),
                None => Arc::new(ExprNode::Arith {
                    op: *op,
                    lhs: l,
                    rhs: r,
                }),
            }
        }
        ExprNode::Not(x) => {
            let inner = fold_expr(x);
            match &*inner {
                ExprNode::LitBool(b) => Arc::new(ExprNode::LitBool(!b)),
                ExprNode::Not(y) => Arc::clone(y),
                _ => Arc::new(ExprNode::Not(inner)),
            }
        }
        ExprNode::Desc(x) => Arc::new(ExprNode::Desc(fold_expr(x))),
        ExprNode::Agg { op, arg } => Arc::new(ExprNode::Agg {
            op: *op,
            arg: fold_expr(arg),
        }),
        ExprNode::Alias { name, expr } => Arc::new(ExprNode::Alias {
            name: name.clone(),
            expr: fold_expr(expr),
        }),
        _ => Arc::clone(e),
    }
}

fn expr_cols(e: &ExprNode, out: &mut Vec<String>) {
    match e {
        ExprNode::Col(n) => {
            if !out.contains(n) {
                out.push(n.clone());
            }
        }
        ExprNode::Cmp { lhs, rhs, .. } | ExprNode::Arith { lhs, rhs, .. } => {
            expr_cols(lhs, out);
            expr_cols(rhs, out);
        }
        ExprNode::And(a, b) | ExprNode::Or(a, b) => {
            expr_cols(a, out);
            expr_cols(b, out);
        }
        ExprNode::Not(x) | ExprNode::Desc(x) => expr_cols(x, out),
        ExprNode::Agg { arg, .. } => expr_cols(arg, out),
        ExprNode::Alias { expr, .. } => expr_cols(expr, out),
        _ => {}
    }
}

// ── Plan fold + render (lockstep port of `fold_lazy_plan`) ──────

struct OptimizedPlan {
    scan_cols: Option<Vec<String>>,
    steps: Vec<PlanOp>,
    projection: Option<Vec<String>>,
    /// The plan's final schema (what a downstream consumer — e.g. a
    /// JOIN parent — sees). Tracked through selects / group-bys / joins.
    final_schema: Vec<String>,
}

/// The output-column name of a group KEY expression — v1: a bare
/// `col(..)`, optionally `alias`ed.
fn group_key_name(k: &ExprNode) -> Result<String, String> {
    match k {
        ExprNode::Col(n) => Ok(n.clone()),
        ExprNode::Alias { name, expr } => match &**expr {
            ExprNode::Col(_) => Ok(name.clone()),
            _ => Err("LazyFrame.group_by: each key must be a bare col(..) in v1".to_string()),
        },
        _ => Err("LazyFrame.group_by: each key must be a bare col(..) in v1".to_string()),
    }
}

/// The bare column a v1 group KEY reads (for dtype carry-over).
fn group_key_inner_col(k: &ExprNode) -> Option<&str> {
    match k {
        ExprNode::Col(n) => Some(n),
        ExprNode::Alias { expr, .. } => match &**expr {
            ExprNode::Col(n) => Some(n),
            _ => None,
        },
        _ => None,
    }
}

/// The output-column name of an aggregate expression: `alias` wins, else
/// `<col>_<op>` (`score_sum`). Non-aggregate entries are an error.
fn agg_output_name(a: &ExprNode) -> Result<String, String> {
    match a {
        ExprNode::Alias { name, expr } => match &**expr {
            ExprNode::Agg { .. } => Ok(name.clone()),
            _ => Err(
                "LazyGroupBy.agg: each entry must be an aggregate (col(..).count() / \
                 sum / mean / min / max), optionally aliased"
                    .to_string(),
            ),
        },
        ExprNode::Agg { op, arg } => match &**arg {
            ExprNode::Col(c) => Ok(format!("{c}_{}", AGG_NAMES[*op as usize])),
            _ => Err(
                "LazyGroupBy.agg: the aggregate argument must be a bare col(..) in v1".to_string(),
            ),
        },
        _ => Err(
            "LazyGroupBy.agg: each entry must be an aggregate (col(..).count() / sum / \
             mean / min / max), optionally aliased"
                .to_string(),
        ),
    }
}

/// The output name of one `with_columns` entry and the expression that
/// computes it: a bare `col(..)` keeps its own name, anything else must
/// be `.alias_(..)`ed.
fn with_columns_output(e: &ExprNode) -> Result<(String, &ExprNode), String> {
    match e {
        ExprNode::Alias { name, expr } => Ok((name.clone(), expr)),
        ExprNode::Col(n) => Ok((n.clone(), e)),
        _ => Err(
            "LazyFrame.with_columns: each entry needs an output name — a bare col(..) keeps \
             its own, anything computed must be .alias_(..)ed"
                .to_string(),
        ),
    }
}

/// Validate + optimize — the single validation authority for `collect`
/// and `explain`, exactly like the interpreter's `fold_lazy_plan`.
fn fold_plan(frame: &Frame, ops: &[PlanOp]) -> Result<OptimizedPlan, String> {
    let source_order: Vec<String> = frame.cols.iter().map(|c| c.name.clone()).collect();
    let mut visible = source_order.clone();
    let mut projection: Option<Vec<String>> = None;
    let mut needed: Vec<String> = Vec::new();
    let mut steps: Vec<PlanOp> = Vec::new();
    // After a GroupBy the schema is DERIVED — downstream column refs no
    // longer touch the scan, so they stop feeding the scan projection.
    let mut past_group_by = false;
    // Pushdown does not yet cross a JOIN (the P2 optimizer-expansion
    // entry): once one appears the scan reads every column.
    let mut past_join = false;
    for op in ops {
        match op {
            PlanOp::Select(cols) => {
                for c in cols {
                    if !visible.contains(c) {
                        return Err(format!(
                            "LazyFrame.select: no column named '{c}' at this plan step"
                        ));
                    }
                }
                visible = cols.clone();
                projection = Some(cols.clone());
            }
            PlanOp::Limit(n) => match steps.last_mut() {
                Some(PlanOp::Limit(m)) => *m = (*m).min(*n),
                _ => steps.push(PlanOp::Limit(*n)),
            },
            PlanOp::Filter(e) => {
                // Validate against the ORIGINAL expression — folding may
                // elide a branch, but a bad column name in it must stay
                // a loud error.
                let mut cols = Vec::new();
                expr_cols(e, &mut cols);
                for c in &cols {
                    if !visible.contains(c) {
                        return Err(format!(
                            "LazyFrame.filter: no column named '{c}' at this plan step"
                        ));
                    }
                }
                let folded = fold_expr(e);
                let mut fcols = Vec::new();
                expr_cols(&folded, &mut fcols);
                for c in fcols {
                    if !past_group_by && !needed.contains(&c) {
                        needed.push(c);
                    }
                }
                if matches!(&*folded, ExprNode::LitBool(true)) {
                    continue; // constant-true filter drops out
                }
                match steps.last_mut() {
                    Some(PlanOp::Filter(prev)) => {
                        *prev = fold_expr(&Arc::new(ExprNode::And(
                            Arc::clone(prev),
                            Arc::clone(&folded),
                        )));
                    }
                    _ => steps.push(PlanOp::Filter(folded)),
                }
            }
            PlanOp::Sort(keys) => {
                // No fusion: a later sort dominates but the EARLIER sort is
                // the stable tie-break within equal keys, so both must run.
                for k in keys {
                    let mut cols = Vec::new();
                    expr_cols(k, &mut cols);
                    for c in &cols {
                        if !visible.contains(c) {
                            return Err(format!(
                                "LazyFrame.sort: no column named '{c}' at this plan step"
                            ));
                        }
                        if !past_group_by && !needed.contains(c) {
                            needed.push(c.clone());
                        }
                    }
                }
                steps.push(PlanOp::Sort(keys.clone()));
            }
            PlanOp::GroupBy { keys, aggs } => {
                let mut out_schema: Vec<String> = Vec::new();
                for k in keys {
                    let name = group_key_name(k)?;
                    if !visible.contains(&name) {
                        return Err(format!(
                            "LazyFrame.group_by: no column named '{name}' at this plan step"
                        ));
                    }
                    if !past_group_by && !needed.contains(&name) {
                        needed.push(name.clone());
                    }
                    out_schema.push(name);
                }
                for a in aggs {
                    let mut cols = Vec::new();
                    expr_cols(a, &mut cols);
                    for c in &cols {
                        if !visible.contains(c) {
                            return Err(format!(
                                "LazyGroupBy.agg: no column named '{c}' at this plan step"
                            ));
                        }
                        if !past_group_by && !needed.contains(c) {
                            needed.push(c.clone());
                        }
                    }
                    out_schema.push(agg_output_name(a)?);
                }
                visible = out_schema;
                projection = None;
                past_group_by = true;
                steps.push(PlanOp::GroupBy {
                    keys: keys.clone(),
                    aggs: aggs.clone(),
                });
            }
            PlanOp::Join { right, on } => {
                let right_plan = fold_plan(&right.frame, &right.ops)?;
                for k in on {
                    if !visible.contains(k) {
                        return Err(format!(
                            "LazyFrame.join: no column named '{k}' on the LEFT side at \
                             this plan step"
                        ));
                    }
                    if !right_plan.final_schema.contains(k) {
                        return Err(format!(
                            "LazyFrame.join: no column named '{k}' on the RIGHT side"
                        ));
                    }
                }
                // Output schema: left, then right minus keys (collisions
                // take a `_right` suffix). A pending left projection is
                // APPLIED at the join (collect materializes it), so fold
                // narrows to it first — and pushes it as an explicit
                // SELECT step so the rendered pipeline stays honest.
                if let Some(p) = &projection {
                    visible = p.clone();
                    steps.push(PlanOp::Select(p.clone()));
                }
                let mut out_schema = visible.clone();
                for rc in &right_plan.final_schema {
                    if on.contains(rc) {
                        continue;
                    }
                    if out_schema.contains(rc) {
                        out_schema.push(format!("{rc}_right"));
                    } else {
                        out_schema.push(rc.clone());
                    }
                }
                visible = out_schema;
                projection = None;
                past_join = true;
                steps.push(PlanOp::Join {
                    right: Arc::clone(right),
                    on: on.clone(),
                });
            }
            PlanOp::WithColumns(exprs) => {
                // Every entry validates against this step's INPUT schema
                // (the Polars parallel semantics — entries never see
                // each other); duplicate output names in one call are a
                // loud error.
                let mut outs: Vec<String> = Vec::new();
                for e in exprs {
                    let (name, _) = with_columns_output(e)?;
                    let mut cols = Vec::new();
                    expr_cols(e, &mut cols);
                    for c in &cols {
                        if !visible.contains(c) {
                            return Err(format!(
                                "LazyFrame.with_columns: no column named '{c}' at this plan step"
                            ));
                        }
                        if !past_group_by && !needed.contains(c) {
                            needed.push(c.clone());
                        }
                    }
                    if outs.contains(&name) {
                        return Err(format!(
                            "LazyFrame.with_columns: duplicate output name '{name}'"
                        ));
                    }
                    outs.push(name);
                }
                // Flush a pending projection as an explicit SELECT step
                // (same boundary rule as JOIN — lifting a select past
                // this step would reorder it against the computed
                // columns). Its columns MATERIALIZE here, so they all
                // join the scan set (they flow through to the output —
                // no top projection narrows them away anymore).
                if let Some(p) = projection.take() {
                    visible = p.clone();
                    for c in &p {
                        if !past_group_by && !needed.contains(c) {
                            needed.push(c.clone());
                        }
                    }
                    steps.push(PlanOp::Select(p));
                }
                for name in outs {
                    if !visible.contains(&name) {
                        visible.push(name);
                    }
                }
                // Constant folding applies inside each entry.
                steps.push(PlanOp::WithColumns(exprs.iter().map(fold_expr).collect()));
            }
        }
    }
    // Scan projection: union of predicate columns + the final projection,
    // in SOURCE order. `None` when nothing narrows it. Past a GroupBy the
    // projection names DERIVED columns, so only pre-groupby refs count.
    let scan_cols = if past_join {
        None
    } else if past_group_by {
        if needed.is_empty() {
            None
        } else {
            Some(
                source_order
                    .iter()
                    .filter(|n| needed.contains(n))
                    .cloned()
                    .collect(),
            )
        }
    } else if projection.is_none() && needed.is_empty() {
        None
    } else {
        let mut wanted: Vec<String> = Vec::new();
        if let Some(p) = &projection {
            wanted.extend(p.iter().cloned());
        }
        wanted.extend(needed.iter().cloned());
        let has_filters = steps.iter().any(|s| {
            matches!(
                s,
                PlanOp::Filter(_)
                    | PlanOp::Sort(_)
                    | PlanOp::GroupBy { .. }
                    | PlanOp::Join { .. }
                    | PlanOp::WithColumns(_)
            )
        });
        if has_filters {
            Some(
                source_order
                    .iter()
                    .filter(|n| wanted.contains(n))
                    .cloned()
                    .collect(),
            )
        } else {
            projection.clone()
        }
    };
    let final_schema = match &projection {
        Some(p) => p.clone(),
        None => visible.clone(),
    };
    Ok(OptimizedPlan {
        scan_cols,
        steps,
        projection,
        final_schema,
    })
}

fn join_exprs(es: &[Arc<ExprNode>]) -> String {
    es.iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// One logical-plan step line — shared by `render_explain` and the
/// compact JOIN sub-plan rendering.
fn render_logical_op(op: &PlanOp) -> String {
    match op {
        PlanOp::Select(cols) => format!("SELECT [{}]", cols.join(", ")),
        PlanOp::Limit(n) => format!("LIMIT {n}"),
        PlanOp::Filter(e) => format!("FILTER {e}"),
        PlanOp::Sort(keys) => format!("SORT [{}]", join_exprs(keys)),
        PlanOp::GroupBy { keys, aggs } => {
            format!("GROUP BY [{}] AGG [{}]", join_exprs(keys), join_exprs(aggs))
        }
        PlanOp::Join { right, on } => {
            format!(
                "JOIN on=[{}] right=({})",
                on.join(", "),
                logical_compact(right)
            )
        }
        PlanOp::WithColumns(exprs) => format!("WITH [{}]", join_exprs(exprs)),
    }
}

/// The right sub-plan's LOGICAL rendering flattened to one line
/// (outermost step first, " <- " separated) — the v1 JOIN rendering
/// (a real two-child tree layout is the P2 explain expansion).
fn logical_compact(plan: &LazyPlan) -> String {
    let src_names: Vec<&str> = plan.frame.cols.iter().map(|c| c.name.as_str()).collect();
    let mut lines: Vec<String> = vec![format!("SCAN [{}]", src_names.join(", "))];
    for op in &plan.ops {
        lines.push(render_logical_op(op));
    }
    lines.reverse();
    lines.join(" <- ")
}

/// The right sub-plan's OPTIMIZED rendering flattened to one line —
/// compact twin of `render_optimized` for the JOIN label. An invalid
/// sub-plan renders its message (the parent fold surfaces the error
/// before collect runs).
fn optimized_compact(plan: &LazyPlan) -> String {
    match fold_plan(&plan.frame, &plan.ops) {
        Ok(p) => render_optimized(&p)
            .lines()
            .map(str::trim)
            .collect::<Vec<_>>()
            .join(" <- "),
        Err(msg) => format!("INVALID: {msg}"),
    }
}

fn render_optimized(plan: &OptimizedPlan) -> String {
    let has_filters = plan.steps.iter().any(|s| {
        matches!(
            s,
            PlanOp::Filter(_) | PlanOp::Sort(_) | PlanOp::GroupBy { .. } | PlanOp::Join { .. }
        )
    });
    let mut scan = match &plan.scan_cols {
        Some(cols) => format!("SCAN cols=[{}]", cols.join(", ")),
        None => "SCAN cols=[*]".to_string(),
    };
    let mut steps: Vec<&PlanOp> = plan.steps.iter().collect();
    if let Some(PlanOp::Limit(n)) = steps.first() {
        scan = format!("{scan} limit={n}");
        steps.remove(0);
    }
    let mut lines: Vec<String> = vec![scan];
    for step in steps {
        lines.push(match step {
            PlanOp::Limit(n) => format!("LIMIT {n}"),
            PlanOp::Filter(e) => format!("FILTER {e}"),
            PlanOp::Sort(keys) => format!("SORT [{}]", join_exprs(keys)),
            PlanOp::GroupBy { keys, aggs } => {
                format!("GROUP BY [{}] AGG [{}]", join_exprs(keys), join_exprs(aggs))
            }
            PlanOp::Join { right, on } => format!(
                "JOIN on=[{}] right=({})",
                on.join(", "),
                optimized_compact(right)
            ),
            PlanOp::WithColumns(exprs) => format!("WITH [{}]", join_exprs(exprs)),
            // Only pushed by the fold at a JOIN/WITH boundary (a pending
            // left projection the step consumes); ordinary selects live
            // in `projection`, not in `steps`.
            PlanOp::Select(cols) => format!("SELECT [{}]", cols.join(", ")),
        });
    }
    if has_filters {
        if let Some(p) = &plan.projection {
            lines.push(format!("SELECT [{}]", p.join(", ")));
        }
    }
    let mut out = String::new();
    for (i, line) in lines.iter().rev().enumerate() {
        out.push_str(&"  ".repeat(i));
        out.push_str(line);
        out.push('\n');
    }
    out.pop();
    out
}

fn render_explain(plan: &LazyPlan) -> String {
    let src_names: Vec<&str> = plan.frame.cols.iter().map(|c| c.name.as_str()).collect();
    let mut lines: Vec<String> = vec![format!("SCAN [{}]", src_names.join(", "))];
    for op in &plan.ops {
        lines.push(render_logical_op(op));
    }
    let mut logical = String::new();
    for (i, line) in lines.iter().rev().enumerate() {
        logical.push_str(&"  ".repeat(i));
        logical.push_str(line);
        logical.push('\n');
    }
    let optimized = match fold_plan(&plan.frame, &plan.ops) {
        Ok(p) => render_optimized(&p),
        Err(msg) => format!("INVALID PLAN: {msg}"),
    };
    format!("== logical plan ==\n{logical}== optimized ==\n{optimized}")
}

// ── Row evaluation (lockstep port of `eval_lazy_pred` / `_scalar`) ──

enum Scalar {
    I(i64),
    F(f64),
    S(String),
    B(bool),
}

fn eval_scalar(e: &ExprNode, cols: &[FrameCol], row: usize) -> Result<Option<Scalar>, String> {
    Ok(match e {
        ExprNode::Col(name) => {
            let Some(col) = cols.iter().find(|c| &c.name == name) else {
                return Err(format!("LazyFrame.filter: no column named '{name}'"));
            };
            if !col.valid.get(row).copied().unwrap_or(false) {
                return Ok(None);
            }
            Some(match &col.data {
                ColData::I64(v) => Scalar::I(v[row]),
                ColData::F64(v) => Scalar::F(v[row]),
                ColData::Bool(v) => Scalar::B(v[row]),
                ColData::Str(v) => Scalar::S(v[row].clone()),
            })
        }
        ExprNode::LitInt(v) => Some(Scalar::I(*v)),
        ExprNode::LitFloat(v) => Some(Scalar::F(*v)),
        ExprNode::LitStr(s) => Some(Scalar::S(s.clone())),
        ExprNode::LitBool(b) => Some(Scalar::B(*b)),
        ExprNode::Desc(_) => {
            return Err("LazyExpr.desc() is only meaningful as a LazyFrame.sort key".to_string())
        }
        ExprNode::Agg { op, .. } => {
            return Err(format!(
                "LazyExpr.{}() is only meaningful inside LazyGroupBy.agg(..)",
                AGG_NAMES[*op as usize]
            ))
        }
        ExprNode::Alias { .. } => {
            return Err(
                "LazyExpr.alias() is only meaningful inside LazyGroupBy.agg(..)".to_string(),
            )
        }
        ExprNode::Arith { op, lhs, rhs } => {
            let (Some(a), Some(b)) = (eval_scalar(lhs, cols, row)?, eval_scalar(rhs, cols, row)?)
            else {
                return Ok(None);
            };
            match (&a, &b) {
                (Scalar::I(x), Scalar::I(y)) => {
                    let v = match op {
                        0 => x.checked_add(*y),
                        1 => x.checked_sub(*y),
                        2 => x.checked_mul(*y),
                        _ => {
                            if *y == 0 {
                                return Err(
                                    "LazyExpr: integer division by zero in expression".to_string()
                                );
                            }
                            x.checked_div(*y)
                        }
                    };
                    match v {
                        Some(v) => Some(Scalar::I(v)),
                        None => return Err("LazyExpr: integer overflow in expression".to_string()),
                    }
                }
                (Scalar::I(_) | Scalar::F(_), Scalar::I(_) | Scalar::F(_)) => {
                    let as_f = |s: &Scalar| match s {
                        Scalar::I(v) => *v as f64,
                        Scalar::F(v) => *v,
                        _ => unreachable!(),
                    };
                    let (x, y) = (as_f(&a), as_f(&b));
                    Some(Scalar::F(match op {
                        0 => x + y,
                        1 => x - y,
                        2 => x * y,
                        _ => x / y,
                    }))
                }
                _ => {
                    return Err(
                        "LazyExpr: arithmetic on non-numeric values (String / bool)".to_string()
                    )
                }
            }
        }
        ExprNode::Cmp { .. } | ExprNode::And(..) | ExprNode::Or(..) | ExprNode::Not(..) => {
            Some(Scalar::B(eval_pred(e, cols, row)?))
        }
    })
}

fn eval_pred(e: &ExprNode, cols: &[FrameCol], row: usize) -> Result<bool, String> {
    match e {
        ExprNode::Cmp { op, lhs, rhs } => {
            let (Some(a), Some(b)) = (eval_scalar(lhs, cols, row)?, eval_scalar(rhs, cols, row)?)
            else {
                return Ok(false); // NULL involved → false
            };
            let ord = match (&a, &b) {
                (Scalar::I(x), Scalar::I(y)) => x.partial_cmp(y),
                (Scalar::F(x), Scalar::F(y)) => x.partial_cmp(y),
                (Scalar::I(x), Scalar::F(y)) => (*x as f64).partial_cmp(y),
                (Scalar::F(x), Scalar::I(y)) => x.partial_cmp(&(*y as f64)),
                (Scalar::S(x), Scalar::S(y)) => Some(x.cmp(y)),
                (Scalar::B(x), Scalar::B(y)) => {
                    if *op >= 4 {
                        Some(x.cmp(y))
                    } else {
                        return Err(
                            "LazyFrame.filter: ordered comparison on bool values".to_string()
                        );
                    }
                }
                _ => {
                    return Err(
                        "LazyFrame.filter: cannot compare values of different types \
                         (String vs number / bool vs non-bool)"
                            .to_string(),
                    )
                }
            };
            let Some(ord) = ord else {
                return Ok(false); // NaN comparisons are false
            };
            Ok(match op {
                0 => ord.is_gt(),
                1 => ord.is_ge(),
                2 => ord.is_lt(),
                3 => ord.is_le(),
                4 => ord.is_eq(),
                _ => ord.is_ne(),
            })
        }
        ExprNode::And(a, b) => Ok(eval_pred(a, cols, row)? && eval_pred(b, cols, row)?),
        ExprNode::Or(a, b) => Ok(eval_pred(a, cols, row)? || eval_pred(b, cols, row)?),
        ExprNode::Not(x) => Ok(!eval_pred(x, cols, row)?),
        ExprNode::LitBool(b) => Ok(*b),
        ExprNode::Col(_) => match eval_scalar(e, cols, row)? {
            Some(Scalar::B(b)) => Ok(b),
            Some(_) => Err(
                "LazyFrame.filter: predicate must be boolean (a bare column reference \
                 must name a bool column)"
                    .to_string(),
            ),
            None => Ok(false),
        },
        _ => Err("LazyFrame.filter: predicate must be boolean".to_string()),
    }
}

// ── Sort keys / grouping (lockstep port of `eval_lazy_sort_key` etc.) ──

/// One evaluated sort-key scalar, ordered by `cmp_sort_keys`.
enum SortVal {
    Null,
    Nan,
    I(i64),
    F(f64),
    S(String),
    B(bool),
}

/// Evaluate one sort key for one row: strip a `Desc` wrapper (the
/// comparator re-checks the direction), evaluate the inner expression,
/// and normalize NULL / NaN into ranks (0 = value, 1 = NaN, 2 = NULL —
/// NULLs last, never reversed, so they stay last under `desc` too).
fn eval_sort_key(key: &ExprNode, cols: &[FrameCol], row: usize) -> Result<(u8, SortVal), String> {
    let inner = match key {
        ExprNode::Desc(x) => x.as_ref(),
        other => other,
    };
    Ok(match eval_scalar(inner, cols, row)? {
        None => (2, SortVal::Null),
        Some(Scalar::F(f)) if f.is_nan() => (1, SortVal::Nan),
        Some(Scalar::I(v)) => (0, SortVal::I(v)),
        Some(Scalar::F(v)) => (0, SortVal::F(v)),
        Some(Scalar::S(v)) => (0, SortVal::S(v)),
        Some(Scalar::B(v)) => (0, SortVal::B(v)),
    })
}

fn sort_val_tag(k: &SortVal) -> u8 {
    match k {
        SortVal::B(_) => 0,
        SortVal::I(_) => 1,
        SortVal::F(_) => 2,
        SortVal::S(_) => 3,
        SortVal::Nan => 4,
        SortVal::Null => 5,
    }
}

/// Multi-key comparator: keys in order; per key, rank first (values <
/// NaN < NULL — never reversed), then the value comparison, reversed for
/// a `Desc`-wrapped key. Mixed value types within one key can only arise
/// from computed expressions over mixed columns — ordered by type tag
/// (deterministic; homogeneous columns never hit it).
fn cmp_sort_keys(
    keys: &[Arc<ExprNode>],
    a: &[(u8, SortVal)],
    b: &[(u8, SortVal)],
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    for (i, key) in keys.iter().enumerate() {
        let desc = matches!(&**key, ExprNode::Desc(_));
        let (ra, ka) = &a[i];
        let (rb, kb) = &b[i];
        let ord = match ra.cmp(rb) {
            Ordering::Equal if *ra == 0 => {
                let v = match (ka, kb) {
                    (SortVal::I(x), SortVal::I(y)) => x.cmp(y),
                    (SortVal::F(x), SortVal::F(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
                    (SortVal::I(x), SortVal::F(y)) => {
                        (*x as f64).partial_cmp(y).unwrap_or(Ordering::Equal)
                    }
                    (SortVal::F(x), SortVal::I(y)) => {
                        x.partial_cmp(&(*y as f64)).unwrap_or(Ordering::Equal)
                    }
                    (SortVal::S(x), SortVal::S(y)) => x.cmp(y),
                    (SortVal::B(x), SortVal::B(y)) => x.cmp(y),
                    _ => sort_val_tag(ka).cmp(&sort_val_tag(kb)),
                };
                if desc {
                    v.reverse()
                } else {
                    v
                }
            }
            other => other, // rank order never reverses (NULLs stay last)
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

/// Key-tuple equality on the (rank, scalar) normalization: NULLs equal
/// NULLs, NaNs equal NaNs (grouping semantics, unlike IEEE comparison).
fn sort_keys_equal(a: &[(u8, SortVal)], b: &[(u8, SortVal)]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b.iter()).all(|((ra, ka), (rb, kb))| {
            ra == rb
                && match (ka, kb) {
                    (SortVal::I(x), SortVal::I(y)) => x == y,
                    (SortVal::F(x), SortVal::F(y)) => x == y,
                    (SortVal::I(x), SortVal::F(y)) => (*x as f64) == *y,
                    (SortVal::F(x), SortVal::I(y)) => *x == (*y as f64),
                    (SortVal::S(x), SortVal::S(y)) => x == y,
                    (SortVal::B(x), SortVal::B(y)) => x == y,
                    (SortVal::Nan, SortVal::Nan) => true,
                    (SortVal::Null, SortVal::Null) => true,
                    _ => false,
                }
        })
}

/// A grouped/representative key scalar back to the cell form
/// (`None` = NULL slot).
fn sort_val_scalar(cell: &(u8, SortVal)) -> Option<Scalar> {
    match cell {
        (2, _) | (_, SortVal::Null) => None,
        (_, SortVal::Nan) => Some(Scalar::F(f64::NAN)),
        (_, SortVal::I(v)) => Some(Scalar::I(*v)),
        (_, SortVal::F(v)) => Some(Scalar::F(*v)),
        (_, SortVal::S(v)) => Some(Scalar::S(v.clone())),
        (_, SortVal::B(v)) => Some(Scalar::B(*v)),
    }
}

// ── Mid-pipeline materialization helpers ────────────────────────

/// The row count of a column list (0 when empty).
fn cols_height(cols: &[FrameCol]) -> usize {
    cols.first().map_or(0, |c| c.valid.len())
}

/// Gather `rows` out of one column into a fresh column (dtype carried).
fn gather_col(col: &FrameCol, rows: &[usize]) -> FrameCol {
    let data = match &col.data {
        ColData::I64(v) => ColData::I64(rows.iter().map(|&i| v[i]).collect()),
        ColData::F64(v) => ColData::F64(rows.iter().map(|&i| v[i]).collect()),
        ColData::Bool(v) => ColData::Bool(rows.iter().map(|&i| v[i]).collect()),
        ColData::Str(v) => ColData::Str(rows.iter().map(|&i| v[i].clone()).collect()),
    };
    FrameCol {
        name: col.name.clone(),
        data,
        valid: rows.iter().map(|&i| col.valid[i]).collect(),
        kind: col.kind,
        elem_size: col.elem_size,
    }
}

/// Gather the surviving rows / projection into output columns — the
/// twin of the interpreter's `materialize_lazy_cols` (clones instead of
/// views when no row ops ran; the compiled store is not `Arc`-shared).
fn materialize_cols(
    cur: &[FrameCol],
    indices: &[usize],
    projection: &Option<Vec<String>>,
    row_ops: bool,
) -> Result<Vec<FrameCol>, String> {
    let names: Vec<String> = match projection {
        Some(cols) => cols.clone(),
        None => cur.iter().map(|c| c.name.clone()).collect(),
    };
    let mut out: Vec<FrameCol> = Vec::with_capacity(names.len());
    for name in names {
        let Some(col) = cur.iter().find(|c| c.name == name) else {
            return Err(format!("LazyFrame.collect: no column named '{name}'"));
        };
        out.push(if row_ops {
            gather_col(col, indices)
        } else {
            col.clone()
        });
    }
    Ok(out)
}

/// Build a DERIVED column from per-group / per-row cells, deriving the
/// dtype from the cell family: String→(4,24), bool→(0,1), f64→(3,8),
/// else i64→(1,8) (an all-NULL column defaults to i64 — every cell's
/// bitmap bit is 0, so the tag is never read).
fn build_frame_col(name: String, cells: Vec<Option<Scalar>>) -> FrameCol {
    let mut has_s = false;
    let mut has_b = false;
    let mut has_f = false;
    for c in cells.iter().flatten() {
        match c {
            Scalar::S(_) => has_s = true,
            Scalar::B(_) => has_b = true,
            Scalar::F(_) => has_f = true,
            Scalar::I(_) => {}
        }
    }
    let valid: Vec<bool> = cells.iter().map(|c| c.is_some()).collect();
    let (data, kind, elem_size) = if has_s {
        (
            ColData::Str(
                cells
                    .iter()
                    .map(|c| match c {
                        Some(Scalar::S(s)) => s.clone(),
                        _ => String::new(),
                    })
                    .collect(),
            ),
            4,
            24,
        )
    } else if has_b {
        (
            ColData::Bool(
                cells
                    .iter()
                    .map(|c| matches!(c, Some(Scalar::B(true))))
                    .collect(),
            ),
            0,
            1,
        )
    } else if has_f {
        (
            ColData::F64(
                cells
                    .iter()
                    .map(|c| match c {
                        Some(Scalar::F(v)) => *v,
                        Some(Scalar::I(v)) => *v as f64,
                        _ => 0.0,
                    })
                    .collect(),
            ),
            3,
            8,
        )
    } else {
        (
            ColData::I64(
                cells
                    .iter()
                    .map(|c| match c {
                        Some(Scalar::I(v)) => *v,
                        _ => 0,
                    })
                    .collect(),
            ),
            1,
            8,
        )
    };
    FrameCol {
        name,
        data,
        valid,
        kind,
        elem_size,
    }
}

// ── GroupBy / aggregate evaluation (port of `eval_lazy_group_by`) ──

/// One aggregate over one group's rows (`None` = NULL result — an
/// all-null group for sum/mean/min/max; count yields `I(0)`).
fn eval_agg(
    op: u64,
    arg: &ExprNode,
    cur: &[FrameCol],
    rows: &[usize],
) -> Result<Option<Scalar>, String> {
    let mut ints: Vec<i64> = Vec::new();
    let mut floats: Vec<f64> = Vec::new();
    let mut strs: Vec<String> = Vec::new();
    let mut count: i64 = 0;
    for &row in rows {
        match eval_scalar(arg, cur, row)? {
            None => {}
            Some(Scalar::I(v)) => {
                count += 1;
                ints.push(v);
            }
            Some(Scalar::F(v)) => {
                count += 1;
                floats.push(v);
            }
            Some(Scalar::S(v)) => {
                count += 1;
                strs.push(v);
            }
            Some(Scalar::B(_)) => {
                count += 1;
            }
        }
    }
    if op == 0 {
        return Ok(Some(Scalar::I(count)));
    }
    if !strs.is_empty() {
        if !ints.is_empty() || !floats.is_empty() {
            return Err("LazyGroupBy.agg: mixed String and numeric values".to_string());
        }
        return match op {
            3 => Ok(strs.into_iter().min().map(Scalar::S)),
            4 => Ok(strs.into_iter().max().map(Scalar::S)),
            _ => Err(format!(
                "LazyGroupBy.agg: {}() needs numeric values",
                AGG_NAMES[op as usize]
            )),
        };
    }
    let all_int = floats.is_empty();
    let mut vals: Vec<f64> = floats;
    vals.extend(ints.iter().map(|&v| v as f64));
    if vals.is_empty() {
        return Ok(None);
    }
    Ok(Some(match op {
        1 => {
            if all_int {
                Scalar::I(ints.iter().sum())
            } else {
                Scalar::F(vals.iter().sum())
            }
        }
        2 => Scalar::F(vals.iter().sum::<f64>() / vals.len() as f64),
        3 => {
            if all_int {
                Scalar::I(*ints.iter().min().unwrap())
            } else {
                Scalar::F(vals.iter().cloned().fold(f64::INFINITY, f64::min))
            }
        }
        _ => {
            if all_int {
                Scalar::I(*ints.iter().max().unwrap())
            } else {
                Scalar::F(vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max))
            }
        }
    }))
}

/// Evaluate a GroupBy step: group the surviving rows by the evaluated
/// key scalars (first-occurrence order; NULL keys group together), then
/// compute each aggregate per group. Returns the materialized output
/// columns: keys first, then one column per aggregate.
fn eval_group_by(
    keys: &[Arc<ExprNode>],
    aggs: &[Arc<ExprNode>],
    cur: &[FrameCol],
    indices: &[usize],
) -> Result<Vec<FrameCol>, String> {
    // 1. Group rows by key tuples (rank+scalar reuses the sort-key
    //    normalization so NULL/NaN keys group deterministically).
    type GroupEntry = (Vec<(u8, SortVal)>, Vec<usize>);
    let mut groups: Vec<GroupEntry> = Vec::new();
    for &row in indices {
        let mut kt = Vec::with_capacity(keys.len());
        for k in keys {
            kt.push(eval_sort_key(k, cur, row)?);
        }
        match groups.iter_mut().find(|(g, _)| sort_keys_equal(g, &kt)) {
            Some((_, rows)) => rows.push(row),
            None => groups.push((kt, vec![row])),
        }
    }
    let n_groups = groups.len();
    let mut out: Vec<FrameCol> = Vec::new();
    // 2. Key columns — the representative (first) row's stored cell.
    //    v1 keys are bare `col(..)` refs, so the SOURCE column's dtype
    //    carries over (collect re-emits it bit-for-bit).
    for (ki, k) in keys.iter().enumerate() {
        let name = group_key_name(k)?;
        let mut cells: Vec<Option<Scalar>> = Vec::with_capacity(n_groups);
        for (kt, _) in &groups {
            cells.push(sort_val_scalar(&kt[ki]));
        }
        let mut kc = build_frame_col(name, cells);
        if let Some(inner) = group_key_inner_col(k) {
            if let Some(src) = cur.iter().find(|c| c.name == inner) {
                kc.kind = src.kind;
                kc.elem_size = src.elem_size;
            }
        }
        out.push(kc);
    }
    // 3. Aggregate columns.
    for a in aggs {
        let name = agg_output_name(a)?;
        let (op, arg) = match &**a {
            ExprNode::Alias { expr, .. } => match &**expr {
                ExprNode::Agg { op, arg } => (*op, arg.as_ref()),
                _ => unreachable!("validated by agg_output_name"),
            },
            ExprNode::Agg { op, arg } => (*op, arg.as_ref()),
            _ => unreachable!("validated by agg_output_name"),
        };
        let mut cells: Vec<Option<Scalar>> = Vec::with_capacity(n_groups);
        for (_, rows) in &groups {
            cells.push(eval_agg(op, arg, cur, rows)?);
        }
        out.push(build_frame_col(name, cells));
    }
    Ok(out)
}

// ── Join evaluation (port of `eval_lazy_join`) ──────────────────

/// Inner join two materialized column sets on equal-named keys. Left
/// row order, then right match order (nested loop — MVP scale,
/// deterministic). NULL keys join nothing. Output: left columns, then
/// right non-key columns (`_right` suffix on collisions).
fn eval_join(
    left: &[FrameCol],
    right: &[FrameCol],
    on: &[String],
) -> Result<Vec<FrameCol>, String> {
    let lh = cols_height(left);
    let rh = cols_height(right);
    // Loud on incompatible key types across the two sides — a
    // String-vs-number key pair would otherwise silently join nothing
    // (the "loud, not empty" rule the filter path already follows). An
    // i64/f64 mix is fine: keys compare numerically, like filter/sort.
    let key_family = |cols: &[FrameCol], h: usize, k: &str| -> Result<Option<u8>, String> {
        for row in 0..h {
            let (rank, sk) = eval_sort_key(&ExprNode::Col(k.to_string()), cols, row)?;
            if rank == 2 {
                continue; // NULL — keep scanning for a typed value
            }
            return Ok(Some(match sk {
                SortVal::S(_) => 1,
                SortVal::B(_) => 2,
                _ => 0, // numeric family: I / F / NaN
            }));
        }
        Ok(None)
    };
    for k in on {
        if let (Some(lf), Some(rf)) = (key_family(left, lh, k)?, key_family(right, rh, k)?) {
            if lf != rf {
                return Err(format!(
                    "LazyFrame.join: key '{k}' has incompatible types on the two sides"
                ));
            }
        }
    }
    // Key tuple per row, `None` when any key slot is NULL.
    let key_tuple = |cols: &[FrameCol], row: usize| -> Result<Option<Vec<(u8, SortVal)>>, String> {
        let mut kt = Vec::with_capacity(on.len());
        for k in on {
            let (rank, sk) = eval_sort_key(&ExprNode::Col(k.clone()), cols, row)?;
            if rank == 2 {
                return Ok(None); // NULL key joins nothing
            }
            kt.push((rank, sk));
        }
        Ok(Some(kt))
    };
    let mut lrows: Vec<usize> = Vec::new();
    let mut rrows: Vec<usize> = Vec::new();
    for lrow in 0..lh {
        let Some(lk) = key_tuple(left, lrow)? else {
            continue;
        };
        for rrow in 0..rh {
            let Some(rk) = key_tuple(right, rrow)? else {
                continue;
            };
            if sort_keys_equal(&lk, &rk) {
                lrows.push(lrow);
                rrows.push(rrow);
            }
        }
    }
    let mut out: Vec<FrameCol> = Vec::new();
    for c in left {
        out.push(gather_col(c, &lrows));
    }
    let left_names: Vec<&String> = left.iter().map(|c| &c.name).collect();
    for c in right {
        if on.contains(&c.name) {
            continue;
        }
        let mut gathered = gather_col(c, &rrows);
        if left_names.contains(&&c.name) {
            gathered.name = format!("{}_right", c.name);
        }
        out.push(gathered);
    }
    Ok(out)
}

// ── Plan evaluation (port of the interpreter's `eval_lazy_plan`) ──

/// Run the ops over a source column set — the recursive evaluation core
/// shared by `collect` and a JOIN parent evaluating its right sub-plan.
/// Row-index pipeline over the ORIGINAL ops in order (the optimizer only
/// feeds explain — same invariant as the interpreter); `GroupBy` /
/// `Join` / `WithColumns` MATERIALIZE and REPLACE the working column set
/// mid-pipeline. Returns the final materialized output columns.
fn eval_ops(source: &[FrameCol], ops: &[PlanOp]) -> Result<Vec<FrameCol>, String> {
    let mut cur: Vec<FrameCol> = source.to_vec();
    let mut height = cols_height(&cur);
    let mut indices: Vec<usize> = (0..height).collect();
    let mut projection: Option<Vec<String>> = None;
    let mut row_ops = false;
    for op in ops {
        match op {
            PlanOp::Select(cols) => projection = Some(cols.clone()),
            PlanOp::Limit(n) => {
                row_ops = true;
                indices.truncate((*n).max(0) as usize);
            }
            PlanOp::Filter(e) => {
                row_ops = true;
                let mut kept = Vec::with_capacity(indices.len());
                for &row in &indices {
                    if eval_pred(e, &cur, row)? {
                        kept.push(row);
                    }
                }
                indices = kept;
            }
            PlanOp::Sort(keys) => {
                row_ops = true;
                let mut keyed: Vec<(usize, Vec<(u8, SortVal)>)> = Vec::with_capacity(indices.len());
                for &row in &indices {
                    let mut kvs = Vec::with_capacity(keys.len());
                    for k in keys {
                        kvs.push(eval_sort_key(k, &cur, row)?);
                    }
                    keyed.push((row, kvs));
                }
                keyed.sort_by(|(_, a), (_, b)| cmp_sort_keys(keys, a, b));
                indices = keyed.into_iter().map(|(row, _)| row).collect();
            }
            PlanOp::GroupBy { keys, aggs } => {
                cur = eval_group_by(keys, aggs, &cur, &indices)?;
                height = cols_height(&cur);
                indices = (0..height).collect();
                row_ops = false;
                projection = None;
            }
            PlanOp::Join { right, on } => {
                // Materialize the LEFT state (applying any pending
                // projection — fold narrows the schema at the join the
                // same way), evaluate the RIGHT sub-plan recursively,
                // then inner-join.
                let left = materialize_cols(&cur, &indices, &projection, true)?;
                let right_cols = eval_ops(&right.frame.cols, &right.ops)?;
                cur = eval_join(&left, &right_cols, on)?;
                height = cols_height(&cur);
                indices = (0..height).collect();
                row_ops = false;
                projection = None;
            }
            PlanOp::WithColumns(exprs) => {
                // Materialize the current state (pending projection
                // applied — fold flushes it the same way), compute every
                // entry against that INPUT frame (the Polars parallel
                // semantics — entries never see each other), then
                // replace-or-append by output name.
                cur = materialize_cols(&cur, &indices, &projection, true)?;
                height = cols_height(&cur);
                indices = (0..height).collect();
                row_ops = false;
                projection = None;
                let mut computed: Vec<FrameCol> = Vec::with_capacity(exprs.len());
                for e in exprs {
                    let (name, inner) = with_columns_output(e)?;
                    let mut cells: Vec<Option<Scalar>> = Vec::with_capacity(height);
                    for row in 0..height {
                        cells.push(eval_scalar(inner, &cur, row)?);
                    }
                    computed.push(build_frame_col(name, cells));
                }
                for col in computed {
                    match cur.iter_mut().find(|c| c.name == col.name) {
                        Some(slot) => *slot = col,
                        None => cur.push(col),
                    }
                }
            }
        }
    }
    materialize_cols(&cur, &indices, &projection, row_ops)
}

// ── FFI boundary: lazy() / explain / collect ────────────────────
//
// The control-block layout contract walked here is the one pinned in
// runtime/src/file.rs (write_csv/read_csv twins): DataFrame ctrl
// {entries+0, n_cols+8, cap+16}; entry stride 40 {name_ptr+0,
// name_len+8, col_ctrl+16, elem_size+24, kind+32}; column ctrl
// {data+0, bitmap+8, len+16, cap+24}; bitmap 1 bit/row little-endian
// within bytes, 1 = valid, null bitmap = all valid; String cell = the
// 24-byte {ptr, len, cap} aggregate, cap == len owned, NULL = zeroed.

#[no_mangle]
/// Deep-copy the source frame into a Rust-native store and start an
/// empty plan. `df_ctrl` is BORROWED (the Kāra `lazy(ref self)`
/// receiver) — the caller's frame stays owned by the caller; derived
/// plans share the copy via `Arc` (single-ownership control blocks
/// cannot be shared safely — the P2 upgrade is refcounting them).
///
/// # Safety
/// `df_ctrl` must be a live DataFrame control block per the layout
/// contract above.
pub unsafe extern "C" fn karac_lazy_new(df_ctrl: *const u8) -> *const LazyPlan {
    let entries = *(df_ctrl as *const *const u8);
    let n_cols = *(df_ctrl.add(8) as *const i64) as usize;
    let mut cols: Vec<FrameCol> = Vec::with_capacity(n_cols);
    for i in 0..n_cols {
        let e = entries.add(i * 40);
        let name_ptr = *(e as *const *const u8);
        let name_len = *(e.add(8) as *const i64) as usize;
        let col_ctrl = *(e.add(16) as *const *const u8);
        let elem_size = *(e.add(24) as *const i64);
        let kind = *(e.add(32) as *const i64);
        let name = str_from_raw(name_ptr, name_len);
        let data_ptr = *(col_ctrl as *const *const u8);
        let bitmap = *(col_ctrl.add(8) as *const *const u8);
        let rows = *(col_ctrl.add(16) as *const i64) as usize;
        let is_valid = |row: usize| -> bool {
            bitmap.is_null() || (*bitmap.add(row / 8) >> (row % 8)) & 1 == 1
        };
        let mut valid = Vec::with_capacity(rows);
        let data = match (kind, elem_size) {
            (0, 1) => {
                let mut v = Vec::with_capacity(rows);
                for r in 0..rows {
                    valid.push(is_valid(r));
                    v.push(*data_ptr.add(r) != 0);
                }
                ColData::Bool(v)
            }
            (1, 1 | 2 | 4 | 8) | (2, 1 | 2 | 4 | 8) => {
                let mut v = Vec::with_capacity(rows);
                for r in 0..rows {
                    valid.push(is_valid(r));
                    let p = data_ptr.add(r * elem_size as usize);
                    let raw: i64 = match (kind, elem_size) {
                        (1, 1) => *(p as *const i8) as i64,
                        (1, 2) => *(p as *const i16) as i64,
                        (1, 4) => *(p as *const i32) as i64,
                        (1, 8) => *(p as *const i64),
                        (2, 1) => *p as i64,
                        (2, 2) => *(p as *const u16) as i64,
                        (2, 4) => *(p as *const u32) as i64,
                        _ => {
                            let u = *(p as *const u64);
                            if u > i64::MAX as u64 {
                                lazy_abort(
                                    "LazyFrame: u64 column value exceeds the i64 evaluation \
                                     range supported by the v1 lazy engine",
                                );
                            }
                            u as i64
                        }
                    };
                    v.push(raw);
                }
                ColData::I64(v)
            }
            (3, 4 | 8) => {
                let mut v = Vec::with_capacity(rows);
                for r in 0..rows {
                    valid.push(is_valid(r));
                    let p = data_ptr.add(r * elem_size as usize);
                    v.push(if elem_size == 4 {
                        *(p as *const f32) as f64
                    } else {
                        *(p as *const f64)
                    });
                }
                ColData::F64(v)
            }
            (4, _) => {
                let mut v = Vec::with_capacity(rows);
                for r in 0..rows {
                    valid.push(is_valid(r));
                    let cell = data_ptr.add(r * 24);
                    let sptr = *(cell as *const *const u8);
                    let slen = *(cell.add(8) as *const i64) as usize;
                    v.push(if sptr.is_null() {
                        String::new()
                    } else {
                        str_from_raw(sptr, slen)
                    });
                }
                ColData::Str(v)
            }
            _ => lazy_abort(
                "LazyFrame: unsupported column dtype for the v1 lazy engine \
                 (supported: bool / signed / unsigned ints, f32/f64, String)",
            ),
        };
        cols.push(FrameCol {
            name,
            data,
            valid,
            kind,
            elem_size,
        });
    }
    plan_new(LazyPlan {
        frame: Arc::new(Frame { cols }),
        ops: Vec::new(),
    })
}

#[no_mangle]
/// Render the plan (the `explain(ref self)` twin — BORROWS the plan).
/// Returns a malloc'd UTF-8 buffer, length via `out_len`; the caller
/// wraps it as an owned Kāra String with cap = max(len, 1) — the regex
/// `replace_all` return convention.
///
/// # Safety
/// `plan` must be a live plan handle; `out_len` writable.
pub unsafe extern "C" fn karac_lazy_explain(plan: *const LazyPlan, out_len: *mut i64) -> *mut u8 {
    let p = plan_borrow(plan);
    let text = render_explain(&p);
    *out_len = text.len() as i64;
    let buf = crate::alloc::karac_alloc_or_panic(text.len().max(1));
    std::ptr::copy_nonoverlapping(text.as_ptr(), buf, text.len());
    buf
}

/// Twin of file.rs `df_alloc_zeroed` — malloc-compatible zeroed block
/// (8-aligned) the caller's ordinary FreeDataFrame path frees.
unsafe fn lz_alloc_zeroed(size: usize) -> *mut u8 {
    if size == 0 {
        return std::ptr::null_mut();
    }
    let layout = std::alloc::Layout::from_size_align(size.max(8), 8).unwrap();
    let p = std::alloc::alloc_zeroed(layout);
    if p.is_null() {
        lazy_abort("LazyFrame: allocation failed");
    }
    p
}

unsafe fn lz_alloc_bytes(bytes: &[u8]) -> *mut u8 {
    if bytes.is_empty() {
        return std::ptr::null_mut();
    }
    let p = lz_alloc_zeroed(bytes.len());
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
    p
}

#[no_mangle]
/// Run the plan and materialize an eager DataFrame control-block graph
/// (the `collect(self)` twin — BORROWS the handle: at the Kāra level the
/// plan is a copyable POD value that stays usable after collect, exactly
/// as in the interpreter; the handle's one release stays with the scope
/// that produced it). The graph is
/// built exactly as `read_csv`'s is: every block malloc-compatible,
/// String cells `cap == len`, NULL slots zeroed data + bitmap 0 — so
/// codegen's ordinary FreeDataFrame cleanup frees it. Errors abort
/// loudly (the stub returns a bare `DataFrame`; the interpreter's
/// equivalent is a program-terminating runtime error).
///
/// # Safety
/// `plan` must be a live plan handle.
pub unsafe extern "C" fn karac_lazy_collect(plan: *const LazyPlan) -> *mut u8 {
    let p = plan_borrow(plan);
    // Validation authority first, exactly like interpreter collect.
    if let Err(msg) = fold_plan(&p.frame, &p.ops) {
        lazy_abort(&msg);
    }
    let out_cols = match eval_ops(&p.frame.cols, &p.ops) {
        Ok(v) => v,
        Err(msg) => lazy_abort(&msg),
    };
    let width = out_cols.len();
    let rows = cols_height(&out_cols);
    let entries = lz_alloc_zeroed(width * 40);
    for (ci, col) in out_cols.iter().enumerate() {
        let data = lz_alloc_zeroed(rows * col.elem_size.max(1) as usize);
        let bitmap = lz_alloc_zeroed(rows.div_ceil(8));
        for r in 0..rows {
            if !col.valid[r] {
                continue; // NULL: zeroed data cell + bitmap bit 0
            }
            *bitmap.add(r / 8) |= 1 << (r % 8);
            let cell = data.add(r * col.elem_size as usize);
            match &col.data {
                ColData::Bool(v) => *cell = v[r] as u8,
                ColData::I64(v) => {
                    let raw = v[r];
                    match (col.kind, col.elem_size) {
                        (1, 1) => *(cell as *mut i8) = raw as i8,
                        (1, 2) => *(cell as *mut i16) = raw as i16,
                        (1, 4) => *(cell as *mut i32) = raw as i32,
                        (1, 8) => *(cell as *mut i64) = raw,
                        (2, 1) => *cell = raw as u8,
                        (2, 2) => *(cell as *mut u16) = raw as u16,
                        (2, 4) => *(cell as *mut u32) = raw as u32,
                        _ => *(cell as *mut u64) = raw as u64,
                    }
                }
                ColData::F64(v) => {
                    if col.elem_size == 4 {
                        *(cell as *mut f32) = v[r] as f32;
                    } else {
                        *(cell as *mut f64) = v[r];
                    }
                }
                ColData::Str(v) => {
                    let s = &v[r];
                    let sptr = lz_alloc_bytes(s.as_bytes());
                    *(cell as *mut *mut u8) = sptr;
                    *(cell.add(8) as *mut i64) = s.len() as i64;
                    *(cell.add(16) as *mut i64) = s.len() as i64; // cap == len → owned
                }
            }
        }
        let col_ctrl = lz_alloc_zeroed(32);
        *(col_ctrl as *mut *mut u8) = data;
        *(col_ctrl.add(8) as *mut *mut u8) = bitmap;
        *(col_ctrl.add(16) as *mut i64) = rows as i64;
        *(col_ctrl.add(24) as *mut i64) = rows as i64;
        let e = entries.add(ci * 40);
        *(e as *mut *mut u8) = lz_alloc_bytes(col.name.as_bytes());
        *(e.add(8) as *mut i64) = col.name.len() as i64;
        *(e.add(16) as *mut *mut u8) = col_ctrl;
        *(e.add(24) as *mut i64) = col.elem_size;
        *(e.add(32) as *mut i64) = col.kind;
    }
    let df_ctrl = lz_alloc_zeroed(24);
    *(df_ctrl as *mut *mut u8) = entries;
    *(df_ctrl.add(8) as *mut i64) = width as i64;
    *(df_ctrl.add(16) as *mut i64) = width as i64;
    df_ctrl
}
