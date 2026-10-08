//! The module-qualified definition table (redesign M0).
//!
//! [`ProgramDefs::build`] walks a [`ProgramTree`] once and gives every
//! definition a [`DefId`] named by its full [`DefPath`]: the module path, then
//! the item name. Two modules that both declare `helper` get two DefIds,
//! `a::helper` and `b::helper`, so nothing downstream of this table needs the
//! flat-unit renaming `module_rename` does for the legacy backend.
//!
//! Paths:
//!
//! * a top-level item: `[..module, name]` (a root-module item is just `[name]`);
//! * an enum variant: `[..enum, Variant]`;
//! * a trait's declared method: `[..trait, method]`;
//! * an impl block: `[..type, "impl#n"]` or `[..type, "impl Trait#n"]`;
//! * an impl's method: `[..impl, method]`;
//! * a baked stdlib item: `["std", <file stem>, name]`.
//!
//! An impl whose target is not a definition in this table (a builtin such as
//! `i64`, or a name that does not resolve) is filed under `["<builtin>", name]`.
//!
//! [`ProgramDefs::lookup`] answers "what does the bare name `n` mean at the top
//! level of module `m`": the module's own item, else the item an import binds
//! to that name (following re-exports to the defining module), else a baked
//! stdlib item. Builtin types (`i64`, `Vec`'s compiler-known operations, …) are
//! not definitions and have no DefId.

use std::collections::HashMap;

use crate::ast::{ExternItem, GenericParams, ImplItem, Item, Program, TraitItem, TypeKind};
use crate::def_path::DefPath;
use crate::ids::{DefId, DefKind, DefTable};
use crate::module::{self, ModuleId, ProgramTree};

/// Every definition in a program, and how each module's top-level names map to
/// them.
#[derive(Debug, Clone, Default)]
pub struct ProgramDefs {
    pub table: DefTable,
    /// Per module (indexed by [`ModuleId`]): the items the module itself
    /// declares, by name.
    pub module_items: Vec<HashMap<String, DefId>>,
    /// Per module: what each name an `import` binds refers to.
    pub module_imports: Vec<HashMap<String, DefId>>,
    /// Baked stdlib items, by name.
    pub stdlib_items: HashMap<String, DefId>,
    /// Enum → its variants, by name.
    pub variants: HashMap<DefId, HashMap<String, DefId>>,
    /// Type or trait → its methods, by name. A type's trait-impl methods are
    /// listed here too; [`ProgramDefs::impls`] on a method's parent tells which
    /// trait it is from.
    pub methods: HashMap<DefId, HashMap<String, Vec<DefId>>>,
    /// Every impl block.
    pub impls: HashMap<DefId, ImplData>,
    /// Per module (indexed by [`ModuleId`]): its impl blocks' DefIds, in the
    /// order the blocks appear among its items. An impl's `#n` counts every
    /// impl of the same target and label, the stdlib's included, so it cannot
    /// be recomputed from one module's items alone.
    pub module_impls: Vec<Vec<DefId>>,
    /// A method's impl block, or the trait that declares it.
    pub parent: HashMap<DefId, DefId>,
    /// Each generic definition's own type and const parameters, in declaration
    /// order. A method's impl parameters are on the impl; see
    /// [`ProgramDefs::generic_params`].
    pub generics: HashMap<DefId, Vec<String>>,
}

/// One impl block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplData {
    /// The type it is for, when that type is a definition (not a builtin).
    pub target: Option<DefId>,
    /// The trait it implements, by its last path segment, when it is a trait impl.
    pub trait_name: Option<String>,
    /// That trait's definition, when it resolves.
    pub trait_def: Option<DefId>,
}

fn generic_names(g: &Option<GenericParams>) -> Vec<String> {
    g.as_ref()
        .map(|g| g.params.iter().map(|p| p.name.clone()).collect())
        .unwrap_or_default()
}

fn item_generics(item: &Item) -> Vec<String> {
    match item {
        Item::Function(f) => generic_names(&f.generic_params),
        Item::StructDef(s) => generic_names(&s.generic_params),
        Item::EnumDef(e) => generic_names(&e.generic_params),
        Item::TraitDef(t) => generic_names(&t.generic_params),
        Item::TypeAlias(t) => generic_names(&t.generic_params),
        _ => Vec::new(),
    }
}

