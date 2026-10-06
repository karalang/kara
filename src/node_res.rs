//! What every name-bearing expression and pattern refers to, by [`NodeId`]
//! (redesign M0).
//!
//! The resolver records, for each `Identifier`, `Path`, struct literal and
//! struct / tuple-variant pattern, the symbol its first segment resolved to
//! and the segments after it ([`crate::resolver::NodeRef`]). [`node_res`]
//! finishes the job against the module-qualified [`ProgramDefs`]: an item
//! becomes [`Res::Def`] with its DefId, a variant or associated function named
//! by a path (`E.A`, `T.new`, `module.f`) becomes the DefId the whole path
//! names, and a local becomes [`Res::Local`].
//!
//! Method calls are not here: which method `x.m()` calls depends on the type
//! of `x`, which the typechecker knows and the resolver does not.

use rustc_hash::FxHashMap;

use crate::def_table::ProgramDefs;
use crate::ids::{DefId, DefKind, NodeId};
use crate::module::{self, ModuleId, ModulePath, ProgramTree};
use crate::resolver::{NodeRef, ResolveResult, SymbolId, SymbolKind};

/// What a name-bearing node refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Res {
    /// A definition: item, variant, or associated function.
    Def(DefId),
    /// A local variable, parameter or `self`. The symbol is unique per binding;
    /// [`ResolveResult::binding_nodes`] gives the pattern that binds it, when
    /// there is one.
    Local(SymbolId),
    /// A generic type or const parameter.
    Generic(SymbolId),
    /// `Self` inside an impl or trait.
    SelfTy,
    /// A name the compiler provides with no definition in any source:
    /// `println`, `i64`, `i64.max`, …. Carries the path as written.
    Builtin(String),
    /// A path whose first segment resolved but whose rest names nothing.
    Err,
}

enum Cursor {
    Def(DefId),
    Module(ModulePath),
}

/// Resolve every node the resolver recorded for module `module`. `tree` is the
/// project's module tree, or `None` for a single file.
pub fn node_res(
    result: &ResolveResult,
    defs: &ProgramDefs,
    module: ModuleId,
    tree: Option<&ProgramTree>,
) -> FxHashMap<NodeId, Res> {
    result
        .node_refs
        .iter()
        .map(|(&node, r)| (node, res_of(result, defs, module, tree, r)))
        .collect()
}

fn res_of(
    result: &ResolveResult,
    defs: &ProgramDefs,
    module: ModuleId,
    tree: Option<&ProgramTree>,
    r: &NodeRef,
) -> Res {
    let sym = result.symbol_table.get_symbol(r.symbol);
    let start = match &sym.kind {
        SymbolKind::Variable { .. } | SymbolKind::SelfValue => {
            return if r.rest.is_empty() {
                Res::Local(r.symbol)
            } else {
                Res::Err
            };
        }
        SymbolKind::TypeParam | SymbolKind::ConstParam => return Res::Generic(r.symbol),
        SymbolKind::EnumVariant { parent_enum, .. } => {
            let parent = result.symbol_table.get_symbol(*parent_enum);
            match defs
                .lookup(module, &parent.name)
                .and_then(|e| defs.variant(e, &sym.name))
            {
                Some(v) => Cursor::Def(v),
                None => return builtin(&sym.name, &r.rest),
            }
        }
        SymbolKind::Module => Cursor::Module(vec![sym.name.clone()]),
        SymbolKind::Import { path } => {
            let is_module = tree.is_some_and(|t| t.graph.lookup(path).is_some());
            if is_module {
                Cursor::Module(path.clone())
            } else {
                match defs.lookup(module, &sym.name) {
                    Some(d) => Cursor::Def(d),
                    None => return builtin(&sym.name, &r.rest),
                }
            }
        }
        _ if sym.name == "Self" && r.rest.is_empty() => return Res::SelfTy,
        _ => match defs
            .lookup(module, &sym.name)
            .or_else(|| defs.prelude_variant(&sym.name))
        {
            Some(d) => Cursor::Def(d),
            None => return builtin(&sym.name, &r.rest),
        },
    };
    let mut cur = start;
    for seg in &r.rest {
        cur = match cur {
            Cursor::Def(d) => match member(defs, d, seg) {
                Some(m) => Cursor::Def(m),
                // A baked stdlib type's compiler-provided operations
                // (`Vec.new`) have no definition of their own.
                None if defs.table.get(d).path.segments.first().map(String::as_str)
                    == Some("std") =>
                {
                    return builtin(&sym.name, &r.rest)
                }
                None => return Res::Err,
            },
            Cursor::Module(path) => {
                let mut sub = path.clone();
                sub.push(seg.clone());
                let tree = match tree {
                    Some(t) => t,
                    None => return Res::Err,
                };
                if tree.graph.lookup(&sub).is_some() {
                    Cursor::Module(sub)
                } else {
                    match module::canonical_origin(tree, &path, seg)
                        .and_then(|(p, n)| defs.module_items[tree.graph.lookup(&p)?].get(&n))
                    {
                        Some(&d) => Cursor::Def(d),
                        None => return Res::Err,
                    }
                }
            }
        };
    }
    match cur {
        Cursor::Def(d) => Res::Def(d),
        Cursor::Module(_) => Res::Err,
    }
}

