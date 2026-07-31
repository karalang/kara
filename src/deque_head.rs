//! `VecDeque` head-index eligibility — the contained fix for the O(n)
//! `pop_front` (B-2026-07-30-5).
//!
//! # The problem
//!
//! `VecDeque[T]` shares `Vec[T]`'s `{ptr, len, cap}` codegen layout, so there
//! is nowhere to record where the live range starts. `pop_front` therefore
//! emits `memmove(data, data + 1, (len - 1) * sizeof(elem))` on every pop:
//! each pop is O(n), and draining a queue is O(n²). Measured on kata #3629
//! that is 41.6% of the program's instructions.
//!
//! # Why not just add a fourth field
//!
//! With three fields you provably cannot have both ends O(1) — you need two
//! independent indices (`head` and `len`) plus `cap`. But widening the struct
//! is not local: `llvm_ty_is_vec_struct` decides "is this a Vec/String buffer"
//! by LLVM *type identity*, and `Vec`, `VecDeque` and `String` are the same
//! type today. A fourth field either makes 69 generic sites stop recognizing
//! deques (silently skipping their drops — a leak) or, if they keep matching,
//! makes every generic element walk read `data[0..len]` when the live range
//! starts at `head` (a use-after-free for element types with drop glue).
//! Either way it is a whole-backend change, not a fix.
//!
//! # What this does instead
//!
//! Keep the layout. Give the *codegen* a private `head` counter — an `i64`
//! alloca beside the deque's own slot — for deque locals where that is
//! provably invisible to everything else. The struct in memory is untouched,
//! so no generic path can observe the difference; the head lives only in the
//! compiling function's stack frame and dies with it.
//!
//! That is only sound when nothing but the rewritten methods ever looks at the
//! deque, because for an eligible deque `data[0..len]` is *not* the live range
//! — the landed lowering reinterprets `len` as the END INDEX of the live range
//! `data[head..len]` (count = `len - head`), which is what lets `push_back`
//! keep its existing lowering verbatim. Hence the eligibility rules
//! below, which are deliberately strict: a candidate must be
//!
//! 1. a `let mut` local of the function being compiled, initialized in place
//!    by `VecDeque.new()` / `VecDeque.with_capacity(..)` — not a parameter, not
//!    a field, nothing that arrived from elsewhere;
//! 2. of a **POD element type** (integers, floats, `bool`, `char`, or tuples of
//!    those). This is what keeps the scope-exit drop correct without touching
//!    it: a POD deque's cleanup is `free(ptr)` and nothing else, and `ptr` is
//!    still the allocation base. An element type with drop glue would need the
//!    cleanup to walk `data[head..head+len]`, which is exactly the generic-site
//!    surgery this design exists to avoid;
//! 3. used **only** as the receiver of [`HEAD_INDEX_METHODS`]. Any other
//!    mention at all — passing it to a function, returning it, storing it in a
//!    struct, calling `get`/`clear`/`push_front`/`pop_back`, iterating it,
//!    cloning it, printing it — disqualifies the local outright;
//! 4. never mentioned inside a closure body or a `par` block. Those regions
//!    copy the `{ptr, len, cap}` aggregate to another frame or another task,
//!    where the head alloca does not exist and the copy's `data[0..len]` would
//!    be read as the live range.
//!
//! Anything that fails a rule keeps today's memmove lowering exactly. The
//! optimization is opt-in per binding and invisible in the type system, so a
//! wrong answer here costs performance, never correctness — with the single
//! exception that a rule which is too *loose* would be a miscompile, which is
//! why each one rejects rather than proves.
//!
//! This covers the shape that motivated the bug: a BFS worklist
//! (`let mut queue: VecDeque[(i64, i64)]`, `push_back` + `pop_front` + `len`),
//! which is also the overwhelmingly common way a deque is used.

use crate::ast::{Block, Expr, ExprKind, Item, PatternKind, Program, Stmt, TypeKind};
use std::collections::{HashMap, HashSet};

/// The deque methods the head-index rewrite implements. A candidate that
/// receives any *other* method is rejected — `len` and `is_empty` are listed
/// because the rewrite must intercept them too: with `len` reinterpreted as
/// the end index, the live count is `len - head` and empty is `len == head`.
pub const HEAD_INDEX_METHODS: &[&str] = &["push_back", "pop_front", "len", "is_empty"];