fn item_def(item: &Item) -> Vec<(&str, DefKind)> {
    match item {
        Item::Function(f) => vec![(f.name.as_str(), DefKind::Fn)],
        Item::ExternFunction(e) => vec![(e.name.as_str(), DefKind::Fn)],
        Item::ExternBlock(b) => b
            .items
            .iter()
            .map(|it| match it {
                ExternItem::Function(f) => (f.name.as_str(), DefKind::Fn),
                ExternItem::OpaqueType(o) => (o.name.as_str(), DefKind::OpaqueType),
            })
            .collect(),
        Item::StructDef(s) => vec![(s.name.as_str(), DefKind::Struct)],
        Item::UnionDef(u) => vec![(u.name.as_str(), DefKind::Union)],
        Item::EnumDef(e) => vec![(e.name.as_str(), DefKind::Enum)],
        Item::TraitDef(t) => vec![(t.name.as_str(), DefKind::Trait)],
        Item::TraitAlias(t) => vec![(t.name.as_str(), DefKind::Trait)],
        Item::MarkerTrait(t) => vec![(t.name.as_str(), DefKind::Trait)],
        Item::TypeAlias(t) => vec![(t.name.as_str(), DefKind::TypeAlias)],
        Item::DistinctType(d) => vec![(d.name.as_str(), DefKind::DistinctType)],
        Item::ConstDecl(c) => vec![(c.name.as_str(), DefKind::Const)],
        Item::ModuleBinding(b) => vec![(b.name.as_str(), DefKind::Static)],
        Item::ImplBlock(_)
        | Item::EffectResource(_)
        | Item::EffectGroup(_)
        | Item::EffectVerbDecl(_)
        | Item::LayoutDef(_)
        | Item::UseDecl(_)
        | Item::Import(_)
        | Item::AliasDecl(_)
        | Item::IndependentDecl(_)
        | Item::TestCase(_) => Vec::new(),
    }
}

/// Types the compiler provides with no source declaration, with their
/// generic parameters. Each gets a def at `std::builtin::<Name>` unless the
/// baked stdlib declares the name, so a Kāra definition replaces it as soon
/// as it lands.
const BUILTIN_TYPES: &[(&str, DefKind, &[&str])] = &[
    ("Range", DefKind::Struct, &["T"]),
    ("RangeInclusive", DefKind::Struct, &["T"]),
    ("RangeFrom", DefKind::Struct, &["T"]),
    ("RangeTo", DefKind::Struct, &["T"]),
    ("RangeToInclusive", DefKind::Struct, &["T"]),
    ("RangeFull", DefKind::Struct, &[]),
    ("Iterator", DefKind::Trait, &["Item"]),
    ("Unit", DefKind::Struct, &[]),
    ("StringSlice", DefKind::Struct, &[]),
    ("CStr", DefKind::Struct, &[]),
    ("CString", DefKind::Struct, &[]),
    ("GpuBuffer", DefKind::Struct, &["T"]),
];

/// The enum each prelude variant name belongs to.
const PRELUDE_VARIANT_ENUMS: &[(&str, &str)] = &[
    ("Some", "Option"),
    ("None", "Option"),
    ("Ok", "Result"),
    ("Err", "Result"),
    ("Less", "Ordering"),
    ("Equal", "Ordering"),
    ("Greater", "Ordering"),
    ("Relaxed", "MemoryOrdering"),
    ("Acquire", "MemoryOrdering"),
    ("Release", "MemoryOrdering"),
    ("AcqRel", "MemoryOrdering"),
    ("SeqCst", "MemoryOrdering"),
    ("Occupied", "Entry"),
    ("Vacant", "Entry"),
    ("Nfc", "NormalizationForm"),
    ("Nfd", "NormalizationForm"),
    ("Nfkc", "NormalizationForm"),
    ("Nfkd", "NormalizationForm"),
];

