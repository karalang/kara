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
        let takes_self = match &trait_name {
            Some(t) => self.trait_method_takes_self(t, method)?,
            None if is_type => self.type_method_takes_self(name, method)?,
            None => return None,
        };
        if !takes_self {
            return None;
        }
        let Some((recv, rest)) = args.split_first() else {
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
        if let Some((node, key)) = self.current_call_node {
            if key == SpanKey::from_span(span) {
                self.node_call_callees.remove(&node);
                self.node_method_callees.remove(&node);
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
        let ret = self.infer_method_call(&recv.value, method, rest, span, args_close_span);
        self.env.ufcs_trait = saved;
        self.env.ufcs_trait_args = saved_args;
        // The name as written: `lowering` matches it against the callee.
        self.ufcs_calls
            .insert(SpanKey::from_span(span), alias.to_string());
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
    fn trait_method_takes_self(&self, trait_name: &str, method: &str) -> Option<bool> {
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
}
