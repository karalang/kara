// src/resolver.rs

//! Name resolution for the Kāra language.
//!
//! Walks the AST produced by the parser, builds a symbol table, resolves all
//! name references to their definitions, and reports errors for undefined
//! names, duplicates, and visibility violations.

use crate::ast::*;
use crate::edit_distance::suggest_similar;
use crate::module::{self, ModuleId, ProgramTree};
use crate::token::Span;
use std::collections::HashMap;

mod collect;
mod resolve_block;
mod resolve_items;
mod resolve_refs;

// ── IDs ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymbolId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScopeId(pub usize);

/// HashMap key derived from a Span's (offset, length).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpanKey(pub usize, pub usize);

impl SpanKey {
    pub fn from_span(span: &Span) -> Self {
        SpanKey(span.offset, span.length)
    }

    /// Method-call side-table key that disambiguates chained calls — keys on
    /// the closing-paren span when distinct, else the receiver span. See
    /// [`crate::token::method_call_key`] for the full rationale.
    pub fn for_method_call(recv: &Span, args_close: &Span) -> Self {
        let (o, l) = crate::token::method_call_key(recv, args_close);
        SpanKey(o, l)
    }
}

// ── Symbols ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Symbol {
    pub id: SymbolId,
    pub name: String,
    pub kind: SymbolKind,
    pub span: Span,
    pub is_pub: bool,
    pub scope: ScopeId,
}

#[derive(Debug, Clone)]
pub enum SymbolKind {
    Variable {
        is_mut: bool,
    },
    Function {
        param_names: Vec<String>,
    },
    Struct {
        field_names: Vec<String>,
    },
    /// `union NAME { ... }` — FFI union type declaration. Holds the
    /// field-name list parallel to [`Struct`] so the resolver can
    /// validate field references against the declared names. Like
    /// structs, unions are a type-namespace entry; unlike structs,
    /// downstream phases enforce per-field `Copy` bounds, require
    /// `#[repr(C)]`, and gate field reads behind `unsafe { ... }`
    /// (use-site rules ship in follow-up slices — see phase 5
    /// tracker line 549).
    Union {
        field_names: Vec<String>,
    },
    Enum {
        variant_names: Vec<String>,
    },
    EnumVariant {
        parent_enum: SymbolId,
        variant_kind: VariantSymbolKind,
    },
    Trait {
        method_names: Vec<String>,
    },
    /// `trait NAME = bound1 + bound2 + ...;` — recognized at parse but
    /// not yet expanded. Use sites in typechecker emit
    /// `E_TRAIT_ALIAS_NOT_IMPLEMENTED_YET`. Bound substitution lands in
    /// P1 (see `docs/deferred.md` § Trait Aliases — Expansion).
    TraitAlias,
    Constant,
    TypeParam,
    /// `const N: Type` const-generic parameter. Distinguished from
    /// `TypeParam` so the typechecker's declaration-site permitted-type
    /// check (spec at `design.md § Type Inference > Const generic
    /// parameters`) can branch on the symbol kind. The associated type
    /// expression is read from the source AST during typechecking.
    ConstParam,
    EffectResource,
    EffectGroup,
    EffectVerb,
    TypeAlias,
    Module,
    Import {
        path: Vec<String>,
    },
    SelfValue,
    ExternFunction,
    Primitive,
    DistinctType,
    /// Opaque foreign type declared inside an `unsafe extern "ABI" { ... }`
    /// block: `type Name;`. The Kāra side knows the name but neither the
    /// size, alignment, nor field layout — values of the type can only
    /// appear behind a pointer (`*const`/`*mut`) or reference
    /// (`ref`/`mut ref`). The typechecker rejects by-value uses with
    /// `E_OPAQUE_TYPE_REQUIRES_INDIRECTION`.
    OpaqueForeignType,
}

#[derive(Debug, Clone)]
pub enum VariantSymbolKind {
    Unit,
    Tuple(usize),
    Struct(Vec<String>),
}

// ── Scopes ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Scope {
    pub id: ScopeId,
    pub parent: Option<ScopeId>,
    pub kind: ScopeKind,
    pub names: HashMap<String, SymbolId>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ScopeKind {
    Global,
    Function,
    Block,
    Impl { target_type: String },
    Closure,
    Loop,
    MatchArm,
}

// ── Symbol Table ────────────────────────────────────────────────

pub struct SymbolTable {
    symbols: Vec<Symbol>,
    scopes: Vec<Scope>,
    current_scope: ScopeId,
    pub type_methods: HashMap<String, Vec<SymbolId>>,
    /// Trait bounds recorded for `TypeParam` symbols (generic parameters). The
    /// stored list is the union of inline bounds (`T: Bound`) and where-clause
    /// bounds (`where T: Bound`) — both apply simultaneously per design. Used
    /// by the typechecker to dispatch `T.method(args)` and bare `method(args)`
    /// calls to methods declared on a bound trait.
    pub generic_param_bounds: HashMap<SymbolId, Vec<TraitBound>>,
    /// `#[deprecated]` payload keyed by the deprecated symbol's id. Slice 3b
    /// plumbing for the use-site `deprecated` lint — populated at definition
    /// time by `collect::collect_*` for the seven attribute-bearing item
    /// kinds plus variants / trait methods / module consts / type aliases.
    /// Slice 4 (use-site warning emission) reads via [`Self::deprecation_for`]
    /// once the lint registry / emission infrastructure lands. Sidecar shape
    /// is preferred over a field on `Symbol` because most symbols are not
    /// deprecated; matches the `generic_param_bounds` pattern.
    pub deprecations: HashMap<SymbolId, Deprecation>,
    /// `#[unstable]` payload keyed by the symbol's id (phase-8 line 49).
    /// Populated by `collect::collect_*` at every site that records
    /// deprecation; consulted by the use-site `unstable_api` lint via
    /// [`Self::unstable_for`]. Same sidecar shape as `deprecations` — most
    /// symbols are stable, so out-of-band storage avoids growing every
    /// `Symbol` for a sparse annotation.
    pub unstables: HashMap<SymbolId, Unstable>,
}

impl Default for SymbolTable {
    fn default() -> Self {
        Self::new()
    }
}

impl SymbolTable {
    pub fn new() -> Self {
        let global_scope = Scope {
            id: ScopeId(0),
            parent: None,
            kind: ScopeKind::Global,
            names: HashMap::new(),
        };
        let mut table = SymbolTable {
            symbols: Vec::new(),
            scopes: vec![global_scope],
            current_scope: ScopeId(0),
            type_methods: HashMap::new(),
            generic_param_bounds: HashMap::new(),
            deprecations: HashMap::new(),
            unstables: HashMap::new(),
        };
        table.register_primitives();
        table
    }