fn path_of(prefix: &[String], tail: &[&str]) -> DefPath {
    let mut segments = prefix.to_vec();
    segments.extend(tail.iter().map(|s| s.to_string()));
    DefPath::new(segments)
}

impl ProgramDefs {
    /// Number every definition in `tree`, plus the baked stdlib.
    pub fn build(tree: &ProgramTree) -> Self {
        let mut defs = ProgramDefs::default();
        defs.add_stdlib();
        let n = tree.modules.len();
        defs.module_items = vec![HashMap::new(); n];
        defs.module_imports = vec![HashMap::new(); n];

        // Declarations first, so impls and imports can refer to any of them.
        for (id, m) in tree.modules.iter().enumerate() {
            let items = defs.add_items(&m.path, &m.items);
            defs.module_items[id] = items;
        }
        for id in 0..n {
            defs.module_imports[id] = Self::imports_of(&defs, tree, id);
        }
        defs.module_impls = vec![Vec::new(); n];
        for (id, m) in tree.modules.iter().enumerate() {
            defs.module_impls[id] = defs.add_impls(&m.items, |defs, name| defs.lookup(id, name));
        }
        defs
    }

    /// Number every definition of a single-file program, as the root module
    /// of a one-module tree.
    pub fn build_for_program(program: &Program) -> Self {
        let mut defs = ProgramDefs::default();
        defs.add_stdlib();
        let items = defs.add_items(&[], &program.items);
        defs.module_items = vec![items];
        defs.module_imports = vec![HashMap::new()];
        defs.module_impls = vec![defs.add_impls(&program.items, |defs, name| defs.lookup(0, name))];
        defs
    }

    /// What the bare name `name` refers to at the top level of `module`.
    pub fn lookup(&self, module: ModuleId, name: &str) -> Option<DefId> {
        self.module_items
            .get(module)
            .and_then(|m| m.get(name))
            .or_else(|| self.module_imports.get(module).and_then(|m| m.get(name)))
            .or_else(|| self.stdlib_items.get(name))
            .copied()
    }

    /// Is `def` the compiler's `std::builtin::Unit`, which names `()`?
    pub fn is_builtin_unit(&self, def: DefId) -> bool {
        self.table.get(def).path.segments == ["std", "builtin", "Unit"]
    }

    /// Every type and const parameter in scope inside `def`, in the order a
    /// substitution lists them: for a method, its impl's (or trait's)
    /// parameters, then its own; otherwise the definition's own.
    pub fn generic_params(&self, def: DefId) -> Vec<String> {
        let mut out = self
            .parent
            .get(&def)
            .and_then(|p| self.generics.get(p))
            .cloned()
            .unwrap_or_default();
        out.extend(self.generics.get(&def).cloned().unwrap_or_default());
        out
    }

    /// Whether method `m` comes from a trait impl (or a trait declaration).
    pub fn is_trait_method(&self, m: DefId) -> bool {
        match self.parent.get(&m) {
            Some(p) => match self.impls.get(p) {
                Some(i) => i.trait_name.is_some(),
                None => true,
            },
            None => false,
        }
    }

    /// The stdlib variant a bare prelude variant name (`Some`, `Ok`,
    /// `Less`, ...) refers to, when the stdlib declares its enum.
    pub fn prelude_variant(&self, name: &str) -> Option<DefId> {
        let (_, parent) = PRELUDE_VARIANT_ENUMS.iter().find(|(v, _)| *v == name)?;
        self.variant(*self.stdlib_items.get(*parent)?, name)
    }

    /// The variant `name` of the enum `enum_def`.
    pub fn variant(&self, enum_def: DefId, name: &str) -> Option<DefId> {
        self.variants.get(&enum_def)?.get(name).copied()
    }

