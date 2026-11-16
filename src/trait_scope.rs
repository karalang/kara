//! Which traits a file's method calls may use (design.md § Method
//! resolution): the traits in scope are the package's own, those the file
//! imports by name, and the prelude's. A dependency's trait the file does
//! not import supplies no method there, and a call it alone would answer is
//! refused with the import that brings it in (B-2026-11-16-180).
//!
//! The checker reads the answer as a set of HIDDEN traits: a dependency
//! trait whose name no in-scope trait carries, with the module that declares
//! it for the help. A package checked module by module has one set per
//! module; the v2 check of a package's combined source has one per file,
//! by byte range ([`HiddenTraits`]).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::ast::Item;
use crate::module::{ModuleId, ProgramTree};

/// Hidden trait name -> the path of the module that declares it.
pub type Hidden = HashMap<String, Vec<String>>;

/// The hidden traits of each file of a combined source, by the byte range
/// the file occupies in it.
#[derive(Debug, Default, Clone)]
pub struct HiddenTraits {
    regions: Vec<(usize, usize, Arc<Hidden>)>,
}

impl HiddenTraits {
    /// One set for the whole source: a module checked on its own.
    pub fn whole(hidden: Hidden) -> HiddenTraits {
        if hidden.is_empty() {
            return HiddenTraits::default();
        }
        HiddenTraits {
            regions: vec![(0, usize::MAX, Arc::new(hidden))],
        }
    }

    /// Add the set of the file at `start..start + len`.
    pub fn push(&mut self, start: usize, len: usize, hidden: Hidden) {
        if !hidden.is_empty() {
            self.regions.push((start, start + len, Arc::new(hidden)));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    /// The hidden traits where `offset` is written.
    pub fn at(&self, offset: usize) -> Option<Arc<Hidden>> {
        self.regions
            .iter()
            .find(|(s, e, _)| offset >= *s && offset < *e)
            .map(|(_, _, h)| h.clone())
    }
}

/// The hidden traits of the package this process compiles, set once by the
/// CLI after amalgamating it. The MIR builder re-checks the same combined
/// source (`mir::lower::build_source`) through
/// [`crate::typecheck_with_pruned_library_source`], which reads them here;
/// the library it appends lies past every file's range.
static PROGRAM: std::sync::RwLock<Option<HiddenTraits>> = std::sync::RwLock::new(None);

/// Record the package's hidden traits for the MIR builder's check.
pub fn set_program(hidden: HiddenTraits) {
    if let Ok(mut p) = PROGRAM.write() {
        *p = Some(hidden);
    }
}

/// The hidden traits [`set_program`] recorded, or none.
pub fn program() -> HiddenTraits {
    PROGRAM
        .read()
        .ok()
        .and_then(|p| p.clone())
        .unwrap_or_default()
}

/// The dependency traits module `id` does not have in scope.
pub fn hidden_traits(tree: &ProgramTree, id: ModuleId) -> Hidden {
    let m = tree.module(id);
    let trait_names = |items: &[Item]| -> Vec<String> {
        items
            .iter()
            .filter_map(|it| match it {
                Item::TraitDef(t) => Some(t.name.clone()),
                _ => None,
            })
            .collect()
    };
    // The package's own traits, wherever in it they are declared.
    let mut in_scope: HashSet<String> = tree
        .modules
        .iter()
        .filter(|o| !o.is_synthetic && o.package == m.package)
        .flat_map(|o| trait_names(&o.items))
        .collect();
    // Those the file imports, by name or through a wildcard.
    for imp in &m.imports {
        if imp.is_wildcard {
            if let Some(&t) = tree.graph.by_path.get::<[String]>(&imp.path) {
                in_scope.extend(trait_names(&tree.module(t).items));
            }
        }
        for it in &imp.items {
            in_scope.insert(it.alias.clone().unwrap_or_else(|| it.name.clone()));
            if let Some((_, origin)) = crate::module::canonical_origin(tree, &imp.path, &it.name) {
                in_scope.insert(origin);
            }
        }
    }
    let mut hidden = Hidden::new();
    for o in &tree.modules {
        if o.is_synthetic || o.package.is_none() || o.package == m.package {
            continue;
        }
        for name in trait_names(&o.items) {
            if !in_scope.contains(&name) {
                hidden.entry(name).or_insert_with(|| o.path.clone());
            }
        }
    }
    hidden
}
