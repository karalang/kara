//! UFCS: a method called with its receiver as the first argument.
//!
//! design.md § Method Resolution: `User.validate(user)` is the call
//! `user.validate()`, and `Shape.area(c)` names the trait whose method it
//! calls, skipping lookup. Both are typed here as the receiver form, the
//! trait form with lookup restricted to the impls of the named trait, and
//! recorded in `ufcs_calls` so that `lowering` rewrites the call into the
//! receiver form every later stage reads (B-2026-10-28-17, -20).
//!
//! Lives in a sibling `impl<'a> super::TypeChecker<'a>` block.

use crate::ast::*;
use crate::resolver::SpanKey;
use crate::token::Span;

use super::types::{type_display, Type};
use super::{FixIt, TypeErrorKind};

impl<'a> super::TypeChecker<'a> {
    /// Type `Trait.method(recv, ..)` or `Type.method(recv, ..)` when
    /// `method` takes `self`. `None` when the receiver is not such a name,
    /// or the method is an associated function, which the type-receiver
    /// path types.
    ///
    /// `Plain.twice(p)` parses as a call of the path `Plain.twice`, and
    /// `plain.twice(p)` as a method call on the identifier `plain`; both
    /// come here with `name` the first segment.
    pub(super) fn try_ufcs_method(
        &mut self,
        name: &str,
        method: &str,
        args: &[CallArg],
        span: &Span,
        args_close_span: &Span,
    ) -> Option<Type> {
        self.try_ufcs_method_of(name, None, method, args, span, args_close_span)
    }

    /// `Conv[String].conv(n)`: a generic trait named with its arguments,
    /// which pick the one impl of that instance (design.md §9 UFCS,
    /// B-2026-11-08-77). `None` unless `object` is a trait so named.
    pub(super) fn try_ufcs_trait_instance(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        span: &Span,
        args_close_span: &Span,
    ) -> Option<Type> {
        let ExprKind::Path {
            segments,
            generic_args: Some(generic_args),
        } = &object.kind
        else {
            return None;
        };
        let [name] = segments.as_slice() else {
            return None;
        };
        if self.local_scope.lookup(name).is_some()
            || !self.env.traits.contains_key(name.as_str())
            || self.env.structs.contains_key(name.as_str())
            || self.env.enums.contains_key(name.as_str())
        {
            return None;
        }
        let trait_args: Vec<Type> = generic_args
            .iter()
            .filter_map(|a| match a {
                GenericArg::Type(t) => Some(self.lower_type_expr(t, &[])),
                _ => None,
            })
            .collect();
        self.try_ufcs_method_of(name, Some(trait_args), method, args, span, args_close_span)
    }

