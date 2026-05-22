//! Stable item identity — path-based DefIds with sub-item structural
//! hashing. Phase-8 stdlib-floor § Compiler queries channel sub-item 1.
//!
//! Today the compiler keys items by `SpanKey` (byte offset + length).
//! That breaks under any source edit that shifts later offsets — adding
//! a single line to the top of a file invalidates every downstream
//! `SpanKey`. Tools that store resolved query answers across compile
//! runs need an identity scheme that survives unrelated edits.
//!
//! [`DefPath`] is the stable identity for top-level items: module path
//! plus item name. [`SubItemHash`] addresses sub-item decision sites
//! (an `if` branch inside a function body, a `match` arm, etc.) via
//! structural hash over the AST subtree, ignoring spans. The pair
//! [`QueryId`] (def_path + sub_item_hash) is the durable key the
//! compiler-query channel emits and tools store.
//!
//! v1 ships the types + a resolver-side `def_paths` index keyed on
//! item name. Sub-item hashing is intentionally a *stub* (always
//! returns `SubItemHash::ROOT`) for the foundation commit; the
//! per-decision-site hash function lands alongside the first
//! catalogue entry that exercises sub-item addressing (P1.3 codegen
//! queries at `match` arms / `if` branches, phase-7-codegen.md
//! line 25).

use std::collections::HashMap;

use crate::ast::*;

/// Path-based stable identity for a top-level item — module segments
/// followed by the item's local name. Single-file builds collapse to
/// a one-segment path (just the item name); project-mode builds carry
/// the module path. `impl` methods are addressed as
/// `["TargetType", "method_name"]` so the type/method pair survives
/// reordering of `impl` blocks.
///
/// Equality + hashing are by `segments` content; insensitive to where
/// in the source the item was declared.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DefPath {
    pub segments: Vec<String>,
}

impl DefPath {
    pub fn new(segments: Vec<String>) -> Self {
        DefPath { segments }
    }

    /// Single-segment path — convenience for top-level items in
    /// single-file builds.
    pub fn item(name: impl Into<String>) -> Self {
        DefPath {
            segments: vec![name.into()],
        }
    }

    /// Render as `seg1::seg2::seg3` — the human-readable form used in
    /// query-channel report output. Matches the path syntax users
    /// already see in error messages and `karac query` output.
    pub fn render(&self) -> String {
        self.segments.join("::")
    }
}

impl std::fmt::Display for DefPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.render())
    }
}

/// Stable identity for a sub-item position inside an item — e.g. one
/// specific `if` branch in a function body. Computed as a structural
/// hash over the AST subtree at the decision site, ignoring spans
/// and including identifiers. `SubItemHash::ROOT` (= 0) addresses
/// the item itself; non-root values address a specific descendant.
///
/// v1 returns `ROOT` from every `of_*` constructor (stub). The
/// real hashing lands when the first catalogue entry needing sub-item
/// addressing ships — at that point the implementation walks the AST
/// subtree with a `siphash`-class hasher seeded by node-kind tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SubItemHash(pub u64);

impl SubItemHash {
    pub const ROOT: SubItemHash = SubItemHash(0);

    /// Stub for v1 — see module docs. Always returns [`SubItemHash::ROOT`].
    pub fn of_expr(_expr: &Expr) -> Self {
        // TODO(phase-8 stdlib-floor sub-item 1): real structural hash.
        // The first consumer (phase-7-codegen.md line 25 — P1.3 codegen
        // queries at inlining sites + match arms) drives the
        // implementation; until then, all sub-item slots collapse to
        // ROOT, which is acceptable because no v1 query yet addresses
        // sub-items.
        SubItemHash::ROOT
    }
}

/// Full stable identity for a query target — item path plus optional
/// sub-item slot. Pair (`def_path`, `sub_item_hash`) is the durable
/// key tools store when persisting resolved query answers across
/// compile runs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QueryId {
    pub def_path: DefPath,
    pub sub_item_hash: SubItemHash,
}

impl QueryId {
    pub fn root(def_path: DefPath) -> Self {
        QueryId {
            def_path,
            sub_item_hash: SubItemHash::ROOT,
        }
    }
}

impl std::fmt::Display for QueryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.sub_item_hash == SubItemHash::ROOT {
            write!(f, "{}", self.def_path)
        } else {
            write!(f, "{}#{:016x}", self.def_path, self.sub_item_hash.0)
        }
    }
}