/// `seg` as a member of definition `d`: a variant of an enum, else an
/// associated function or method, preferring the inherent one.
fn member(defs: &ProgramDefs, d: DefId, seg: &str) -> Option<DefId> {
    if defs.table.get(d).kind == DefKind::Enum {
        if let Some(v) = defs.variant(d, seg) {
            return Some(v);
        }
    }
    let cands = defs.methods.get(&d)?.get(seg)?;
    cands
        .iter()
        .copied()
        .find(|&m| !defs.is_trait_method(m))
        .or_else(|| cands.first().copied())
}

fn builtin(first: &str, rest: &[String]) -> Res {
    let mut s = first.to_string();
    for seg in rest {
        s.push('.');
        s.push_str(seg);
    }
    Res::Builtin(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::Resolver;
    use crate::walker::{walk_project, WalkerOpts};

    /// Every recorded reference in `module`, as `written path -> rendered res`.
    fn refs(tree: &ProgramTree, defs: &ProgramDefs, id: ModuleId) -> Vec<(String, String)> {
        let program = crate::ast::Program {
            items: tree.modules[id].items.clone(),
            ..Default::default()
        };
        let result = Resolver::new(&program).with_tree(tree, id).resolve();
        let res = node_res(&result, defs, id, Some(tree));
        let mut out: Vec<(String, String)> = result
            .node_refs
            .iter()
            .map(|(node, r)| {
                let mut written = result.symbol_table.get_symbol(r.symbol).name.clone();
                for s in &r.rest {
                    written.push('.');
                    written.push_str(s);
                }
                let rendered = match &res[node] {
                    Res::Def(d) => defs.table.get(*d).path.render(),
                    Res::Local(s) => format!(
                        "local {}{}",
                        result.symbol_table.get_symbol(*s).name,
                        if result.binding_nodes.contains_key(s) {
                            " (pattern)"
                        } else {
                            ""
                        }
                    ),
                    other => format!("{other:?}"),
                };
                (written, rendered)
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    fn project(files: &[(&str, &str)]) -> ProgramTree {
        let root = std::env::temp_dir().join(format!("karac-node-res-{}", std::process::id()));
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

    #[test]
    fn paths_resolve_to_module_qualified_defs() {
        let tree = project(&[
            (
                "kara.toml",
                "[package]\nname = \"p\"\nversion = \"0.1.0\"\n",
            ),
            (
                "src/shapes.kara",
                "pub enum Shape { Dot, Sq(i64) }\n\
                 pub struct P { pub x: i64 }\n\
                 impl P { pub fn new(x: i64) -> P { P { x: x } } }\n\
                 pub fn helper() -> i64 { 1 }\n",
            ),
            (
                "src/main.kara",
                "import shapes.{Shape, P, helper};\n\
                 fn helper2(s: Shape) -> i64 {\n\
                 \x20   match s { Shape.Sq(n) => n, Shape.Dot => 0 }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let p = P.new(helper());\n\
                 \x20   let q = P { x: p.x };\n\
                 \x20   println(helper2(Shape.Sq(q.x)));\n\
                 \x20   let o = Some(1);\n\
                 \x20   match o { Some(n) => println(n), None => println(0) }\n\
                 }\n",
            ),
        ]);
        let defs = ProgramDefs::build(&tree);
        let got = refs(&tree, &defs, tree.root);
        let want = |w: &str, r: &str| {
            assert!(
                got.iter().any(|(a, b)| a == w && b == r),
                "missing {w} -> {r} in {got:#?}"
            )
        };
        want("Shape.Sq", "shapes::Shape::Sq");
        want("Shape.Dot", "shapes::Shape::Dot");
        want("P.new", "shapes::P::impl#0::new");
        want("P", "shapes::P");
        want("helper", "shapes::helper");
        want("helper2", "helper2");
        want("p", "local p (pattern)");
        want("s", "local s (pattern)");
        want("println", "Builtin(\"println\")");
        want("Some", "std::option::Option::Some");
        want("None", "std::option::Option::None");
        assert!(!got.iter().any(|(_, r)| r == "Err"), "{got:#?}");
    }

    #[test]
    fn selfhost_names_resolve_without_errors() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("selfhost");
        let walked = walk_project(&root, WalkerOpts::default()).unwrap();
        let tree = module::build_program_tree(&walked).unwrap().tree;
        let defs = ProgramDefs::build(&tree);
        let mut errs = Vec::new();
        let mut total = 0;
        for id in 0..tree.modules.len() {
            if tree.modules[id].is_synthetic {
                continue;
            }
            for (w, r) in refs(&tree, &defs, id) {
                total += 1;
                if r == "Err" {
                    errs.push(format!("{}: {w}", tree.modules[id].file.display()));
                }
            }
        }
        assert!(total > 1000, "only {total} distinct references");
        assert!(errs.is_empty(), "{errs:#?}");
        // A bare variant in `ast` names `ast::Expr`'s, not anything of `token`'s.
        let ast = tree.graph.lookup(&["ast".to_string()]).unwrap();
        let in_ast = refs(&tree, &defs, ast);
        assert!(
            in_ast
                .iter()
                .any(|(w, r)| w == "Int" && r == "ast::Expr::Int"),
            "{in_ast:#?}"
        );
    }
}
