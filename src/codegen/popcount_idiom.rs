//! Kernighan popcount-loop recognition — lower `while x != 0 { x = x & (x - 1);
//! c = c + 1 }` to a single `llvm.ctpop`, preserving the source's overflow
//! traps.
//!
//! ## Why this exists
//!
//! Brian Kernighan's bit-count loop is the canonical way to write a Hamming
//! weight without an intrinsic, and it appears verbatim in real bit-manipulation
//! code. `clang -O3` recognizes it (LLVM's `LoopIdiomRecognize` → `llvm.ctpop`)
//! and, at a v3 baseline, then **auto-vectorizes the enclosing loop** into an
//! AVX2 nibble-table popcount (`vpshufb` / `vpsadbw`). Measured on kata #191
//! (2M values × 10 rounds, x86 container): `clang -O3` at the default v1
//! baseline runs 372.9 ms, and the same source at `-march=x86-64-v3` runs
//! **24.4 ms — 15.3× faster**.
//!
//! `karac` targets v3 and uses the same LLVM, so it should get that for free.
//! It does not, and the reason is Kāra's checked arithmetic. The emitted inner
//! loop is:
//!
//! ```text
//!   mov  %rdi,%r8
//!   dec  %r8        ; x - 1
//!   jo   <trap>     ; <-- overflow check
//!   inc  %rsi       ; c + 1
//!   jo   <trap>     ; <-- overflow check
//!   and  %r8,%rdi   ; x & (x - 1)
//!   jne  <top>
//! ```
//!
//! Those two `jo` edges split the body into several basic blocks with exits to
//! trap blocks. `LoopIdiomRecognize` requires a single-block loop of the exact
//! `x &= x - 1` shape, so it never matches, no `ctpop` appears, and the outer
//! loop cannot vectorize. C has no such checks — signed overflow is UB there —
//! which is the entire reason clang matches and `karac` does not. Kāra measured
//! 407.2 ms against `clang -O3` v1's 372.9 ms, i.e. **at parity on equal ISA**;
//! the whole deficit is this missed idiom. (`KARAC_OPT_LEVEL=3` does not help —
//! it measured *slower*, 470.8 ms. The blocker is pattern-matching, not pass
//! aggressiveness.)
//!
//! ## The rewrite, and why it is sound
//!
//! The loop is **not** a total popcount: it traps. Verified against the real
//! compiler before writing any of this —
//! `hamming_weight(1i64 << 63)` panics with `integer overflow` at the `x - 1`.
//! So `ctpop` alone would be a miscompile, turning a trap into `1`. The lowering
//! therefore emits, for a signed `x` of width W:
//!
//! ```text
//!   if x == INT_MIN { trap "integer overflow" }   // at the `x - 1` span
//!   c = checked_add(c, ctpop(x))                  // at the `c = c + 1` span
//!   x = 0
//! ```
//!
//! Case analysis against the source loop, for signed `x`:
//!
//! * **`x == 0`** — body never runs; `c` unchanged, `x` stays 0. The guard is
//!   false and `ctpop(0) == 0`, so `c + 0 == c`. Matches.
//! * **`x == INT_MIN`** — body runs once, `x - 1` overflows, source traps on the
//!   *first* statement. The guard traps first, before touching `c`. Matches,
//!   including the ordering (see below).
//! * **any other `x != 0`** — `x > INT_MIN`, so `x - 1` never overflows on this
//!   or any later iteration (`x & (x-1)` is non-increasing in magnitude and
//!   stays `>= 0` once past the sign bit). Each iteration clears exactly the
//!   lowest set bit, so the trip count is exactly `popcount(x)`, and the final
//!   `x` is 0. `c` receives `popcount(x)` increments of 1.
//!
//! **Why one checked add is exactly equivalent to N checked increments.** `c`
//! increases monotonically by 1 per iteration, so the running value overflows at
//! some iteration **iff** `c + popcount(x)` overflows. A single
//! `sadd.with.overflow` therefore traps on precisely the inputs the loop would,
//! and it is attributed to the `c = c + 1` span, so the panic message and
//! location are unchanged. This is why the lowering does **not** require `c` to
//! start at 0 — a large initial `c` still traps correctly.
//!
//! **Trap ordering is load-bearing.** The source evaluates `x = x & (x - 1)`
//! before `c = c + 1`, so when *both* would trap the `x - 1` overflow wins. The
//! guard is emitted first for that reason, and [`match_kernighan_popcount`]
//! **requires the `x` update to be the first statement** — a body in the other
//! order is declined rather than reordered, because there the `c` trap would
//! come first and this emission order would report the wrong one.
//!
//! Unsigned `x` needs no guard at all (`x != 0` implies `x - 1` cannot wrap), but
//! the guard is harmless there and the match is restricted to signed widths for
//! now; extending it is a matter of skipping the guard.
//!
//! ## Matching is fail-closed
//!
//! Codegen runs on the **lowered** AST, where `x != 0` and `x - 1` have become
//! trait-method `Call`s on a `Path` (`ne` / `sub` / `add` / `bitand`) rather than
//! `Binary` nodes. Both spellings are matched. Missing that is exactly why the
//! first cut of the sibling accumulator analysis silently never fired
//! (`accum_overflow.rs`), so it is handled here from the start and pinned by a
//! unit test in both forms.
//!
//! Every deviation from the exact shape returns `None`: any body statement count
//! other than two, any `break`/`continue`/`return` inside, a compound-assign
//! spelling, a non-local operand, a mismatched variable, or a literal other than
//! the required `0` / `1`. Sequential lowering is always correct, so declining
//! costs only the optimization.

