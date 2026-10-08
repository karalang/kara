//! D5 migration: borrow-by-default parameters (`docs/core-semantics.md` §4.1,
//! decision 8). Until the flip a bare parameter `x: T` is owned; after it, a
//! bare parameter borrows and only `x: own T` is owned. `own T` already parses
//! and means `T`, so writing `own` wherever a body needs ownership is a no-op
//! today and keeps the program's meaning across the flip.
//!
//! This pass finds those places. It reruns the body walk with bare parameters
//! treated as borrows, so passing a value to a bare parameter of another
//! function reads it rather than moving it, and records every bare parameter
//! (and bare `self`) whose body still moves, mutates or calls it uniquely. Those
//! become `own`, which turns the call sites passing to them back into moves,
//! so the walk repeats until nothing new needs `own`. Starting from "every
//! bare parameter borrows" and only ever adding `own` gives the smallest set.
//!
//! A trait method's positions are decided once for the trait: when one impl
//! needs `own` at a position, the trait's declaration and every impl of that
//! method get it, because they must keep one signature.

use std::collections::HashMap;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ast::*;
use crate::resolver::{SpanKey, TextEdit};
use crate::token::Span;
use crate::typechecker::{Type, TypeCheckResult};

use super::{OwnershipChecker, OwnershipMode, ParamUsage};

/// The parameter index standing for the receiver.
pub(crate) const SELF_IDX: usize = usize::MAX;

/// The D5 walk's state on an [`OwnershipChecker`].
#[derive(Default)]
pub(crate) struct D5State {
    /// Bare positions already decided `own`; every other bare position is a
    /// borrow in this walk.
    pub(crate) own: FxHashSet<(String, usize)>,
    /// Bare positions this walk found needing `own`.
    pub(crate) needs: Vec<(String, usize)>,
}

/// A user-written function, with the key the ownership pass knows it by and
/// the trait method it implements, if any.
struct Decl<'p> {
    key: String,
    group: Option<String>,
    params: &'p [Param],
    self_param: Option<&'p SelfParam>,
    self_is_own: bool,
    self_span: Option<Span>,
}