    fn try_ufcs_method_of(
        &mut self,
        name: &str,
        trait_args: Option<Vec<Type>>,
        method: &str,
        args: &[CallArg],
        span: &Span,
        args_close_span: &Span,
    ) -> Option<Type> {
        if self.local_scope.lookup(name).is_some() {
            return None;
        }
        // `Wrap.get(w)` through `type Wrap = W` names `W` (B-2026-11-04-50).
        let alias = name;
        let canon = self.unaliased_type_name(name);
        let name = canon.as_str();
        let is_type = self.env.structs.contains_key(name) || self.env.enums.contains_key(name);
        let trait_name =
            (!is_type && self.env.traits.contains_key(alias)).then(|| alias.to_string());
        // `Clone` has no declaration for the impl lookup to narrow to: every
        // `clone` is its method, so `Clone.clone(s)` is `s.clone()`
        // (B-2026-11-09-32). It was typed as an error with no diagnostic, and
        // MIR refused the program `karac check` had passed.
        let builtin_clone = trait_name.is_none()
            && !is_type
            && alias == "Clone"
            && method == "clone"
            && self.local_scope.lookup("Clone").is_none();
        // `i64.abs(x)`, `Vec.len(v)`, `char.is_alphabetic(c)`: a built-in
        // type's own method, which no impl declares (B-2026-11-25-40).
        let builtin = trait_name.is_none()
            && alias == name
            && trait_args.is_none()
            && builtin_type_has_method(name, method);
        let takes_self = match &trait_name {
            Some(t) => self.trait_method_takes_self(t, method)?,
            None if is_type || builtin => match self.type_method_takes_self(name, method) {
                Some(b) => b,
                None if builtin => true,
                None => return None,
            },
            None if builtin_clone => true,
            None => return None,
        };
        if !takes_self {
            return None;
        }
        let Some((recv, rest)) = args.split_first() else {
            // A struct with no fields has one value, `A {}`: Rust's unit
            // struct `A` called as `A.hi()` means that value's method
            // (B-2026-11-20-20).
            if is_type
                && alias == name
                && self
                    .env
                    .structs
                    .get(name)
                    .is_some_and(|s| s.fields.is_empty())
            {
                let at = span.offset + alias.len();
                self.type_error_with_fix_it(
                    format!(
                        "`{name}.{method}` takes `self`, and `{name}` has no fields: its one \
                         value is `{name} {{}}`, so call `{name} {{}}.{method}()` (Rust's unit \
                         struct value `{name}` is written `{name} {{}}`)"
                    ),
                    *span,
                    TypeErrorKind::WrongNumberOfArgs,
                    FixIt {
                        span: Span {
                            offset: at,
                            length: 0,
                            column: span.column + alias.len(),
                            ..*span
                        },
                        replacement: " {}".to_string(),
                    },
                );
                return Some(Type::Error);
            }
            self.type_error(
                format!(
                    "`{name}.{method}` takes `self`: pass the receiver as the first \
                     argument, `{name}.{method}(x)`, or call `x.{method}()`"
                ),
                *span,
                TypeErrorKind::WrongNumberOfArgs,
            );
            return Some(Type::Error);
        };
        // The call is the method call now: its callee is the impl method
        // the receiver form picks, not the path `Plain.twice`, which names
        // the trait's declaration.
        // A check against an expected type records the path as the callee
        // again after this unless the node is marked (B-2026-11-19-14:
        // `fn r(c: C) -> i64 { Reader.access(c) }` ran an impl method MIR
        // never built).
        if let Some((node, key)) = self.current_call_node {
            if key == SpanKey::from_span(span) {
                self.node_call_callees.remove(&node);
                self.node_method_callees.remove(&node);
                self.routed_trait_calls.insert(node);
            }
        }
        // design.md § Call-site `mut` markers: a UFCS call marks its
        // receiver, `Counter.bump(mut c)`, when the method takes `mut ref
        // self` (B-2026-11-04-69).
        let in_program = recv.value.span.offset < self.program.library_start.unwrap_or(usize::MAX);
        if !recv.mut_marker
            && (self.cli_lint_overrides.strict_core
                || (self.library_methods_from_source && in_program))
            && self.ufcs_self_mode(is_type.then_some(name), trait_name.as_deref(), method)
                == Some(SelfParam::MutRef)
        {
            let mut edit_span = recv.value.span;
            edit_span.length = 0;
            self.type_error_with_fix_it(
                format!(
                    "`{alias}.{method}` takes `mut ref self`, so its receiver is marked: \
                     write `{alias}.{method}(mut <expr>, ..)` (design.md § Call-site `mut` \
                     markers)"
                ),
                recv.span,
                TypeErrorKind::MissingMutMarker,
                FixIt {
                    span: edit_span,
                    replacement: "mut ".to_string(),
                },
            );
        }
        let errors = self.errors.len();
        let saved = self.env.ufcs_trait.take();
        let saved_args = self.env.ufcs_trait_args.take();
        self.env.ufcs_trait_args = trait_name.as_ref().and(trait_args);
        self.env.ufcs_trait = trait_name.map(|t| (t, method.to_string()));
        // `i32.wrapping_add(1, 2)`: a bare literal receiver is the named
        // type's, as `lowering::rewrite_ufcs_call` writes it.
        let pinned = builtin.then(|| recv.value.clone()).and_then(|mut e| {
            crate::lowering::pin_ufcs_literal_receiver(name, &mut e).then_some(e)
        });
        let recv_expr = pinned.as_ref().unwrap_or(&recv.value);
        let ret = self.infer_method_call(recv_expr, method, rest, span, args_close_span);
        self.env.ufcs_trait = saved;
        self.env.ufcs_trait_args = saved_args;
        // The name as written: `lowering` matches it against the callee.
        self.ufcs_calls
            .insert(SpanKey::from_span(span), alias.to_string());
        // `i64.abs(x)` calls `i64`'s method, so `x` is an `i64`.
        if builtin && !is_type && self.errors.len() == errors {
            let rt = self
                .expr_types
                .get(&SpanKey::from_span(&recv.value.span))
                .cloned()
                .unwrap_or(Type::Error);
            let rt = super::inference::resolve_type_var_top(&rt, &self.env.substitutions);
            let mut base = &rt;
            while let Type::Ref(inner) | Type::MutRef(inner) = base {
                base = inner;
            }
            let fits = match (self.primitive_type(name), base) {
                (_, Type::Error | Type::TypeVar(_)) => true,
                (Some(Type::Str), Type::Str) => true,
                (Some(Type::Str) | Some(Type::Named { .. }), Type::Named { name: n, .. })
                    if matches!(name, "String" | "Str") =>
                {
                    n == "StringSlice"
                }
                (Some(Type::Named { .. }), Type::Str) => name == "Str",
                (Some(want), got) => want == *got,
                (None, Type::Named { name: n, .. }) => n.as_str() == name,
                (None, _) => false,
            };
            if !fits {
                self.type_error(
                    format!(
                        "`{name}.{method}` takes a `{name}` receiver, found `{}`",
                        type_display(&rt)
                    ),
                    recv.value.span,
                    TypeErrorKind::TypeMismatch,
                );
                return Some(Type::Error);
            }
        }
        // `User.validate(x)` calls `User`'s method, so `x` is a `User`.
        if is_type && self.errors.len() == errors {
            let rt = self
                .expr_types
                .get(&SpanKey::from_span(&recv.value.span))
                .cloned()
                .unwrap_or(Type::Error);
            let mut base = &rt;
            while let Type::Ref(inner) | Type::MutRef(inner) = base {
                base = inner;
            }
            let fits = match base {
                Type::Named { name: n, .. } => n.as_str() == name,
                Type::Error => true,
                _ => false,
            };
            if !fits {
                self.type_error(
                    format!(
                        "`{name}.{method}` takes a `{name}` receiver, found `{}`",
                        type_display(&rt)
                    ),
                    recv.value.span,
                    TypeErrorKind::TypeMismatch,
                );
                return Some(Type::Error);
            }
        }
        Some(ret)
    }