use crate::ast::{BinOp, Block, Expr, ExprKind, Stmt, StmtKind};

/// A matched Kernighan popcount loop: the bit-source variable and the counter.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct KernighanPopcount {
    /// The variable the loop consumes bits from (`x`). Left as 0 by the loop.
    pub(super) source: String,
    /// The variable accumulating the bit count (`c`).
    pub(super) counter: String,
    /// Span of the `x - 1` expression. The `x == INT_MIN` guard's panic is
    /// attributed here so the diagnostic names the same place the unoptimized
    /// loop's overflow would — verified: without this the reported location
    /// moved from the subtraction to the `while` keyword.
    pub(super) sub_span: crate::token::Span,
    /// Span of the `c = c + 1` statement, for the collapsed checked add.
    pub(super) add_span: crate::token::Span,
    /// A loop literal carried an explicit SIGNED integer suffix (`0i64`), which
    /// proves `x` is signed even when the binding was type-inferred and so is
    /// absent from `var_type_names`. `x != 0i64` would not typecheck against an
    /// unsigned `x`, so this is sound evidence, not a heuristic.
    pub(super) signed_suffix: bool,
    /// A loop literal carried an explicit UNSIGNED suffix — declines outright.
    pub(super) unsigned_suffix: bool,
}

/// Match `while <src> != 0 { <src> = <src> & (<src> - 1); <cnt> = <cnt> + 1; }`.
///
/// Returns `None` for anything outside that exact shape. See the module docs for
/// the soundness argument and for why statement *order* is required rather than
/// normalized.
pub(super) fn match_kernighan_popcount(
    condition: &Expr,
    body: &Block,
) -> Option<KernighanPopcount> {
    // `while x != 0`
    let source = ne_zero_operand(condition)?;

    // Exactly two statements, no trailing expression: the `x` update then the
    // counter bump. A third statement could observe the intermediate `x` (or
    // trap first), so anything else is declined.
    if body.stmts.len() != 2 || body.final_expr.is_some() {
        return None;
    }
    if body.stmts.iter().any(stmt_has_jump) {
        return None;
    }

    // Statement 1 must be the `x` update, because its trap outranks the
    // counter's when both would fire (module docs § Trap ordering).
    let sub_span = is_clear_lowest_set_bit(&body.stmts[0], &source)?;
    let counter = increment_by_one_target(&body.stmts[1])?;

    // A single variable serving as both source and counter is not this idiom.
    if counter == source {
        return None;
    }
    let mut signed_suffix = false;
    let mut unsigned_suffix = false;
    collect_suffixes(condition, &mut signed_suffix, &mut unsigned_suffix);
    for st in &body.stmts {
        if let StmtKind::Assign { value, .. } = &st.kind {
            collect_suffixes(value, &mut signed_suffix, &mut unsigned_suffix);
        }
    }
    Some(KernighanPopcount {
        source,
        counter,
        sub_span,
        add_span: body.stmts[1].span.clone(),
        signed_suffix,
        unsigned_suffix,
    })
}

