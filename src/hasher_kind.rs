//! Which hash a `Map` / `Set` uses — the plain-data form of the trailing type
//! argument in `Map[K, V, H]` / `Set[T, H]` (design.md § `Hash` and `Hasher`,
//! "Default hasher for v1"; B-2026-08-21-6).
//!
//! Lives outside both backends on purpose. The PARSER removes the hasher
//! argument from the type and records this enum on
//! [`crate::ast::Program::container_hashers`], keyed by the container path's
//! span — see that field for why deleting it is the safe shape. Each backend
//! reads the table back at the site that CONSTRUCTS the container, which is
//! the only place the choice matters: codegen stores the selected hash
//! function in the map's control block, and the interpreter stores it in
//! `MapData` / `SetData`. Both go through this one enum, off the same span, so
//! they cannot disagree about what a spelling means — and it holds no LLVM
//! types, so codegen containment holds.
//!
//! The typechecker still VALIDATES the argument
//! (`TypeChecker::take_hasher_type_arg`): the parser removes only a recognized
//! selector in the trailing position, so a misspelling, an extra argument, or
//! a hasher on an ordered container survives to be reported there.

/// The hasher a container was constructed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HasherKind {
    /// `SipHash13BuildHasher` — SipHash-1-3 under the per-process seed. The
    /// default for every container that does not name a hasher, which is why
    /// it is `#[default]`: a `Map` built where no type is in sight (an
    /// inference-driven `Map.new()` in argument position, a monomorphized
    /// generic body) must land on the SPEC's hasher, never on the fast one.
    #[default]
    SipHash13,
    /// `FxBuildHasher` — unkeyed rotate-xor-multiply. Opted into explicitly;
    /// see `runtime/stdlib/hash.kara` for what is given up.
    Fx,
}

impl HasherKind {
    /// The `karac-runtime` entry point the compiled backends call.
    pub fn runtime_symbol(self) -> &'static str {
        match self {
            HasherKind::SipHash13 => "karac_hash_bytes",
            HasherKind::Fx => "karac_hash_bytes_fx",
        }
    }

    /// Suffix distinguishing the synthesized per-key-type hash functions of
    /// one hasher from another's. The default hasher takes the EMPTY suffix so
    /// that every symbol name in a program that never mentions a hasher — i.e.
    /// almost every program — is byte-identical to what it was before the
    /// parameter existed.
    pub fn mangle_suffix(self) -> &'static str {
        match self {
            HasherKind::SipHash13 => "",
            HasherKind::Fx => "_fx",
        }
    }
}
