//! Node and definition identities for the ownership redesign (M0).
//!
//! [`NodeId`] names one expression, pattern or statement in a parsed
//! program. The parser leaves every node at [`NodeId::DUMMY`]; a single
//! numbering pass after parsing (and again after desugaring and lowering,
//! which build nodes of their own) gives each one a distinct id. Analysis
//! results for the new MIR builder are keyed by `NodeId` rather than by
//! `SpanKey`, which two nodes can share.
//!
//! [`DefId`] names one module-qualified definition: a function, type,
//! variant, trait, constant or method. It is an index into the
//! [`DefTable`] the resolver produces, so it is `Copy` and compares in
//! one instruction; the [`DefPath`] it stands for is one lookup away.

use std::collections::HashMap;

use crate::def_path::DefPath;

/// Identity of one expression, pattern or statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

impl NodeId {
    /// The id every node carries until the numbering pass runs.
    pub const DUMMY: NodeId = NodeId(u32::MAX);

    pub fn is_dummy(self) -> bool {
        self == Self::DUMMY
    }
}

/// Hands out fresh, increasing [`NodeId`]s.
#[derive(Debug, Default)]
pub struct NodeIdGen {
    next: u32,
}

impl NodeIdGen {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn fresh(&mut self) -> NodeId {
        let id = NodeId(self.next);
        self.next += 1;
        assert!(!id.is_dummy(), "NodeId space exhausted");
        id
    }
}

/// Identity of one module-qualified definition: an index into a
/// [`DefTable`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DefId(pub u32);

/// What kind of definition a [`DefId`] names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DefKind {
    Fn,
    Method,
    Impl,
    Struct,
    Union,
    Enum,
    Variant,
    Trait,
    TypeAlias,
    DistinctType,
    OpaqueType,
    Const,
    Static,
    Module,
}

/// One entry of a [`DefTable`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefData {
    pub path: DefPath,
    pub kind: DefKind,
}

/// Every definition in a program, indexed by [`DefId`]. Paths are unique:
/// interning the same path twice returns the same id.
#[derive(Debug, Clone, Default)]
pub struct DefTable {
    defs: Vec<DefData>,
    by_path: HashMap<DefPath, DefId>,
}

impl DefTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// The id for `path`, adding it with `kind` if it is not there yet.
    pub fn intern(&mut self, path: DefPath, kind: DefKind) -> DefId {
        if let Some(&id) = self.by_path.get(&path) {
            return id;
        }
        let id = DefId(u32::try_from(self.defs.len()).expect("DefId space exhausted"));
        self.by_path.insert(path.clone(), id);
        self.defs.push(DefData { path, kind });
        id
    }

    pub fn lookup(&self, path: &DefPath) -> Option<DefId> {
        self.by_path.get(path).copied()
    }

    pub fn get(&self, id: DefId) -> &DefData {
        &self.defs[id.0 as usize]
    }

    pub fn len(&self) -> usize {
        self.defs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (DefId, &DefData)> {
        self.defs
            .iter()
            .enumerate()
            .map(|(i, d)| (DefId(i as u32), d))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(segs: &[&str]) -> DefPath {
        DefPath {
            segments: segs.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn node_ids_are_fresh_and_never_dummy() {
        let mut g = NodeIdGen::new();
        let a = g.fresh();
        let b = g.fresh();
        assert_ne!(a, b);
        assert!(!a.is_dummy() && !b.is_dummy());
    }

    #[test]
    fn def_table_interns_by_path() {
        let mut t = DefTable::new();
        let f = t.intern(path(&["a", "f"]), DefKind::Fn);
        let g = t.intern(path(&["b", "f"]), DefKind::Fn);
        assert_ne!(f, g);
        assert_eq!(t.intern(path(&["a", "f"]), DefKind::Fn), f);
        assert_eq!(t.lookup(&path(&["b", "f"])), Some(g));
        assert_eq!(t.get(g).path, path(&["b", "f"]));
        assert_eq!(t.len(), 2);
    }
}
