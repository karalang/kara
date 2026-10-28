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
use super::TypeErrorKind;

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
        if self.local_scope.lookup(name).is_some() {
            return None;
        }
        let is_type = self.env.structs.contains_key(name) || self.env.enums.contains_key(name);
        let trait_name = (!is_type && self.env.traits.contains_key(name)).then(|| name.to_string());
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
        let errors = self.errors.len();
        let saved = self.env.ufcs_trait.take();
        self.env.ufcs_trait = trait_name.map(|t| (t, method.to_string()));
        let ret = self.infer_method_call(&recv.value, method, rest, span, args_close_span);
        self.env.ufcs_trait = saved;
        self.ufcs_calls
            .insert(SpanKey::from_span(span), name.to_string());
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
    fn type_method_takes_self(&self, type_name: &str, method: &str) -> Option<bool> {
        for item in &self.program.items {
            let Item::ImplBlock(imp) = item else { continue };
            let TypeKind::Path(path) = &imp.target_type.kind else {
                continue;
            };
            if imp.trait_name.is_some()
                || path.segments.last().map(String::as_str) != Some(type_name)
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