    /// `name`, or the type a non-generic alias of it names (`type Wrap =
    /// W`), followed through aliases of aliases.
    pub(super) fn unaliased_type_name(&self, name: &str) -> String {
        let mut cur = name.to_string();
        for _ in 0..8 {
            match self.env.type_aliases.get(&cur) {
                Some(Type::Named { name: to, args }) if args.is_empty() && *to != cur => {
                    cur = to.clone();
                }
                _ => break,
            }
        }
        cur
    }

    /// `srt[i64]` used as a value: a generic function item indexed by a type
    /// name. design.md §8 writes no type arguments at a use site, so the
    /// message says so rather than reading `i64` as a value
    /// (B-2026-11-04-69).
    pub(super) fn call_site_type_args_on_fn_value(
        &self,
        object: &Expr,
        index: &Expr,
    ) -> Option<String> {
        let ExprKind::Identifier(f) = &object.kind else {
            return None;
        };
        let ExprKind::Identifier(t) = &index.kind else {
            return None;
        };
        if self.local_scope.lookup(f).is_some() || self.local_scope.lookup(t).is_some() {
            return None;
        }
        let generic_fn = self.program.items.iter().any(|item| {
            matches!(item, Item::Function(func) if func.name == *f
                && func.generic_params.as_ref().is_some_and(|g| !g.params.is_empty()))
        });
        let names_type = super::types::is_prelude_type_or_module_name(t)
            || self.env.structs.contains_key(t.as_str())
            || self.env.enums.contains_key(t.as_str())
            || matches!(
                t.as_str(),
                "i8" | "i16"
                    | "i32"
                    | "i64"
                    | "i128"
                    | "u8"
                    | "u16"
                    | "u32"
                    | "u64"
                    | "u128"
                    | "usize"
                    | "isize"
                    | "f16"
                    | "bf16"
                    | "f32"
                    | "f64"
                    | "bool"
                    | "char"
            );
        (generic_fn && names_type).then(|| {
            format!(
                "`{f}[{t}]`: a function takes no type arguments where it is used \
                 (design.md §8 Generic parameters and arguments); the arguments or the expected type \
                 decide them. Annotate the binding, `let g: Fn(..) -> .. = {f};`, or call \
                 `{f}(x)`"
            )
        })
    }