/// Build the `name → DefPath` map for every top-level item in a
/// `Program`. Single-file build: paths are one segment (the item's
/// local name; impl methods get `["TargetType", "method"]`).
///
/// Project-mode multi-module builds prepend module segments at the
/// pipeline level — the resolver records single-segment paths here;
/// the cross-module assembly step in `cli.rs` is the right layer to
/// rewrite to module-qualified paths when it walks the module tree.
pub fn collect_item_def_paths(program: &Program) -> HashMap<String, DefPath> {
    let mut out = HashMap::new();
    for item in &program.items {
        match item {
            Item::Function(f) => {
                out.insert(f.name.clone(), DefPath::item(f.name.clone()));
            }
            Item::StructDef(s) => {
                out.insert(s.name.clone(), DefPath::item(s.name.clone()));
            }
            Item::UnionDef(u) => {
                out.insert(u.name.clone(), DefPath::item(u.name.clone()));
            }
            Item::EnumDef(e) => {
                out.insert(e.name.clone(), DefPath::item(e.name.clone()));
            }
            Item::TraitDef(t) => {
                out.insert(t.name.clone(), DefPath::item(t.name.clone()));
            }
            Item::TraitAlias(t) => {
                out.insert(t.name.clone(), DefPath::item(t.name.clone()));
            }
            Item::MarkerTrait(t) => {
                out.insert(t.name.clone(), DefPath::item(t.name.clone()));
            }
            Item::ImplBlock(imp) => {
                let target = impl_target_name(&imp.target_type);
                if let Some(target) = target {
                    for impl_item in &imp.items {
                        if let ImplItem::Method(m) = impl_item {
                            let qualified = format!("{}.{}", target, m.name);
                            out.insert(
                                qualified,
                                DefPath::new(vec![target.clone(), m.name.clone()]),
                            );
                        }
                    }
                }
            }
            Item::EffectResource(d) => {
                out.insert(d.name.clone(), DefPath::item(d.name.clone()));
            }
            Item::EffectGroup(d) => {
                out.insert(d.name.clone(), DefPath::item(d.name.clone()));
            }
            Item::EffectVerbDecl(d) => {
                out.insert(d.verb_name.clone(), DefPath::item(d.verb_name.clone()));
            }
            Item::LayoutDef(d) => {
                out.insert(d.name.clone(), DefPath::item(d.name.clone()));
            }
            Item::ConstDecl(d) => {
                out.insert(d.name.clone(), DefPath::item(d.name.clone()));
            }
            Item::ModuleBinding(d) => {
                out.insert(d.name.clone(), DefPath::item(d.name.clone()));
            }
            Item::ExternFunction(f) => {
                out.insert(f.name.clone(), DefPath::item(f.name.clone()));
            }
            Item::TypeAlias(t) => {
                out.insert(t.name.clone(), DefPath::item(t.name.clone()));
            }
            Item::DistinctType(t) => {
                out.insert(t.name.clone(), DefPath::item(t.name.clone()));
            }
            // Use / Import / ExternBlock declarations don't introduce
            // names visible to query identity — they re-export or
            // alias names that belong to other items.
            // AliasDecl / IndependentDecl are name-relation
            // declarations, not single-named items; the names in
            // `left`/`right` belong to whichever Function /
            // EffectResource introduced them.
            // TestCase does not introduce a user-visible name — the
            // case-name string is the JSONL event payload, not a
            // resolvable identifier; the synthesized opaque function
            // (slice 3) carries its own DefPath via the lowered
            // Item::Function.
            Item::UseDecl(_)
            | Item::Import(_)
            | Item::ExternBlock(_)
            | Item::AliasDecl(_)
            | Item::IndependentDecl(_)
            | Item::TestCase(_) => {}
        }
    }
    out
}

/// Recover the target-type name from an `impl Block`'s target. Mirrors
/// the helper at `src/codegen.rs` (`impl_target_name`) — duplicated
/// here to keep the def-path module's surface free of cross-module
/// imports. Returns `None` when the target shape isn't a bare path
/// (e.g. complex generic patterns that don't yet feed the query
/// channel).
fn impl_target_name(target: &TypeExpr) -> Option<String> {
    match &target.kind {
        TypeKind::Path(path) => path.segments.last().cloned(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn def_path_render_single_segment() {
        let p = DefPath::item("sort_inplace");
        assert_eq!(p.render(), "sort_inplace");
        assert_eq!(p.to_string(), "sort_inplace");
    }

    #[test]
    fn def_path_render_multi_segment() {
        let p = DefPath::new(vec!["Point".to_string(), "eq".to_string()]);
        assert_eq!(p.render(), "Point::eq");
    }

    #[test]
    fn query_id_root_renders_without_hash() {
        let id = QueryId::root(DefPath::item("foo"));
        assert_eq!(id.to_string(), "foo");
    }

    #[test]
    fn sub_item_hash_root_is_zero() {
        assert_eq!(SubItemHash::ROOT.0, 0);
    }
}
