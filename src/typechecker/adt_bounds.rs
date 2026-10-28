//! B-2026-10-28-2: a generic struct's or enum's declared parameter bounds
//! hold wherever the type is written with arguments (design.md § Generics),
//! as a type alias's do: `struct M[T: Hash + Eq]` refuses `M[f64]` at the
//! write, not only where a use of the field happens to need the bound.
//!
//! Judged like the table keys of B-2026-10-27-25 (table_keys.rs): the
//! declarations' written types in the pre-pass, under each item's bounds,
//! and a body's annotations as they are lowered. An argument that is the
//! enclosing item's type parameter needs the bound declared on it; a write
//! whose parameter the scope in hand does not cover is left for one that
//! does. Each write is judged once.

use super::types::{type_display, Type};
use super::{FixIt, TypeErrorKind};
use crate::ast::*;
use crate::resolver::SpanKey;
use crate::token::Span;
use rustc_hash::FxHashMap;
use std::rc::Rc;

/// Each bounded generic struct or enum: per parameter, in order, its name
/// and the traits it is declared with.
pub(super) type AdtBounds = FxHashMap<String, Vec<(String, Vec<String>)>>;

impl<'a> super::TypeChecker<'a> {
    pub(super) fn adt_bounds(&mut self) -> Rc<AdtBounds> {
        if let Some(b) = &self.adt_bounds {
            return b.clone();
        }
        let mut out = AdtBounds::default();
        for item in &self.program.items {
            let (name, g, w) = match item {
                Item::StructDef(s) => (&s.name, &s.generic_params, &s.where_clause),
                Item::EnumDef(e) => (&e.name, &e.generic_params, &e.where_clause),
                _ => continue,
            };
            let Some(params) = g else {
                continue;
            };
            let bounds = Self::collect_param_bounds(g, w);
            let per: Vec<(String, Vec<String>)> = params
                .params
                .iter()
                .map(|p| {
                    let traits = bounds
                        .get(&p.name)
                        .map(|bs| bs.iter().filter_map(|b| b.path.last().cloned()).collect())
                        .unwrap_or_default();
                    (p.name.clone(), traits)
                })
                .collect();
            if per.iter().any(|(_, ts)| !ts.is_empty()) {
                out.insert(name.clone(), per);
            }
        }
        let out = Rc::new(out);
        self.adt_bounds = Some(out.clone());
        out
    }