    /// The receiver mode of the method a UFCS call names: the type's
    /// inherent method first, then the trait's declaration.
    fn ufcs_self_mode(
        &self,
        type_name: Option<&str>,
        trait_name: Option<&str>,
        method: &str,
    ) -> Option<SelfParam> {
        if let Some(t) = trait_name {
            return self
                .find_trait_method(t, method)
                .and_then(|m| m.self_param.clone());
        }
        let type_name = type_name?;
        for item in &self.program.items {
            let Item::ImplBlock(imp) = item else { continue };
            let TypeKind::Path(path) = &imp.target_type.kind else {
                continue;
            };
            if path
                .segments
                .last()
                .map(|n| self.unaliased_type_name(n))
                .as_deref()
                != Some(type_name)
            {
                continue;
            }
            for ii in &imp.items {
                if let ImplItem::Method(f) = ii {
                    if f.name == method {
                        return f.self_param.clone();
                    }
                }
            }
        }
        None
    }

    /// Whether `Trait.method` takes `self`, or `None` when the trait (or a
    /// supertrait) declares no such method.
    pub(super) fn trait_method_takes_self(&self, trait_name: &str, method: &str) -> Option<bool> {
        if let Some(m) = self.find_trait_method(trait_name, method) {
            return Some(m.self_param.is_some());
        }
        None
    }

    /// Whether the method `Type.method` takes `self`: an inherent method of
    /// the program's, or the method of a trait the type implements.
    /// The receiver an inherent method `type_name.method` declares, or
    /// `None` for an associated function (no `self`) or no such method.
    pub(super) fn inherent_self_param(&self, type_name: &str, method: &str) -> Option<SelfParam> {
        for item in &self.program.items {
            let Item::ImplBlock(imp) = item else { continue };
            let TypeKind::Path(path) = &imp.target_type.kind else {
                continue;
            };
            if imp.trait_name.is_some()
                || path
                    .segments
                    .last()
                    .map(|n| self.unaliased_type_name(n))
                    .as_deref()
                    != Some(type_name)
            {
                continue;
            }
            for ii in &imp.items {
                if let ImplItem::Method(f) = ii {
                    if f.name == method {
                        return f.self_param.clone();
                    }
                }
            }
        }
        None
    }

    fn type_method_takes_self(&self, type_name: &str, method: &str) -> Option<bool> {
        for item in &self.program.items {
            let Item::ImplBlock(imp) = item else { continue };
            let TypeKind::Path(path) = &imp.target_type.kind else {
                continue;
            };
            if imp.trait_name.is_some()
                || path
                    .segments
                    .last()
                    .map(|n| self.unaliased_type_name(n))
                    .as_deref()
                    != Some(type_name)
            {
                continue;
            }
            for ii in &imp.items {
                if let ImplItem::Method(f) = ii {
                    if f.name == method {
                        return Some(f.self_param.is_some());
                    }
                }
            }
        }
        let traits: Vec<String> = self
            .env
            .impls
            .iter()
            .filter(|imp| imp.target_type == type_name && imp.methods.contains_key(method))
            .filter_map(|imp| imp.trait_name.clone())
            .collect();
        traits
            .iter()
            .find_map(|t| self.trait_method_takes_self(t, method))
    }

