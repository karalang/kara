//! B-2026-10-27-25: `K: Hash + Eq` is a bound of every hash table, checked
//! wherever a `Map[K, …]` or `Set[K, …]` is written (collections.md § Map),
//! not only where a method is called on one.
//!
//! A key that is, or contains, a type parameter needs the bound on the item
//! that declares the parameter: `struct Matcher[T] { b2j: Map[T, Vec[i64]] }`
//! is refused, and `karac fix` writes `T: Hash + Eq` there. Every impl or fn
//! that writes `Map[T, _]` declares its own `T` and needs its own bound. A
//! concrete key needs the impls: floats have none, a struct needs
//! `#[derive(Hash, PartialEq, Eq)]` or the impls by hand, and a generic
//! type's derive holds only at arguments that have the trait.
//!
//! Two halves. [`TypeChecker::check_declared_table_keys`] walks every
//! declaration's written types before bodies are checked, with that item's
//! bounds in scope, since a struct's fields are lowered once while the type
//! environment is built, before any bound is known. Body annotations are
//! judged as they are lowered ([`TypeChecker::check_table_key_at`]), where the
//! enclosing function's bounds are in scope. Each write is judged once.

use super::types::{type_display, Type};
use super::{FixIt, TypeErrorKind};
use crate::ast::*;
use crate::resolver::SpanKey;
use crate::token::Span;
use rustc_hash::FxHashMap;

impl<'a> super::TypeChecker<'a> {
    /// The pre-pass over declarations. Runs after the type environment is
    /// built and before item bodies, so every impl and derive is known.
    pub(super) fn check_declared_table_keys(&mut self) {
        let program = self.program;
        for item in &program.items {
            match item {
                Item::StructDef(s) => {
                    let tys: Vec<&TypeExpr> = s.fields.iter().map(|f| &f.ty).collect();
                    self.table_keys_in(&[(&s.generic_params, &s.where_clause)], &tys);
                }
                Item::EnumDef(e) => {
                    let mut tys: Vec<&TypeExpr> = Vec::new();
                    for v in &e.variants {
                        match &v.kind {
                            VariantKind::Unit => {}
                            VariantKind::Tuple(ts) => tys.extend(ts.iter()),
                            VariantKind::Struct(fs) => tys.extend(fs.iter().map(|f| &f.ty)),
                        }
                    }
                    self.table_keys_in(&[(&e.generic_params, &e.where_clause)], &tys);
                }
                Item::TypeAlias(t) => self.table_keys_in(&[(&t.generic_params, &None)], &[&t.ty]),
                Item::Function(f) => {
                    let tys = Self::signature_types(f);
                    self.table_keys_in(&[(&f.generic_params, &f.where_clause)], &tys);
                }
                Item::ImplBlock(imp) => {
                    let outer = (&imp.generic_params, &imp.where_clause);
                    self.table_keys_in(&[outer], &[&imp.target_type]);
                    for it in &imp.items {
                        match it {
                            ImplItem::Method(f) => {
                                let tys = Self::signature_types(f);
                                self.table_keys_in(
                                    &[outer, (&f.generic_params, &f.where_clause)],
                                    &tys,
                                );
                            }
                            ImplItem::AssocType(b) => self.table_keys_in(
                                &[outer, (&b.generic_params, &b.where_clause)],
                                &[&b.ty],
                            ),
                        }
                    }
                }
                Item::TraitDef(t) => {
                    let outer = (&t.generic_params, &t.where_clause);
                    for it in &t.items {
                        if let TraitItem::Method(m) = it {
                            let mut tys: Vec<&TypeExpr> = m.params.iter().map(|p| &p.ty).collect();
                            tys.extend(m.return_type.as_ref());
                            self.table_keys_in(
                                &[outer, (&m.generic_params, &m.where_clause)],
                                &tys,
                            );
                        }
                    }
                }
                _ => {}
            }
        }
        self.table_keys_live = true;
    }

    fn signature_types(f: &Function) -> Vec<&TypeExpr> {
        let mut tys: Vec<&TypeExpr> = f.params.iter().map(|p| &p.ty).collect();
        tys.extend(f.return_type.as_ref());
        tys
    }