/// Element type names that carry no drop glue, so a deque of them cleans up
/// with a bare `free(ptr)` regardless of where the live range starts.
const POD_SCALARS: &[&str] = &[
    "i8", "i16", "i32", "i64", "isize", "u8", "u16", "u32", "u64", "usize", "f32", "f64", "bool",
    "char",
];

/// Per-function sets of deque locals that may use the head-index lowering.
/// Keyed by function name, matching the other codegen side-tables
/// (`rc_fallback_fns`, `elided_bindings`, …).
pub fn eligible_deque_locals(program: &Program) -> HashMap<String, HashSet<String>> {
    let mut out: HashMap<String, HashSet<String>> = HashMap::new();
    // Escape hatch, mirroring `KARAC_RC_ELIDE_REF_PARAMS`. Setting this to `0`
    // restores the memmove lowering everywhere, which is what makes a clean
    // A/B of the optimization possible on one compiler build.
    if std::env::var("KARAC_DEQUE_HEAD_INDEX").is_ok_and(|v| v == "0") {
        return out;
    }
    for item in &program.items {
        let Item::Function(f) = item else { continue };
        let eligible = analyze_fn(&f.body);
        if !eligible.is_empty() {
            out.insert(f.name.clone(), eligible);
        }
    }
    out
}

fn analyze_fn(body: &Block) -> HashSet<String> {
    // Only statements directly in the function body. The head counter is an
    // entry-block alloca zeroed once on entry, so a `let` nested inside a loop
    // — which re-creates the deque every iteration and must re-zero the head —
    // is out of scope. Rejecting it here keeps the init story trivial.
    let mut candidates = HashSet::new();
    for s in &body.stmts {
        collect_candidate_stmt_shallow(s, &mut candidates);
    }
    if candidates.is_empty() {
        return candidates;
    }

    // Every mention of a candidate that is NOT a safe-method receiver
    // disqualifies it. `walk` recurses through the whole body; `isolated` is
    // set once we are under a closure or `par`, where even a safe-looking
    // method call is disqualifying because the aggregate is copied out of this
    // frame.
    let mut rejected = HashSet::new();
    walk_block(body, false, &candidates, &mut rejected);
    candidates.retain(|c| !rejected.contains(c));
    candidates
}

/// Collect into `bad` every name from `cands` mentioned ANYWHERE in `stmt`,
/// including as the receiver of a safe head-index method. Consumed by the
/// codegen materialization gate (B-2026-07-31-35): a top-level statement that
/// auto-par compiles into a `__par_branch_*` / fan-out worker function
/// executes with the memmove lowering there, so any deque it mentions must
/// not be on the head-index path in the sequential lane either — otherwise
/// the two lanes disagree on whether `len` is a count or an end index for
/// the same deque header once `head > 0`. Mentions only: a `let` pattern
/// that (re-)introduces the name inside the statement is not a mention, and
/// is safe — a group-local intro reaches the join with `head == 0`, where
/// the two readings coincide.
pub fn names_mentioned_in_stmt(stmt: &Stmt, cands: &HashSet<String>, bad: &mut HashSet<String>) {
    crate::rc_elide::walk_stmt_children_pub(stmt, &mut |e| walk_expr(e, true, cands, bad));
    if let crate::ast::StmtKind::Assign { target, .. }
    | crate::ast::StmtKind::CompoundAssign { target, .. } = &stmt.kind
    {
        if let ExprKind::Identifier(n) = &target.kind {
            if cands.contains(n) {
                bad.insert(n.clone());
            }
        }
    }
}

// ── Candidate collection ────────────────────────────────────────────

fn collect_candidate_stmt_shallow(stmt: &Stmt, out: &mut HashSet<String>) {
    let crate::ast::StmtKind::Let {
        is_mut: true,
        pattern,
        ty,
        value,
    } = &stmt.kind
    else {
        return;
    };
    let PatternKind::Binding(name) = &pattern.kind else {
        return;
    };
    if is_deque_init(value) && deque_elem_is_pod(ty.as_ref()) {
        out.insert(name.clone());
    }
}

/// `VecDeque.new()` / `VecDeque.with_capacity(n)` — the only initializers that
/// leave the deque empty and owned by this frame, so `head = 0` is correct at
/// the binding site.
fn is_deque_init(value: &Expr) -> bool {
    let ExprKind::Call { callee, .. } = &value.kind else {
        return false;
    };
    let ExprKind::Path { segments, .. } = &callee.kind else {
        return false;
    };
    segments.len() == 2
        && segments[0] == "VecDeque"
        && (segments[1] == "new" || segments[1] == "with_capacity")
}

