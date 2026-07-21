//! LazyFrame runtime engine — the codegen twin of the interpreter's
//! phase-11 LazyDataFrame surface (`src/interpreter/method_call_dataframe.rs`).
//!
//! Compiled code builds plans at RUNTIME through the `karac_lazy_*` FFI
//! (plan shape can depend on runtime values and loops, so a static
//! compile-time encoding was rejected). Two ABI facts anchor everything:
//!
//! * **Handles are refcounted, borrow-everywhere.** `LazyExpr` /
//!   `LazyFrame` are POD one-field structs at the Kāra level, so the
//!   ownership checker treats them as copyable and a binding may be
//!   used many times (the interpreter clones its `Arc`'d IR on every
//!   builder call). Handles here are `Arc::into_raw` pointers; every
//!   constructor / builder / consumer BORROWS its arguments (internal
//!   `Arc` clone) and returns a fresh +1 handle. Codegen stores each
//!   produced handle in an alloca and releases it once at the scope
//!   where it was produced (the FreeFileHandle/FreeGpuBuffer cleanup
//!   pattern) — no retains, no move tracking. A raw word-copied
//!   binding (`let b = a`) is valid only while the producing scope
//!   lives; escaping one through a block expression is outside the v1
//!   twin (see the tracker's KNOWN LIMITATION note).
//! * **`lazy()` deep-copies the source frame** into a Rust-native column
//!   store shared by `Arc` across derived plans. Compiled `DataFrame`
//!   control blocks are single-ownership (no refcount), so borrowing
//!   would dangle if the frame dropped before `collect` — the interpreter
//!   `Arc`-shares, this is the closest safe equivalent (refcounted
//!   sharing of the control blocks themselves is the P2 upgrade).
//!
//! v1 twin scope: `select` / `limit` / `filter` plan ops, the full filter
//! expression set (col/lit/cmp/and/or/not/arith), `explain` (byte-parity
//! with the interpreter, including the constant-folding + CSE passes),
//! and `collect`. sort / group_by / join / with_columns bail loudly at
//! CODEGEN, so this engine never sees them. The fold/render/eval logic
//! here is a lockstep port of the interpreter's — the two must stay in
//! sync the same way the CSV splitter twins do.

use std::sync::Arc;

// ── Expression nodes ────────────────────────────────────────────

/// Comparison op codes shared with codegen (`src/codegen/dataframe.rs`):
/// 0=gt 1=ge 2=lt 3=le 4=eq 5=ne.
const CMP_SYMBOLS: [&str; 6] = [">", ">=", "<", "<=", "==", "!="];
/// Arithmetic op codes shared with codegen: 0=add 1=sub 2=mul 3=div.
const ARITH_SYMBOLS: [&str; 4] = ["+", "-", "*", "/"];

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
/// # Safety
/// `x` must be a live expr handle; one count is released.
pub unsafe extern "C" fn karac_lazy_expr_release(x: *const ExprNode) {
    drop(Arc::from_raw(x));
}

// ── Frame + plan ────────────────────────────────────────────────

/// One deep-copied column in Rust-native form, widened for evaluation
/// (narrow ints → i64, f32 → f64 — exact roundtrips). The original
/// `(kind, elem_size)` ride along on `FrameCol` so `collect` re-emits
/// the source dtype bit-for-bit.
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
/// from one `lazy()` call.
#[derive(Debug)]
pub struct Frame {
    pub cols: Vec<FrameCol>,
    pub height: usize,
}

/// One recorded plan step — the twin of the interpreter's `LazyOp`
/// (v1: the ops the codegen twin lowers).
#[derive(Debug)]
pub enum PlanOp {
    Select(Vec<String>),
    Limit(i64),
    Filter(Arc<ExprNode>),
}

/// A lazy plan: `Arc`-shared frame + immutable op list. Builders clone
/// the list (value semantics — the receiver plan stays usable, exactly
/// like the interpreter's `Value::LazyFrame`).
#[derive(Debug)]
pub struct LazyPlan {
    pub frame: Arc<Frame>,
    pub ops: Vec<PlanOp>,
}

unsafe fn plan_borrow(h: *const LazyPlan) -> Arc<LazyPlan> {
    Arc::increment_strong_count(h);
    Arc::from_raw(h)
}

fn plan_new(p: LazyPlan) -> *const LazyPlan {
    Arc::into_raw(Arc::new(p))
}

/// Clone an op list (ExprNode arcs are shared, not deep-copied).
fn clone_ops(ops: &[PlanOp]) -> Vec<PlanOp> {
    ops.iter()
        .map(|op| match op {
            PlanOp::Select(c) => PlanOp::Select(c.clone()),
            PlanOp::Limit(n) => PlanOp::Limit(*n),
            PlanOp::Filter(e) => PlanOp::Filter(Arc::clone(e)),
        })
        .collect()
}

#[no_mangle]
/// # Safety
/// `plan` must be a live plan handle; one count is released.
pub unsafe extern "C" fn karac_lazy_release(plan: *const LazyPlan) {
    drop(Arc::from_raw(plan));
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
    let mut cols = Vec::with_capacity(count);
    for i in 0..count {
        let cell = elems.add(i * 24);
        let sptr = *(cell as *const *const u8);
        let slen = *(cell.add(8) as *const i64) as usize;
        cols.push(str_from_raw(sptr, slen));
    }
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
        ExprNode::Not(x) => expr_cols(x, out),
        _ => {}
    }
}

// ── Plan fold + render (lockstep port of `fold_lazy_plan`) ──────

struct OptimizedPlan {
    scan_cols: Option<Vec<String>>,
    steps: Vec<PlanOp>,
    projection: Option<Vec<String>>,
}