/// The variable `v` in `v != 0` (either the `Binary` spelling or the lowered
/// `PartialEq::ne` call). Order-insensitive: `0 != v` matches too.
fn ne_zero_operand(cond: &Expr) -> Option<String> {
    if let ExprKind::Binary { op, left, right } = &cond.kind {
        if !matches!(op, BinOp::NotEq) {
            return None;
        }
        return pair_var_and_int(left, right, 0);
    }
    // Lowered form: `PartialEq::ne(v, 0)`.
    if let ExprKind::Call { callee, args } = &cond.kind {
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2 && segments[1] == "ne" && args.len() == 2 {
                return pair_var_and_int(&args[0].value, &args[1].value, 0);
            }
        }
    }
    None
}

/// `true` iff `stmt` is exactly `v = v & (v - 1)` for `v == var`.
fn is_clear_lowest_set_bit(stmt: &Stmt, var: &str) -> Option<crate::token::Span> {
    let StmtKind::Assign { target, value } = &stmt.kind else {
        // A compound `v &= v - 1` is deliberately not matched: it is a distinct
        // lowering with its own overflow-check placement, and admitting it here
        // without re-deriving that would be guessing.
        return None;
    };
    if identifier_name(target).as_deref() != Some(var) {
        return None;
    }
    let (lhs, rhs) = binary_operands(value, BinOp::BitAnd, "bitand")?;
    // `v & (v - 1)` or `(v - 1) & v`.
    let (a, b) = (identifier_name(lhs), identifier_name(rhs));
    if a.as_deref() == Some(var) {
        return is_sub_one_of(rhs, var);
    }
    if b.as_deref() == Some(var) {
        return is_sub_one_of(lhs, var);
    }
    None
}

/// Record whether any integer literal in `e` carries a signed / unsigned suffix.
fn collect_suffixes(e: &Expr, signed: &mut bool, unsigned: &mut bool) {
    use crate::token::IntSuffix as S;
    match &e.kind {
        ExprKind::Integer(_, Some(suf)) => match suf {
            S::I8 | S::I16 | S::I32 | S::I64 | S::I128 => *signed = true,
            S::U8 | S::U16 | S::U32 | S::U64 | S::U128 => *unsigned = true,
        },
        ExprKind::Binary { left, right, .. } => {
            collect_suffixes(left, signed, unsigned);
            collect_suffixes(right, signed, unsigned);
        }
        ExprKind::Call { args, .. } => {
            for a in args {
                collect_suffixes(&a.value, signed, unsigned);
            }
        }
        _ => {}
    }
}

/// If `e` is `var - 1`, the span of the `1` LITERAL.
///
/// The literal's span, not the subtraction's: the unoptimized checked-sub
/// attributes its overflow panic to the right operand (measured — `x - 1i64` on
/// column 18 reports at column 22, the `1i64`), so matching it is what keeps the
/// guard's diagnostic byte-identical to the loop it replaces.
fn is_sub_one_of(e: &Expr, var: &str) -> Option<crate::token::Span> {
    let (lhs, rhs) = binary_operands(e, BinOp::Sub, "sub")?;
    if identifier_name(lhs).as_deref() == Some(var) && int_literal_is(rhs, 1) {
        return Some(rhs.span.clone());
    }
    None
}