    /// Whether the trait's associated function `method` names `Self` in its
    /// return type, so that an expected type can say whose impl runs.
    pub(super) fn trait_fn_returns_self(&self, trait_name: &str, method: &str) -> bool {
        self.find_trait_method(trait_name, method)
            .and_then(|m| m.return_type.as_ref())
            .is_some_and(type_expr_names_self)
    }

    /// The type the expected type of the `Trait.method(..)` call at `span`
    /// fixes `Self` to, through the method's declared return type.
    pub(super) fn expected_self(
        &self,
        trait_name: &str,
        method: &str,
        span: &Span,
    ) -> Option<Type> {
        let (key, expected) = self.trait_call_expected.clone()?;
        if key != SpanKey::from_span(span) {
            return None;
        }
        let rt = self
            .find_trait_method(trait_name, method)?
            .return_type
            .clone()?;
        let expected = super::inference::resolve_type_var_top(&expected, &self.env.substitutions);
        let self_ty = self_from_expected(&rt, &expected)?;
        let self_ty = super::inference::resolve_type_var_top(&self_ty, &self.env.substitutions);
        (!matches!(self_ty, Type::TypeVar(_) | Type::Error)).then_some(self_ty)
    }

    /// `let p: P = Make.name()`: the `Self` the expected type fixes in
    /// `name`'s return type picks `P`'s impl of `Make`, ahead of an inherent
    /// `P.name()` (review-thread ruling 2026-11-16, design.md §9 Associated
    /// functions). `None` when the call has no expected type, its return
    /// type does not fix `Self`, or the type it fixes has no impl of the
    /// trait: the caller then refuses the call.
    pub(super) fn trait_call_by_expected(
        &mut self,
        trait_name: &str,
        method: &str,
        callee: &Expr,
        args: &[CallArg],
        span: &Span,
    ) -> Option<Type> {
        let self_ty = self.expected_self(trait_name, method, span)?;
        let (owner, _) = super::types::impl_table_key(&self_ty)?;
        let target_span = self
            .env
            .impls
            .iter()
            .find(|i| i.trait_name.as_deref() == Some(trait_name) && i.target_type == owner)
            .and_then(|i| i.target_span)?;
        let routed = Expr {
            kind: ExprKind::Path {
                segments: vec![owner.clone(), method.to_string()],
                generic_args: None,
            },
            span: callee.span,
            id: callee.id,
        };
        let saved = self.env.ufcs_trait.take();
        let saved_args = self.env.ufcs_trait_args.take();
        self.env.ufcs_trait = Some((trait_name.to_string(), method.to_string()));
        let ty = self.infer_call_inner(&routed, args, span);
        self.env.ufcs_trait = saved;
        self.env.ufcs_trait_args = saved_args;
        // The callee path names the trait's declaration, which has no body:
        // typed HIR takes the call's callee from the impl recorded here.
        let key = SpanKey::from_span(span);
        self.record_method_callee(key, format!("{owner}.{method}"));
        self.record_node_method_impl(span, &target_span);
        if let Some((node, call_key)) = self.current_call_node {
            if call_key == key {
                self.node_call_callees.remove(&node);
                self.routed_trait_calls.insert(node);
            }
        }
        Some(ty)
    }
}

/// Whether a type expression names `Self` anywhere.
fn type_expr_names_self(te: &TypeExpr) -> bool {
    match &te.kind {
        TypeKind::Path(p) => {
            p.segments.first().map(String::as_str) == Some("Self")
                || p.generic_args.as_ref().is_some_and(|ga| {
                    ga.iter()
                        .any(|a| matches!(a, GenericArg::Type(t) if type_expr_names_self(t)))
                })
        }
        TypeKind::Tuple(ts) => ts.iter().any(type_expr_names_self),
        TypeKind::Array { element, .. } => type_expr_names_self(element),
        TypeKind::Ref(inner) | TypeKind::MutRef(inner) => type_expr_names_self(inner),
        _ => false,
    }
}