    /// Seed scope 0 with every prelude name. CR-24 slice 8 routes the lists
    /// through `crate::prelude` so the resolver, the typechecker, and the
    /// synthetic `std.prelude` module entry in [`crate::module::ProgramTree`]
    /// agree on a single source of truth. The actual symbol kinds still
    /// match what the rest of the resolver expects (primitives for type and
    /// trait names; functions for compiler builtins and enum variants), so
    /// there is no behaviour change at this layer.
    fn register_primitives(&mut self) {
        let synthetic_span = Span {
            line: 0,
            column: 0,
            offset: 0,
            length: 0,
        };

        let push = |table: &mut SymbolTable, name: &str, kind: SymbolKind| {
            let id = SymbolId(table.symbols.len());
            table.symbols.push(Symbol {
                id,
                name: name.to_string(),
                kind,
                span: synthetic_span.clone(),
                is_pub: true,
                scope: ScopeId(0),
            });
            table.scopes[0].names.insert(name.to_string(), id);
        };

        for name in crate::prelude::PRELUDE_PRIMITIVES {
            push(self, name, SymbolKind::Primitive);
        }
        for name in crate::prelude::PRELUDE_FUNCTIONS {
            push(
                self,
                name,
                SymbolKind::Function {
                    param_names: Vec::new(),
                },
            );
        }
        for name in crate::prelude::PRELUDE_TYPES {
            push(self, name, SymbolKind::Primitive);
        }
        for name in crate::prelude::PRELUDE_TRAITS {
            push(self, name, SymbolKind::Primitive);
        }
        for name in crate::prelude::PRELUDE_VARIANTS {
            push(
                self,
                name,
                SymbolKind::Function {
                    param_names: Vec::new(),
                },
            );
        }
        for name in crate::prelude::PRELUDE_EFFECT_RESOURCES {
            push(self, name, SymbolKind::EffectResource);
        }

        // `Type` is the built-in comptime pseudotype (substrate 2) — the type
        // of a first-class type value. It appears in annotation position on
        // `comptime T: Type` parameters and must resolve as a known type name;
        // the typechecker maps it to the `Type` pseudotype and gates its use
        // to comptime contexts. Spec: deferred.md § Comptime — Types as
        // first-class values.
        push(self, "Type", SymbolKind::Primitive);

        // Comptime stdlib surface (substrates 3–4): `compiler` and `ast` are
        // comptime-only magic modules (`compiler.error(msg)`, `ast.expr(s)`,
        // `ast.item(s)`), and `Expr` / `Item` are the AST-node pseudotypes the
        // quasi-quote builders return (`Item` is the derive-desugaring return
        // element — `derive_x(comptime T: Type) -> Vec[Item]`). Registered as
        // always-visible names; the typechecker gates their use to comptime
        // contexts. Spec: deferred.md § Comptime — AST builder API / Comptime
        // stdlib surface / Code generation and derive desugaring.
        push(self, "compiler", SymbolKind::Module);
        push(self, "ast", SymbolKind::Module);
        push(self, "Expr", SymbolKind::Primitive);
        push(self, "Item", SymbolKind::Primitive);

        // `process` is a built-in module (for `process::exit`). Not tracked
        // in `prelude.rs` because it is not part of the prelude per design —
        // it is a permanent magic module the resolver makes visible.
        push(self, "process", SymbolKind::Module);

        // `gpu` is the magic dispatch module (design.md § GPU Subset
        // Constraints): `gpu.dispatch(kernel, buffer)` runs a `#[gpu]` kernel
        // on the GPU. Registered so the lowercase receiver resolves like
        // `process` / `ast`; the typechecker + codegen + interpreter intercept
        // `gpu.dispatch` before it is treated as a value method (spike
        // slice-0c, `docs/spikes/gpu-wgsl-slice0.md`).
        push(self, "gpu", SymbolKind::Module);

        // Lowercase stdlib module aliases per design.md § I/O: users write
        // `env.args()`, `clock.now()`, `stdout.println(s)` — lowercase module,
        // capitalized resource name dispatches at interpreter/codegen layer.
        // The capitalized targets are `Env` / `Clock` / `RandomSource` /
        // `Stdin` / `Stdout` / `Stderr` / `FileSystem` (the alias table lives
        // in the interpreter's `eval_method_call` and codegen's
        // `ambient_resource_for_alias`); the lowercase method signatures are
        // registered as `env.functions` path entries in
        // `register_compiler_intrinsic_env`.
        for alias in ["env", "clock", "rand", "stdin", "stdout", "stderr", "fs"] {
            push(self, alias, SymbolKind::Module);
        }

        // `ptr` is a built-in module hosting the strict-provenance pointer
        // APIs (`ptr.addr`, `ptr.with_addr`, `ptr.expose`, `ptr.from_exposed`,
        // …) per `design.md § Pointer Provenance` (v60 item 20). Like
        // `process` / `env`, the entries are programmatically registered
        // in `env.functions` rather than living in baked `.kara` source —
        // function names cannot syntactically contain a `.`.
        push(self, "ptr", SymbolKind::Module);

        // `critical_section` is a built-in module hosting the RAII
        // interrupt-mask primitive (`critical_section.acquire()`, design.md §
        // Critical sections). Registered as a module symbol like `ptr` / `gpu`
        // so the lowercase receiver resolves (a local binding still shadows it
        // by name); the typechecker + codegen + interpreter intercept
        // `critical_section.acquire` before it is treated as a value method.
        push(self, "critical_section", SymbolKind::Module);

        // `cpu` is a built-in module hosting the runtime CPU-feature probe
        // (`cpu.supports("avx2") -> bool`, design.md § Multiversioning; the
        // `#[multiversion]` dispatch primitive). Registered like `ptr` / `gpu` /
        // `critical_section` so the lowercase receiver resolves (a local binding
        // still shadows it by name); the typechecker + codegen + interpreter
        // intercept `cpu.supports` before it is treated as a value method.
        push(self, "cpu", SymbolKind::Module);
    }

    pub fn push_scope(&mut self, kind: ScopeKind) -> ScopeId {
        let id = ScopeId(self.scopes.len());
        self.scopes.push(Scope {
            id,
            parent: Some(self.current_scope),
            kind,
            names: HashMap::new(),
        });
        self.current_scope = id;
        id
    }

    pub fn pop_scope(&mut self) {
        if let Some(parent) = self.scopes[self.current_scope.0].parent {
            self.current_scope = parent;
        }
    }

    pub fn current_scope_id(&self) -> ScopeId {
        self.current_scope
    }

