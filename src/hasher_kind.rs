//! Which hash a `Map` / `Set` uses — the plain-data form of the trailing type
//! argument in `Map[K, V, H]` / `Set[T, H]` (design.md § `Hash` and `Hasher`,
//! "Default hasher for v1"; B-2026-08-21-6, B-2026-08-22-6).
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
//! (`TypeChecker::check_recorded_container_hasher`): the parser strips a trailing
//! single-segment path whatever it names, so `Map[K, V, i64]`, a misspelled
//! selector and a type that does not `impl BuildHasher` all reach that pass to
//! be reported. An extra argument, a parameterized one, or a hasher on an
//! ordered container is left in place and reported by
//! `TypeChecker::take_hasher_type_arg` instead.

/// The hasher a container was constructed with.
///
/// NOT `Copy`: [`HasherKind::User`] carries the impl's type name. The variant
/// has to name the type because a user hasher is dispatched through the user's
/// own `BuildHasher` / `Hasher` methods, and both backends find those by name —
/// codegen through the `Type.method` LLVM symbols, the interpreter through the
/// same key in its env. Everything else about the enum is unchanged, so the two
/// builtin arms stay a bare discriminant.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
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
    /// A user type that `impl BuildHasher for` — design.md § `Hash` and
    /// `Hasher`, "User-extensible hashers" (B-2026-08-22-6). The `String` is
    /// the BUILDER's type name as written in the container's trailing slot
    /// (`Map[K, V, MyBuildHasher]` → `"MyBuildHasher"`), not the per-hash state
    /// type; the state type is the builder's `type Hasher = …` associated
    /// binding, which each backend resolves from the AST at the point it needs
    /// it. Keeping the BUILDER here mirrors the source spelling, so a
    /// diagnostic can quote back what the user wrote.
    User(String),
}

impl HasherKind {
    /// The `karac-runtime` entry point the compiled backends call, or `None`
    /// for a user hasher — which has no runtime entry point by construction,
    /// because its permutation lives in user code that codegen calls directly
    /// (see `Codegen::emit_hash_bytes_call`).
    pub fn runtime_symbol(&self) -> Option<&'static str> {
        match self {
            HasherKind::SipHash13 => Some("karac_hash_bytes"),
            HasherKind::Fx => Some("karac_hash_bytes_fx"),
            HasherKind::User(_) => None,
        }
    }

    /// The builder type name for a user hasher; `None` for the two builtins.
    pub fn user_builder(&self) -> Option<&str> {
        match self {
            HasherKind::User(name) => Some(name.as_str()),
            _ => None,
        }
    }

    /// Suffix distinguishing the synthesized per-key-type hash functions of
    /// one hasher from another's. The default hasher takes the EMPTY suffix so
    /// that every symbol name in a program that never mentions a hasher — i.e.
    /// almost every program — is byte-identical to what it was before the
    /// parameter existed.
    pub fn mangle_suffix(&self) -> std::borrow::Cow<'static, str> {
        match self {
            HasherKind::SipHash13 => std::borrow::Cow::Borrowed(""),
            HasherKind::Fx => std::borrow::Cow::Borrowed("_fx"),
            // Two user builders in one program need two distinct per-key-type
            // hash functions, so the builder's name rides in the symbol.
            HasherKind::User(name) => std::borrow::Cow::Owned(format!("_u_{name}")),
        }
    }
}