    fn add_stdlib(&mut self) {
        for (file, program) in crate::prelude::STDLIB_PROGRAMS.iter() {
            let stem = file.strip_suffix(".kara").unwrap_or(file);
            let prefix = vec!["std".to_string(), stem.to_string()];
            let items = self.add_items(&prefix, &program.items);
            self.stdlib_items.extend(items);
        }
        let builtin = ["std".to_string(), "builtin".to_string()];
        for &(name, kind, params) in BUILTIN_TYPES {
            if self.stdlib_items.contains_key(name) {
                continue;
            }
            let id = self.table.intern(path_of(&builtin, &[name]), kind);
            if !params.is_empty() {
                self.generics
                    .insert(id, params.iter().map(|p| p.to_string()).collect());
            }
            self.stdlib_items.insert(name.to_string(), id);
        }
        for (_, program) in crate::prelude::STDLIB_PROGRAMS.iter() {
            self.add_impls(&program.items, |defs, name| {
                defs.stdlib_items.get(name).copied()
            });
        }
    }

    /// Intern the declarations in `items` under `prefix`; returns them by name.
    fn add_items(&mut self, prefix: &[String], items: &[Item]) -> HashMap<String, DefId> {
        let mut out = HashMap::new();
        for item in items {
            for (name, kind) in item_def(item) {
                let id = self.table.intern(path_of(prefix, &[name]), kind);
                out.entry(name.to_string()).or_insert(id);
                let g = item_generics(item);
                if !g.is_empty() {
                    self.generics.insert(id, g);
                }
                match item {
                    Item::EnumDef(e) => {
                        for v in &e.variants {
                            let vid = self
                                .table
                                .intern(path_of(prefix, &[name, &v.name]), DefKind::Variant);
                            self.variants
                                .entry(id)
                                .or_default()
                                .insert(v.name.clone(), vid);
                        }
                    }
                    Item::TraitDef(t) => {
                        for m in t.items.iter().filter_map(|ti| match ti {
                            TraitItem::Method(m) => Some(m),
                            TraitItem::AssocType(_) => None,
                        }) {
                            let mid = self
                                .table
                                .intern(path_of(prefix, &[name, &m.name]), DefKind::Method);
                            self.parent.insert(mid, id);
                            let g = generic_names(&m.generic_params);
                            if !g.is_empty() {
                                self.generics.insert(mid, g);
                            }
                            self.methods
                                .entry(id)
                                .or_default()
                                .entry(m.name.clone())
                                .or_default()
                                .push(mid);
                        }
                    }
                    _ => {}
                }
            }
        }
        out
    }

    /// Intern every impl block in `items` and its methods. `resolve` names the
    /// impl's target type and trait the way its module sees them.
    ///
    /// An impl is `[..type, "impl#n"]` (inherent) or `[..type, "impl Trait#n"]`,
    /// `n` counting that type's impls of that trait in build order, so two
    /// impls of one generic trait (`From[i32]`, `From[String]`) stay apart; its
    /// methods hang off it.
    /// Intern the impl blocks in `items`; returns their DefIds in order.
    fn add_impls(
        &mut self,
        items: &[Item],
        resolve: impl Fn(&Self, &str) -> Option<DefId>,
    ) -> Vec<DefId> {
        let mut out = Vec::new();
        for item in items {
            let Item::ImplBlock(b) = item else { continue };
            let target_name = match &b.target_type.kind {
                TypeKind::Path(p) => p.segments.last().cloned(),
                _ => None,
            };
            let target = target_name.as_deref().and_then(|n| resolve(self, n));
            let target_path = match (target, &target_name) {
                (Some(t), _) => self.table.get(t).path.segments.clone(),
                (None, Some(n)) => vec!["<builtin>".to_string(), n.clone()],
                (None, None) => vec!["<builtin>".to_string(), "<type>".to_string()],
            };
            let trait_name = b
                .trait_name
                .as_ref()
                .and_then(|t| t.segments.last().cloned());
            let label = match &trait_name {
                Some(t) => format!("impl {t}"),
                None => "impl".to_string(),
            };
            let mut n = 0;
            let impl_path = loop {
                let p = path_of(&target_path, &[&format!("{label}#{n}")]);
                if self.table.lookup(&p).is_none() {
                    break p;
                }
                n += 1;
            };
            let impl_id = self.table.intern(impl_path.clone(), DefKind::Impl);
            out.push(impl_id);
            let trait_def = trait_name.as_deref().and_then(|t| resolve(self, t));
            self.impls.insert(
                impl_id,
                ImplData {
                    target,
                    trait_name: trait_name.clone(),
                    trait_def,
                },
            );
            let g = generic_names(&b.generic_params);
            if !g.is_empty() {
                self.generics.insert(impl_id, g);
            }
            for ii in &b.items {
                let ImplItem::Method(f) = ii else { continue };
                let mid = self
                    .table
                    .intern(path_of(&impl_path.segments, &[&f.name]), DefKind::Method);
                self.parent.insert(mid, impl_id);
                let g = generic_names(&f.generic_params);
                if !g.is_empty() {
                    self.generics.insert(mid, g);
                }
                if let Some(t) = target {
                    self.methods
                        .entry(t)
                        .or_default()
                        .entry(f.name.clone())
                        .or_default()
                        .push(mid);
                }
            }
        }
        out
    }

