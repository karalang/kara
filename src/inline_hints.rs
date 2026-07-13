//! Compiler-driven inline-hint heuristics (phase-11 Codegen Optimization).
//!
//! A whole-program pass that decides, for each concrete user function, whether
//! codegen should attach an `inlinehint` or `noinline` LLVM function attribute
//! that the user did **not** write by hand. It composes with the user-facing
//! `#[inline]` / `#[inline(always)]` / `#[inline(never)]` surface
//! (`codegen::functions::emit_codegen_hint_attrs`): a user hint always wins, so
//! this pass is consulted only for functions with no explicit `inline_hint`.
//!
//! These are advisory hints — reported behavior, not guaranteed semantics
//! (design.md § Codegen Hint Attributes). They shift LLVM's inliner cost model;
//! they never change program meaning. The three signals the checklist names
//! drive the decision:
//!
//! - **Callee size** — a small body (`<= SMALL` nodes) is a leaf helper whose
//!   call overhead dominates its work: `inlinehint`. A very large body
//!   (`>= LARGE` nodes) is one you never want pulled into a caller: `noinline`.
//! - **Single call site** — a function reached from exactly one `Call` site is
//!   almost always a win to inline (the call *and* often the out-of-line copy
//!   vanish), up to a generous size cap.
//! - **Loop-hot call** — a modest function called from inside a loop pays its
//!   call overhead every iteration: `inlinehint`.
//!
//! The node counter is an *exhaustive* match (no `_` arm), so the Rust compiler
//! guarantees every `ExprKind` / `StmtKind` is accounted for — a function can
//! never be mis-sized *small* by a forgotten variant (which would risk inlining
//! something large). The census counts `Call`-position references by name,
//! which is complete for free functions (they are never reached via method-call
//! syntax); a function value taken by reference is simply not counted as a call
//! site, which only makes the single-call-site signal fire less often
//! (conservative).

use std::collections::HashMap;

use crate::ast::*;

/// A heuristic inline decision for one function. Absent from the result map
/// means "no compiler hint" — codegen leaves the inline axis to LLVM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeuristicHint {
    /// Emit `inlinehint` — nudge LLVM to inline (small / single-use / loop-hot).
    Inline,
    /// Emit `noinline` — keep out of line (very large body).
    NoInline,
}

// ── Thresholds (node counts) ──────────────────────────────────────
// Deliberately conservative: `inlinehint` is advisory so a slightly-off
// SMALL/MEDIUM bound cannot regress correctness, and LARGE is set high enough
// that `noinline` only lands on functions no sane inliner would pull in anyway.
const SMALL: usize = 16;
const MEDIUM: usize = 48;
const SINGLE_SITE_CAP: usize = 256;
const LARGE: usize = 512;

#[derive(Default, Clone, Copy)]
struct CallInfo {
    sites: usize,
    any_in_loop: bool,
}

/// Compute per-function heuristic inline hints for `program`. Keyed by the
/// function's source name; only concrete (non-generic) free functions that
/// carry no user `inline_hint` are candidates. Generic bases (declared per
/// monomorphization under synthetic names) and FFI/`main` are skipped.
pub fn compute(program: &Program) -> HashMap<String, HeuristicHint> {
    // Pass 1: global call census over every function body (generic included —
    // their monos are called at runtime, so their call sites count).
    let mut census: HashMap<String, CallInfo> = HashMap::new();
    for item in &program.items {
        if let Item::Function(f) = item {
            let mut size = 0usize;
            let mut calls_self = false;
            walk_block(
                &f.body,
                false,
                &mut size,
                &mut census,
                &f.name,
                &mut calls_self,
            );
        }
    }

    // Pass 2: per-candidate decision. Re-derive each candidate's own size and
    // self-recursion in a second walk (the census walk above already visited
    // every body; recomputing size here keeps the two concerns readable and is
    // cheap relative to codegen).
    let mut out = HashMap::new();
    for item in &program.items {
        let Item::Function(f) = item else { continue };
        // Skip: user already chose an inline hint (they win); generic bases
        // (declared under mono names, not `f.name`); FFI exports (C ABI); and
        // `main` (runtime entry).
        if f.inline_hint.is_some()
            || f.generic_params.is_some()
            || f.abi.is_some()
            || f.name == "main"
        {
            continue;
        }
        let mut size = 0usize;
        let mut calls_self = false;
        let mut ignored = HashMap::new();
        walk_block(
            &f.body,
            false,
            &mut size,
            &mut ignored,
            &f.name,
            &mut calls_self,
        );
        // A self-recursive function cannot be force-inlined (it would expand
        // forever); leave its inline axis entirely to LLVM.
        if calls_self {
            continue;
        }
        let info = census.get(&f.name).copied().unwrap_or_default();
        let decision = if size <= SMALL {
            Some(HeuristicHint::Inline)
        } else if info.sites <= 1 && size <= SINGLE_SITE_CAP {
            // 0 or 1 direct call sites: inlining removes the call (and often
            // the sole out-of-line copy). `0` covers a helper whose only
            // remaining reference is indirect/dead — still cheap to hint.
            Some(HeuristicHint::Inline)
        } else if info.any_in_loop && size <= MEDIUM {
            Some(HeuristicHint::Inline)
        } else if size >= LARGE {
            Some(HeuristicHint::NoInline)
        } else {
            None
        };
        if let Some(d) = decision {
            out.insert(f.name.clone(), d);
        }
    }
    out
}

