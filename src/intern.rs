//! String interning for compiler-internal name keys (name-interning
//! spike, stage 3). A [`Symbol`] is a `u32` handle into a
//! per-compilation [`Interner`]; hashing and comparing a `Symbol` is a
//! single-word operation, and copying one allocates nothing — which is
//! the whole point: the front end's profile after the FxHash stages was
//! ~33% allocator, dominated by `String` key clones flowing through the
//! checkers' tables (`docs/spikes/name-interning.md`).
//!
//! Design notes:
//!
//! - **Per-phase, not global.** An `Interner` is owned by the checker
//!   that uses it (the effectchecker first); symbols from different
//!   interners must never be mixed. Keeping it per-compilation also
//!   means no locks and no cross-compile leakage.
//! - **Interior mutability.** `intern` takes `&self` so read-mostly
//!   walkers (`&self` methods threaded through deep match arms) can
//!   mint symbols without `&mut` plumbing. Single-threaded by
//!   construction (`RefCell`, `Rc<str>`), like the rest of the front
//!   end.
//! - **`get` vs `intern`.** Lookups that only *probe* ("is there a
//!   function by this name?") use [`Interner::get`], which never
//!   inserts: a miss proves the name was never minted, so any
//!   symbol-keyed map lookup would miss too. Insertion stays reserved
//!   for sites that *define* keys.
//! - **Dotted composites.** `"Type.method"` keys are minted through
//!   [`Interner::dotted`], which caches on the `(Symbol, Symbol)` pair —
//!   the `format!("{}.{}", ..)` allocation happens once per distinct
//!   pair instead of once per call site probed.

use rustc_hash::FxHashMap;
use std::cell::RefCell;
use std::rc::Rc;

/// An interned string handle. `Copy`, hashes/compares as a single
/// `u32`. Ordering follows mint order, NOT lexicographic order — sort
/// by [`Interner::resolve`]d text wherever alphabetical output order is
/// part of the contract (diagnostics, traces).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Symbol(u32);

impl Symbol {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Default)]
struct InternerInner {
    map: FxHashMap<Rc<str>, u32>,
    strings: Vec<Rc<str>>,
    /// `(lhs, rhs)` → the symbol for `"{lhs}.{rhs}"`.
    dotted: FxHashMap<(Symbol, Symbol), Symbol>,
}

/// A per-compilation string interner. See the module docs for the
/// usage contract.
#[derive(Default)]
pub struct Interner {
    inner: RefCell<InternerInner>,
}

impl Interner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint (or fetch) the symbol for `s`.
    pub fn intern(&self, s: &str) -> Symbol {
        let mut inner = self.inner.borrow_mut();
        if let Some(&id) = inner.map.get(s) {
            return Symbol(id);
        }
        let id = inner.strings.len() as u32;
        let rc: Rc<str> = Rc::from(s);
        inner.strings.push(Rc::clone(&rc));
        inner.map.insert(rc, id);
        Symbol(id)
    }

    /// Probe for `s` without inserting. `None` proves no symbol-keyed
    /// table can contain `s` (every key in such a table was minted
    /// through `intern`).
    pub fn get(&self, s: &str) -> Option<Symbol> {
        self.inner.borrow().map.get(s).copied().map(Symbol)
    }

    /// The text behind `sym`. Returns an `Rc` clone (refcount bump, no
    /// allocation); deref to `&str` for comparisons and formatting.
    ///
    /// Panics on a symbol from a different interner (out of range) —
    /// mixing interners is a bug, not a recoverable condition.
    pub fn resolve(&self, sym: Symbol) -> Rc<str> {
        Rc::clone(&self.inner.borrow().strings[sym.index()])
    }

    /// Mint (or fetch) the symbol for `"{lhs}.{rhs}"`, allocating the
    /// composite string only the first time a given pair is seen.
    pub fn dotted(&self, lhs: Symbol, rhs: Symbol) -> Symbol {
        if let Some(&sym) = self.inner.borrow().dotted.get(&(lhs, rhs)) {
            return sym;
        }
        let composite = {
            let inner = self.inner.borrow();
            format!(
                "{}.{}",
                inner.strings[lhs.index()],
                inner.strings[rhs.index()]
            )
        };
        let sym = self.intern(&composite);
        self.inner.borrow_mut().dotted.insert((lhs, rhs), sym);
        sym
    }

    /// Convenience: `dotted` with string sides (interns both first).
    pub fn dotted_str(&self, lhs: &str, rhs: &str) -> Symbol {
        let l = self.intern(lhs);
        let r = self.intern(rhs);
        self.dotted(l, r)
    }

    /// Probe for the composite `"{lhs}.{rhs}"` without inserting
    /// anything — no allocation on the probe path. CAVEAT: only finds
    /// pairs minted through [`Interner::dotted`] / [`dotted_str`]; a
    /// composite interned directly as one string is invisible here. Use
    /// only against tables whose keys are all dotted-minted.
    pub fn get_dotted(&self, lhs: &str, rhs: &str) -> Option<Symbol> {
        let inner = self.inner.borrow();
        let l = inner.map.get(lhs).copied().map(Symbol)?;
        let r = inner.map.get(rhs).copied().map(Symbol)?;
        inner.dotted.get(&(l, r)).copied()
    }

    /// Number of distinct symbols minted.
    pub fn len(&self) -> usize {
        self.inner.borrow().strings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.borrow().strings.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_is_idempotent() {
        let i = Interner::new();
        let a = i.intern("alpha");
        let b = i.intern("beta");
        assert_ne!(a, b);
        assert_eq!(i.intern("alpha"), a);
        assert_eq!(i.intern("beta"), b);
        assert_eq!(i.len(), 2);
    }

    #[test]
    fn resolve_round_trips() {
        let i = Interner::new();
        let a = i.intern("Vec.push");
        assert_eq!(&*i.resolve(a), "Vec.push");
    }

    #[test]
    fn get_never_inserts() {
        let i = Interner::new();
        assert_eq!(i.get("missing"), None);
        assert!(i.is_empty());
        let a = i.intern("present");
        assert_eq!(i.get("present"), Some(a));
        assert_eq!(i.len(), 1);
    }

    #[test]
    fn dotted_caches_composites() {
        let i = Interner::new();
        let t = i.intern("Wrapper");
        let m = i.intern("get");
        let d1 = i.dotted(t, m);
        assert_eq!(&*i.resolve(d1), "Wrapper.get");
        // Second call hits the pair cache and the same symbol comes back.
        assert_eq!(i.dotted(t, m), d1);
        // A directly-interned equal string unifies with the composite.
        assert_eq!(i.intern("Wrapper.get"), d1);
        assert_eq!(i.dotted_str("Wrapper", "get"), d1);
    }

    #[test]
    fn empty_string_is_a_valid_symbol() {
        let i = Interner::new();
        let e = i.intern("");
        assert_eq!(&*i.resolve(e), "");
        assert_eq!(i.intern(""), e);
    }
}
