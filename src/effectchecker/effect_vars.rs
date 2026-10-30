//! Effect variables solved per value (design.md § Effect variables).
//!
//! A variable an `impl[with E]` block or a `struct S[with E]` declares
//! belongs to each value of the type, not to a call: `bus.register(h)` binds
//! `h`'s effects into `bus`'s instance of `E`. The variables of a function's
//! own `[with E]` are solved per call by `compute_call_var_bindings`.
//!
//! Within one function the instance is the union of the value's construction
//! and every later binding, so the order of `bus.register(..)` calls does not
//! matter, and nothing here can refuse one (review-thread ruling 2026-10-30).
//! What a written type argument fixes is checked instead: a value declared
//! `EventBus[with writes(OrderDb)]`, by a `let` or by the parameter another
//! function sees it through, takes only function values whose effects fit
//! that set, and one with an effect outside it is refused, naming the effect
//! and the type argument.
//!
//! What a call through the value is charged needs nothing here: naming a
//! function value charges its effects to the function that names it
//! (B-2026-08-23-7), and that is the function where the variable is solved.
//! A value coerced to `dyn Trait with E` is bound the same way, with its
//! concrete type's methods' effects (`dyn_effects.rs`).

use std::collections::HashSet;

use rustc_hash::FxHashMap;

use crate::ast::*;
use crate::intern::Symbol;
use crate::token::Span;

use super::{
    declared_admits, effect_var_names_in_type, verb_name, Effect, EffectError, EffectErrorKind,
    EffectSubtypeTrace,
};

/// The set a written type argument fixes for one value's effect variable.
#[derive(Debug, Clone)]
pub(super) struct VarInstance {
    var: String,
    set: HashSet<Effect>,
    /// The written type argument.
    fixed_at: Span,
}

/// The values whose type argument is written, by the place that holds the
/// value (`bus`, `self.bus`), for one body.
pub(super) type Instances = Vec<(String, VarInstance)>;

/// `writes(Log)`, or a bare `blocks` for a verb that names no resource.
fn effect_text(e: &Effect) -> String {
    if e.resource.is_empty() {
        verb_name(&e.verb)
    } else {
        format!("{}({})", verb_name(&e.verb), e.resource)
    }
}

fn set_text(set: &HashSet<Effect>) -> Vec<String> {
    let mut v: Vec<String> = set.iter().map(effect_text).collect();
    v.sort();
    v
}

/// Whether `ty` names `var` in a function type's clause, inside generic
/// arguments too (`Vec[Fn(Event) with E]`), which
/// [`effect_var_names_in_type`] does not walk.
fn mentions_var(ty: &TypeExpr, var: &str) -> bool {
    if effect_var_names_in_type(ty).iter().any(|n| n == var) {
        return true;
    }
    match &ty.kind {
        TypeKind::Path(p) => p.generic_args.iter().flatten().any(|a| match a {
            GenericArg::Type(t) => mentions_var(t, var),
            _ => false,
        }),
        TypeKind::Tuple(elems) => elems.iter().any(|t| mentions_var(t, var)),
        TypeKind::Array { element, .. } => mentions_var(element, var),
        _ => false,
    }
}

