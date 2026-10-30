//! Which traits can be used as `dyn` (design.md § `dyn Trait`): every
//! required method and associated function of the trait, and of its
//! supertraits, must be dispatchable through the method table, or the
//! `dyn` type is `E_DYN_NOT_DISPATCHABLE`, naming the first one that is
//! not. A default method needs no slot (it runs with `Self` the `dyn` type),
//! and `Clone`, whose `clone` returns `Self`, is the one supertrait allowed
//! to have one.
//!
//! Lives in a sibling `impl<'a> super::TypeChecker<'a>` block.

use crate::ast::*;

impl<'a> super::TypeChecker<'a> {
    /// The first required method of `trait_name` or a supertrait that a
    /// `dyn` value cannot dispatch, as `(trait, method, why)`, in
    /// declaration order, the trait's own first, then each supertrait's
    /// breadth-first. A trait with no declaration in this program (a baked
    /// standard-library one) is not judged.
    pub(super) fn dyn_first_undispatchable(
        &self,
        trait_name: &str,
    ) -> Option<(String, String, &'static str)> {
        for t in self.env.supertrait_closure_traits(trait_name) {
            if t == "Clone" {
                continue;
            }
            let Some(def) = self.program.items.iter().find_map(|item| match item {
                Item::TraitDef(d) if d.name == t => Some(d),
                _ => None,
            }) else {
                continue;
            };
            for it in &def.items {
                let TraitItem::Method(m) = it else { continue };
                if m.body.is_some() || m.is_dyn_dispatchable() {
                    continue;
                }
                let why = if m.self_param.is_none() {
                    "it has no receiver"
                } else if m
                    .generic_params
                    .as_ref()
                    .is_some_and(|g| !g.params.is_empty())
                {
                    "it has type or const parameters of its own"
                } else {
                    "it names `Self` outside the receiver"
                };
                return Some((t, m.name.clone(), why));
            }
        }
        None
    }
}