    /// Judge every table written in `tys` under the bounds of `scopes`,
    /// outermost first.
    fn table_keys_in(
        &mut self,
        scopes: &[(&Option<GenericParams>, &Option<WhereClause>)],
        tys: &[&TypeExpr],
    ) {
        let mut tables: Vec<(&str, &TypeExpr, Span)> = Vec::new();
        let bounded = self.adt_bounds();
        let mut adts: Vec<(&str, Vec<&TypeExpr>, Span)> = Vec::new();
        for t in tys {
            Self::written_tables(t, &mut tables);
            Self::written_bounded_adts(&bounded, t, &mut adts);
        }
        if tables.is_empty() && adts.is_empty() {
            return;
        }
        let mut bounds: FxHashMap<String, Vec<TraitBound>> = FxHashMap::default();
        let mut gp: Vec<String> = Vec::new();
        for (g, w) in scopes {
            for (name, bs) in Self::collect_param_bounds(g, w) {
                bounds.insert(name, bs);
            }
            gp.extend(Self::generic_param_names(g));
        }
        let saved = std::mem::replace(&mut self.enclosing_bounds, bounds);
        for (name, key, span) in tables {
            // The key is lowered only to be judged: what lowering reports
            // about it is reported where the type is lowered for real.
            let (errors, warnings) = (self.errors.len(), self.warnings.len());
            let hashers = self.checked_container_hashers.clone();
            let key_ty = self.lower_type_expr(key, &gp);
            self.errors.truncate(errors);
            self.warnings.truncate(warnings);
            self.checked_container_hashers = hashers;
            self.judge_table_key(name, &key_ty, span);
        }
        // The bounds of a generic struct or enum written with arguments
        // (B-2026-10-28-2), judged the same way.
        for (name, args, span) in adts {
            let (errors, warnings) = (self.errors.len(), self.warnings.len());
            let hashers = self.checked_container_hashers.clone();
            let arg_tys: Vec<Type> = args.iter().map(|a| self.lower_type_expr(a, &gp)).collect();
            self.errors.truncate(errors);
            self.warnings.truncate(warnings);
            self.checked_container_hashers = hashers;
            self.judge_adt_bounds(name, &arg_tys, span);
        }
        self.enclosing_bounds = saved;
    }