/// True when the annotation is `VecDeque[T]` with a POD `T`. A missing or
/// non-`VecDeque` annotation rejects: the element type is what makes the
/// untouched scope-exit drop correct, so it has to be known here, not guessed.
fn deque_elem_is_pod(ty: Option<&crate::ast::TypeExpr>) -> bool {
    let Some(ty) = ty else { return false };
    let TypeKind::Path(p) = &ty.kind else {
        return false;
    };
    if p.segments.last().map(|s| s.as_str()) != Some("VecDeque") {
        return false;
    }
    let Some(args) = &p.generic_args else {
        return false;
    };
    matches!(args.as_slice(), [crate::ast::GenericArg::Type(elem)] if type_is_pod(elem))
}

fn type_is_pod(ty: &crate::ast::TypeExpr) -> bool {
    match &ty.kind {
        TypeKind::Path(p) => {
            p.generic_args.as_ref().is_none_or(|a| a.is_empty())
                && p.segments.len() == 1
                && POD_SCALARS.contains(&p.segments[0].as_str())
        }
        TypeKind::Tuple(elems) => elems.iter().all(type_is_pod),
        _ => false,
    }
}

// ── Disqualification walk ───────────────────────────────────────────

fn walk_block(block: &Block, isolated: bool, cands: &HashSet<String>, bad: &mut HashSet<String>) {
    for s in &block.stmts {
        crate::rc_elide::walk_stmt_children_pub(s, &mut |e| walk_expr(e, isolated, cands, bad));
        // A candidate re-bound or assigned through is not the local we proved
        // things about; reject on any assignment whose target names one.
        if let crate::ast::StmtKind::Assign { target, .. }
        | crate::ast::StmtKind::CompoundAssign { target, .. } = &s.kind
        {
            if let ExprKind::Identifier(n) = &target.kind {
                if cands.contains(n) {
                    bad.insert(n.clone());
                }
            }
        }
    }
    if let Some(e) = &block.final_expr {
        walk_expr(e, isolated, cands, bad);
    }
}

fn walk_expr(expr: &Expr, isolated: bool, cands: &HashSet<String>, bad: &mut HashSet<String>) {
    // Entering a closure body or a `par` region: the deque aggregate is copied
    // to another frame or task, where the head alloca does not exist.
    let isolated = isolated || matches!(&expr.kind, ExprKind::Closure { .. } | ExprKind::Par(_));

    if let ExprKind::MethodCall { object, method, .. } = &expr.kind {
        if let ExprKind::Identifier(n) = &object.kind {
            if cands.contains(n) {
                if isolated || !HEAD_INDEX_METHODS.contains(&method.as_str()) {
                    bad.insert(n.clone());
                }
                // The receiver is accounted for; arguments still need walking
                // (`q.push_back(f(q))` must reject via the inner mention).
                if let ExprKind::MethodCall { args, .. } = &expr.kind {
                    for a in args {
                        walk_expr(&a.value, isolated, cands, bad);
                    }
                }
                return;
            }
        }
    }

    // Any other bare mention of a candidate is disqualifying.
    if let ExprKind::Identifier(n) = &expr.kind {
        if cands.contains(n) {
            bad.insert(n.clone());
        }
    }

    for_each_block(&expr.kind, &mut |b| walk_block(b, isolated, cands, bad));
    crate::rc_elide::walk_children_pub(&expr.kind, &mut |sub| walk_expr(sub, isolated, cands, bad));
}

/// Blocks that hang off an expression and are not reached via
/// `walk_children` (which yields sub-*expressions* only).
fn for_each_block(kind: &ExprKind, f: &mut dyn FnMut(&Block)) {
    match kind {
        ExprKind::Block(b) | ExprKind::Par(b) | ExprKind::Loop { body: b, .. } => f(b),
        ExprKind::If {
            then_block,
            else_branch,
            ..
        } => {
            f(then_block);
            if let Some(e) = else_branch {
                f_expr_block(e, f);
            }
        }
        ExprKind::While { body, .. } => f(body),
        ExprKind::For { body, .. } => f(body),
        ExprKind::Closure { body, .. } => f_expr_block(body, f),
        ExprKind::IfLet {
            then_block,
            else_branch,
            ..
        } => {
            f(then_block);
            if let Some(e) = else_branch {
                f_expr_block(e, f);
            }
        }
        ExprKind::Match { arms, .. } => {
            for a in arms {
                f_expr_block(&a.body, f);
            }
        }
        _ => {}
    }
}

