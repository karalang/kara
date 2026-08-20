//! Module-scoped name disambiguation for the FLATTENED compilation unit.
//!
//! # Why this exists
//!
//! design.md § Module System namespaces every top-level declaration by its
//! module path: `db.connection.open` and `db.pool.open` are different items,
//! and two modules of one package may both declare an `open`, a `Node`, or a
//! `helper`. Per-module resolution honours that — `karac check` accepts such a
//! package.
//!
//! Both EXECUTION paths, though, concatenate every module's items into one flat
//! `Program` and resolve it in a single scope: `karac run`'s
//! `build_super_program_for_run` — which `karac test` also runs on — and
//! `karac build`'s `run_multi_file_codegen`.
//! Nothing in that flat unit remembers which module an item came from, so two
//! same-named items became a `SymbolTable::define` duplicate and the program was
//! rejected — *by the executors only*, while the checker said it was fine
//! (B-2026-08-20-24). The reported span made it worse: module span rebasing
//! keeps `line`/`column` file-local, so the clash rendered against the ENTRY
//! file at the other module's line and column, pointing at whatever happened to
//! sit there.
//!
//! Any two modules with a `new`, a `parse`, a `size`, or a private `helper` hit
//! this, so it was not an exotic shape.
//!
//! # What this does
//!
//! Before the merge, every module that has to give up a name gets its
//! declaration renamed to a fresh one, and every reference to it — in that
//! module and in the modules that import it — is rewritten to match. The
//! rewrite reuses [`crate::import_alias`]'s substitution walker, which already
//! existed to keep import ALIASES alive across the same merge; a rename and an
//! alias are the same operation on the same AST, so they are applied as one
//! substitution rather than two passes.
//!
//! # What it deliberately does not do
//!
//! * **Nothing happens without a collision.** A program whose module-scoped
//!   names are already distinct — which is every program that worked before —
//!   gets an empty rename map and is copied unchanged.
//! * **The root module never renames.** Entry-file items hoist to the package
//!   root and are the names a reader of the program sees first; `main` is one of
//!   them, and it must stay `main` for codegen to find it.
//! * **Names that ARE linkage are never renamed** — a foreign import, a Kāra
//!   `extern "C"` export, or anything marked `#[unsafe(no_mangle)]`. Moving one
//!   would retarget or unexport a symbol resolved by name from outside the
//!   program, which is exactly what design.md's `#[unsafe(no_mangle)]` exists to
//!   prevent. Two modules declaring the same foreign function keep the duplicate
//!   they always had.
//! * **A name the module also binds as a local keeps its name.** Rewriting
//!   references is syntactic, so a `let helper = …` sharing the item's name
//!   would be rewritten with it. Such a module is left alone rather than risking
//!   a silently redirected variable — the same conservative guard the alias
//!   rewrite has always used. It degrades gently: the collision is broken as
//!   soon as ONE declarer moves, so a program only keeps its pre-existing
//!   duplicate error when every module declaring the name is guarded.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::module::{self, ModuleId, ProgramTree};
use crate::token::Span;

/// Per-module `declared name -> name it takes in the flat unit`, for the names
/// that collide across modules. Empty for every module in a program whose
/// top-level names are already distinct.
#[derive(Debug, Default)]
pub struct FlatRenames {
    per_module: HashMap<ModuleId, HashMap<String, String>>,
}

impl FlatRenames {
    /// What `name`, declared by module `id`, is called in the flat unit —
    /// `None` when it keeps the name it was written with.
    fn get(&self, id: ModuleId, name: &str) -> Option<&String> {
        self.per_module.get(&id)?.get(name)
    }

    /// No module had to give up a name — the state every program that already
    /// worked is in, and the one in which flattening copies items unchanged.
    pub fn is_empty(&self) -> bool {
        self.per_module.is_empty()
    }
}