// ── Exhaustive walkers (size accumulation + call census) ──────────

fn walk_block(
    b: &Block,
    in_loop: bool,
    size: &mut usize,
    census: &mut HashMap<String, CallInfo>,
    self_name: &str,
    calls_self: &mut bool,
) {
    for s in &b.stmts {
        walk_stmt(s, in_loop, size, census, self_name, calls_self);
    }
    if let Some(e) = &b.final_expr {
        walk_expr(e, in_loop, size, census, self_name, calls_self);
    }
}

fn walk_stmt(
    s: &Stmt,
    in_loop: bool,
    size: &mut usize,
    census: &mut HashMap<String, CallInfo>,
    self_name: &str,
    calls_self: &mut bool,
) {
    *size += 1;
    match &s.kind {
        StmtKind::Let { value, .. }
        | StmtKind::LetElse { value, .. }
        | StmtKind::Expr(value)
        | StmtKind::Assign { value, .. }
        | StmtKind::CompoundAssign { value, .. } => {
            walk_expr(value, in_loop, size, census, self_name, calls_self);
            // `Assign` / `CompoundAssign` also carry a target place-expr.
            if let StmtKind::Assign { target, .. } | StmtKind::CompoundAssign { target, .. } =
                &s.kind
            {
                walk_expr(target, in_loop, size, census, self_name, calls_self);
            }
            if let StmtKind::LetElse { else_block, .. } = &s.kind {
                walk_block(else_block, in_loop, size, census, self_name, calls_self);
            }
        }
        StmtKind::LetUninit { .. } => {}
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            walk_block(body, in_loop, size, census, self_name, calls_self);
        }
        StmtKind::MultiAssign { targets, values } => {
            for t in targets {
                walk_expr(t, in_loop, size, census, self_name, calls_self);
            }
            for v in values {
                walk_expr(v, in_loop, size, census, self_name, calls_self);
            }
        }
    }
}