fn f_expr_block(e: &Expr, f: &mut dyn FnMut(&Block)) {
    if let ExprKind::Block(b) = &e.kind {
        f(b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eligible(src: &str) -> HashSet<String> {
        let parsed = crate::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "test source must parse: {:?}",
            parsed.errors
        );
        eligible_deque_locals(&parsed.program)
            .remove("f")
            .unwrap_or_default()
    }

    fn wrap(body: &str) -> String {
        format!("fn f() {{\n{body}\n}}\n")
    }

    #[test]
    fn let_nested_in_a_loop_rejected() {
        // The head counter is an entry-block alloca zeroed once; a deque
        // re-created each iteration would need it re-zeroed per iteration.
        let s = wrap(
            r#"
    let mut i = 0i64;
    while i < 3 {
        let mut q: VecDeque[i64] = VecDeque.new();
        q.push_back(i);
        let _ = q.pop_front();
        i = i + 1;
    }
"#,
        );
        assert!(!eligible(&s).contains("q"));
    }

    /// The shape from kata #3629: BFS worklist, POD tuple elements, only
    /// push_back / pop_front / len.
    #[test]
    fn bfs_worklist_is_eligible() {
        let s = wrap(
            r#"
    let mut q: VecDeque[(i64, i64)] = VecDeque.new();
    q.push_back((0, 0));
    loop {
        match q.pop_front() {
            None => { break; },
            Some(node) => {
                let (i, d) = node;
                if i < 10 { q.push_back((i + 1, d + 1)); }
            },
        }
    }
    println(q.len());
"#,
        );
        assert!(eligible(&s).contains("q"));
    }

    #[test]
    fn scalar_element_and_with_capacity_are_eligible() {
        let s = wrap(
            r#"
    let mut q: VecDeque[i64] = VecDeque.with_capacity(16);
    q.push_back(1);
    let _ = q.pop_front();
"#,
        );
        assert!(eligible(&s).contains("q"));
    }

    // ── Rejections. Each of these would be a miscompile if accepted. ──

    #[test]
    fn non_pod_element_rejected() {
        // Drop glue on the element means the scope-exit cleanup must walk the
        // live range, which this design deliberately does not touch.
        let s = wrap(
            r#"
    let mut q: VecDeque[String] = VecDeque.new();
    q.push_back("a");
    let _ = q.pop_front();
"#,
        );
        assert!(!eligible(&s).contains("q"));
    }

    #[test]
    fn unsupported_method_rejected() {
        for m in ["q.clear();", "let _ = q.pop_back();", "q.push_front(1);"] {
            let s = wrap(&format!(
                "let mut q: VecDeque[i64] = VecDeque.new();\nq.push_back(1);\n{m}"
            ));
            assert!(!eligible(&s).contains("q"), "should reject: {m}");
        }
    }

    #[test]
    fn escaping_mention_rejected() {
        // Passed to a function: the callee sees {ptr,len,cap} with no head.
        let s = wrap(
            r#"
    let mut q: VecDeque[i64] = VecDeque.new();
    q.push_back(1);
    consume(q);
"#,
        );
        assert!(!eligible(&s).contains("q"));
    }

    #[test]
    fn mention_inside_par_rejected() {
        // A par region copies the aggregate to another task, where the head
        // alloca does not exist.
        let s = wrap(
            r#"
    let mut q: VecDeque[i64] = VecDeque.new();
    par {
        q.push_back(1);
    }
"#,
        );
        assert!(!eligible(&s).contains("q"));
    }

    #[test]
    fn reassignment_rejected() {
        let s = wrap(
            r#"
    let mut q: VecDeque[i64] = VecDeque.new();
    q.push_back(1);
    q = VecDeque.new();
"#,
        );
        assert!(!eligible(&s).contains("q"));
    }

    #[test]
    fn unannotated_binding_rejected() {
        // Element type unknown here, so POD-ness cannot be established.
        let s = wrap(
            r#"
    let mut q = VecDeque.new();
    q.push_back(1);
    let _ = q.pop_front();
"#,
        );
        assert!(!eligible(&s).contains("q"));
    }

    #[test]
    fn immutable_or_foreign_binding_rejected() {
        // Not initialized in place by VecDeque.new() in this frame.
        let s = wrap(
            r#"
    let mut q: VecDeque[i64] = make_queue();
    let _ = q.pop_front();
"#,
        );
        assert!(!eligible(&s).contains("q"));
    }
}