/// Validate + optimize — the single validation authority for `collect`
/// and `explain`, exactly like the interpreter's `fold_lazy_plan`
/// (subset: the v1 twin ops).
fn fold_plan(frame: &Frame, ops: &[PlanOp]) -> Result<OptimizedPlan, String> {
    let source_order: Vec<String> = frame.cols.iter().map(|c| c.name.clone()).collect();
    let mut visible = source_order.clone();
    let mut projection: Option<Vec<String>> = None;
    let mut needed: Vec<String> = Vec::new();
    let mut steps: Vec<PlanOp> = Vec::new();
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
                    if !needed.contains(&c) {
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
        }
    }
    let scan_cols = if projection.is_none() && needed.is_empty() {
        None
    } else {
        let mut wanted: Vec<String> = Vec::new();
        if let Some(p) = &projection {
            wanted.extend(p.iter().cloned());
        }
        wanted.extend(needed.iter().cloned());
        let has_filters = steps.iter().any(|s| matches!(s, PlanOp::Filter(_)));
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
    Ok(OptimizedPlan {
        scan_cols,
        steps,
        projection,
    })
}

fn render_optimized(plan: &OptimizedPlan) -> String {
    let has_filters = plan.steps.iter().any(|s| matches!(s, PlanOp::Filter(_)));
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
        lines.push(match op {
            PlanOp::Select(cols) => format!("SELECT [{}]", cols.join(", ")),
            PlanOp::Limit(n) => format!("LIMIT {n}"),
            PlanOp::Filter(e) => format!("FILTER {e}"),
        });
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

fn eval_scalar(e: &ExprNode, frame: &Frame, row: usize) -> Result<Option<Scalar>, String> {
    Ok(match e {
        ExprNode::Col(name) => {
            let Some(col) = frame.cols.iter().find(|c| &c.name == name) else {
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
        ExprNode::Arith { op, lhs, rhs } => {
            let (Some(a), Some(b)) = (eval_scalar(lhs, frame, row)?, eval_scalar(rhs, frame, row)?)
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
            Some(Scalar::B(eval_pred(e, frame, row)?))
        }
    })
}

fn eval_pred(e: &ExprNode, frame: &Frame, row: usize) -> Result<bool, String> {
    match e {
        ExprNode::Cmp { op, lhs, rhs } => {
            let (Some(a), Some(b)) = (eval_scalar(lhs, frame, row)?, eval_scalar(rhs, frame, row)?)
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
        ExprNode::And(a, b) => Ok(eval_pred(a, frame, row)? && eval_pred(b, frame, row)?),
        ExprNode::Or(a, b) => Ok(eval_pred(a, frame, row)? || eval_pred(b, frame, row)?),
        ExprNode::Not(x) => Ok(!eval_pred(x, frame, row)?),
        ExprNode::LitBool(b) => Ok(*b),
        ExprNode::Col(_) => match eval_scalar(e, frame, row)? {
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

/// Run the plan: row-index pipeline over the ORIGINAL ops in order (the
/// optimizer only feeds explain — same invariant as the interpreter).
/// Returns (surviving row indices, output column names).
fn eval_plan(plan: &LazyPlan) -> Result<(Vec<usize>, Vec<String>), String> {
    // Validation authority first, exactly like interpreter collect.
    fold_plan(&plan.frame, &plan.ops)?;
    let mut indices: Vec<usize> = (0..plan.frame.height).collect();
    let mut projection: Option<Vec<String>> = None;
    for op in &plan.ops {
        match op {
            PlanOp::Select(cols) => projection = Some(cols.clone()),
            PlanOp::Limit(n) => indices.truncate((*n).max(0) as usize),
            PlanOp::Filter(e) => {
                let mut kept = Vec::with_capacity(indices.len());
                for &row in &indices {
                    if eval_pred(e, &plan.frame, row)? {
                        kept.push(row);
                    }
                }
                indices = kept;
            }
        }
    }
    let names = match projection {
        Some(cols) => cols,
        None => plan.frame.cols.iter().map(|c| c.name.clone()).collect(),
    };
    Ok((indices, names))
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
    let mut height = 0usize;
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
        if i == 0 {
            height = rows;
        }
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
        frame: Arc::new(Frame { cols, height }),
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
    let (indices, names) = match eval_plan(&p) {
        Ok(v) => v,
        Err(msg) => lazy_abort(&msg),
    };
    let width = names.len();
    let rows = indices.len();
    let entries = lz_alloc_zeroed(width * 40);
    for (ci, name) in names.iter().enumerate() {
        let col = p
            .frame
            .cols
            .iter()
            .find(|c| &c.name == name)
            .unwrap_or_else(|| lazy_abort("LazyFrame.collect: projection names a missing column"));
        let data = lz_alloc_zeroed(rows * col.elem_size.max(1) as usize);
        let bitmap = lz_alloc_zeroed(rows.div_ceil(8));
        for (out_r, &src_r) in indices.iter().enumerate() {
            if !col.valid[src_r] {
                continue; // NULL: zeroed data cell + bitmap bit 0
            }
            *bitmap.add(out_r / 8) |= 1 << (out_r % 8);
            let cell = data.add(out_r * col.elem_size as usize);
            match &col.data {
                ColData::Bool(v) => *cell = v[src_r] as u8,
                ColData::I64(v) => {
                    let raw = v[src_r];
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
                        *(cell as *mut f32) = v[src_r] as f32;
                    } else {
                        *(cell as *mut f64) = v[src_r];
                    }
                }
                ColData::Str(v) => {
                    let s = &v[src_r];
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
        *(e as *mut *mut u8) = lz_alloc_bytes(name.as_bytes());
        *(e.add(8) as *mut i64) = name.len() as i64;
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