/// The assignment target of `c = c + 1`, or `None`.
fn increment_by_one_target(stmt: &Stmt) -> Option<String> {
    let StmtKind::Assign { target, value } = &stmt.kind else {
        return None;
    };
    let name = identifier_name(target)?;
    let (lhs, rhs) = binary_operands(value, BinOp::Add, "add")?;
    // `c + 1` or `1 + c`.
    if identifier_name(lhs).as_deref() == Some(name.as_str()) && int_literal_is(rhs, 1) {
        return Some(name);
    }
    if identifier_name(rhs).as_deref() == Some(name.as_str()) && int_literal_is(lhs, 1) {
        return Some(name);
    }
    None
}

/// Operands of a binary op, accepting both the `Binary` spelling and the lowered
/// two-argument trait-method `Call` (`segments[1] == method`).
fn binary_operands<'e>(e: &'e Expr, op: BinOp, method: &str) -> Option<(&'e Expr, &'e Expr)> {
    if let ExprKind::Binary {
        op: got,
        left,
        right,
    } = &e.kind
    {
        if *got == op {
            return Some((left, right));
        }
        return None;
    }
    if let ExprKind::Call { callee, args } = &e.kind {
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2 && segments[1] == method && args.len() == 2 {
                return Some((&args[0].value, &args[1].value));
            }
        }
    }
    None
}

/// One side a bare identifier, the other the given integer literal → the name.
fn pair_var_and_int(a: &Expr, b: &Expr, want: i128) -> Option<String> {
    if let Some(n) = identifier_name(a) {
        if int_literal_is(b, want) {
            return Some(n);
        }
    }
    if let Some(n) = identifier_name(b) {
        if int_literal_is(a, want) {
            return Some(n);
        }
    }
    None
}

fn identifier_name(e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Identifier(n) => Some(n.clone()),
        _ => None,
    }
}

fn int_literal_is(e: &Expr, want: i128) -> bool {
    matches!(&e.kind, ExprKind::Integer(v, _) if *v as i128 == want)
}

/// `true` if the statement contains a `break` / `continue` / `return` anywhere.
/// Such a loop can exit with `x != 0`, which breaks the "final `x` is 0"
/// half of the rewrite, so it is declined.
fn stmt_has_jump(stmt: &Stmt) -> bool {
    match &stmt.kind {
        StmtKind::Let { value, .. }
        | StmtKind::Assign { value, .. }
        | StmtKind::CompoundAssign { value, .. }
        | StmtKind::Expr(value) => expr_has_jump(value),
        _ => true, // Anything else in this body means it is not the idiom.
    }
}

fn expr_has_jump(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Break { .. } | ExprKind::Continue { .. } | ExprKind::Return(_) => true,
        ExprKind::Binary { left, right, .. } => expr_has_jump(left) || expr_has_jump(right),
        ExprKind::Unary { operand, .. } => expr_has_jump(operand),
        ExprKind::Call { callee, args } => {
            expr_has_jump(callee) || args.iter().any(|a| expr_has_jump(&a.value))
        }
        // `Path` is the callee of a LOWERED trait-method call (`Ops::sub`), so it
        // must be recognized as jump-free or every lowered-form loop is declined
        // and this whole module silently never fires — which is precisely what
        // the `matches_lowered_call_form` test caught.
        ExprKind::Identifier(_) | ExprKind::Integer(_, _) | ExprKind::Path { .. } => false,
        // Fail closed: an unmodelled expression may hide a jump.
        _ => true,
    }
}

// ── Emission ───────────────────────────────────────────────────