fn walk_expr(
    e: &Expr,
    in_loop: bool,
    size: &mut usize,
    census: &mut HashMap<String, CallInfo>,
    self_name: &str,
    calls_self: &mut bool,
) {
    *size += 1;
    macro_rules! w {
        ($e:expr) => {
            walk_expr($e, in_loop, size, census, self_name, calls_self)
        };
    }
    match &e.kind {
        // Leaves — no sub-expressions.
        ExprKind::Integer(..)
        | ExprKind::Float(..)
        | ExprKind::CharLit(_)
        | ExprKind::ByteLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::MultiStringLit(_)
        | ExprKind::Bool(_)
        | ExprKind::Identifier(_)
        | ExprKind::Path { .. }
        | ExprKind::SelfValue
        | ExprKind::SelfType
        | ExprKind::PipePlaceholder
        | ExprKind::OffsetOf { .. }
        | ExprKind::CStringLit { .. }
        | ExprKind::Continue { .. }
        | ExprKind::Error => {}
        // Interpolated string parts carry embedded expressions.
        ExprKind::InterpolatedStringLit(parts) => {
            for p in parts {
                if let ParsedInterpolationPart::Expr(pe, _) = p {
                    w!(pe);
                }
            }
        }
        ExprKind::Call { callee, args } => {
            // Census: count a direct call to a named free function.
            if let ExprKind::Identifier(name) = &callee.kind {
                let entry = census.entry(name.clone()).or_default();
                entry.sites += 1;
                entry.any_in_loop |= in_loop;
                if name == self_name {
                    *calls_self = true;
                }
            } else if let ExprKind::Path { segments, .. } = &callee.kind {
                if segments.len() == 1 {
                    let name = &segments[0];
                    let entry = census.entry(name.clone()).or_default();
                    entry.sites += 1;
                    entry.any_in_loop |= in_loop;
                    if name == self_name {
                        *calls_self = true;
                    }
                }
            }
            w!(callee);
            for a in args {
                w!(&a.value);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            w!(object);
            for a in args {
                w!(&a.value);
            }
        }
        ExprKind::OptionalChain { object, args, .. } => {
            w!(object);
            if let Some(args) = args {
                for a in args {
                    w!(&a.value);
                }
            }
        }
        ExprKind::Binary { left, right, .. }
        | ExprKind::NilCoalesce { left, right }
        | ExprKind::Pipe { left, right } => {
            w!(left);
            w!(right);
        }
        ExprKind::Unary { operand, .. } => w!(operand),
        ExprKind::Question(inner) => w!(inner),
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            w!(object)
        }
        ExprKind::Index { object, index } => {
            w!(object);
            w!(index);
        }
        ExprKind::Cast { expr, .. } => w!(expr),
        ExprKind::Block(b)
        | ExprKind::Comptime(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Try(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b) => walk_block(b, in_loop, size, census, self_name, calls_self),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            w!(condition);
            walk_block(then_block, in_loop, size, census, self_name, calls_self);
            if let Some(eb) = else_branch {
                w!(eb);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            w!(value);
            walk_block(then_block, in_loop, size, census, self_name, calls_self);
            if let Some(eb) = else_branch {
                w!(eb);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            w!(scrutinee);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    w!(g);
                }
                w!(&arm.body);
            }
        }
        // Loop bodies mark `in_loop` for nested call sites; the condition /
        // iterable runs once per entry, not per iteration, so it stays at the
        // enclosing loop level.
        ExprKind::While {
            condition, body, ..
        } => {
            w!(condition);
            walk_block(body, true, size, census, self_name, calls_self);
        }
        ExprKind::WhileLet { value, body, .. } => {
            w!(value);
            walk_block(body, true, size, census, self_name, calls_self);
        }
        ExprKind::For { iterable, body, .. } => {
            w!(iterable);
            walk_block(body, true, size, census, self_name, calls_self);
        }
        ExprKind::Loop { body, .. } => walk_block(body, true, size, census, self_name, calls_self),
        // A labeled block is a break target, not a loop — no `in_loop`.
        ExprKind::LabeledBlock { body, .. } => {
            walk_block(body, in_loop, size, census, self_name, calls_self)
        }
        ExprKind::Closure { body, .. } => w!(body),
        ExprKind::Return(inner) => {
            if let Some(e) = inner {
                w!(e);
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(e) = value {
                w!(e);
            }
        }
        ExprKind::Tuple(items)
        | ExprKind::ArrayLiteral(items)
        | ExprKind::PrefixCollectionLiteral { items, .. } => {
            for it in items {
                w!(it);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            w!(value);
            w!(count);
        }
        ExprKind::MapLiteral(pairs) => {
            for (k, v) in pairs {
                w!(k);
                w!(v);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields {
                w!(&f.value);
            }
            if let Some(s) = spread {
                w!(s);
            }
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                w!(s);
            }
            if let Some(e) = end {
                w!(e);
            }
        }
        ExprKind::Lock { mutex, body, .. } => {
            w!(mutex);
            walk_block(body, in_loop, size, census, self_name, calls_self);
        }
        ExprKind::Providers { bindings, body } => {
            for b in bindings {
                w!(&b.value);
            }
            walk_block(body, in_loop, size, census, self_name, calls_self);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hints(src: &str) -> HashMap<String, HeuristicHint> {
        let parsed = crate::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        compute(&parsed.program)
    }

    #[test]
    fn small_leaf_helper_gets_inline() {
        let h = hints("fn add(a: i64, b: i64) -> i64 { a + b }\nfn main() { let _ = add(1, 2); }");
        assert_eq!(h.get("add"), Some(&HeuristicHint::Inline));
    }

    #[test]
    fn user_inline_never_is_not_overridden() {
        // A user hint means the function is skipped entirely — no heuristic
        // entry, so codegen emits only the user's `noinline`.
        let h = hints(
            "#[inline(never)]\nfn add(a: i64, b: i64) -> i64 { a + b }\nfn main() { let _ = add(1, 2); }",
        );
        assert_eq!(h.get("add"), None);
    }

    #[test]
    fn recursive_function_gets_no_hint() {
        let h = hints(
            "fn fac(n: i64) -> i64 { if n <= 1 { 1 } else { n * fac(n - 1) } }\nfn main() { let _ = fac(5); }",
        );
        assert_eq!(h.get("fac"), None);
    }

    #[test]
    fn main_is_never_a_candidate() {
        let h = hints("fn main() { let x = 1; let _ = x; }");
        assert_eq!(h.get("main"), None);
    }

    #[test]
    fn very_large_function_gets_noinline() {
        // Build a body well past LARGE nodes with a long arithmetic chain.
        let chain = (0..600)
            .map(|i| format!("+ {i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let src = format!(
            "fn big() -> i64 {{ let mut s = 0; s = 0 {chain}; s }}\nfn a() {{ let _ = big(); }}\nfn b() {{ let _ = big(); }}"
        );
        let h = hints(&src);
        assert_eq!(h.get("big"), Some(&HeuristicHint::NoInline));
    }
}