    /// Every `Map[K, …]` / `Set[K, …]` written in `ty`, with its key's type
    /// expression and the span of the whole write.
    fn written_tables<'t>(ty: &'t TypeExpr, out: &mut Vec<(&'t str, &'t TypeExpr, Span)>) {
        let each = |args: &'t [GenericArg], out: &mut Vec<(&'t str, &'t TypeExpr, Span)>| {
            for a in args {
                if let GenericArg::Type(t) = a {
                    Self::written_tables(t, out);
                }
            }
        };
        match &ty.kind {
            TypeKind::Path(p) => {
                let args = p.generic_args.as_deref().unwrap_or(&[]);
                if let ([name], Some(GenericArg::Type(key))) = (p.segments.as_slice(), args.first())
                {
                    if name == "Map" || name == "Set" {
                        out.push((name.as_str(), key, p.span));
                    }
                }
                each(args, out);
            }
            TypeKind::Tuple(ts) => ts.iter().for_each(|t| Self::written_tables(t, out)),
            TypeKind::Array { element, .. } => Self::written_tables(element, out),
            TypeKind::Pointer { inner, .. }
            | TypeKind::Ref(inner)
            | TypeKind::MutRef(inner)
            | TypeKind::Frozen(inner)
            | TypeKind::MutSlice(inner)
            | TypeKind::Weak(inner) => Self::written_tables(inner, out),
            TypeKind::FnType {
                params,
                return_type,
                ..
            } => {
                params.iter().for_each(|t| Self::written_tables(t, out));
                if let Some(r) = return_type {
                    Self::written_tables(r, out);
                }
            }
            TypeKind::ImplTrait { args, .. } => each(args, out),
            _ => {}
        }
    }

    /// The lowering-time half: a table written in a body, judged under the
    /// enclosing function's bounds. A write whose key names a type parameter
    /// those bounds do not cover is left for a scope that has them.
    pub(super) fn check_table_key_at(&mut self, name: &str, args: &[Type], span: Span) {
        if !self.table_keys_live || (name != "Map" && name != "Set") {
            return;
        }
        let Some(key) = args.first() else {
            return;
        };
        if self.checked_table_keys.contains(&SpanKey::from_span(&span)) {
            return;
        }
        let mut params = Vec::new();
        self.key_params(key, &mut params);
        if params
            .iter()
            .any(|p| !self.enclosing_bounds.contains_key(p))
        {
            return;
        }
        self.judge_table_key(name, key, span);
    }

    /// The type parameters `key` names: a `TypeParam`, or the bare name a
    /// body annotation lowers an enclosing parameter to.
    pub(super) fn key_params(&self, key: &Type, out: &mut Vec<String>) {
        match key {
            Type::TypeParam(p) => out.push(p.clone()),
            Type::Named { name, args } => {
                if args.is_empty()
                    && self.enclosing_bounds.contains_key(name)
                    && !self.env.structs.contains_key(name)
                    && !self.env.enums.contains_key(name)
                {
                    out.push(name.clone());
                }
                args.iter().for_each(|a| self.key_params(a, out));
            }
            Type::Tuple(ts) => ts.iter().for_each(|t| self.key_params(t, out)),
            Type::Array { element, .. }
            | Type::Vector { element, .. }
            | Type::Slice { element, .. } => self.key_params(element, out),
            Type::Ref(t) | Type::MutRef(t) | Type::Rc(t) | Type::Arc(t) => self.key_params(t, out),
            Type::Refinement { base, .. } => self.key_params(base, out),
            _ => {}
        }
    }

    fn judge_table_key(&mut self, name: &str, key: &Type, span: Span) {
        if !self.checked_table_keys.insert(SpanKey::from_span(&span)) {
            return;
        }
        let what = if name == "Map" { "key" } else { "element" };
        let mut params = Vec::new();
        self.key_params(key, &mut params);
        params.dedup();
        if !params.is_empty() {
            for p in params {
                let missing: Vec<&str> = ["Hash", "Eq"]
                    .into_iter()
                    .filter(|t| !self.type_param_satisfies_bound(&p, t))
                    .collect();
                if missing.is_empty() {
                    continue;
                }
                let missing = missing.join(" + ");
                let decl = self.param_decl_span(span.offset, &p);
                // One report per declaration: its other tables need the same
                // bound, and the one edit gives it to all of them.
                let at = decl.unwrap_or(span);
                if !self
                    .reported_table_key_params
                    .insert(SpanKey::from_span(&at))
                {
                    continue;
                }
                let message = format!(
                    "the {what} type `{}` of this `{name}` must implement `Hash + Eq`, but the \
                     type parameter `{p}` is not bounded by `{missing}`: declare `{p}: {missing}` \
                     where `{p}` is declared. Every item that writes a `Map` or `Set` keyed by \
                     `{p}` needs the bound (collections.md § Map)",
                    type_display(key)
                );
                match self.param_bound_edit(span.offset, &p, &missing) {
                    Some(edit) => self.type_error_with_fix_it(
                        message,
                        span,
                        TypeErrorKind::TraitBoundNotSatisfied,
                        FixIt {
                            span: Span {
                                offset: edit.offset,
                                length: edit.length,
                                line: at.line,
                                column: at.column,
                            },
                            replacement: edit.replacement,
                        },
                    ),
                    None => self.type_error(message, span, TypeErrorKind::TraitBoundNotSatisfied),
                }
            }
            return;
        }
        let (hash, eq) = (self.type_supports_hash(key), self.type_supports_eq(key));
        if hash && eq {
            return;
        }
        let shown = type_display(key);
        self.reported_table_keys.insert(shown.clone());
        let missing = match (hash, eq) {
            (false, false) => "`Hash` and `Eq`",
            (false, true) => "`Hash`",
            _ => "`Eq`",
        };
        let hint = match key {
            Type::Float(_) => " A float has neither: `NaN != NaN`. Key by its bits \
                               (`x.to_bits()`) or use an integer."
                .to_string(),
            Type::Named { name: n, args } if !args.is_empty() => {
                format!(" A derive on `{n}` holds only where its type arguments have the traits.")
            }
            // A key with half the pair written by hand is told which half is
            // left (B-2026-08-26-10): the table dispatches to those impls.
            Type::Named { name: n, .. } | Type::Shared(n)
                if ["Hash", "PartialEq", "Eq"]
                    .iter()
                    .any(|t| self.env.has_impl(t, n, &[])) =>
            {
                let has = |t: &str| self.env.has_impl(t, n, &[]);
                let written: Vec<&str> = ["Hash", "PartialEq", "Eq"]
                    .into_iter()
                    .filter(|t| has(t))
                    .collect();
                let left = if !has("Hash") {
                    "`impl Hash`"
                } else if !has("PartialEq") {
                    "`impl PartialEq`"
                } else {
                    "the `impl Eq` marker"
                };
                format!(
                    " You have written `impl {} for {n}`, and the table uses your impls once \
                     the pair is complete: add {left} (or `#[derive(Hash, PartialEq, Eq)]`, \
                     which compares fields instead).",
                    written.join("` and `impl ")
                )
            }
            Type::Named { name: n, .. } | Type::Shared(n)
                if self.env.structs.contains_key(n) || self.env.enums.contains_key(n) =>
            {
                format!(
                    " Derive them on `{n}` (`#[derive(Hash, PartialEq, Eq)]`, which needs \
                     every field to have them), or write the impls."
                )
            }
            _ => String::new(),
        };
        let message = format!(
            "the {what} type `{shown}` of this `{name}` must implement `Hash + Eq`, but it \
             does not implement {missing}.{hint}"
        );
        match self.table_key_derive_fix(key, hash, eq) {
            Some(fix) => self.type_error_with_fix_it(
                message,
                span,
                TypeErrorKind::TraitBoundNotSatisfied,
                fix,
            ),
            None => self.type_error(message, span, TypeErrorKind::TraitBoundNotSatisfied),
        }
    }

    /// The declaration of type parameter `param` in the item around
    /// `offset`, innermost first.
    pub(super) fn param_decl_span(&self, offset: usize, param: &str) -> Option<Span> {
        self.param_scopes_at(offset)
            .into_iter()
            .flatten()
            .flat_map(|g| g.params.iter())
            .find(|g| g.name == param && !g.is_const)
            .map(|g| g.span)
            .filter(|s| s.length > 0)
    }

    /// `#[derive(…)]` for a non-generic struct or enum of the program whose
    /// fields all have what the derive needs.
    fn table_key_derive_fix(&self, key: &Type, hash: bool, eq: bool) -> Option<FixIt> {
        let name = match key {
            Type::Named { name, args } if args.is_empty() => name,
            Type::Shared(name) => name,
            _ => return None,
        };
        let library = self.program.library_start.unwrap_or(usize::MAX);
        let library = library.min(crate::prelude::gated_span_base(0));
        let (span, fields, derived) = self.program.items.iter().find_map(|item| match item {
            Item::StructDef(s) if &s.name == name && s.generic_params.is_none() => {
                let info = self.env.structs.get(name)?;
                let fields: Vec<Type> = info.fields.iter().map(|(_, t, _)| t.clone()).collect();
                Some((s.span, fields, info.derived_traits.clone()))
            }
            Item::EnumDef(e) if &e.name == name && e.generic_params.is_none() => {
                let info = self.env.enums.get(name)?;
                let fields: Vec<Type> = info
                    .variants
                    .iter()
                    .flat_map(|(_, v)| match v {
                        super::types::VariantTypeInfo::Unit => Vec::new(),
                        super::types::VariantTypeInfo::Tuple(ts) => ts.clone(),
                        super::types::VariantTypeInfo::Struct(fs) => {
                            fs.iter().map(|(_, t)| t.clone()).collect()
                        }
                    })
                    .collect();
                Some((e.span, fields, info.derived_traits.clone()))
            }
            _ => None,
        })?;
        if span.length == 0 || span.column == 0 || span.offset >= library {
            return None;
        }
        if self.env.has_user_impl_hash(name) || self.env.has_impl("PartialEq", name, &[]) {
            return None;
        }
        if !fields
            .iter()
            .all(|f| self.type_supports_hash(f) && self.type_supports_eq(f))
        {
            return None;
        }
        let mut wanted: Vec<&str> = Vec::new();
        if !hash {
            wanted.push("Hash");
        }
        if !eq {
            for t in ["PartialEq", "Eq"] {
                if !derived.contains(t) {
                    wanted.push(t);
                }
            }
        }
        let indent = " ".repeat(span.column - 1);
        Some(FixIt {
            span: Span {
                offset: span.offset - (span.column - 1),
                length: 0,
                line: span.line,
                column: 1,
            },
            replacement: format!("{indent}#[derive({})]\n", wanted.join(", ")),
        })
    }
}