impl<'ctx> super::Codegen<'ctx> {
    /// If this `while` is a Kernighan popcount loop over signed locals, replace
    /// the whole loop with `llvm.ctpop` plus the two trap checks the source
    /// semantics require, and return `true`. Returns `false` (emitting nothing)
    /// for any other loop, leaving the caller to lower it normally.
    ///
    /// See the module docs for the case analysis. The three preconditions
    /// enforced here that the AST matcher cannot see:
    ///
    /// 1. **Both variables are declared SIGNED integers of the same width.**
    ///    Checked positively against `var_type_names`; an unknown or unsigned
    ///    type declines. This is load-bearing — on an unsigned `x` the
    ///    `x == INT_MIN` guard would trap on a perfectly ordinary large value,
    ///    turning a correct program into a panic.
    /// 2. **Neither variable is RC-promoted or a borrowed param.** Those read
    ///    through an extra indirection (`get_data_ptr`'s RC-aware path), and
    ///    reproducing that here would be duplicating logic this optimization
    ///    does not need. A plain alloca is the only shape admitted.
    /// 3. **Width ≥ 2**, so `INT_MIN` is a distinct value from `0`.
    pub(super) fn try_emit_kernighan_popcount(
        &mut self,
        condition: &crate::ast::Expr,
        body: &crate::ast::Block,
    ) -> Result<bool, String> {
        let Some(m) = super::popcount_idiom::match_kernighan_popcount(condition, body) else {
            return Ok(false);
        };

        // Precondition 1: prove SIGNED. Two independent oracles, because
        // `var_type_names` only carries EXPLICITLY annotated bindings — the
        // idiom is normally written `let mut x = n;` with the type inferred, and
        // requiring the annotation made this decline on exactly the shape it
        // exists to optimize (verified: #191's kata declined until the literal
        // suffixes were consulted). A signed suffix on a loop literal is sound
        // evidence on its own: `x != 0i64` does not typecheck for unsigned `x`.
        fn is_signed_int_name(s: &str) -> bool {
            matches!(s, "i8" | "i16" | "i32" | "i64" | "i128" | "isize")
        }
        fn is_unsigned_int_name(s: &str) -> bool {
            matches!(s, "u8" | "u16" | "u32" | "u64" | "u128" | "usize")
        }
        if m.unsigned_suffix {
            return Ok(false);
        }
        for name in [&m.source, &m.counter] {
            if self
                .var_type_names
                .get(name.as_str())
                .is_some_and(|t| is_unsigned_int_name(t.as_str()))
            {
                return Ok(false);
            }
        }
        let annotated_signed = [&m.source, &m.counter].iter().all(|n| {
            self.var_type_names
                .get(n.as_str())
                .is_some_and(|t| is_signed_int_name(t.as_str()))
        });
        if !annotated_signed && !m.signed_suffix {
            return Ok(false);
        }

        // Precondition 2: plain allocas only.
        for name in [&m.source, &m.counter] {
            if self.rc_fallback_heap_types.contains_key(name.as_str())
                || self.ref_params.contains_key(name.as_str())
            {
                return Ok(false);
            }
        }

        let (Some(src), Some(cnt)) = (
            self.variables.get(m.source.as_str()).copied(),
            self.variables.get(m.counter.as_str()).copied(),
        ) else {
            return Ok(false);
        };
        let (Ok(src_ty), Ok(cnt_ty)) = (
            inkwell::types::IntType::try_from(src.ty),
            inkwell::types::IntType::try_from(cnt.ty),
        ) else {
            return Ok(false);
        };
        let w = src_ty.get_bit_width();
        // Precondition 3, plus matching widths: a narrower counter would need a
        // coercion whose overflow semantics differ from the loop's.
        if !(2..=64).contains(&w) || cnt_ty.get_bit_width() != w {
            return Ok(false);
        }

        let fn_val = self.current_fn.unwrap();
        let x = self
            .builder
            .build_load(src_ty, src.ptr, "popcnt.x")
            .unwrap()
            .into_int_value();

        // Guard: `x == INT_MIN` traps, because the source's first statement
        // computes `x - 1` and that is the one input where it overflows. Emitted
        // BEFORE the counter add so the reported panic matches the source's
        // evaluation order (module docs § Trap ordering).
        // Attribute the guard's panic to the `x - 1` expression, not to the
        // `while` keyword: `current_span` is what `emit_panic` bakes into the
        // message, and without this the reported location shifts (observed:
        // `5:22` became `4:5`).
        let saved_span = self.current_span.clone();
        self.current_span = Some(m.sub_span.clone());
        let min = src_ty.const_int(1u64 << (w - 1), false);
        let is_min = self
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, x, min, "popcnt.is.min")
            .unwrap();
        let trap_bb = self.context.append_basic_block(fn_val, "popcnt.min.trap");
        let ok_bb = self.context.append_basic_block(fn_val, "popcnt.ok");
        self.builder
            .build_conditional_branch(is_min, trap_bb, ok_bb)
            .unwrap();
        self.builder.position_at_end(trap_bb);
        self.emit_panic("integer overflow");
        self.builder.build_unreachable().unwrap();
        self.builder.position_at_end(ok_bb);