    /// Reserved identifiers that cannot be used as user-defined names.
    const RESERVED_IDENTIFIERS: &'static [(&'static str, &'static str)] = &[
        ("Fn", "reserved for closure/function type constructor"),
        (
            "split_by_variant",
            "reserved as a contextual keyword in layout blocks",
        ),
    ];

    pub fn define(
        &mut self,
        name: String,
        kind: SymbolKind,
        span: Span,
        is_pub: bool,
    ) -> Result<SymbolId, ResolveError> {
        // Check reserved identifiers
        for &(reserved, reason) in Self::RESERVED_IDENTIFIERS {
            if name == reserved {
                return Err(ResolveError {
                    message: format!("'{}' is {}", name, reason),
                    span,
                    kind: ResolveErrorKind::ReservedIdentifier,
                    suggestion: None,
                    replacement: None,
                    stub_hint: None,
                });
            }
        }

        let scope_id = self.current_scope;
        let scope = &self.scopes[scope_id.0];
        if let Some(&existing_id) = scope.names.get(&name) {
            let existing = &self.symbols[existing_id.0];
            // Allow user definitions to shadow prelude/built-in symbols (synthetic span 0:0)
            let is_prelude = existing.span.line == 0 && existing.span.column == 0;
            if !is_prelude {
                return Err(ResolveError {
                    message: format!(
                        "'{}' is already defined in this scope (first defined at {}:{})",
                        name, existing.span.line, existing.span.column
                    ),
                    span,
                    kind: ResolveErrorKind::DuplicateDefinition,
                    suggestion: None,
                    replacement: None,
                    stub_hint: None,
                });
            }
        }
        let id = SymbolId(self.symbols.len());
        self.symbols.push(Symbol {
            id,
            name: name.clone(),
            kind,
            span,
            is_pub,
            scope: scope_id,
        });
        self.scopes[scope_id.0].names.insert(name, id);
        Ok(id)
    }

    /// Like [`define`](Self::define) but permits *shadowing*: a name already
    /// bound in the current scope is replaced by a fresh symbol (new
    /// `SymbolId`, the old binding's name→id mapping is overwritten) instead
    /// of raising a duplicate-definition error. The old symbol stays in
    /// `symbols` so any use-site already resolved to it keeps pointing there;
    /// later lookups of the name find the new binding. Used only for
    /// `let`/`let mut` binding sites, where shadowing is a committed v1
    /// feature (design.md § Variables > Shadowing). Reserved-identifier
    /// rejection still applies.
    pub fn define_shadowable(
        &mut self,
        name: String,
        kind: SymbolKind,
        span: Span,
        is_pub: bool,
    ) -> Result<SymbolId, ResolveError> {
        for &(reserved, reason) in Self::RESERVED_IDENTIFIERS {
            if name == reserved {
                return Err(ResolveError {
                    message: format!("'{}' is {}", name, reason),
                    span,
                    kind: ResolveErrorKind::ReservedIdentifier,
                    suggestion: None,
                    replacement: None,
                    stub_hint: None,
                });
            }
        }

        let scope_id = self.current_scope;
        let id = SymbolId(self.symbols.len());
        self.symbols.push(Symbol {
            id,
            name: name.clone(),
            kind,
            span,
            is_pub,
            scope: scope_id,
        });
        // Overwrites any existing same-scope binding (shadowing) or inserts.
        self.scopes[scope_id.0].names.insert(name, id);
        Ok(id)
    }

    pub fn lookup(&self, name: &str) -> Option<&Symbol> {
        let mut scope_id = self.current_scope;
        loop {
            let scope = &self.scopes[scope_id.0];
            if let Some(&sym_id) = scope.names.get(name) {
                return Some(&self.symbols[sym_id.0]);
            }
            {
                let parent = scope.parent?;
                scope_id = parent
            }
        }
    }

    pub fn lookup_in_scope(&self, scope_id: ScopeId, name: &str) -> Option<&Symbol> {
        self.scopes[scope_id.0]
            .names
            .get(name)
            .map(|&id| &self.symbols[id.0])
    }

    pub fn get_symbol(&self, id: SymbolId) -> &Symbol {
        &self.symbols[id.0]
    }

    pub fn register_method(&mut self, type_name: &str, method_id: SymbolId) {
        self.type_methods
            .entry(type_name.to_string())
            .or_default()
            .push(method_id);
    }

    /// Append trait bounds to the entry for `param_id`. Idempotent on identical
    /// bounds is not enforced — callers should not record the same bound twice.
    pub fn record_generic_bounds(&mut self, param_id: SymbolId, bounds: &[TraitBound]) {
        if bounds.is_empty() {
            return;
        }
        self.generic_param_bounds
            .entry(param_id)
            .or_default()
            .extend(bounds.iter().cloned());
    }

    /// Record a `#[deprecated]` payload against `id`. Idempotent on identical
    /// payloads is not enforced — the parser already collapses duplicate
    /// `#[deprecated]` attributes on one item to the first (slice 1 E_DEPRECATED_DUPLICATE),
    /// so each symbol reaches us at most once.
    pub fn record_deprecation(&mut self, id: SymbolId, dep: Deprecation) {
        self.deprecations.insert(id, dep);
    }

    /// Look up the recorded `#[deprecated]` payload for `id`, if any.
    /// Used by the use-site lint emission pass in slice 4 (once the
    /// lint emission infrastructure lands at line 419 slice 4).
    pub fn deprecation_for(&self, id: SymbolId) -> Option<&Deprecation> {
        self.deprecations.get(&id)
    }

    /// Record a `#[unstable]` payload against `id` (phase-8 line 49).
    /// Idempotent on identical payloads is not enforced — the parser
    /// already collapses duplicate `#[unstable]` attributes on one item
    /// to the first (`E_UNSTABLE_DUPLICATE`), so each symbol reaches
    /// us at most once.
    pub fn record_unstable(&mut self, id: SymbolId, payload: Unstable) {
        self.unstables.insert(id, payload);
    }

    /// Look up the recorded `#[unstable]` payload for `id`, if any.
    /// Consulted by the use-site `unstable_api` lint emission pass
    /// (`TypeChecker::check_unstable_use_at`).
    pub fn unstable_for(&self, id: SymbolId) -> Option<&Unstable> {
        self.unstables.get(&id)
    }

    /// Trait bounds attached to `param_id`. Empty slice if the symbol is not a
    /// generic parameter or has no bounds.
    pub fn get_generic_bounds(&self, param_id: SymbolId) -> &[TraitBound] {
        self.generic_param_bounds
            .get(&param_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Iterate every registered symbol. Used by tests that need to assert
    /// on symbols defined in nested scopes (e.g. generic params inside a
    /// function body) that aren't reachable via `lookup_in_scope` against
    /// the global scope.
    pub fn all_symbols(&self) -> &[Symbol] {
        &self.symbols
    }

    pub fn lookup_method(&self, type_name: &str, method_name: &str) -> Option<&Symbol> {
        self.type_methods.get(type_name).and_then(|methods| {
            methods
                .iter()
                .map(|&id| &self.symbols[id.0])
                .find(|sym| sym.name == method_name)
        })
    }

    /// Collect all visible names from the current scope chain (for typo suggestions).
    pub fn visible_names(&self) -> Vec<&str> {
        let mut names = Vec::new();
        let mut scope_id = self.current_scope;
        loop {
            let scope = &self.scopes[scope_id.0];
            for name in scope.names.keys() {
                names.push(name.as_str());
            }
            match scope.parent {
                Some(parent) => scope_id = parent,
                None => break,
            }
        }
        names
    }
}

// ── Errors ──────────────────────────────────────────────────────

/// A precise source-text edit attached to a diagnostic. Consumers like
/// `karac fix` apply each edit by replacing `source[offset..offset+length]`
/// with `replacement`. Distinct from `suggestion` (a free-form
/// human-readable hint): `TextEdit` is *machine-applicable* — only attached
/// where the compiler can name an exact byte range and a verbatim
/// replacement (today: `did you mean` corrections on undefined names /
/// types where the resolver knows the misspelled identifier's span and
/// the visible-name candidate).
#[derive(Debug, Clone)]
pub struct TextEdit {
    pub offset: usize,
    pub length: usize,
    pub replacement: String,
}

#[derive(Debug, Clone)]
pub struct ResolveError {
    pub message: String,
    pub span: Span,
    pub kind: ResolveErrorKind,
    pub suggestion: Option<String>,
    /// Machine-applicable edit, when one can be derived. `karac fix` walks
    /// the diagnostics produced by the resolver looking for `Some(...)`
    /// here and applies each edit to the source file. `None` means the
    /// suggestion (if any) is descriptive only — humans can act on it but
    /// no precise byte-range rewrite was synthesized. Boxed so the sparse
    /// case (most errors carry no edit) doesn't bloat the enum's payload
    /// in fallible-resolver paths that return `Result<_, ResolveError>`.
    pub replacement: Option<Box<TextEdit>>,
    /// Signature-from-call-site stub proposal for unresolved-call sites
    /// inside `_test.kara` files (phase-5-diagnostics line 633). Carries
    /// the inferred function signature so the CLI JSON envelope can emit
    /// a `hints[].diff` entry pointing at the sibling production file.
    /// `None` for every other resolver diagnostic. Slice 1 ships with
    /// all-`_` argument types; slice 2 fills literal-derived types where
    /// the resolver can infer them locally.
    pub stub_hint: Option<Box<StubHint>>,
}

/// A signature proposal emitted alongside an unresolved-call diagnostic
/// inside a `_test.kara` file — the classic TDD red opener where the test
/// references a function the user is about to write. The CLI lifts this
/// into a `hints[].diff` entry whose `new` field is the rendered stub
/// source. See deferred.md § P1 § Signature-from-Call-Site Stub
/// Diagnostic.
#[derive(Debug, Clone)]
pub struct StubHint {
    /// The unresolved callee name — becomes the function name in the
    /// proposed stub.
    pub callee_name: String,
    /// One entry per call argument. Slice 1 always emits
    /// `inferred_type: None` (rendered as `_`); slice 2 fills literal-
    /// derived types (`add(2, 3)` → `i32, i32`).
    pub args: Vec<StubArg>,
    /// Return type when the resolver can infer it locally (e.g. from a
    /// surrounding `assert_eq(call, literal)` context). `None` →
    /// rendered as `_`. Slice 1 always emits `None`.
    pub return_type: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StubArg {
    /// `None` → `_` placeholder. Some(type) → concrete type from
    /// best-effort literal-argument inference (slice 2).
    pub inferred_type: Option<String>,
}

impl StubHint {
    /// Render the proposed stub as a self-contained Kāra item — a
    /// `fn name(arg0: T0, arg1: T1) -> R { todo() }` block, trailing
    /// newline included. Slot all unknown types as `_` per the deferred-
    /// design contract.
    pub fn render_source(&self) -> String {
        let params: Vec<String> = self
            .args
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let ty = a.inferred_type.as_deref().unwrap_or("_");
                format!("arg{}: {}", i, ty)
            })
            .collect();
        let ret = self.return_type.as_deref().unwrap_or("_");
        format!(
            "fn {}({}) -> {} {{\n    todo()\n}}\n",
            self.callee_name,
            params.join(", "),
            ret
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResolveErrorKind {
    UndefinedName,
    DuplicateDefinition,
    ReservedIdentifier,
    PrivateAccess,
    UndefinedType,
    UndefinedVariant,
    UndefinedField,
    UndefinedLabel,
    /// User-defined `impl Add for MyType` (and peers) — operator-trait
    /// implementations are restricted to stdlib types in v1.
    OperatorTraitImplRestricted,
    /// User-defined `impl Into[T] for S` / `impl TryInto[T] for S` —
    /// rejected because these are derived from `From` / `TryFrom` via blanket
    /// impl. The user must implement `From` / `TryFrom` instead.
    IntoTraitImplNotAllowed,
    /// `impl[..., with E] Trait for Type` — impl-level effect-variable
    /// quantification is not supported. Effect polymorphism is expressed
    /// via `with _` on the trait method declaration, not bound at impl
    /// level. See design.md § Conversion Traits.
    ImplLevelEffectVarNotAllowed,
    /// `import a.b.c;` — the prefix `a.b` does not match any module in the
    /// project graph (CR-24 slice 5, `E0224`).
    UnknownModule,
    /// `import a.b.Item;` — `a.b` exists but has no top-level `Item`, and
    /// `a.b.Item` is not itself a module (CR-24 slice 5, `E0225`).
    UnknownItemInModule,
    /// Cross-module visibility violation: the imported or referenced item is
    /// declared `private` (same-directory-only) and the importer lives in a
    /// different directory (CR-24 slice 6, `E0222`).
    PrivateItemAccess,
    /// `effect resource CompileTimeEnv;` or `effect resource CompileTimeHeap;`
    /// — these names are reserved for the deferred comptime feature (`E0228`).
    ReservedEffectResource,
    /// `#[compiler_builtin]` on an item in user code. The attribute is
    /// reserved for stdlib source baked into the compiler binary
    /// (CR-202 slice 1). `E0237`.
    CompilerBuiltinReserved,
    /// `#[non_exhaustive]` placed on an item that does not support it.
    /// The attribute is valid only on `pub struct` and `pub enum`
    /// declarations — placing it on a private type, a trait, a
    /// function, an impl block, a type alias, or an individual
    /// enum variant is rejected with `E_NON_EXHAUSTIVE_INVALID_TARGET`.
    /// See design.md § `#[non_exhaustive]` for Evolvable Public Types.
    NonExhaustiveInvalidTarget,
    /// `#[track_caller]` placed on an item that is not a `fn`
    /// declaration. The attribute redirects the panic-site source
    /// location and only makes sense on functions (trait method
    /// declarations also accept it once attribute support on trait
    /// methods lands — that's a separate enabling change). See
    /// design.md § Error Handling > "Stdlib panic-emitters report the
    /// caller's source location". `E0240`.
    TrackCallerInvalidTarget,
    /// `#[gpu]` placed on an item that is not a `fn` declaration. The
    /// attribute is the GPU-subset *constraint* marker — it asserts a
    /// function uses only GPU-compatible features and makes it
    /// GPU-callable, so it only makes sense on functions (free `fn`,
    /// inherent / trait-impl method, trait method declaration). Placing
    /// it on a struct / enum / union / trait / impl block / const / type
    /// alias, or a field / variant, is rejected with
    /// `E_GPU_INVALID_TARGET`. See design.md § GPU Subset Constraints.
    /// `E0800`.
    GpuInvalidTarget,
    /// A codegen-hint attribute (`#[inline]`, `#[inline(always)]`,
    /// `#[inline(never)]`, `#[cold]`) placed on an item that is not an
    /// inlinable function body — a struct/enum/union/trait/impl-block/
    /// const/type-alias, an enum variant, a field, or an entire `impl`
    /// block. The hints attach per named function (free `fn`, inherent
    /// / trait-impl method, trait method declaration, `extern "C" fn`
    /// definition, `Drop` destructor). See design.md § Codegen Hint
    /// Attributes > "Where they may appear". `E_CODEGEN_HINT_INVALID_POSITION`.
    CodegenHintInvalidTarget,
    /// A codegen-hint attribute placed on a foreign-function
    /// *declaration* inside `unsafe extern { ... }`. There is no
    /// Kāra-side body to inline; the compiler cannot reach inside a
    /// foreign symbol. `E_CODEGEN_HINT_ON_EXTERN_DECL`.
    CodegenHintOnExternDecl,
    /// `#[profile(...)]` placed on a non-`fn` item. The attribute
    /// asserts per-function profile compatibility and is fn-only at
    /// v1. Module-level placement is part of the spec but blocked on
    /// the module-attribute AST surface.
    ProfileInvalidTarget,
    /// A name inside `#[profile(...)]` doesn't match the closed v1
    /// set (`default` / `embedded` / `kernel`). The diagnostic lists
    /// the valid names.
    UnknownProfile,
    /// `#[deprecated]` placed on an `impl` block. The spec rejects
    /// this with `E_DEPRECATED_ON_IMPL` — impl-level deprecation
    /// would be ambiguous (which methods?); the user should
    /// deprecate the underlying methods individually. See design.md §
    /// `#[deprecated]` for Item Deprecation. `E0241`.
    DeprecatedOnImpl,
    /// `#[deprecated]` placed on a struct *field*. The spec defers
    /// field-level deprecation to post-v1 — use-site detection for
    /// field reads/writes is non-trivial and is bundled with the
    /// post-v1 lint expansion. `E_DEPRECATED_ON_FIELD`. `E0242`.
    DeprecatedOnField,
    /// `continue label` where `label` refers to a labeled block (rather
    /// than a loop). `continue` is only valid for loop labels — reject
    /// the use site with `error[E_CONTINUE_LABEL_BLOCK]`. The diagnostic
    /// carries a secondary span pointing at the labeled-block declaration
    /// so users can rename the label or restructure the block.
    /// See design.md § Loops > "Labeled blocks".
    ContinueOnBlockLabel,
    /// `#[no_such_thing]` — a bare-name attribute path whose name is not
    /// in the v1 recognised set. Emitted by
    /// [`crate::attribute_validator::validate_program_attributes`] (slice
    /// 2 of the `#[diagnostic::*]` attribute namespace entry; see
    /// design.md § Diagnostic Namespace Attributes and design.md §
    /// Tool-Namespaced Attributes for the bare-vs-namespaced
    /// discriminator). Mapped to `E_UNKNOWN_ATTRIBUTE` / `E0243`.
    UnknownAttribute,
    /// Phase-8 stdlib-floor § Compiler queries channel sub-item 5.
    /// Two attributes on the same item resolve the same query in
    /// conflicting ways — e.g. `#[inline]` + `#[cold]` (inlining
    /// query) or `#[no_rc]` + `#[prefer_rc]` (RC-fallback query).
    /// Conflict pairs are registered in
    /// [`crate::attribute_validator::QUERY_RESOLUTION_CONFLICT_PAIRS`].
    /// Mapped to `E_QUERY_RESOLUTION_CONFLICT`.
    QueryResolutionConflict,
    /// Phase-5 FFI unions slice 3b — `#[non_exhaustive]` placed on a
    /// `union` declaration. Distinct from the generic
    /// [`NonExhaustiveInvalidTarget`] because unions are an FFI
    /// boundary shape: their field list is determined by the C side
    /// and cannot be extended in a backwards-compatible way the way
    /// `pub struct` / `pub enum` can. The focused diagnostic body
    /// (`E_UNION_NON_EXHAUSTIVE_FORBIDDEN`) calls that reasoning out
    /// directly instead of routing through the generic "attribute is
    /// not valid on {kind}" template. Field-level `#[non_exhaustive]`
    /// inside a union still routes through the generic helper —
    /// the focused code is type-level only.
    UnionNonExhaustiveForbidden,
    /// `#[default]` placed anywhere other than on a unit enum variant
    /// under `#[derive(Default)]` — on a struct, struct field, function,
    /// type alias, distinct type, trait, impl block, const, or extern
    /// declaration. The position checker runs before the per-derive
    /// validators so the diagnostic is location-focused rather than
    /// coming out of the derive machinery. Mapped to
    /// `E_DEFAULT_ATTRIBUTE_INVALID_POSITION`. Phase-8 stdlib-floor
    /// item `#[derive(Default)]` / `#[default]` on enum variants.
    DefaultAttributeInvalidPosition,
    /// `#[default]` on an enum variant whose enclosing enum does not
    /// carry `#[derive(Default)]` — the marker is inert without the
    /// derive, so the dead annotation is rejected rather than silently
    /// ignored. Mapped to `E_DEFAULT_ATTRIBUTE_WITHOUT_DERIVE`.
    DefaultAttributeWithoutDerive,
    /// `#[default(...)]` or `#[default = ...]` — the `#[default]`
    /// attribute accepts no arguments. Mapped to
    /// `E_MALFORMED_ATTRIBUTE_ARGS`.
    MalformedAttributeArgs,
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}: {}",
            self.span.line, self.span.column, self.message
        )
    }
}