/// The type `Self` stands for when a declared return type `te` is matched
/// against the call's expected type: `Self` against `P` is `P`,
/// `Option[Self]` against `Option[P]` is `P`.
fn self_from_expected(te: &TypeExpr, expected: &Type) -> Option<Type> {
    match (&te.kind, expected) {
        (TypeKind::Path(p), _) if p.segments == ["Self"] && p.generic_args.is_none() => {
            Some(expected.clone())
        }
        (TypeKind::Path(p), Type::Named { name, args }) => {
            if p.segments.last() != Some(name) {
                return None;
            }
            let ga = p.generic_args.as_ref()?;
            ga.iter().zip(args.iter()).find_map(|(a, t)| match a {
                GenericArg::Type(inner) => self_from_expected(inner, t),
                _ => None,
            })
        }
        (TypeKind::Tuple(ts), Type::Tuple(es)) if ts.len() == es.len() => ts
            .iter()
            .zip(es.iter())
            .find_map(|(t, e)| self_from_expected(t, e)),
        _ => None,
    }
}

/// Whether `name` is a built-in type with a method `method` the compiler
/// implements natively, so that `name.method(recv, ..)` is the receiver
/// form `recv.method(..)` (design.md § Method calls: "Method calls and UFCS
/// are the same call"; B-2026-11-25-40). An associated function of the type
/// (`Vec.new`, `char.from_u32`, `i64.parse`) is not one of these names, so
/// it keeps the type-receiver path.
pub(super) fn builtin_type_has_method(name: &str, method: &str) -> bool {
    use super::expr_method_call::{
        MAP_BUILTIN_METHODS, SET_BUILTIN_METHODS, SLICE_BUILTIN_METHODS, SLICE_MUTATING_METHODS,
        SORTED_MAP_BUILTIN_METHODS, SORTED_SET_BUILTIN_METHODS, STRING_BUILTIN_METHODS,
    };
    use super::method_numeric::{
        FLOAT_METHODS, INT_METHODS, SIGNED_INT_METHODS, UNSIGNED_INT_METHODS,
    };
    let any = |lists: &[&[&str]]| lists.iter().any(|l| l.contains(&method));
    // Every scalar renders, copies and compares itself.
    let scalar_common = matches!(method, "to_string" | "clone" | "cmp" | "eq" | "hash");
    // The ASCII surface a `char` and a `u8` share.
    let ascii = method.starts_with("is_ascii")
        || matches!(
            method,
            "to_ascii_uppercase" | "to_ascii_lowercase" | "eq_ignore_ascii_case"
        );
    match name {
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" => {
            scalar_common || any(&[INT_METHODS, SIGNED_INT_METHODS])
        }
        "u16" | "u32" | "u64" | "u128" | "usize" => {
            scalar_common || any(&[INT_METHODS, UNSIGNED_INT_METHODS])
        }
        "u8" => scalar_common || ascii || any(&[INT_METHODS, UNSIGNED_INT_METHODS]),
        "f16" | "bf16" | "f32" | "f64" => scalar_common || any(&[FLOAT_METHODS]),
        "bool" => scalar_common,
        "char" => {
            scalar_common
                || ascii
                || matches!(
                    method,
                    "is_alphabetic"
                        | "is_numeric"
                        | "is_alphanumeric"
                        | "is_whitespace"
                        | "is_uppercase"
                        | "is_lowercase"
                        | "is_control"
                        | "is_digit"
                        | "to_digit"
                        | "to_uppercase"
                        | "to_lowercase"
                        | "len_utf8"
                )
        }
        "String" | "Str" => any(&[STRING_BUILTIN_METHODS]),
        "Vec" => any(&[
            SLICE_BUILTIN_METHODS,
            SLICE_MUTATING_METHODS,
            super::method_user_impl::VEC_ONLY_METHODS,
        ]),
        "VecDeque" => any(&[super::method_vec_mutation::DEQUE_METHODS]),
        "Map" => any(&[MAP_BUILTIN_METHODS]),
        "Set" => any(&[SET_BUILTIN_METHODS]),
        "SortedMap" => any(&[SORTED_MAP_BUILTIN_METHODS]),
        "SortedSet" => any(&[SORTED_SET_BUILTIN_METHODS]),
        _ => false,
    }
}