        // `ctpop` is the whole point: it is what LLVM's vectorizer can widen
        // into an AVX2 nibble-table popcount once the enclosing loop is seen.
        let intrinsic = inkwell::intrinsics::Intrinsic::find("llvm.ctpop")
            .ok_or("llvm.ctpop intrinsic must exist")?;
        let decl = intrinsic
            .get_declaration(&self.module, &[src_ty.into()])
            .ok_or_else(|| format!("llvm.ctpop has no declaration for width {w}"))?;
        let bits = self
            .builder
            .build_call(decl, &[x.into()], "popcnt.bits")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();

        // One checked add replaces N checked increments — exactly equivalent
        // because the counter rises monotonically by 1 (module docs).
        let c0 = self
            .builder
            .build_load(cnt_ty, cnt.ptr, "popcnt.c0")
            .unwrap()
            .into_int_value();
        self.current_span = Some(m.add_span.clone());
        let total = self.emit_checked_int_arith("add", c0, bits, false)?;
        self.current_span = saved_span;
        self.builder.build_store(cnt.ptr, total).unwrap();
        // The loop can only exit with the source exhausted.
        self.builder
            .build_store(src.ptr, src_ty.const_zero())
            .unwrap();
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Pull the single `while` loop out of `fn main`'s body.
    fn loop_of(src: &str) -> (Expr, Block) {
        let parsed = crate::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        for item in &parsed.program.items {
            if let crate::ast::Item::Function(f) = item {
                for stmt in &f.body.stmts {
                    if let StmtKind::Expr(e) = &stmt.kind {
                        if let ExprKind::While {
                            condition, body, ..
                        } = &e.kind
                        {
                            return ((**condition).clone(), body.clone());
                        }
                    }
                }
            }
        }
        panic!("no while loop found");
    }

    fn matches(src: &str) -> Option<KernighanPopcount> {
        let (c, b) = loop_of(src);
        match_kernighan_popcount(&c, &b)
    }

    const CANONICAL: &str = r#"
fn main() {
    let mut x = 11i64;
    let mut c = 0i64;
    while x != 0i64 {
        x = x & (x - 1i64);
        c = c + 1i64;
    }
    println(c);
}
"#;

    #[test]
    fn matches_canonical_shape() {
        let m = matches(CANONICAL).expect("canonical shape must match");
        assert_eq!((m.source.as_str(), m.counter.as_str()), ("x", "c"));
        // The suffixed literals are what prove signedness for an inferred `let`.
        assert!(m.signed_suffix && !m.unsigned_suffix);
    }

    #[test]
    fn matches_commuted_operands() {
        // `(x - 1) & x` and `1 + c` are the same idiom.
        let src = CANONICAL
            .replace("x = x & (x - 1i64);", "x = (x - 1i64) & x;")
            .replace("c = c + 1i64;", "c = 1i64 + c;");
        assert!(matches(&src).is_some());
    }