// ── Result ──────────────────────────────────────────────────────

pub struct ResolveResult {
    pub resolutions: HashMap<SpanKey, SymbolId>,
    pub symbol_table: SymbolTable,
    pub errors: Vec<ResolveError>,
    /// Phase-8 stdlib-floor § Compiler queries channel sub-item 1.
    /// Stable item identity index: top-level item name → its `DefPath`.
    /// Built during item collection at single-file resolve time;
    /// project-mode multi-module assembly prepends module segments
    /// at the cli.rs pipeline level. Surface lock — future query
    /// catalogue entries consume this to mint stable `QueryId`s
    /// that survive unrelated source edits.
    pub def_paths: HashMap<String, crate::def_path::DefPath>,
    /// Phase-8 stdlib-floor § Compiler queries channel sub-item 2.
    /// Empty in v1; future catalogue entries push `CompilerQuery`
    /// values from the resolver here.
    pub queries: Vec<crate::queries::CompilerQuery>,
}

// ── Cross-module lookup helpers (CR-24 slice 5) ─────────────────

/// True iff the module at `path` exposes a top-level item called `name` —
/// either as a real item or via a `pub import` re-export (slice 7).
/// Visibility is **not** enforced here — `E0221` / `E0222` layer on top.
pub(crate) fn module_exposes_name(tree: &ProgramTree, path: &[String], name: &str) -> bool {
    module::module_exposes_item(tree, path, name)
}

