//! The numbering pass that gives every expression, pattern and statement a
//! distinct [`NodeId`] (redesign M0).
//!
//! The parser builds every node with [`NodeId::DUMMY`], and so do desugaring,
//! lowering and anything else that builds nodes. [`assign_node_ids`] runs after
//! each of those and fixes the tree up: a node that already holds an id no
//! earlier node holds keeps it, and every other node (a `DUMMY`, or a clone
//! that copied its original's id) gets a fresh one above the largest id in
//! use. So ids survive a pass that moves nodes around, and the pass can run as
//! often as needed.

use crate::ast::{Item, Program};
use crate::ids::NodeId;
use crate::span_visitor::{walk_item_mut_with, MutVisitor};
use crate::token::Span;

struct MaxId(Option<u32>);

impl MutVisitor for MaxId {
    fn span(&mut self, _span: &mut Span) {}
    fn node_id(&mut self, id: &mut NodeId) {
        if !id.is_dummy() {
            self.0 = Some(self.0.map_or(id.0, |m| m.max(id.0)));
        }
    }
}

/// The ids a walk has met. Ids are dense from 0 (every one was handed out
/// by a numbering pass), so a bit per id beats a hash set: hashing every
/// node of the program-with-library was about 5% of a small check
/// (B-2026-11-09-28).
#[derive(Default)]
struct IdSet(Vec<u64>);

impl IdSet {
    /// Add `id`; whether it was not already there.
    fn insert(&mut self, id: NodeId) -> bool {
        let (word, bit) = ((id.0 / 64) as usize, 1u64 << (id.0 % 64));
        if word >= self.0.len() {
            self.0.resize(word + 1, 0);
        }
        let fresh = self.0[word] & bit == 0;
        self.0[word] |= bit;
        fresh
    }
}

struct Assign {
    seen: IdSet,
    next: u32,
}

impl MutVisitor for Assign {
    fn span(&mut self, _span: &mut Span) {}
    fn node_id(&mut self, id: &mut NodeId) {
        if id.is_dummy() || !self.seen.insert(*id) {
            *id = NodeId(self.next);
            self.next += 1;
            assert!(!id.is_dummy(), "NodeId space exhausted");
            self.seen.insert(*id);
        }
    }
}

/// Give every expression, pattern and statement in `items` a distinct id.
pub fn assign_item_node_ids(items: &mut [Item]) {
    let mut max = MaxId(None);
    for item in items.iter_mut() {
        walk_item_mut_with(item, &mut max);
    }
    let mut assign = Assign {
        seen: IdSet::default(),
        next: max.0.map_or(0, |m| m + 1),
    };
    for item in items.iter_mut() {
        walk_item_mut_with(item, &mut assign);
    }
}

/// Number every node of `items`, which were parsed on their own, from
/// `*next` up, so they can join a program whose ids are all below `*next`.
/// A parse numbers its nodes from 0, so items spliced in as parsed hold ids
/// the program's own nodes hold, and [`assign_node_ids`] then renumbers
/// whichever of the two it walks second: the program's nodes after a
/// `#[derive]`d type, whose ids the type checker's tables were keyed by.
pub fn number_items_from(items: &mut [Item], next: &mut u32) {
    struct Fresh<'a>(&'a mut u32);
    impl MutVisitor for Fresh<'_> {
        fn span(&mut self, _span: &mut Span) {}
        fn node_id(&mut self, id: &mut NodeId) {
            *id = NodeId(*self.0);
            *self.0 += 1;
            assert!(!id.is_dummy(), "NodeId space exhausted");
        }
    }
    for item in items.iter_mut() {
        walk_item_mut_with(item, &mut Fresh(next));
    }
}

/// One above the largest id `items` hold; 0 when they hold none.
pub fn next_free_id(items: &mut [Item]) -> u32 {
    let mut max = MaxId(None);
    for item in items.iter_mut() {
        walk_item_mut_with(item, &mut max);
    }
    max.0.map_or(0, |m| m + 1)
}

/// Give every expression, pattern and statement in `program` a distinct id.
pub fn assign_node_ids(program: &mut Program) {
    assign_item_node_ids(&mut program.items);
}

/// The number of `DUMMY` ids and of ids held by more than one node in
/// `program`. Both are zero straight after [`assign_node_ids`].
pub fn node_id_defects(program: &mut Program) -> (usize, usize) {
    struct Count {
        seen: IdSet,
        dummies: usize,
        dups: usize,
    }
    impl MutVisitor for Count {
        fn span(&mut self, _span: &mut Span) {}
        fn node_id(&mut self, id: &mut NodeId) {
            if id.is_dummy() {
                self.dummies += 1;
            } else if !self.seen.insert(*id) {
                self.dups += 1;
            }
        }
    }
    let mut c = Count {
        seen: IdSet::default(),
        dummies: 0,
        dups: 0,
    };
    for item in program.items.iter_mut() {
        walk_item_mut_with(item, &mut c);
    }
    (c.dummies, c.dups)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "
struct P { x: i64 }
enum E { A(i64), B }
fn f(p: P, e: E) -> i64 {
    let q = P { x: p.x + 1 };
    let mut t = 0;
    for i in 0..3 { t = t + i; }
    match e { E.A(n) => n + q.x + t, E.B => 0 }
}
#[derive(Default)]
struct D { v: i64 }
fn swap() -> i64 {
    let mut a = 1;
    let mut b = 2;
    a, b = b, a;
    let d = D.default();
    a * 10 + b + d.v
}
fn main() { println(f(P { x: 1 }, E.A(2)) + swap()); }
";

    #[test]
    fn parse_numbers_every_node() {
        let mut p = crate::parse(SRC).program;
        assert_eq!(node_id_defects(&mut p), (0, 0));
    }

    #[test]
    fn renumbering_keeps_ids_and_fixes_clones() {
        let mut p = crate::parse(SRC).program;
        let before = format!("{:?}", p.items[2]);
        let dup = p.items[2].clone();
        p.items.push(dup);
        assert_eq!(node_id_defects(&mut p).0, 0);
        assert!(node_id_defects(&mut p).1 > 0);
        assign_node_ids(&mut p);
        assert_eq!(node_id_defects(&mut p), (0, 0));
        assert_eq!(format!("{:?}", p.items[2]), before);
    }

    #[test]
    fn desugar_and_lowering_leave_no_dummies() {
        let mut p = crate::parse(SRC).program;
        crate::prepare_for_resolve(&mut p);
        assert_eq!(node_id_defects(&mut p), (0, 0));
        let r = crate::resolve(&p);
        let tc = crate::typecheck(&p, &r);
        crate::lower(&mut p, &tc);
        assert_eq!(node_id_defects(&mut p), (0, 0));
    }
}