fn decls(program: &Program) -> Vec<Decl<'_>> {
    let mut out = Vec::new();
    for item in &program.items {
        match item {
            Item::Function(f) if !f.stdlib_origin => out.push(Decl {
                key: f.name.clone(),
                group: None,
                params: &f.params,
                self_param: None,
                self_is_own: false,
                self_span: None,
            }),
            Item::ImplBlock(imp) => {
                let TypeKind::Path(p) = &imp.target_type.kind else {
                    continue;
                };
                let Some(target) = p.segments.last() else {
                    continue;
                };
                let trait_name = imp.trait_name.as_ref().and_then(|t| t.segments.last());
                for it in &imp.items {
                    if let ImplItem::Method(m) = it {
                        if m.stdlib_origin {
                            continue;
                        }
                        out.push(Decl {
                            key: format!("{target}.{}", m.name),
                            group: trait_name.map(|t| format!("{t}.{}", m.name)),
                            params: &m.params,
                            self_param: m.self_param.as_ref(),
                            self_is_own: m.self_is_own,
                            self_span: m.self_span,
                        });
                    }
                }
            }
            Item::TraitDef(t) => {
                for it in &t.items {
                    if let TraitItem::Method(m) = it {
                        let key = format!("{}.{}", t.name, m.name);
                        out.push(Decl {
                            key: key.clone(),
                            group: Some(key),
                            params: &m.params,
                            self_param: m.self_param.as_ref(),
                            self_is_own: m.self_is_own,
                            self_span: m.self_span,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Whether parameter `p` is written bare: owned today, a borrow after D5.
pub(crate) fn param_is_bare(p: &Param) -> bool {
    !p.is_own
        && matches!(
            super::param_modes_from_signature(std::slice::from_ref(p)).first(),
            Some(OwnershipMode::Own)
        )
}

/// Every bare position of `decl`, with the byte offset `own ` goes in front of.
fn bare_positions(decl: &Decl<'_>) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = decl
        .params
        .iter()
        .enumerate()
        .filter(|(_, p)| param_is_bare(p))
        .map(|(i, p)| (i, p.ty.span.offset))
        .collect();
    if matches!(decl.self_param, Some(SelfParam::Owned)) && !decl.self_is_own {
        if let Some(sp) = decl.self_span {
            out.push((SELF_IDX, sp.offset));
        }
    }
    out
}

impl<'a> OwnershipChecker<'a> {
    /// Treat every bare position outside `own` as a borrow, so an argument
    /// passed to one is read rather than moved.
    pub(crate) fn with_d5(mut self, own: FxHashSet<(String, usize)>) -> Self {
        for d in decls(self.program) {
            for (i, _) in bare_positions(&d) {
                if own.contains(&(d.key.clone(), i)) {
                    continue;
                }
                if i == SELF_IDX {
                    if let Some(m) = self.method_self_modes.get_mut(&d.key) {
                        *m = SelfParam::Ref;
                    }
                    continue;
                }
                let table = if d.self_param.is_some() {
                    &mut self.method_param_modes
                } else {
                    &mut self.callee_param_modes
                };
                if let Some(slot) = table.get_mut(&d.key).and_then(|v| v.get_mut(i)) {
                    *slot = OwnershipMode::Ref;
                }
            }
        }
        self.d5 = Some(D5State {
            own,
            needs: Vec::new(),
        });
        self
    }

    /// Run the body walk and return the bare positions that need `own`.
    pub(crate) fn d5_needs(mut self) -> Vec<(String, usize)> {
        self.check_items();
        self.d5.take().map(|d| d.needs).unwrap_or_default()
    }

    /// After `f`'s body walk: record each bare position the body moved,
    /// mutated, or calls as a `MutFn`/`OnceFn`.
    pub(crate) fn d5_record(
        &mut self,
        f: &Function,
        fn_key: &str,
        param_types: &HashMap<String, Type>,
        param_usage: &HashMap<String, ParamUsage>,
    ) {
        if self.d5.is_none() {
            return;
        }
        let mut needs = Vec::new();
        for (i, p) in f.params.iter().enumerate() {
            if !param_is_bare(p) {
                continue;
            }
            let unique_fn = match &p.ty.kind {
                TypeKind::FnType { is_once, .. } => {
                    *is_once
                        || self
                            .program
                            .mut_fn_types
                            .contains(&SpanKey::from_span(&p.ty.span))
                }
                _ => false,
            };
            let moved = p.pattern.binding_names().iter().any(|n| {
                let Some(t) = param_types.get(n) else {
                    return false;
                };
                if self.is_copy_type(t) {
                    return false;
                }
                // A value with a `Drop` body anywhere inside is dropped in
                // the callee today; as a borrow it would be dropped at the
                // caller's scope end, which prints differently. Keep it owned.
                matches!(
                    param_usage.get(n),
                    Some(ParamUsage::Consumed | ParamUsage::Mutated)
                ) || self.d5_runs_user_drop(t, &mut Vec::new())
            });
            if unique_fn || moved {
                needs.push((fn_key.to_string(), i));
            }
        }
        let self_drops = || {
            let target = fn_key.split('.').next().unwrap_or_default();
            !target.is_empty()
                && self.d5_runs_user_drop(
                    &Type::Named {
                        name: target.to_string(),
                        args: Vec::new(),
                    },
                    &mut Vec::new(),
                )
        };
        if param_usage.get("self").is_some_and(|u| {
            matches!(u, ParamUsage::Consumed)
                || (f.self_param == Some(SelfParam::Owned) && self_drops())
        }) {
            needs.push((fn_key.to_string(), SELF_IDX));
        }
        if let Some(d5) = self.d5.as_mut() {
            needs.retain(|n| !d5.own.contains(n));
            d5.needs.extend(needs);
        }
    }
}

impl OwnershipChecker<'_> {
    /// Does a value of `ty` run a user `Drop` body when it dies: its own, a
    /// field's, a payload's or an element's. A `shared` value counts, since
    /// the handle a callee owns may be the last one. A generic parameter
    /// counts when the program declares any `Drop` body, since it may be
    /// instantiated with that type.
    fn d5_runs_user_drop(&self, ty: &Type, seen: &mut Vec<String>) -> bool {
        match ty {
            Type::Named { name, args } => {
                if args.iter().any(|a| self.d5_runs_user_drop(a, seen)) {
                    return true;
                }
                if args.is_empty() && self.d5_program_has_drop() && is_generic_name(self, name) {
                    return true;
                }
                self.d5_named_runs_user_drop(name, seen)
            }
            Type::Shared(name) => self.d5_named_runs_user_drop(name, seen),
            Type::TypeParam(_) => self.d5_program_has_drop(),
            Type::Tuple(elems) => elems.iter().any(|e| self.d5_runs_user_drop(e, seen)),
            Type::Array { element, .. } => self.d5_runs_user_drop(element, seen),
            _ => false,
        }
    }

    fn d5_named_runs_user_drop(&self, name: &str, seen: &mut Vec<String>) -> bool {
        use crate::typechecker::VariantTypeInfo;
        let tc = self.typecheck_result;
        if tc.drop_method_keys.contains_key(name) {
            return true;
        }
        if seen.iter().any(|s| s == name) {
            return false;
        }
        seen.push(name.to_string());
        if let Some(info) = tc.struct_info.get(name) {
            return info
                .fields
                .iter()
                .any(|(_, t, _)| self.d5_runs_user_drop(t, seen));
        }
        if let Some(info) = tc.enum_info.get(name) {
            return info.variants.iter().any(|(_, v)| match v {
                VariantTypeInfo::Unit => false,
                VariantTypeInfo::Tuple(ts) => ts.iter().any(|t| self.d5_runs_user_drop(t, seen)),
                VariantTypeInfo::Struct(fs) => {
                    fs.iter().any(|(_, t)| self.d5_runs_user_drop(t, seen))
                }
            });
        }
        false
    }

    fn d5_program_has_drop(&self) -> bool {
        !self.typecheck_result.drop_method_keys.is_empty()
    }
}

/// Whether `name`, spelled bare by the D5 lowering, is a generic parameter
/// rather than a type: it names no struct, enum or distinct type.
fn is_generic_name(checker: &OwnershipChecker<'_>, name: &str) -> bool {
    let tc = checker.typecheck_result;
    !tc.struct_info.contains_key(name)
        && !tc.enum_info.contains_key(name)
        && !tc.distinct_type_traits.contains_key(name)
        && !matches!(name, "String" | "Str" | "unknown")
}

/// The `own ` insertions that keep `program`'s meaning across D5.
///
/// With `keep_meaning`, every bare non-`Copy` position gets `own` whatever
/// its body does: for signatures whose body is a placeholder (the baked
/// stdlib's compiler builtins), where the body says nothing about what the
/// real implementation keeps.
pub fn d5_own_param_edits(
    program: &Program,
    typecheck_result: &TypeCheckResult,
    keep_meaning: bool,
) -> Vec<TextEdit> {
    let decls = decls(program);
    if keep_meaning {
        let checker = OwnershipChecker::new(program, typecheck_result);
        let mut edits: Vec<TextEdit> = Vec::new();
        let mut seen = FxHashSet::default();
        for d in &decls {
            for (i, offset) in bare_positions(d) {
                let copy = i != SELF_IDX
                    && checker.is_copy_type(&checker.lower_type_for_ownership(&d.params[i].ty));
                if !copy && seen.insert(offset) {
                    edits.push(TextEdit {
                        offset,
                        length: 0,
                        replacement: "own ".to_string(),
                    });
                }
            }
        }
        edits.sort_by_key(|e| e.offset);
        return edits;
    }
    let mut groups: FxHashMap<&str, Vec<&str>> = FxHashMap::default();
    for d in &decls {
        if let Some(g) = &d.group {
            groups.entry(g.as_str()).or_default().push(d.key.as_str());
        }
    }
    let group_of: FxHashMap<&str, &str> = decls
        .iter()
        .filter_map(|d| d.group.as_deref().map(|g| (d.key.as_str(), g)))
        .collect();

    let mut own: FxHashSet<(String, usize)> = FxHashSet::default();
    // Each round only adds, and there are finitely many positions; the cap is
    // a guard, not a limit any program reaches.
    for _ in 0..64 {
        let needs = OwnershipChecker::new(program, typecheck_result)
            .with_d5(own.clone())
            .d5_needs();
        let mut grew = false;
        for (key, i) in needs {
            let members: Vec<&str> = match group_of.get(key.as_str()) {
                Some(g) => groups[g].clone(),
                None => vec![key.as_str()],
            };
            for m in members {
                grew |= own.insert((m.to_string(), i));
            }
        }
        if !grew {
            break;
        }
    }

    let mut seen = FxHashSet::default();
    let mut edits = Vec::new();
    for d in &decls {
        for (i, offset) in bare_positions(d) {
            if own.contains(&(d.key.clone(), i)) && seen.insert(offset) {
                edits.push(TextEdit {
                    offset,
                    length: 0,
                    replacement: "own ".to_string(),
                });
            }
        }
    }
    edits.sort_by_key(|e| e.offset);
    edits
}