pub(crate) fn module_top_level_names(tree: &ProgramTree, path: &[String]) -> Vec<String> {
    match tree.graph.by_path.get(path) {
        Some(&id) => module_top_level_names_for_id(tree, id),
        None => Vec::new(),
    }
}

/// List the names a module exposes to other modules — real top-level items
/// plus `pub import` re-exports (slice 7). Submodule re-exports are excluded
/// because they are not items.
fn module_top_level_names_for_id(tree: &ProgramTree, id: ModuleId) -> Vec<String> {
    let module = tree.module(id);
    let mut names = Vec::new();
    for item in &module.items {
        match item {
            Item::Function(f) => names.push(f.name.clone()),
            Item::StructDef(s) => names.push(s.name.clone()),
            Item::UnionDef(u) => names.push(u.name.clone()),
            Item::EnumDef(e) => names.push(e.name.clone()),
            Item::TraitDef(t) => names.push(t.name.clone()),
            Item::TraitAlias(t) => names.push(t.name.clone()),
            Item::MarkerTrait(t) => names.push(t.name.clone()),
            Item::ConstDecl(c) => names.push(c.name.clone()),
            Item::ModuleBinding(b) => names.push(b.name.clone()),
            Item::TypeAlias(t) => names.push(t.name.clone()),
            Item::DistinctType(d) => names.push(d.name.clone()),
            Item::ExternFunction(e) => names.push(e.name.clone()),
            Item::ExternBlock(b) => {
                for it in &b.items {
                    match it {
                        ExternItem::Function(f) => names.push(f.name.clone()),
                        ExternItem::OpaqueType(o) => names.push(o.name.clone()),
                    }
                }
            }
            Item::EffectResource(r) => names.push(r.name.clone()),
            Item::EffectGroup(g) => names.push(g.name.clone()),
            Item::EffectVerbDecl(v) => names.push(v.verb_name.clone()),
            Item::Import(imp) if imp.is_pub => {
                // `pub import` re-exports expose each bound item name at
                // the re-exporter's public surface. Submodule re-exports
                // are filtered out — they are module paths, not items.
                for ii in &imp.items {
                    let bound = ii.alias.clone().unwrap_or_else(|| ii.name.clone());
                    let mut full = imp.path.clone();
                    full.push(ii.name.clone());
                    if tree.graph.lookup(&full).is_none() {
                        names.push(bound);
                    }
                }
            }
            // Enum variants surface through their parent enum; impl blocks
            // aren't top-level named items; non-`pub` imports stay internal;
            // use / alias / independent / layout have no importable name;
            // test cases are not callables and expose no cross-module name.
            Item::ImplBlock(_)
            | Item::LayoutDef(_)
            | Item::UseDecl(_)
            | Item::Import(_)
            | Item::AliasDecl(_)
            | Item::IndependentDecl(_)
            | Item::TestCase(_) => {}
        }
    }
    names
}

