//! Compile-time proof that a `let`-bound String is a stable, all-ASCII
//! constant — the precondition for the branch-free stride-1 `.chars()` loop
//! in [`super::control_flow_for`] (B-2026-07-27-7).
//!
//! # Why the loop shape needs a proof
//!
//! The general `for ch in s.chars()` lowering peeks each byte and branches:
//! `< 0x80` takes an inline 1-byte fast path, anything else calls
//! `karac_string_decode_char`. The two arms rejoin on a PHI of the byte
//! offset (advance 1, or whatever the decoder returned), so even when the
//! multibyte arm is dynamically dead LLVM cannot recover "offset == iteration
//! index, stride 1" and cannot reduce a search over a constant string to a
//! single indexed load. Measured on the #127 kata's `nth_letter` shape
//! (4.25M calls over a 26-char literal): the branch loop runs 699.4M
//! instructions against 32.3M for the identical Rust source — 21.7x. The
//! same program written as `for b in s.bytes()` — which is already the
//! branch-free stride-1 shape this module unlocks for `.chars()` — runs
//! 42.8M, i.e. 1.33x Rust. The whole gap is loop shape, not per-iteration
//! cost.
//!
//! # Why it must fail closed
//!
//! Getting the proof wrong is a SILENT MISCOMPILE: the branch-free loop
//! binds each *byte* as a `char`, so a 2-byte scalar like `é` would yield
//! two bogus chars (0xC3, 0xA9) instead of one (0xE9). No crash, no
//! diagnostic — just wrong output. Every rule below is therefore a
//! whitelist, and anything not positively proven keeps the existing loop.
//!
//! # The proof
//!
//! A name qualifies when, within the block that `let`-binds it:
//!
//! 1. it is bound exactly once, by `let NAME[: String] = "<literal>"` whose
//!    literal is entirely `< 0x80`;
//! 2. it is never an assignment-target root (`s = …`, `s[i] = …`)
//!    — [`collect_assigned_roots_block`];
//! 3. it is never the receiver of a mutating built-in method (`s.push(c)`,
//!    `s.clear()`, …) — [`collect_mut_method_receiver_roots_block`];
//! 4. it is never the root of a `mut`-marked call argument (`f(mut s)`) —
//!    collected by [`Scan`] below, alongside rule 1.
//!
//! Rules 2-4 are the complete set of mutation channels a binding has:
//! Kāra has no free-standing borrow expression, so a callee can only write
//! through a `mut ref` parameter, and reaching one requires the call-site
//! `mut` marker (enforced by the typechecker). Note that rule 1 deliberately
//! does NOT lean on the `let`'s `is_mut` flag: `let s: String = "abc";
//! s.push('z')` currently passes `karac check` and really does mutate, so
//! `is_mut == false` proves nothing here.
//!
//! The name-level result is paired with a second, independent check at the
//! loop site: codegen only takes the fast path when the loop's receiver
//! resolves to the very alloca the qualifying `let` created. Allocas are
//! unique per creation, so a shadowing binding, a same-named parameter, or a
//! stale entry from another function all miss and fall back. Neither
//! mechanism is trusted alone.

use crate::ast::{
    assign_target_root, collect_assigned_roots_block, collect_mut_method_receiver_roots_block,
    Block, Expr, ExprKind, ParsedInterpolationPart, PatternKind, StmtKind,
};
use std::collections::HashSet;

/// The names in `block` that are provably stable all-ASCII string constants
/// (see the module docs for the exact rules). Analysed per-block rather than
/// per-function because a binding's mutation channels are all reachable from
/// the block that introduces it — the collectors recurse into nested blocks
/// and closure bodies.
pub(super) fn ascii_const_string_lets(block: &Block) -> HashSet<String> {
    let mut scan = Scan::default();
    scan.block(block);
    let Scan {
        mut ascii_lets,
        mut poisoned,
    } = scan;
    if ascii_lets.is_empty() {
        return ascii_lets;
    }
    collect_assigned_roots_block(block, &mut poisoned);
    collect_mut_method_receiver_roots_block(block, &mut poisoned);
    ascii_lets.retain(|n| !poisoned.contains(n));
    ascii_lets
}

/// Single-pass collector for rules 1 and 4: qualifying `let` sites, names
/// rebound by any other `let` form, and `mut`-marked argument roots.
#[derive(Default)]
struct Scan {
    ascii_lets: HashSet<String>,
    poisoned: HashSet<String>,
}

