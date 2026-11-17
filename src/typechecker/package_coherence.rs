//! The orphan, blanket and inherent-impl rules between the packages of one
//! build (design.md § Coherence and the orphan rule; B-2026-11-17-36).
//!
//! Overlap is caught once two impls meet in one build, but the rule that keeps
//! two independent libraries from ever writing the same impl is per package:
//! a library may implement a trait for a type only if it defines the trait,
//! the type, or a type in the trait's arguments, and a blanket impl only of
//! its own trait. Every package, the application included, writes inherent
//! impls only for its own types. Each module is checked with the whole
//! package tree attached, so an item's package is its defining module's.

use super::*;
use crate::module::ModuleId;

/// Which package an item belongs to, seen from the module being checked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Owner {
    /// This module's package.
    Current,
    /// Another user package of the build.
    Other,
    /// The standard library, a builtin or a primitive.
    Std,
}

impl<'a> TypeChecker<'a> {
    pub(super) fn check_package_coherence(&mut self) {
        if self.compiling_stdlib {
            return;
        }
        let (Some(tree), Some(cur)) = (self.tree, self.current_module) else {
            return;
        };
        let cur_pkg = tree.module(cur).package.clone();
        // The final application is exempt from the orphan rule, and so from
        // the blanket rule (design.md § Coherence); a library, whether a
        // dependency or the package being checked, is not.
        let application = cur_pkg.is_none()
            && tree
                .module(tree.root)
                .file
                .file_name()
                .is_some_and(|f| f == "main.kara");
        let library = self.program.library_start.unwrap_or(usize::MAX);
        let mut reports: Vec<(String, Span)> = Vec::new();
        for item in &self.program.items {
            let Item::ImplBlock(imp) = item else { continue };
            if imp.span.offset >= library
                || crate::prelude::gated_module_at(imp.span.offset).is_some()
                || matches!(imp.target_type.kind, TypeKind::Error)
            {
                continue;
            }
            let params = Self::generic_param_names(&imp.generic_params);
            let head = match &imp.target_type.kind {
                TypeKind::Path(p) => p.segments.last().cloned(),
                _ => None,
            };
            let target = crate::formatter::render_type_expr(&imp.target_type);
            let Some(trait_path) = &imp.trait_name else {
                // An inherent impl on a library type is refused by
                // `check_inherent_impl_locality`; this one is on another
                // package's type, which that check reads as declared.
                if let Some(h) = &head {
                    if self.item_owner(tree, &cur_pkg, h) == Owner::Other {
                        reports.push((
                            format!(
                                "inherent impl on `{target}`, a type another package declares: \
                                 a package may write `impl Type {{ .. }}` only for its own \
                                 types, and the application is no exception (design.md § \
                                 Coherence). Write a trait and implement it for `{target}`, \
                                 or wrap the type in a `distinct type`"
                            ),
                            imp.span,
                        ));
                    }
                }
                continue;
            };
            if application {
                continue;
            }
            let Some(trait_name) = trait_path.segments.last() else {
                continue;
            };
            if self.item_owner(tree, &cur_pkg, trait_name) == Owner::Current {
                continue;
            }
            let blanket = head.as_ref().is_some_and(|h| params.contains(h));
            if blanket {
                reports.push((
                    format!(
                        "blanket impl `impl[{h}] {trait_name} for {h}` of a trait this package \
                         does not define: `{h}` ranges over every type, so no package owns it, \
                         and a blanket impl is allowed only in the package that defines \
                         `{trait_name}` (design.md § Coherence, Blanket impls). Implement \
                         `{trait_name}` for this package's own types instead",
                        h = head.clone().unwrap_or_default(),
                    ),
                    imp.span,
                ));
                continue;
            }
            if head
                .as_ref()
                .is_some_and(|h| self.item_owner(tree, &cur_pkg, h) == Owner::Current)
            {
                continue;
            }
            // A local type anywhere in the trait's arguments counts
            // (`impl Mul[Column] for f64`); a bare parameter does not.
            let mut named: Vec<String> = Vec::new();
            for a in trait_path.generic_args.iter().flatten() {
                if let GenericArg::Type(t) = a {
                    collect_path_heads(t, &mut named);
                }
            }
            if named.iter().any(|n| {
                !params.contains(n) && self.item_owner(tree, &cur_pkg, n) == Owner::Current
            }) {
                continue;
            }
            reports.push((
                format!(
                    "orphan impl: this package defines neither `{trait_name}` nor `{target}`, \
                     nor a type in `{trait_name}`'s arguments, so it may not write `impl \
                     {trait_name} for {target}`: two packages could then write the same impl \
                     and never build together (design.md § Coherence, the orphan rule). Only \
                     the final application is exempt. Implement it on a type this package \
                     declares, or wrap `{target}` in a `distinct type`"
                ),
                imp.span,
            ));
        }
        for (message, span) in reports {
            self.type_error(message, span, TypeErrorKind::OrphanImpl);
        }
    }

    /// The package that declares the type or trait the module binds as
    /// `name`: an import's defining module's, else this module's own item,
    /// else the library's.
    fn item_owner(
        &self,
        tree: &crate::module::ProgramTree,
        cur_pkg: &Option<String>,
        name: &str,
    ) -> Owner {
        if let Some((path, _, _)) = self.type_origins.get(name) {
            let Some(&id) = tree.graph.by_path.get::<[String]>(path) else {
                return Owner::Std;
            };
            let m = tree.module(id as ModuleId);
            if m.is_synthetic {
                return Owner::Std;
            }
            return if m.package == *cur_pkg {
                Owner::Current
            } else {
                Owner::Other
            };
        }
        let library = self.program.library_start.unwrap_or(usize::MAX);
        let declares = self.program.items.iter().any(|it| {
            let (n, span) = match it {
                Item::StructDef(s) => (s.name.as_str(), s.span),
                Item::EnumDef(e) => (e.name.as_str(), e.span),
                Item::UnionDef(u) => (u.name.as_str(), u.span),
                Item::DistinctType(d) => (d.name.as_str(), d.span),
                Item::TypeAlias(a) => (a.name.as_str(), a.span),
                Item::TraitDef(t) => (t.name.as_str(), t.span),
                Item::TraitAlias(t) => (t.name.as_str(), t.span),
                Item::MarkerTrait(t) => (t.name.as_str(), t.span),
                _ => return false,
            };
            n == name && span.offset < library
        });
        if declares {
            Owner::Current
        } else {
            Owner::Std
        }
    }
}

/// Every path head a type names (`Vec[Column]` gives `Vec`, `Column`).
fn collect_path_heads(t: &TypeExpr, out: &mut Vec<String>) {
    match &t.kind {
        TypeKind::Path(p) => {
            if let Some(h) = p.segments.last() {
                out.push(h.clone());
            }
            for a in p.generic_args.iter().flatten() {
                if let GenericArg::Type(t) = a {
                    collect_path_heads(t, out);
                }
            }
        }
        TypeKind::Ref(i) | TypeKind::MutRef(i) => collect_path_heads(i, out),
        TypeKind::Tuple(ts) => {
            for t in ts {
                collect_path_heads(t, out);
            }
        }
        _ => {}
    }
}
