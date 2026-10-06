//! Scope-aware renaming of ONE binding: its declaration in a pattern, and
//! every use it reaches, stopping where an inner binding of the same name
//! shadows it.
//!
//! Moved out of `codegen/scrutinee_shadow.rs` (B-2026-09-29-41) so the
//! post-typecheck lowering can use it too (B-2026-10-05-77,
//! `lowering::rename_param_shadowing_lets`): the analyses both backends share
//! are keyed by NAME, so the cure for one name naming two bindings is the same
//! on either side of the `llvm` feature gate.

use crate::ast::*;

/// The suffix [`crate::lowering`] gives a `let` that rebinds a by-value
/// parameter's name (B-2026-10-05-77).
pub(crate) const PARAM_SHADOW_SUFFIX: &str = "__param_shadow";

/// `text` with every `<name>__param_shadow<N>` written back as `<name>`, for
/// a diagnostic composed after lowering renamed the binding: the user wrote
/// `s`, and that is the name the message must use.
pub(crate) fn display_names(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(k) = rest.find(PARAM_SHADOW_SUFFIX) {
        out.push_str(&rest[..k]);
        let tail = &rest[k + PARAM_SHADOW_SUFFIX.len()..];
        let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            out.push_str(PARAM_SHADOW_SUFFIX);
        }
        rest = &tail[digits..];
    }
    out.push_str(rest);
    out
}

pub(crate) fn pattern_binds(p: &Pattern, name: &str) -> bool {
    p.binding_names().iter().any(|b| b == name)
}

/// Rename the DECLARATION of `from` in `p`. Every alternative of an
/// or-pattern binds the same names, so each one is renamed.
pub(crate) fn rename_pattern_binding(p: &mut Pattern, from: &str, to: &str) {
    match &mut p.kind {
        PatternKind::Binding(n) => {
            if n == from {
                *n = to.to_string();
            }
        }
        PatternKind::AtBinding { name, pattern, .. } => {
            if name == from {
                *name = to.to_string();
            }
            rename_pattern_binding(pattern, from, to);
        }
        PatternKind::Struct { fields, .. } => {
            for f in fields.iter_mut() {
                match f.pattern.as_mut() {
                    Some(sub) => rename_pattern_binding(sub, from, to),
                    // Shorthand `S { s }` binds the field's name; spell it out
                    // so the field keeps its name and the binding changes.
                    None if f.name == from => {
                        f.pattern = Some(Pattern {
                            id: crate::ids::NodeId::DUMMY,
                            kind: PatternKind::Binding(to.to_string()),
                            span: f.span,
                        });
                    }
                    None => {}
                }
            }
        }
        PatternKind::TupleVariant { patterns, .. }
        | PatternKind::Tuple(patterns)
        | PatternKind::Or(patterns) => {
            for sub in patterns.iter_mut() {
                rename_pattern_binding(sub, from, to);
            }
        }
        PatternKind::Slice {
            prefix,
            rest,
            suffix,
        } => {
            for sub in prefix.iter_mut().chain(suffix.iter_mut()) {
                rename_pattern_binding(sub, from, to);
            }
            if let Some(RestPattern::Bound(n)) = rest {
                if n == from {
                    *n = to.to_string();
                }
            }
        }
        PatternKind::Wildcard | PatternKind::Literal(_) | PatternKind::RangePattern { .. } => {}
    }
}

/// Rename every USE of `from` that `b` reaches, stopping at a rebinding.
pub(crate) fn rename_uses_block(b: &mut Block, from: &str, to: &str) {
    for stmt in b.stmts.iter_mut() {
        match &mut stmt.kind {
            StmtKind::Let { pattern, value, .. } => {
                rename_uses_expr(value, from, to);
                if pattern_binds(pattern, from) {
                    return;
                }
            }
            StmtKind::LetUninit { name, .. } => {
                if name == from {
                    return;
                }
            }
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                rename_uses_expr(value, from, to);
                rename_uses_block(else_block, from, to);
                if pattern_binds(pattern, from) {
                    return;
                }
            }
            StmtKind::Defer { body } => rename_uses_block(body, from, to),
            StmtKind::ErrDefer { binding, body } => {
                if binding.as_deref() != Some(from) {
                    rename_uses_block(body, from, to);
                }
            }
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                rename_uses_expr(target, from, to);
                rename_uses_expr(value, from, to);
            }
            StmtKind::MultiAssign { targets, values } => {
                for e in targets.iter_mut().chain(values.iter_mut()) {
                    rename_uses_expr(e, from, to);
                }
            }
            StmtKind::Expr(e) => rename_uses_expr(e, from, to),
        }
    }
    if let Some(fe) = b.final_expr.as_mut() {
        rename_uses_expr(fe, from, to);
    }
}

pub(crate) fn rename_uses_expr(e: &mut Expr, from: &str, to: &str) {
    match &mut e.kind {
        ExprKind::Identifier(n) => {
            if n == from {
                *n = to.to_string();
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields.iter_mut() {
                rename_uses_expr(&mut f.value, from, to);
                if f.shorthand && !matches!(&f.value.kind, ExprKind::Identifier(v) if *v == f.name)
                {
                    f.shorthand = false;
                }
            }
            if let Some(s) = spread.as_mut() {
                rename_uses_expr(s, from, to);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            rename_uses_expr(scrutinee, from, to);
            for arm in arms.iter_mut() {
                if pattern_binds(&arm.pattern, from) {
                    continue;
                }
                if let Some(g) = arm.guard.as_mut() {
                    rename_uses_expr(g, from, to);
                }
                rename_uses_expr(&mut arm.body, from, to);
            }
        }
        ExprKind::IfLet {
            pattern,
            value,
            then_block,
            else_branch,
        } => {
            rename_uses_expr(value, from, to);
            if !pattern_binds(pattern, from) {
                rename_uses_block(then_block, from, to);
            }
            if let Some(eb) = else_branch.as_mut() {
                rename_uses_expr(eb, from, to);
            }
        }
        ExprKind::WhileLet {
            pattern,
            value,
            body,
            ..
        }
        | ExprKind::For {
            pattern,
            iterable: value,
            body,
            ..
        } => {
            rename_uses_expr(value, from, to);
            if !pattern_binds(pattern, from) {
                rename_uses_block(body, from, to);
            }
        }
        ExprKind::Closure { params, body, .. } => {
            if !params.iter().any(|p| pattern_binds(&p.pattern, from)) {
                rename_uses_expr(body, from, to);
            }
        }
        ExprKind::Lock { mutex, alias, body } => {
            // Without an alias, an `Identifier` place's own name is shadowed
            // to the inner value in the body, so renaming the place renames
            // that shadow too and the body follows it.
            rename_uses_expr(mutex, from, to);
            if alias.as_deref() != Some(from) {
                rename_uses_block(body, from, to);
            }
        }
        _ => crate::import_alias::walk_expr_children(
            e,
            &mut |c| rename_uses_expr(c, from, to),
            &mut |b| rename_uses_block(b, from, to),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::display_names;

    #[test]
    fn display_names_strips_only_the_numbered_param_shadow_suffix() {
        assert_eq!(
            display_names("value 's__param_shadow1' moved; 's__param_shadow12' too"),
            "value 's' moved; 's' too"
        );
        assert_eq!(display_names("a__param_shadow b"), "a__param_shadow b");
        assert_eq!(display_names("plain"), "plain");
    }
}