    /// The names module `id`'s imports bind, each followed to the module that
    /// defines it.
    fn imports_of(defs: &Self, tree: &ProgramTree, id: ModuleId) -> HashMap<String, DefId> {
        let mut out = HashMap::new();
        for item in &tree.modules[id].items {
            let Item::Import(imp) = item else { continue };
            for ii in &imp.items {
                let bound = ii.alias.as_deref().unwrap_or(&ii.name);
                let Some((origin_path, origin_name)) =
                    module::canonical_origin(tree, &imp.path, &ii.name)
                else {
                    continue;
                };
                let Some(origin) = tree.graph.lookup(&origin_path) else {
                    continue;
                };
                if let Some(&def) = defs.module_items[origin].get(&origin_name) {
                    out.insert(bound.to_string(), def);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::walker::{walk_project, WalkerOpts};

    fn project(files: &[(&str, &str)]) -> ProgramTree {
        let root = std::env::temp_dir().join(format!(
            "karac-def-table-{}-{}",
            std::process::id(),
            files.len()
        ));
        let _ = std::fs::remove_dir_all(&root);
        for (rel, src) in files {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, src).unwrap();
        }
        let walked = walk_project(&root, WalkerOpts::default()).unwrap();
        let tree = module::build_program_tree(&walked).unwrap().tree;
        let _ = std::fs::remove_dir_all(&root);
        tree
    }

    fn path(defs: &ProgramDefs, id: DefId) -> String {
        defs.table.get(id).path.render()
    }

    #[test]
    fn same_names_in_two_modules_get_two_defs() {
        let tree = project(&[
            ("kara.toml", "[package]\nname = \"p\"\nversion = \"0.1.0\"\n"),
            (
                "src/token.kara",
                "pub enum Part { Lit(String), Expr(i64) }\npub fn helper() -> i64 { 1 }\n",
            ),
            (
                "src/ast.kara",
                "pub shared enum Expr { Int(i64), Neg(Expr) }\n\
                 fn helper() -> i64 { 2 }\n\
                 impl Expr { pub fn size(self) -> i64 { 1 } }\n",
            ),
            (
                "src/main.kara",
                "import token.{Part, helper};\nimport ast.Expr;\nfn main() { println(helper()); }\n",
            ),
        ]);
        let defs = ProgramDefs::build(&tree);
        let token = tree.graph.lookup(&["token".to_string()]).unwrap();
        let ast = tree.graph.lookup(&["ast".to_string()]).unwrap();
        let root = tree.root;

        let t_helper = defs.lookup(token, "helper").unwrap();
        let a_helper = defs.lookup(ast, "helper").unwrap();
        assert_ne!(t_helper, a_helper);
        assert_eq!(path(&defs, t_helper), "token::helper");
        assert_eq!(path(&defs, a_helper), "ast::helper");

        // The root sees the names its imports bind, at their defining module.
        assert_eq!(defs.lookup(root, "helper"), Some(t_helper));
        let expr = defs.lookup(root, "Expr").unwrap();
        assert_eq!(path(&defs, expr), "ast::Expr");
        assert_eq!(defs.table.get(expr).kind, DefKind::Enum);

        // A variant named like a type in another module is its own def.
        let part = defs.lookup(token, "Part").unwrap();
        let variant = defs.variant(part, "Expr").unwrap();
        assert_eq!(path(&defs, variant), "token::Part::Expr");
        assert_ne!(variant, expr);

        // Methods hang off their type's path.
        let size = defs.methods[&expr]["size"][0];
        assert_eq!(path(&defs, size), "ast::Expr::impl#0::size");

        // `ast` does not import `Part`, so it does not see it.
        assert_eq!(defs.lookup(ast, "Part"), None);
    }

    #[test]
    fn stdlib_items_are_qualified_and_visible() {
        let p = crate::parse("fn main() {}\n").program;
        let defs = ProgramDefs::build_for_program(&p);
        let option = defs.lookup(0, "Option").unwrap();
        assert_eq!(path(&defs, option), "std::option::Option");
        let some = defs.variant(option, "Some").unwrap();
        assert_eq!(path(&defs, some), "std::option::Option::Some");
        assert_eq!(path(&defs, defs.lookup(0, "main").unwrap()), "main");
        assert_eq!(defs.prelude_variant("Some"), Some(some));
        assert_eq!(
            path(&defs, defs.prelude_variant("Err").unwrap()),
            "std::result::Result::Err"
        );
        // Compiler-provided types without a declaration get a builtin def.
        let range = defs.lookup(0, "Range").unwrap();
        assert_eq!(path(&defs, range), "std::builtin::Range");
        assert_eq!(defs.generic_params(range), ["T"]);
        let iter = defs.lookup(0, "Iterator").unwrap();
        assert_eq!(defs.table.get(iter).kind, DefKind::Trait);
    }

    #[test]
    fn selfhost_tree_keeps_its_two_exprs_apart() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("selfhost");
        let walked = walk_project(&root, WalkerOpts::default()).unwrap();
        let tree = module::build_program_tree(&walked).unwrap().tree;
        let defs = ProgramDefs::build(&tree);
        let ast = tree.graph.lookup(&["ast".to_string()]).unwrap();
        let token = tree.graph.lookup(&["token".to_string()]).unwrap();
        let expr = defs.lookup(ast, "Expr").unwrap();
        assert_eq!(path(&defs, expr), "ast::Expr");
        let part = defs.lookup(token, "InterpPart").unwrap();
        assert_eq!(
            path(&defs, defs.variant(part, "Expr").unwrap()),
            "token::InterpPart::Expr"
        );
    }

    #[test]
    fn impls_get_defs_and_generics_list_impl_then_fn() {
        let p = crate::parse(
            "struct W[T] { v: T }\n\
             trait Conv[S] { fn conv(s: S) -> Self; }\n\
             impl[T] W[T] { fn map[U](self, u: U) -> U { u } }\n\
             impl Conv[i64] for W[i64] { fn conv(s: i64) -> W[i64] { W { v: s } } }\n\
             impl Conv[bool] for W[i64] { fn conv(s: bool) -> W[i64] { W { v: 0 } } }\n\
             fn main() {}\n",
        )
        .program;
        let defs = ProgramDefs::build_for_program(&p);
        let w = defs.lookup(0, "W").unwrap();
        assert_eq!(defs.generic_params(w), vec!["T"]);
        let map = defs.methods[&w]["map"][0];
        assert_eq!(path(&defs, map), "W::impl#0::map");
        assert_eq!(defs.generic_params(map), vec!["T", "U"]);
        assert!(!defs.is_trait_method(map));
        let convs = &defs.methods[&w]["conv"];
        assert_eq!(convs.len(), 2);
        let rendered: Vec<String> = convs.iter().map(|&c| path(&defs, c)).collect();
        assert_eq!(
            rendered,
            vec!["W::impl Conv#0::conv", "W::impl Conv#1::conv"]
        );
        assert!(convs.iter().all(|&c| defs.is_trait_method(c)));
        let imp = defs.parent[&convs[0]];
        assert_eq!(defs.table.get(imp).kind, DefKind::Impl);
        assert_eq!(defs.impls[&imp].target, Some(w));
        assert_eq!(defs.impls[&imp].trait_def, defs.lookup(0, "Conv"));
    }
}