    /// The `x` update must come first: with the order swapped the counter's
    /// overflow trap would fire before the `x - 1` one, so the emission order
    /// this module uses would report the wrong panic.
    #[test]
    fn declines_swapped_statement_order() {
        let src = CANONICAL.replace(
            "        x = x & (x - 1i64);\n        c = c + 1i64;\n",
            "        c = c + 1i64;\n        x = x & (x - 1i64);\n",
        );
        assert_eq!(matches(&src), None);
    }

    #[test]
    fn declines_wrong_mask() {
        // `x & (x - 2)` does not clear exactly the lowest set bit.
        let src = CANONICAL.replace("(x - 1i64)", "(x - 2i64)");
        assert_eq!(matches(&src), None);
    }

    #[test]
    fn declines_step_other_than_one() {
        let src = CANONICAL.replace("c = c + 1i64;", "c = c + 2i64;");
        assert_eq!(matches(&src), None);
    }

    #[test]
    fn declines_extra_statement() {
        // A third statement can observe the intermediate `x`.
        let src = CANONICAL.replace(
            "        c = c + 1i64;\n",
            "        c = c + 1i64;\n        println(x);\n",
        );
        assert_eq!(matches(&src), None);
    }

    #[test]
    fn declines_break_in_body() {
        let src = CANONICAL.replace(
            "        c = c + 1i64;\n",
            "        c = c + 1i64;\n        if c > 3i64 { break; }\n",
        );
        assert_eq!(matches(&src), None);
    }

    #[test]
    fn declines_wrong_guard() {
        // `x > 0` is a different loop: it never sees the sign-bit case, and the
        // trip count argument in the module docs does not carry over.
        let src = CANONICAL.replace("while x != 0i64", "while x > 0i64");
        assert_eq!(matches(&src), None);
    }

    #[test]
    fn declines_mismatched_variable() {
        let src = CANONICAL.replace("x = x & (x - 1i64);", "x = x & (c - 1i64);");
        assert_eq!(matches(&src), None);
    }

    #[test]
    fn declines_counter_aliasing_source() {
        let src = CANONICAL.replace("c = c + 1i64;", "x = x + 1i64;");
        assert_eq!(matches(&src), None);
    }

    /// Codegen sees the LOWERED AST, where the comparison and the arithmetic are
    /// trait-method calls rather than `Binary` nodes. Missing this is why the
    /// sibling accumulator analysis silently never fired, so both spellings are
    /// pinned here.
    #[test]
    fn matches_lowered_call_form() {
        use crate::ast::{Expr as E, ExprKind as EK};
        use crate::token::Span;
        let sp = Span::default();
        let id = |n: &str| E {
            kind: EK::Identifier(n.into()),
            span: sp.clone(),
        };
        let int = |v: i64| E {
            kind: EK::Integer(v, None),
            span: sp.clone(),
        };
        let arg = |v: E| crate::ast::CallArg {
            label: None,
            mut_marker: false,
            span: v.span.clone(),
            value: v,
        };
        let call = |m: &str, a: E, b: E| E {
            kind: EK::Call {
                callee: Box::new(E {
                    kind: EK::Path {
                        segments: vec!["Ops".into(), m.into()],
                        generic_args: None,
                    },
                    span: sp.clone(),
                }),
                args: vec![arg(a), arg(b)],
            },
            span: sp.clone(),
        };
        let cond = call("ne", id("x"), int(0));
        let body = Block {
            stmts: vec![
                Stmt {
                    kind: StmtKind::Assign {
                        target: id("x"),
                        value: call("bitand", id("x"), call("sub", id("x"), int(1))),
                    },
                    span: sp.clone(),
                },
                Stmt {
                    kind: StmtKind::Assign {
                        target: id("c"),
                        value: call("add", id("c"), int(1)),
                    },
                    span: sp.clone(),
                },
            ],
            final_expr: None,
            span: sp.clone(),
        };
        let m = match_kernighan_popcount(&cond, &body).expect("lowered form must match");
        assert_eq!((m.source.as_str(), m.counter.as_str()), ("x", "c"));
    }
}