/// Find the `Visibility` that `name` has when looked up at `path` from an
/// outside module — following `pub import` re-export chains to the canonical
/// defining item. Returns `None` when the module or the item does not exist
/// (the slice-5 `E0224`/`E0225` diagnostics already cover those cases).
pub(crate) fn module_item_visibility(
    tree: &ProgramTree,
    path: &[String],
    name: &str,
) -> Option<Visibility> {
    module::canonical_item_visibility(tree, path, name)
}

/// The directory in the crate tree that holds this module's source file.
/// Entry files (`main.kara` / `lib.kara`) and top-level modules all live in
/// `src/` — represented as an empty path. A nested module like
/// `db.connection` lives in `db/` — its directory is `["db"]`. Test files
/// (`foo_test.kara`) share the directory of their sibling per design.md.
///
/// Implementation: the walker already hoists entry files to the empty path,
/// so we just drop the last segment of the module path to recover the
/// directory. The "test file shares directory" rule falls out of the walker's
/// `is_test_file` classification: test files share the same `ModulePath` as
/// their subject, so this function returns the same directory for them.
fn module_directory(path: &[String]) -> Vec<String> {
    if path.is_empty() {
        Vec::new()
    } else {
        path[..path.len() - 1].to_vec()
    }
}

/// True iff an item declared in module `def_path` with visibility `vis` is
/// accessible from module `use_path`, per design.md § Three-level
/// visibility. `Default` is project-internal (always OK in v1's
/// single-package mode); `Private` requires same parent directory; `Pub` is
/// always OK.
pub(crate) fn visibility_allows_access(
    vis: Visibility,
    def_path: &[String],
    use_path: &[String],
) -> bool {
    match vis {
        Visibility::Pub | Visibility::Default => true,
        Visibility::Private => module_directory(def_path) == module_directory(use_path),
    }
}

// ── Resolver ────────────────────────────────────────────────────

