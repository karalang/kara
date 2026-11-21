//! `self_rebinding`: a `let` that binds names to themselves and nothing
//! else, `let n = n;` or `let (a, b) = (a, b);`. Rust code writes `let n =
//! n as i64;` to turn a `usize` into an `i64`; a length is already an
//! `i64` in Kāra, so `karac fix` deletes the cast as redundant and used to
//! leave `let n = n;` behind, a statement that does nothing (katas 2851,
//! 2852). The lint names it and the fix deletes the statement, with its
//! line when it stands alone.
//!
//! Deleting it is the same program only when the name it shadows is not
//! `mut`: `let x = x;` after `let mut x` freezes `x`, an idiom worth
//! keeping. So the lint fires only in a function where no binding of any
//! of its names is `mut`, which a parameter never is.

use crate::ast::*;
use crate::span_visitor::MutVisitor;

use super::{FixIt, Span, TypeErrorKind};

/// Every name a `mut` binding in the body binds, and every candidate
/// statement: its start offset and the names it rebinds.
#[derive(Default)]
struct Scan {
    mutable: std::collections::HashSet<String>,
    candidates: Vec<(Span, Vec<String>)>,
}

impl MutVisitor for Scan {
    fn span(&mut self, _span: &mut Span) {}

    fn stmt(&mut self, s: &mut Stmt) {
        match &s.kind {
            StmtKind::Let {
                is_mut: true,
                pattern,
                ..
            } => self.mutable.extend(pattern.binding_names()),
            StmtKind::LetUninit {
                is_mut: true, name, ..
            } => {
                self.mutable.insert(name.clone());
            }
            StmtKind::Let {
                is_mut: false,
                ty: None,
                pattern,
                value,
            } if binds_names_to_themselves(pattern, value) => {
                self.candidates.push((s.span, pattern.binding_names()));
            }
            _ => {}
        }
    }
}

impl<'a> super::TypeChecker<'a> {
    /// The `self_rebinding` lint over one function's body (see the module
    /// documentation).
    pub(super) fn lint_self_rebinding(&mut self, f: &Function) {
        if f.stdlib_origin || self.program.self_rebinding_lets.is_empty() {
            return;
        }
        let mut body = f.clone();
        let mut scan = Scan::default();
        crate::span_visitor::walk_function_mut_with(&mut body, &mut scan);
        for (span, names) in scan.candidates {
            if names.iter().any(|n| scan.mutable.contains(n)) {
                continue;
            }
            let Some(&whole) = self.program.self_rebinding_lets.get(&span.offset) else {
                continue;
            };
            let shown = if names.len() == 1 {
                format!("`{}`", names[0])
            } else {
                names
                    .iter()
                    .map(|n| format!("`{n}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            self.type_lint_warning_with_fix(
                format!(
                    "this `let` binds {shown} to {}, and nothing else: the names \
                     already hold these values, so the statement can go",
                    if names.len() == 1 {
                        "itself"
                    } else {
                        "themselves"
                    }
                ),
                span,
                TypeErrorKind::StyleLint,
                "self_rebinding",
                Some(FixIt {
                    span: whole,
                    replacement: String::new(),
                }),
            );
        }
    }
}