impl Scan {
    /// Record a `let NAME = "<ascii literal>"` site, poisoning the name
    /// instead if this is its second binding in the subtree.
    fn note_let(&mut self, pattern: &crate::ast::Pattern, value: Option<&Expr>) {
        let PatternKind::Binding(name) = &pattern.kind else {
            // A destructuring `let` binds names this analysis does not model;
            // poison every simple name it could shadow by poisoning nothing
            // here and letting the loop-site alloca identity check catch it.
            return;
        };
        let qualifies = matches!(
            value.map(|v| &v.kind),
            Some(ExprKind::StringLit(s)) if s.is_ascii()
        );
        // Second binding of the name — a shadow, so neither occurrence is a
        // stable constant under a name-keyed lookup.
        if !self.ascii_lets.insert(name.clone()) || !qualifies {
            self.ascii_lets.remove(name);
            self.poisoned.insert(name.clone());
        }
    }

    fn block(&mut self, block: &Block) {
        for stmt in &block.stmts {
            match &stmt.kind {
                StmtKind::Let { pattern, value, .. } => {
                    self.expr(value);
                    self.note_let(pattern, Some(value));
                }
                StmtKind::LetElse {
                    pattern,
                    value,
                    else_block,
                    ..
                } => {
                    self.expr(value);
                    self.block(else_block);
                    self.note_let(pattern, None);
                }
                StmtKind::LetUninit { name, .. } => {
                    self.ascii_lets.remove(name);
                    self.poisoned.insert(name.clone());
                }
                StmtKind::Assign { target, value }
                | StmtKind::CompoundAssign { target, value, .. } => {
                    self.expr(target);
                    self.expr(value);
                }
                StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => self.block(body),
                StmtKind::Expr(e) => self.expr(e),
                StmtKind::MultiAssign { .. } => {}
            }
        }
        if let Some(final_expr) = &block.final_expr {
            self.expr(final_expr);
        }
    }

    fn expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                self.expr(callee);
                for arg in args {
                    if arg.mut_marker {
                        if let Some(root) = assign_target_root(&arg.value) {
                            self.ascii_lets.remove(&root);
                            self.poisoned.insert(root);
                        }
                    }
                    self.expr(&arg.value);
                }
            }
            ExprKind::MethodCall { object, args, .. } => {
                self.expr(object);
                for arg in args {
                    if arg.mut_marker {
                        if let Some(root) = assign_target_root(&arg.value) {
                            self.ascii_lets.remove(&root);
                            self.poisoned.insert(root);
                        }
                    }
                    self.expr(&arg.value);
                }
            }
            ExprKind::InterpolatedStringLit(parts) => {
                for part in parts {
                    if let ParsedInterpolationPart::Expr(e, _) = part {
                        self.expr(e);
                    }
                }
            }
            ExprKind::Binary { left, right, .. }
            | ExprKind::NilCoalesce { left, right }
            | ExprKind::Pipe { left, right } => {
                self.expr(left);
                self.expr(right);
            }
            ExprKind::Unary { operand, .. } => self.expr(operand),
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.expr(object)
            }
            ExprKind::Index { object, index } => {
                self.expr(object);
                self.expr(index);
            }
            ExprKind::Block(b)
            | ExprKind::Comptime(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Par(b)
            | ExprKind::Seq(b) => self.block(b),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.expr(condition);
                self.block(then_block);
                if let Some(eb) = else_branch {
                    self.expr(eb);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                self.expr(value);
                self.block(then_block);
                if let Some(eb) = else_branch {
                    self.expr(eb);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.expr(condition);
                self.block(body);
            }
            ExprKind::WhileLet { value, body, .. } => {
                self.expr(value);
                self.block(body);
            }
            ExprKind::Loop { body, .. } | ExprKind::LabeledBlock { body, .. } => self.block(body),
            ExprKind::For { iterable, body, .. } => {
                self.expr(iterable);
                self.block(body);
            }
            ExprKind::Match { scrutinee, arms } => {
                self.expr(scrutinee);
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        self.expr(g);
                    }
                    self.expr(&arm.body);
                }
            }
            ExprKind::Closure { body, .. } => self.expr(body),
            ExprKind::Question(inner) | ExprKind::Cast { expr: inner, .. } => self.expr(inner),
            ExprKind::Lock { mutex, body, .. } => {
                self.expr(mutex);
                self.block(body);
            }
            ExprKind::Tuple(items)
            | ExprKind::ArrayLiteral(items)
            | ExprKind::PrefixCollectionLiteral { items, .. } => {
                for it in items {
                    self.expr(it);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.expr(&f.value);
                }
                if let Some(s) = spread {
                    self.expr(s);
                }
            }
            _ => {}
        }
    }
}