/// Every native method of the built-in type `name`: the names of the
/// method tables [`builtin_type_has_method`] consults, filtered by it, less
/// the names it lets through that are no method of `name`, which it leaves
/// for the method checker to refuse with its own words (`Str.push`: "a
/// `Str` is a read-only view"; B-2026-11-26-17).
pub(super) fn builtin_type_methods(name: &str) -> Vec<&'static str> {
    use super::expr_method_call::{
        MAP_BUILTIN_METHODS, SET_BUILTIN_METHODS, SLICE_BUILTIN_METHODS, SLICE_MUTATING_METHODS,
        SORTED_MAP_BUILTIN_METHODS, SORTED_SET_BUILTIN_METHODS, STRING_BUILTIN_METHODS,
    };
    use super::method_numeric::{
        FLOAT_METHODS, INT_METHODS, SIGNED_INT_METHODS, UNSIGNED_INT_METHODS,
    };
    const SCALAR: &[&str] = &[
        "to_string",
        "clone",
        "cmp",
        "eq",
        "hash",
        "is_ascii",
        "is_ascii_digit",
        "is_ascii_alphabetic",
        "is_ascii_alphanumeric",
        "is_ascii_whitespace",
        "is_ascii_uppercase",
        "is_ascii_lowercase",
        "is_ascii_punctuation",
        "is_ascii_hexdigit",
        "to_ascii_uppercase",
        "to_ascii_lowercase",
        "eq_ignore_ascii_case",
        "is_alphabetic",
        "is_numeric",
        "is_alphanumeric",
        "is_whitespace",
        "is_uppercase",
        "is_lowercase",
        "is_digit",
        "to_digit",
        "to_uppercase",
        "to_lowercase",
        "len_utf8",
    ];
    let lists: &[&[&'static str]] = &[
        SCALAR,
        INT_METHODS,
        SIGNED_INT_METHODS,
        UNSIGNED_INT_METHODS,
        FLOAT_METHODS,
        STRING_BUILTIN_METHODS,
        SLICE_BUILTIN_METHODS,
        SLICE_MUTATING_METHODS,
        super::method_user_impl::VEC_ONLY_METHODS,
        super::method_vec_mutation::DEQUE_METHODS,
        MAP_BUILTIN_METHODS,
        SET_BUILTIN_METHODS,
        SORTED_MAP_BUILTIN_METHODS,
        SORTED_SET_BUILTIN_METHODS,
    ];
    let mut out: Vec<&'static str> = lists
        .iter()
        .flat_map(|l| l.iter().copied())
        .filter(|m| builtin_type_has_method(name, m))
        .filter(|m| !no_method_of(name, m))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// A name [`builtin_type_has_method`] admits for `name` that the method
/// checker has no method for.
fn no_method_of(name: &str, method: &str) -> bool {
    match name {
        "f32" | "f64" | "f16" | "bf16" => matches!(method, "cmp" | "eq" | "hash"),
        "Str" => matches!(
            method,
            "push" | "push_str" | "pop" | "clear" | "truncate" | "reserve"
        ),
        "u8" => method == "is_ascii",
        "char" => method == "is_control",
        _ => false,
    }
}

/// The refusal for `ty.method` named as a value when the library has no
/// function to stand for that method (runtime/stdlib/source/method_values.kara):
/// the call form and a closure both work (B-2026-11-25-40).
pub(super) fn builtin_method_value_refusal(ty: &str, method: &str) -> String {
    format!(
        "`{ty}.{method}` names a method of `{ty}`, which has no function value yet: write \
         a closure that calls it, `|x| x.{method}(..)`, or call it with the receiver first, \
         `{ty}.{method}(x, ..)`"
    )
}