impl<'a> super::EffectChecker<'a> {
    /// The values a field initializer binds: a function value or a value
    /// coerced to `dyn` there, or those inside a collection literal or a
    /// tuple of them.
    fn bound_values<'e>(&self, e: &'e Expr) -> Vec<&'e Expr> {
        if self.coerced_methods(e).is_some() {
            return vec![e];
        }
        match &e.kind {
            ExprKind::Closure { .. } | ExprKind::Identifier(_) => vec![e],
            ExprKind::ArrayLiteral(items)
            | ExprKind::PrefixCollectionLiteral { items, .. }
            | ExprKind::Tuple(items) => items.iter().flat_map(|i| self.bound_values(i)).collect(),
            _ => Vec::new(),
        }
    }

    /// For each method of an `impl[with E]` block, the parameter positions
    /// whose function type names a variable the block declares (and the
    /// method does not redeclare), keyed like `fn_effect_var_positions`.
    pub(crate) fn build_impl_effect_var_positions(
        &self,
    ) -> FxHashMap<Symbol, FxHashMap<String, Vec<usize>>> {
        let mut index: FxHashMap<Symbol, FxHashMap<String, Vec<usize>>> = FxHashMap::default();
        for item in &self.program.items {
            let Item::ImplBlock(imp) = item else {
                continue;
            };
            let Some(gp) = &imp.generic_params else {
                continue;
            };
            if gp.effect_params.is_empty() {
                continue;
            }
            let TypeKind::Path(p) = &imp.target_type.kind else {
                continue;
            };
            let Some(type_name) = p.segments.last() else {
                continue;
            };
            for impl_item in &imp.items {
                let ImplItem::Method(m) = impl_item else {
                    continue;
                };
                let own: HashSet<&str> = m
                    .generic_params
                    .as_ref()
                    .map(|g| g.effect_params.iter().map(|e| e.name.as_str()).collect())
                    .unwrap_or_default();
                let mut by_var: FxHashMap<String, Vec<usize>> = FxHashMap::default();
                for (idx, param) in m.params.iter().enumerate() {
                    for name in effect_var_names_in_type(&param.ty) {
                        if !own.contains(name.as_str())
                            && gp.effect_params.iter().any(|e| e.name == name)
                        {
                            by_var.entry(name).or_default().push(idx);
                        }
                    }
                }
                if !by_var.is_empty() {
                    index.insert(self.interner.dotted_str(type_name, &m.name), by_var);
                }
            }
        }
        index
    }

    /// The non-transparent effects of a function value bound to a slot.
    fn bound_effects(&self, value: &Expr) -> HashSet<Effect> {
        self.get_arg_effects(value)
            .effects
            .iter()
            .filter(|te| !self.is_slot_transparent(&te.effect))
            .map(|te| te.effect.clone())
            .collect()
    }

    /// The instance a written type argument fixes, when `ty` writes one on
    /// a type that declares a variable.
    fn written_instance(&self, ty: &TypeExpr) -> Option<VarInstance> {
        let list = ty.effect_args()?;
        let TypeKind::Path(p) = &ty.kind else {
            return None;
        };
        let var = self.type_effect_var(p.segments.last()?)?;
        Some(VarInstance {
            var,
            set: self.resolve_effect_list_to_set(list, None),
            fixed_at: list.span,
        })
    }

    /// The effect variable a struct or enum declares, by name.
    fn type_effect_var(&self, type_name: &str) -> Option<String> {
        self.program.items.iter().find_map(|item| {
            let gp = match item {
                Item::StructDef(sd) if sd.name == type_name => sd.generic_params.as_ref(),
                Item::EnumDef(ed) if ed.name == type_name => ed.generic_params.as_ref(),
                _ => None,
            }?;
            gp.effect_params.first().map(|e| e.name.clone())
        })
    }

    /// Bind `value` into `inst`: refused when it brings an effect outside the
    /// set, with the set and the type argument that fixed it.
    fn fit_bound_value(&mut self, inst: &VarInstance, value: &Expr) {
        let effects = self.bound_effects(value);
        let mut extra: Vec<String> = effects
            .iter()
            .filter(|e| !declared_admits(&inst.set, e))
            .map(effect_text)
            .collect();
        if extra.is_empty() {
            return;
        }
        extra.sort();
        let set = set_text(&inst.set);
        let extra_text: Vec<String> = extra.iter().map(|e| format!("`{e}`")).collect();
        let what = match self.dyn_coercions.get(&value.id) {
            Some(c) => format!("value coerced to `dyn {}`", c.trait_name),
            None => "function value".to_string(),
        };
        let message = format!(
            "{what} has {}, outside the effect set of `{}` (`{{{}}}`), which was \
             fixed at {}:{} by its type argument",
            extra_text.join(", "),
            inst.var,
            set.join(", "),
            inst.fixed_at.line,
            inst.fixed_at.column,
        );
        self.errors.push(EffectError {
            message,
            span: value.span,
            kind: EffectErrorKind::EffectSubtypeViolation,
            subtype_trace: Some(EffectSubtypeTrace {
                slot_effects: set,
                argument_effects: set_text(&effects),
                offending_effects: extra,
                monomorphized_signature: None,
            }),
            replacement: None,
        });
    }

    /// `let name[: ty] = value;`: a fresh value under `name`. A written type
    /// argument fixes its set, and the function values its construction
    /// binds must fit it; without one, the construction is part of the union
    /// and refuses nothing.
    pub(super) fn bind_construction(
        &mut self,
        name: &str,
        ty: Option<&TypeExpr>,
        value: &Expr,
        instances: &mut Instances,
    ) {
        let prefix = format!("{name}.");
        instances.retain(|(place, _)| place != name && !place.starts_with(&prefix));
        let Some(inst) = ty.and_then(|t| self.written_instance(t)) else {
            return;
        };
        if let ExprKind::StructLiteral { path, fields, .. } = &value.kind {
            if let Some(var) = path.last().and_then(|s| self.type_effect_var(s)) {
                let sname = path.last().cloned().unwrap_or_default();
                for f in fields {
                    let Some(field_ty) = self.struct_field_type(&sname, &f.name) else {
                        continue;
                    };
                    if !mentions_var(&field_ty, &var) {
                        continue;
                    }
                    for v in self.bound_values(&f.value) {
                        self.fit_bound_value(&inst, v);
                    }
                }
            }
        }
        instances.push((name.to_string(), inst));
    }

    /// `recv.m(args)` where `m` binds an argument into a variable its
    /// `impl` declares: when the receiver's type argument is written (on its
    /// `let`, or on the parameter or field it is reached through), each
    /// binding must fit that set. Returns the argument positions handled
    /// here, which the per-call slot check skips.
    ///
    /// Inside the type's own methods (`self.register(h)`) the variable is
    /// the method's own, unsolved, so the positions are skipped unbound.
    pub(super) fn bind_receiver_vars(
        &mut self,
        callee_key: Symbol,
        place: Option<String>,
        written: Option<TypeExpr>,
        in_own_impl: bool,
        args: &[CallArg],
        instances: &mut Instances,
    ) -> Vec<usize> {
        let Some(by_var) = self.impl_effect_var_positions.get(&callee_key) else {
            return Vec::new();
        };
        let mut handled: Vec<usize> = by_var.values().flatten().copied().collect();
        handled.sort_unstable();
        handled.dedup();
        if in_own_impl {
            return handled;
        }
        let Some(place) = place else {
            return handled;
        };
        if !instances.iter().any(|(p, _)| *p == place) {
            if let Some(inst) = written.as_ref().and_then(|t| self.written_instance(t)) {
                instances.push((place.clone(), inst));
            }
        }
        let Some(inst) = instances
            .iter()
            .rev()
            .find(|(p, _)| *p == place)
            .map(|(_, i)| i.clone())
        else {
            return handled;
        };
        for &idx in &handled {
            if let Some(arg) = args.get(idx) {
                self.fit_bound_value(&inst, &arg.value);
            }
        }
        handled
    }
}