/// Decide what every module has to be called in the flat unit.
///
/// A name declared by two or more non-synthetic modules is a collision. Each
/// declarer except the root gets a fresh name derived from its module path, so
/// the two `common`s of `alpha` and `beta` become `common__alpha` and
/// `common__beta`. The derived name is checked against every name the program
/// declares (and against the ones already minted) so it cannot introduce a
/// second collision of its own.
pub fn plan(tree: &ProgramTree) -> FlatRenames {
    // Deterministic module order — `ModuleId` is the tree's own registration
    // index, so iterating it sorted gives the same plan on every run,
    // regardless of hash iteration order.
    let mut ids: Vec<ModuleId> = tree
        .modules
        .iter()
        .filter(|m| !m.is_synthetic)
        .map(|m| m.id)
        .collect();
    ids.sort_unstable();

    let mut taken: HashSet<String> = HashSet::new();
    let mut names_of: HashMap<ModuleId, Vec<String>> = HashMap::new();
    for &id in &ids {
        let mut names: Vec<String> = crate::import_alias::declared_names(&tree.module(id).items)
            .into_iter()
            .collect();
        names.sort_unstable(); // a HashSet — sorted so minting order is stable
        for name in &names {
            taken.insert(name.clone());
        }
        names_of.insert(id, names);
    }

    let mut declared_by: HashMap<&str, Vec<ModuleId>> = HashMap::new();
    for &id in &ids {
        for name in &names_of[&id] {
            declared_by.entry(name.as_str()).or_default().push(id);
        }
    }
    let mut colliding: Vec<&str> = declared_by
        .iter()
        .filter(|(_, mods)| mods.len() > 1)
        .map(|(name, _)| *name)
        .collect();
    colliding.sort_unstable();
    let mut renames = FlatRenames::default();
    if colliding.is_empty() {
        return renames; // the overwhelmingly common case — nothing to do
    }

    // Only now that a collision is known to exist, pay for the guard set:
    // `local_value_bindings` walks every function body in the module.
    let mut locals_of: HashMap<ModuleId, HashSet<String>> = HashMap::new();
    for &id in &ids {
        let mut items = tree.module(id).items.clone();
        locals_of.insert(id, crate::import_alias::local_value_bindings(&mut items));
    }

    for name in colliding {
        for &id in &declared_by[name] {
            if id == tree.root {
                continue; // the root keeps every name it declares
            }
            if locals_of[&id].contains(name) || name_is_linkage(tree.module(id), name) {
                continue; // see the module doc's two guards
            }
            let fresh = mint(name, &tree.module(id).path, &mut taken);
            renames
                .per_module
                .entry(id)
                .or_default()
                .insert(name.to_string(), fresh);
        }
    }
    renames
}

/// Does `m` declare `name` in a position where the identifier IS the linkage?
/// Those are never renamed — moving one would retarget or unexport a symbol
/// something outside the program resolves by name.
///
/// Three shapes qualify:
///
/// * a foreign IMPORT (`unsafe extern "C" { fn abs(...); }`) — the name is the
///   C symbol being linked against;
/// * a Kāra EXPORT under a foreign ABI (`pub extern "C" fn kernel_main()`) —
///   codegen gives it External linkage under its bare source name, which is the
///   whole point of the marker (see `declare_function`'s linkage arm);
/// * anything carrying `#[unsafe(no_mangle)]`, which design.md §
///   `#[unsafe(no_mangle)]` vs ABI defines as "use the Kāra identifier as-is;
///   disable name mangling". `declare_function` already records it as the
///   opt-out "future mangling passes" should honour — this is one.
fn name_is_linkage(m: &module::Module, name: &str) -> bool {
    m.items.iter().any(|it| match it {
        Item::ExternFunction(f) => f.name == name,
        Item::ExternBlock(b) => b.items.iter().any(|i| match i {
            ExternItem::Function(f) => f.name == name,
            ExternItem::OpaqueType(o) => o.name == name,
        }),
        Item::Function(f) => f.name == name && (f.abi.is_some() || has_no_mangle(&f.attributes)),
        Item::ConstDecl(c) => c.name == name && has_no_mangle(&c.attributes),
        Item::ModuleBinding(b) => b.name == name && has_no_mangle(&b.attributes),
        _ => false,
    })
}

fn has_no_mangle(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|a| a.is_bare("no_mangle"))
}

/// Derive a free name for `name` as declared by the module at `path`.
///
/// `alpha`'s `common` becomes `common__alpha`, `db.conn`'s becomes
/// `common__db_conn`. The suffix goes last so the leading character — which
/// carries Kāra's Value-vs-Type naming class — is preserved. A derived name
/// that is somehow taken gets a numeric tail rather than silently colliding.
fn mint(name: &str, path: &[String], taken: &mut HashSet<String>) -> String {
    let suffix = if path.is_empty() {
        "root".to_string()
    } else {
        path.join("_")
    };
    let base = format!("{name}__{suffix}");
    let mut candidate = base.clone();
    let mut n = 2;
    while taken.contains(&candidate) {
        candidate = format!("{base}_{n}");
        n += 1;
    }
    taken.insert(candidate.clone());
    candidate
}