pub struct Resolver<'a> {
    pub(crate) program: &'a Program,
    /// Optional multi-module context. When set, `import` declarations are
    /// validated against the project-wide `ProgramTree`; when unset (single-
    /// file mode), imports are silently registered without cross-module
    /// lookup. CR-24 slice 5.
    pub(crate) tree: Option<&'a ProgramTree>,
    /// The id of the module being resolved, used to exclude self from
    /// sibling-lookup diagnostics. Set iff `tree` is set.
    pub(crate) current_module: Option<ModuleId>,
    pub(crate) table: SymbolTable,
    pub(crate) resolutions: HashMap<SpanKey, SymbolId>,
    pub(crate) errors: Vec<ResolveError>,
    /// The target type name when inside an impl block.
    pub(crate) current_impl_type: Option<String>,
    /// Stack of label-stack entries for validating `break` / `continue`
    /// targets. Each entry is `(name, kind)` where `name: Option<String>`
    /// is `None` for an unlabeled loop and `Some(label)` for a labeled
    /// loop / labeled block; `kind: LabelKind` distinguishes loops from
    /// labeled blocks. `continue label` referring to a `Block` entry is
    /// rejected with `error[E_CONTINUE_LABEL_BLOCK]`. The stack is
    /// reset to empty at each closure boundary (LB4 — labels are lexical
    /// to the function-body control flow; closure bodies cannot target
    /// outer labels).
    pub(crate) loop_labels: Vec<(Option<String>, LabelKind)>,
    /// B-2026-07-02-5: while resolving a statement-branch of an explicit
    /// `par { }` block, the binding names introduced by SIBLING branches
    /// (name → the sibling binding's span). Each top-level statement of a
    /// `par { }` is a concurrent branch with its own scope, so sibling
    /// bindings are not in scope — but a bare "undefined name" diagnostic
    /// hides the actual rule. `error_undefined_name` consults this map to
    /// emit the tailored cross-branch diagnostic instead. Empty outside
    /// `par` branch resolution; saved/merged/restored around nested `par`s.
    pub(crate) par_sibling_bindings: HashMap<String, Span>,
    /// True iff the program being resolved is the synthetic stdlib package
    /// (baked into the compiler binary by CR-202 slice 3). When false,
    /// `#[compiler_builtin]` on any item is rejected with `E0237`. The flag
    /// has no other effect — name resolution semantics are otherwise
    /// identical between user code and stdlib source.
    pub(crate) is_stdlib_source: bool,
    /// True iff the program being resolved originates from a `_test.kara`
    /// file (or a merged test-companion module). Gates the signature-from-
    /// call-site stub diagnostic (phase-5-diagnostics line 633): an
    /// unresolved-identifier *call* in a test file becomes the augmented
    /// `UndefinedName` carrying a `StubHint`; in production files the
    /// plain diagnostic is emitted. The CLI sets this via
    /// `Resolver::with_test_file(module.is_test_file)`.
    pub(crate) is_test_file: bool,
    /// Phase-10 `#[target(...)]` tombstones: item name → rendered target
    /// spec, for every item `target::filter_inactive_items` removed before
    /// this resolve session. Consulted by `error_undefined_name` so a
    /// reference to a filtered item reports "not available on target X"
    /// instead of a bare undefined-name. Single-file mode threads the map
    /// via [`Resolver::with_target_tombstones`]; project mode adopts it
    /// off the `ProgramTree` in [`Resolver::with_tree`].
    pub(crate) target_tombstones: HashMap<String, String>,
}

impl<'a> Resolver<'a> {
    pub fn new(program: &'a Program) -> Self {
        Resolver {
            program,
            tree: None,
            current_module: None,
            table: SymbolTable::new(),
            resolutions: HashMap::new(),
            errors: Vec::new(),
            current_impl_type: None,
            loop_labels: Vec::new(),
            par_sibling_bindings: HashMap::new(),
            is_stdlib_source: false,
            is_test_file: false,
            target_tombstones: HashMap::new(),
        }
    }

    /// Attach a project-wide `ProgramTree` so `import` declarations can be
    /// validated across modules. Use [`Resolver::new`] followed by
    /// `.with_tree(tree, module_id)` when resolving a specific module in the
    /// project. Also adopts the tree's `#[target(...)]` tombstones so
    /// references to target-filtered items get the targeted diagnostic.
    pub fn with_tree(mut self, tree: &'a ProgramTree, module_id: ModuleId) -> Self {
        self.tree = Some(tree);
        self.current_module = Some(module_id);
        if self.target_tombstones.is_empty() {
            self.target_tombstones = tree.target_tombstones.clone();
        }
        self
    }

    /// Phase-10: provide name → rendered-target-spec tombstones for items
    /// removed by `target::filter_inactive_items` (single-file pipeline).
    pub fn with_target_tombstones(mut self, tombstones: HashMap<String, String>) -> Self {
        self.target_tombstones = tombstones;
        self
    }

    /// Mark the program as stdlib source (the synthetic package baked into
    /// the compiler binary by CR-202 slice 3). When set, `#[compiler_builtin]`
    /// is permitted; when unset (the default), it is rejected with `E0237`.
    pub fn with_stdlib_source(mut self, is_stdlib: bool) -> Self {
        self.is_stdlib_source = is_stdlib;
        self
    }

    /// Mark the module being resolved as a `_test.kara` file (or merged
    /// test companion). Enables the signature-from-call-site stub
    /// diagnostic (phase-5-diagnostics line 633) on unresolved-identifier
    /// calls. Defaults to `false` — production files keep the plain
    /// diagnostic.
    pub fn with_test_file(mut self, is_test: bool) -> Self {
        self.is_test_file = is_test;
        self
    }

    pub fn resolve(mut self) -> ResolveResult {
        // Pass 1: collect all top-level declarations
        self.collect_top_level_items();
        // Pass 1.5: validate layout blocks against collected struct definitions
        self.validate_layouts();
        // Pass 1.6: attribute-name validation. Unknown bare-name
        // attributes (`#[no_such_thing]`) emit `E_UNKNOWN_ATTRIBUTE`;
        // multi-segment paths (`#[diagnostic::*]`, tool namespaces)
        // are silently accepted at this layer — see
        // `attribute_validator` for the per-namespace policy. Slice 2
        // of the `#[diagnostic::*]` entry.
        self.errors
            .extend(crate::attribute_validator::validate_program_attributes(
                self.program,
            ));
        // Pass 1.7: `#[default]` placement + arg-shape. `#[default]` is
        // legal only on a unit enum variant under `#[derive(Default)]`;
        // this emits the position / without-derive / malformed-args
        // diagnostics before the per-derive validators run.
        self.errors
            .extend(crate::attribute_validator::validate_default_attribute(
                self.program,
            ));
        // Pass 2: resolve all bodies
        self.resolve_items();

        ResolveResult {
            resolutions: self.resolutions,
            symbol_table: self.table,
            errors: self.errors,
            def_paths: crate::def_path::collect_item_def_paths(self.program),
            queries: Vec::new(),
        }
    }