    /// Every write of a bounded struct or enum with arguments in `ty`, with
    /// its argument type expressions and the span of the write.
    pub(super) fn written_bounded_adts<'t>(
        bounded: &AdtBounds,
        ty: &'t TypeExpr,
        out: &mut Vec<(&'t str, Vec<&'t TypeExpr>, Span)>,
    ) {
        let each = |args: &'t [GenericArg], out: &mut Vec<(&'t str, Vec<&'t TypeExpr>, Span)>| {
            for a in args {
                if let GenericArg::Type(t) = a {
                    Self::written_bounded_adts(bounded, t, out);
                }
            }
        };
        match &ty.kind {
            TypeKind::Path(p) => {
                let args = p.generic_args.as_deref().unwrap_or(&[]);
                if let [name] = p.segments.as_slice() {
                    if bounded.contains_key(name) && !args.is_empty() {
                        let tys = args
                            .iter()
                            .filter_map(|a| match a {
                                GenericArg::Type(t) => Some(t),
                                _ => None,
                            })
                            .collect();
                        out.push((name.as_str(), tys, p.span));
                    }
                }
                each(args, out);
            }
            TypeKind::Tuple(ts) => ts
                .iter()
                .for_each(|t| Self::written_bounded_adts(bounded, t, out)),
            TypeKind::Array { element, .. } => Self::written_bounded_adts(bounded, element, out),
            TypeKind::Pointer { inner, .. }
            | TypeKind::Ref(inner)
            | TypeKind::MutRef(inner)
            | TypeKind::Frozen(inner)
            | TypeKind::MutSlice(inner)
            | TypeKind::Weak(inner) => Self::written_bounded_adts(bounded, inner, out),
            TypeKind::FnType {
                params,
                return_type,
                ..
            } => {
                params
                    .iter()
                    .for_each(|t| Self::written_bounded_adts(bounded, t, out));
                if let Some(r) = return_type {
                    Self::written_bounded_adts(bounded, r, out);
                }
            }
            TypeKind::ImplTrait { args, .. } => each(args, out),
            _ => {}
        }
    }

    /// The lowering-time half: a bounded struct or enum written in a body,
    /// judged under the enclosing function's bounds.
    pub(super) fn check_adt_bounds_at(&mut self, name: &str, args: &[Type], span: Span) {
        if !self.table_keys_live || args.is_empty() {
            return;
        }
        if self.checked_adt_writes.contains(&SpanKey::from_span(&span)) {
            return;
        }
        let bounded = self.adt_bounds();
        if !bounded.contains_key(name) {
            return;
        }
        let mut params = Vec::new();
        for a in args {
            self.key_params(a, &mut params);
        }
        if params
            .iter()
            .any(|p| !self.enclosing_bounds.contains_key(p))
        {
            return;
        }
        self.judge_adt_bounds(name, args, span);
    }

    pub(super) fn judge_adt_bounds(&mut self, name: &str, args: &[Type], span: Span) {
        let library = self.program.library_start.unwrap_or(usize::MAX);
        if span.offset >= library || self.current_fn_stdlib_origin {
            return;
        }
        if !self.checked_adt_writes.insert(SpanKey::from_span(&span)) {
            return;
        }
        let bounded = self.adt_bounds();
        let Some(params) = bounded.get(name) else {
            return;
        };
        for ((param, traits), arg) in params.iter().zip(args) {
            if traits.is_empty() || Self::unsettled(arg) {
                continue;
            }
            let mut named = Vec::new();
            self.key_params(arg, &mut named);
            let bare = match arg {
                Type::TypeParam(p) => Some(p),
                Type::Named { name: p, args } if args.is_empty() && named == [p.clone()] => Some(p),
                _ => None,
            };
            let mut fix = None;
            let message = match (bare, named.is_empty()) {
                // The enclosing item's own parameter: its declared bounds.
                (Some(p), _) => {
                    let missing: Vec<&str> = traits
                        .iter()
                        .filter(|t| !self.type_param_satisfies_bound(p, t))
                        .map(String::as_str)
                        .collect();
                    if missing.is_empty() {
                        continue;
                    }
                    let missing = missing.join(" + ");
                    fix = self.param_bound_edit(span.offset, p, &missing).map(|edit| {
                        let at = self.param_decl_span(span.offset, p).unwrap_or(span);
                        FixIt {
                            span: Span {
                                offset: edit.offset,
                                length: edit.length,
                                line: at.line,
                                column: at.column,
                            },
                            replacement: edit.replacement,
                        }
                    });
                    format!(
                        "`{name}` requires its parameter `{param}` to implement `{missing}`, but \
                         the type parameter `{p}` given for it is not bounded by `{missing}`: \
                         declare `{p}: {missing}` where `{p}` is declared"
                    )
                }
                // A compound naming a parameter (`Vec[T]`) depends on impls
                // its bounds do not settle here.
                (None, false) => continue,
                (None, true) => {
                    let missing: Vec<&str> = traits
                        .iter()
                        .filter(|t| !self.type_satisfies_bound(arg, t))
                        .map(String::as_str)
                        .collect();
                    if missing.is_empty() {
                        continue;
                    }
                    format!(
                        "`{name}` requires its parameter `{param}` to implement `{}`, but `{}` \
                         does not implement `{}`",
                        traits.join(" + "),
                        type_display(arg),
                        missing.join(" + "),
                    )
                }
            };
            match fix {
                Some(fix) => self.type_error_with_fix_it(
                    message,
                    span,
                    TypeErrorKind::TraitBoundNotSatisfied,
                    fix,
                ),
                None => self.type_error(message, span, TypeErrorKind::TraitBoundNotSatisfied),
            }
        }
    }

    /// A type still being solved, or one an earlier error left: nothing to
    /// judge yet.
    fn unsettled(t: &Type) -> bool {
        match t {
            Type::Error | Type::TypeVar(_) => true,
            Type::Named { args, .. } => args.iter().any(Self::unsettled),
            Type::Tuple(ts) => ts.iter().any(Self::unsettled),
            Type::Ref(t) | Type::MutRef(t) => Self::unsettled(t),
            _ => false,
        }
    }
}