/// One module's items, ready to be appended to the flat unit: `import`
/// declarations dropped, colliding declarations renamed, and every reference —
/// to a renamed item or through an import alias — rewritten to the name the
/// item actually has in the flat unit.
///
/// This is the whole of what each merge site has to do per module, so both of
/// them call it and cannot drift apart again.
pub fn flatten_module_items(tree: &ProgramTree, id: ModuleId, renames: &FlatRenames) -> Vec<Item> {
    let m = tree.module(id);
    let mut items: Vec<Item> = m
        .items
        .iter()
        .filter(|it| !matches!(it, Item::Import(_)))
        .cloned()
        .collect();

    // Both guard sets read the module's ORIGINAL names, so they are taken
    // before any declaration moves.
    let local_names = crate::import_alias::declared_names(&items);
    let bound_values = crate::import_alias::bound_value_names(&mut items);

    let mut subst: HashMap<String, TypeExpr> = HashMap::new();
    if let Some(own) = renames.per_module.get(&id) {
        for item in &mut items {
            if let Some((old, span)) = rename_declaration(item, own) {
                let fresh = own[&old].clone();
                subst.insert(old, name_type_expr(&fresh, span));
            }
        }
    }
    add_import_subst(tree, m, renames, &local_names, &bound_values, &mut subst);

    for item in &mut items {
        crate::import_alias::rewrite_item(item, &subst);
    }
    items
}

/// Add, to `subst`, every name this module IMPORTS whose flat-unit spelling
/// differs from the name it is bound under — because the declaring module
/// renamed it, because the import carried an `as` alias, or both.
///
/// Both cases live in one map on purpose. `import m.{Impl as Widget};` where
/// `Impl` was also renamed has to land on the renamed name in a single step;
/// composing an alias pass with a rename pass would have the first produce a
/// name the second no longer recognises.
fn add_import_subst(
    tree: &ProgramTree,
    m: &module::Module,
    renames: &FlatRenames,
    local_names: &HashSet<String>,
    bound_values: &HashSet<String>,
    subst: &mut HashMap<String, TypeExpr>,
) {
    for imp in &m.imports {
        for item in &imp.items {
            let bound = item.alias.clone().unwrap_or_else(|| item.name.clone());
            // A local declaration or a local value binding owns the name;
            // rewriting a reference to the import's target would silently
            // redirect it. This module's OWN rename is already in `subst` and
            // is not affected — that entry IS the declaration. Same guard, and
            // the same reason, as the alias rewrite has always applied.
            if subst.contains_key(&bound)
                || local_names.contains(&bound)
                || bound_values.contains(&bound)
            {
                continue;
            }
            // Follow `pub import` re-exports to the module that really
            // declares the item: that is the one whose rename applies, and its
            // name is the one actually present in the flat unit.
            let (def_path, def_name) = module::canonical_origin(tree, &imp.path, &item.name)
                .unwrap_or_else(|| (imp.path.clone(), item.name.clone()));
            let target = tree
                .graph
                .lookup(&def_path)
                .and_then(|def_id| renames.get(def_id, &def_name))
                .cloned()
                .unwrap_or(def_name);
            if target != bound {
                subst.insert(bound, name_type_expr(&target, item.span));
            }
        }
    }
}

fn name_type_expr(name: &str, span: Span) -> TypeExpr {
    TypeExpr {
        kind: TypeKind::Path(PathExpr {
            segments: vec![name.to_string()],
            generic_args: None,
            span,
        }),
        span,
    }
}

/// Rename the DECLARATION `item` introduces, if this module gave that name up.
/// References are the substitution walker's job; this only moves the
/// declaration site, which no substitution touches.
fn rename_declaration(
    item: &mut Item,
    renames: &HashMap<String, String>,
) -> Option<(String, Span)> {
    let (slot, span): (&mut String, Span) = match item {
        Item::Function(f) => (&mut f.name, f.span),
        Item::StructDef(s) => (&mut s.name, s.span),
        Item::UnionDef(u) => (&mut u.name, u.span),
        Item::EnumDef(e) => (&mut e.name, e.span),
        Item::TraitDef(t) => (&mut t.name, t.span),
        Item::TraitAlias(t) => (&mut t.name, t.span),
        Item::MarkerTrait(t) => (&mut t.name, t.span),
        Item::ConstDecl(c) => (&mut c.name, c.span),
        Item::TypeAlias(t) => (&mut t.name, t.span),
        Item::DistinctType(d) => (&mut d.name, d.span),
        Item::EffectResource(r) => (&mut r.name, r.span),
        Item::EffectGroup(g) => (&mut g.name, g.span),
        Item::EffectVerbDecl(v) => (&mut v.verb_name, v.span),
        Item::ModuleBinding(b) => (&mut b.name, b.span),
        Item::LayoutDef(l) => (&mut l.name, l.span),
        // `extern` names are linkage, never renamed (see the module doc);
        // impl blocks and the rest declare no module-scope name of their own.
        Item::ExternFunction(_)
        | Item::ExternBlock(_)
        | Item::ImplBlock(_)
        | Item::UseDecl(_)
        | Item::Import(_)
        | Item::AliasDecl(_)
        | Item::IndependentDecl(_)
        | Item::TestCase(_) => return None,
    };
    let fresh = renames.get(slot.as_str())?.clone();
    let old = std::mem::replace(slot, fresh);
    Some((old, span))
}