    fn error_undefined_name(&mut self, name: &str, span: Span) {
        // B-2026-07-02-5: the name exists — as a binding of a SIBLING branch
        // of the enclosing `par { }` block. Each top-level statement of a
        // `par { }` is a concurrent branch with its own scope, so the read
        // is illegal; say so instead of "undefined name" (pre-fix this shape
        // slipped resolution entirely and panicked the interpreter /
        // errored ungracefully in codegen).
        if let Some(sibling_span) = self.par_sibling_bindings.get(name) {
            self.errors.push(ResolveError {
                message: format!(
                    "cannot read '{}' from a sibling `par` branch: each top-level statement in a `par {{ }}` block is a concurrent branch with its own scope (bound at {}:{}); the branch bindings are only combinable after the join — in the block's tail expression or after the `par {{ }}` block",
                    name, sibling_span.line, sibling_span.column,
                ),
                span,
                kind: ResolveErrorKind::UndefinedName,
                suggestion: None,
                replacement: None,
                stub_hint: None,
            });
            return;
        }
        // Phase-10 `#[target(...)]`: a name that exists in source but was
        // filtered for the current compilation target gets the targeted
        // diagnostic instead of a bare undefined-name + fuzzy suggestion.
        if let Some(spec) = self.target_tombstones.get(name) {
            self.errors.push(ResolveError {
                message: format!(
                    "'{}' is not available on target `{}` — it is gated to \
                     `#[target({})]`",
                    name,
                    crate::target::active_target(),
                    spec,
                ),
                span,
                kind: ResolveErrorKind::UndefinedName,
                suggestion: None,
                replacement: None,
                stub_hint: None,
            });
            return;
        }
        let visible = self.table.visible_names();
        let suggestion = suggest_similar(name, &visible);
        let mut message = format!("undefined name '{}'", name);
        if let Some(ref s) = suggestion {
            message.push_str(&format!(", did you mean '{}'?", s));
        }
        let replacement = suggestion.as_ref().map(|s| {
            Box::new(TextEdit {
                offset: span.offset,
                length: span.length,
                replacement: s.clone(),
            })
        });
        self.errors.push(ResolveError {
            message,
            span,
            kind: ResolveErrorKind::UndefinedName,
            suggestion,
            replacement,
            stub_hint: None,
        });
    }

    /// Variant of `error_undefined_name` used at call positions inside a
    /// `_test.kara` file. Attaches a `StubHint` that the CLI lifts into a
    /// `hints[].diff` entry proposing a stub for the missing function in
    /// the sibling production file. Slice 1: argument types ship as `_`
    /// placeholders unless [`infer_stub_arg_type`] can read a literal
    /// expression (slice 2 extends inference to explicit-typed bindings
    /// and to comparison-context return types). Reuses the existing
    /// `did you mean` suggestion logic so the diagnostic remains useful
    /// when the unresolved name is in fact a typo.
    pub(crate) fn error_undefined_call_with_stub(
        &mut self,
        name: &str,
        span: Span,
        args: &[CallArg],
    ) {
        let visible = self.table.visible_names();
        let suggestion = suggest_similar(name, &visible);
        let mut message = format!("undefined name '{}'", name);
        if let Some(ref s) = suggestion {
            message.push_str(&format!(", did you mean '{}'?", s));
        }
        let replacement = suggestion.as_ref().map(|s| {
            Box::new(TextEdit {
                offset: span.offset,
                length: span.length,
                replacement: s.clone(),
            })
        });
        let stub_args: Vec<StubArg> = args
            .iter()
            .map(|a| StubArg {
                inferred_type: infer_stub_arg_type(&a.value),
            })
            .collect();
        let stub = Box::new(StubHint {
            callee_name: name.to_string(),
            args: stub_args,
            return_type: None,
        });
        self.errors.push(ResolveError {
            message,
            span,
            kind: ResolveErrorKind::UndefinedName,
            suggestion,
            replacement,
            stub_hint: Some(stub),
        });
    }

    fn error_undefined_type(&mut self, name: &str, span: Span) {
        let visible = self.table.visible_names();
        let suggestion = suggest_similar(name, &visible);
        let mut message = format!("undefined type '{}'", name);
        if let Some(ref s) = suggestion {
            message.push_str(&format!(", did you mean '{}'?", s));
        }
        let replacement = suggestion.as_ref().map(|s| {
            Box::new(TextEdit {
                offset: span.offset,
                length: span.length,
                replacement: s.clone(),
            })
        });
        self.errors.push(ResolveError {
            message,
            span,
            kind: ResolveErrorKind::UndefinedType,
            suggestion,
            replacement,
            stub_hint: None,
        });
    }

    fn record_resolution(&mut self, span: &Span, id: SymbolId) {
        self.resolutions.insert(SpanKey::from_span(span), id);
    }
}

/// Best-effort local inference of a call-argument's type for the
/// signature-from-call-site stub diagnostic. Returns `None` (rendered as
/// `_`) when the expression's type depends on type-checker state the
/// resolver cannot consult. Slice 2 covers the literal cases that are
/// fully determinable at parse time:
///
/// - Integer literal with a suffix → the suffix's wire form (`i32`,
///   `u64`, `i128`, …). Unsuffixed integers fall back to `i64`, the
///   language's default integer type (see
///   `typechecker::const_eval::infer_operand_target_ty` for the canonical
///   reference).
/// - Float literal with a suffix → `f32` / `f64`. Unsuffixed floats
///   default to `f64`.
/// - Boolean literal → `bool`. Char literal → `char`.
/// - Plain / multi-line string literal → `String` (matches
///   `type_display(Type::Str)`).
/// - C-string literal → `ref CStr` (matches the
///   `infer_expr` arm shipped with line 587).
///
/// Identifier arguments, complex expressions, struct literals, and
/// collection literals fall back to `None`. The post-typecheck-refinement
/// layer (deferred — see the design.md inference-scope table) is where
/// those gain types; the resolver only sees what's local to the call.
fn infer_stub_arg_type(expr: &Expr) -> Option<String> {
    use crate::token::{FloatSuffix, IntSuffix};
    let ty = match &expr.kind {
        ExprKind::Integer(_, suffix) => match suffix {
            Some(IntSuffix::I8) => "i8",
            Some(IntSuffix::I16) => "i16",
            Some(IntSuffix::I32) => "i32",
            Some(IntSuffix::I64) => "i64",
            Some(IntSuffix::I128) => "i128",
            Some(IntSuffix::U8) => "u8",
            Some(IntSuffix::U16) => "u16",
            Some(IntSuffix::U32) => "u32",
            Some(IntSuffix::U64) => "u64",
            Some(IntSuffix::U128) => "u128",
            None => "i64",
        },
        ExprKind::Float(_, suffix) => match suffix {
            Some(FloatSuffix::F16) => "f16",
            Some(FloatSuffix::BF16) => "bf16",
            Some(FloatSuffix::F32) => "f32",
            Some(FloatSuffix::F64) => "f64",
            None => "f64",
        },
        ExprKind::Bool(_) => "bool",
        ExprKind::CharLit(_) => "char",
        ExprKind::ByteLit(_) => "u8",
        ExprKind::StringLit(_) | ExprKind::MultiStringLit(_) => "String",
        ExprKind::CStringLit { .. } => "ref CStr",
        _ => return None,
    };
    Some(ty.to_string())
}
