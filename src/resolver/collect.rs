//! Pass 1: top-level declaration collection.
//!
//! Walks the program's items once, registers every declaration with
//! the symbol table, and assembles the
//! cross-module use / import tables. Methods are kept thin —
//! each item form has its own dedicated `collect_<kind>` that
//! pushes its symbol(s) onto `table` and defers body resolution
//! to Pass 2.
//!
//! Houses `collect_top_level_items` (the dispatcher) + every
//! per-item-form helper, the `check_compiler_builtin_attr` gate, the two name
//! helpers `type_expr_name` / `is_trait_assoc_fn_name`, and the
//! big `collect_import` body that drives cross-module visibility
//! resolution against `ProgramTree`.
//!
//! Lives in a sibling `impl<'a> super::Resolver<'a>` block.

use crate::ast::*;
use crate::edit_distance::suggest_similar;
use crate::module::{self};

use super::{
    module_exposes_name, module_item_visibility, module_top_level_names, visibility_allows_access,
    ResolveError, ResolveErrorKind, Symbol, SymbolId, SymbolKind, TextEdit, VariantSymbolKind,
};

impl<'a> super::Resolver<'a> {
    // ── Pass 1: Top-level declaration collection ────────────────

    pub(crate) fn collect_top_level_items(&mut self) {
        for item in &self.program.items {
            match item {
                Item::Function(f) => self.collect_function(f),
                Item::StructDef(s) => self.collect_struct(s),
                Item::UnionDef(u) => self.collect_union(u),
                Item::EnumDef(e) => self.collect_enum(e),
                Item::TraitDef(t) => self.collect_trait(t),
                Item::TraitAlias(t) => self.collect_trait_alias(t),
                Item::MarkerTrait(t) => self.collect_marker_trait(t),
                Item::ImplBlock(i) => self.collect_impl(i),
                Item::EffectResource(e) => self.collect_effect_resource(e),
                Item::EffectGroup(e) => self.collect_effect_group(e),
                Item::EffectVerbDecl(e) => self.collect_effect_verb(e),
                Item::ConstDecl(c) => self.collect_const(c),
                Item::ModuleBinding(b) => self.collect_module_binding(b),
                Item::TypeAlias(t) => self.collect_type_alias(t),
                Item::Import(i) => self.collect_import(i),
                Item::ExternFunction(e) => self.collect_extern_function(e),
                Item::ExternBlock(b) => {
                    for it in &b.items {
                        match it {
                            ExternItem::Function(f) => self.collect_extern_function(f),
                            ExternItem::OpaqueType(o) => self.collect_opaque_foreign_type(o),
                        }
                    }
                }
                Item::DistinctType(d) => self.collect_distinct_type(d),
                // Test cases don't introduce any module-scope name —
                // the case body is resolved when the test runner
                // lowers `Item::TestCase` to a synthetic
                // `Item::Function` (slice 3) and feeds that lowered
                // program back through the resolver.
                Item::LayoutDef(_)
                | Item::AliasDecl(_)
                | Item::IndependentDecl(_)
                | Item::TestCase(_) => {}
            }
        }
        self.report_type_alias_cycles();
    }

    /// B-2026-11-19-9: an alias names an existing type and is transparent
    /// (design.md § Type aliases), so aliases that name each other
    /// (`type A = Vec[B]; type B = A;`) name no type at all. Checked at the
    /// declarations, once per cycle, so a program that never uses them is
    /// refused too, and one that does is not met by a later internal error.
    fn report_type_alias_cycles(&mut self) {
        fn names(ty: &TypeExpr, out: &mut Vec<String>) {
            match &ty.kind {
                TypeKind::Path(p) => {
                    if p.segments.len() == 1 {
                        out.push(p.segments[0].clone());
                    }
                    for a in p.generic_args.iter().flatten() {
                        if let GenericArg::Type(t) = a {
                            names(t, out);
                        }
                    }
                }
                TypeKind::Tuple(ts) => ts.iter().for_each(|t| names(t, out)),
                TypeKind::Array { element, .. } => names(element, out),
                TypeKind::Pointer { inner, .. }
                | TypeKind::Ref(inner)
                | TypeKind::MutRef(inner)
                | TypeKind::Frozen(inner)
                | TypeKind::MutSlice(inner)
                | TypeKind::Weak(inner) => names(inner, out),
                TypeKind::FnType {
                    params,
                    return_type,
                    ..
                } => {
                    params.iter().for_each(|t| names(t, out));
                    if let Some(r) = return_type {
                        names(r, out);
                    }
                }
                _ => {}
            }
        }
        let aliases: Vec<&TypeAliasDef> = self
            .program
            .items
            .iter()
            .filter_map(|it| match it {
                Item::TypeAlias(t) => Some(t),
                _ => None,
            })
            .collect();
        let index: std::collections::HashMap<&str, usize> = aliases
            .iter()
            .enumerate()
            .map(|(i, a)| (a.name.as_str(), i))
            .collect();
        let edges: Vec<Vec<usize>> = aliases
            .iter()
            .map(|a| {
                let own: Vec<&str> = a
                    .generic_params
                    .iter()
                    .flat_map(|g| g.params.iter().map(|p| p.name.as_str()))
                    .collect();
                let mut out = Vec::new();
                names(&a.ty, &mut out);
                let mut to: Vec<usize> = out
                    .iter()
                    .filter(|n| !own.contains(&n.as_str()))
                    .filter_map(|n| index.get(n.as_str()).copied())
                    .collect();
                to.sort_unstable();
                to.dedup();
                to
            })
            .collect();
        let mut reported = vec![false; aliases.len()];
        for start in 0..aliases.len() {
            if reported[start] {
                continue;
            }
            // A path from `start` back to itself, if there is one.
            let mut stack = vec![(start, vec![start])];
            let mut seen = vec![false; aliases.len()];
            let mut cycle = None;
            while let Some((at, path)) = stack.pop() {
                for &n in &edges[at] {
                    if n == start {
                        cycle = Some(path.clone());
                        break;
                    }
                    if !seen[n] {
                        seen[n] = true;
                        let mut p = path.clone();
                        p.push(n);
                        stack.push((n, p));
                    }
                }
                if cycle.is_some() {
                    break;
                }
            }
            let Some(cycle) = cycle else { continue };
            for &i in &cycle {
                reported[i] = true;
            }
            let chain: Vec<String> = cycle
                .iter()
                .chain(std::iter::once(&start))
                .map(|&i| format!("`{}`", aliases[i].name))
                .collect();
            let a = aliases[start];
            self.errors.push(ResolveError {
                message: format!(
                    "type alias `{}` names no type: the aliases form a cycle ({}), and an \
                     alias only renames an existing type (design.md § Type aliases); a \
                     recursive type is a `struct` or `enum`",
                    a.name,
                    chain.join(" -> ")
                ),
                span: a.span,
                kind: ResolveErrorKind::UndefinedType,
                suggestion: None,
                replacement: None,
                stub_hint: None,
            });
        }
    }

    /// Reject `#[deprecated]` placed on an `impl` block per design.md
    /// § `#[deprecated]` for Item Deprecation > "Where it cannot
    /// appear" — impl-level deprecation would be ambiguous (which
    /// methods does it cover?); the user should deprecate the
    /// individual methods instead.
    fn reject_deprecated_on_impl(&mut self, attrs: &[Attribute]) {
        for attr in attrs {
            if attr.is_bare("deprecated") {
                self.errors.push(ResolveError {
                    message: "error[E_DEPRECATED_ON_IMPL]: \
                              `#[deprecated]` is not valid on an `impl` \
                              block — deprecate the underlying methods \
                              individually instead. See design.md § \
                              `#[deprecated]` for Item Deprecation."
                        .to_string(),
                    span: attr.span,
                    kind: ResolveErrorKind::DeprecatedOnImpl,
                    suggestion: None,
                    replacement: None,
                    stub_hint: None,
                });
            }
        }
    }

    /// Reject `#[deprecated]` placed on a struct field. Field-level
    /// deprecation is post-v1 — use-site detection for field
    /// reads/writes is non-trivial and is bundled with the post-v1
    /// lint expansion. Per design.md § `#[deprecated]` for Item
    /// Deprecation > "Where it cannot appear".
    fn reject_deprecated_on_field(&mut self, attrs: &[Attribute]) {
        for attr in attrs {
            if attr.is_bare("deprecated") {
                self.errors.push(ResolveError {
                    message: "error[E_DEPRECATED_ON_FIELD]: \
                              `#[deprecated]` on individual struct fields \
                              is not in 0.8; only item-level deprecation is \
                              supported."
                        .to_string(),
                    span: attr.span,
                    kind: ResolveErrorKind::DeprecatedOnField,
                    suggestion: None,
                    replacement: None,
                    stub_hint: None,
                });
            }
        }
    }

    /// Reject `#[track_caller]` placed on an item kind that is not a
    /// `fn` declaration. Per design.md § Error Handling > "Stdlib
    /// panic-emitters report the caller's source location", the
    /// attribute redirects the panic-site source location and only
    /// makes sense on functions. Trait method declarations are *also*
    /// legal targets per the spec (last-writer-wins propagation to
    /// impls), but trait-method attribute support is a separate
    /// enabling change — `TraitMethod` has no `attributes` field
    /// today, so the attribute cannot attach there and this helper
    /// covers every other site (struct, enum, trait, marker trait,
    /// trait alias, impl block, struct field).
    ///
    /// `target_kind` is the human-readable role name surfaced in the
    /// diagnostic message. Function and impl-method callers skip this
    /// helper entirely — the attribute is legal at those sites.
    fn reject_track_caller_attr(&mut self, attrs: &[Attribute], target_kind: &str) {
        for attr in attrs {
            if attr.is_bare("track_caller") {
                self.errors.push(ResolveError {
                    message: format!(
                        "error[E_TRACK_CALLER_INVALID_TARGET]: \
                         `#[track_caller]` is not valid on {target_kind}; \
                         the attribute redirects the panic-site source \
                         location and only applies to `fn` declarations \
                         — see design.md § 10, \"Panics report the caller's \
                         location\"",
                    ),
                    span: attr.span,
                    kind: ResolveErrorKind::TrackCallerInvalidTarget,
                    suggestion: None,
                    replacement: None,
                    stub_hint: None,
                });
            }
        }
    }

    /// Reject `#[gpu]` placed on an item kind that is not a `fn`
    /// declaration. Per design.md § GPU Subset Constraints, `#[gpu]` is
    /// the constraint marker that asserts a function uses only
    /// GPU-compatible features and makes it GPU-callable — it only
    /// applies to functions (free `fn`, inherent / trait-impl method,
    /// trait method declaration). Mirrors [`Self::reject_track_caller_attr`]:
    /// called once per non-fn item kind (struct, enum, union, trait
    /// decl, trait alias, marker trait, impl block, module const, type
    /// alias) and on their fields / variants. Function and impl-method
    /// callers skip this helper — the attribute is legal at those sites.
    ///
    /// `target_kind` is the human-readable role name surfaced in the
    /// diagnostic message.
    fn reject_gpu_attr(&mut self, attrs: &[Attribute], target_kind: &str) {
        for attr in attrs {
            if attr.is_bare("gpu") {
                self.errors.push(ResolveError {
                    message: format!(
                        "error[E_GPU_INVALID_TARGET]: \
                         `#[gpu]` is not valid on {target_kind}; the \
                         attribute is the GPU-subset constraint marker and \
                         only applies to `fn` declarations — it asserts the \
                         function uses only GPU-compatible features and \
                         makes it callable from `gpu.dispatch`. See \
                         design.md § GPU Subset Constraints.",
                    ),
                    span: attr.span,
                    kind: ResolveErrorKind::GpuInvalidTarget,
                    suggestion: None,
                    replacement: None,
                    stub_hint: None,
                });
            }
        }
    }

    /// Reject the codegen-hint attributes (`#[inline]`,
    /// `#[inline(always)]`, `#[inline(never)]`, `#[cold]`) on a position
    /// where they are not valid. Mirrors [`Self::reject_track_caller_attr`]:
    /// called once per non-fn item kind (struct, enum, union, trait
    /// decl, trait alias, marker trait, impl block, module const, type
    /// alias) and on their fields / variants. Function and impl-method
    /// callers skip it — the hints are legal there. Foreign-function
    /// *declarations* use [`Self::reject_codegen_hint_on_extern`]
    /// instead, which carries the extern-specific diagnostic. Closures
    /// are rejected earlier, at parse (`E_CODEGEN_HINT_ON_CLOSURE`).
    ///
    /// `target_kind` is the human-readable role name surfaced in the
    /// message. See design.md § Codegen Hint Attributes > "Where they
    /// may appear".
    fn reject_codegen_hint_attrs(&mut self, attrs: &[Attribute], target_kind: &str) {
        for attr in attrs {
            if let Some(name) = attr.codegen_hint_name() {
                self.errors.push(ResolveError {
                    message: format!(
                        "error[E_CODEGEN_HINT_INVALID_POSITION]: \
                         `#[{name}]` is not valid on {target_kind}; codegen hints \
                         attach to named functions — a free `fn`, an inherent or \
                         trait-impl method, a trait method declaration, an \
                         `extern \"C\" fn` definition, or a `Drop` destructor. To \
                         hint every method of an `impl` block, annotate each method \
                         (block-level shorthand is not in 0.8).",
                    ),
                    span: attr.span,
                    kind: ResolveErrorKind::CodegenHintInvalidTarget,
                    suggestion: None,
                    replacement: None,
                    stub_hint: None,
                });
            }
        }
    }

    /// Reject the codegen-hint attributes on a foreign-function
    /// declaration inside `unsafe extern { ... }`. There is no
    /// Kāra-side body to inline, so the hints cannot apply.
    fn reject_codegen_hint_on_extern(&mut self, attrs: &[Attribute]) {
        for attr in attrs {
            if let Some(name) = attr.codegen_hint_name() {
                self.errors.push(ResolveError {
                    message: format!(
                        "error[E_CODEGEN_HINT_ON_EXTERN_DECL]: \
                         `#[{name}]` cannot apply to a foreign-function declaration; \
                         there is no Kāra-side body to inline. The hint belongs on a \
                         Kāra function definition — including an `extern \"C\" fn name(...) \
                         {{ ... }}` *definition* with a body.",
                    ),
                    span: attr.span,
                    kind: ResolveErrorKind::CodegenHintOnExternDecl,
                    suggestion: None,
                    replacement: None,
                    stub_hint: None,
                });
            }
        }
    }

    /// Reject `#[profile(...)]` placed on an item kind that doesn't
    /// support it. Slice 1+2 of v60 item entry at line 499 — the
    /// attribute asserts function-level profile compatibility and is
    /// fn-only at v1. Module-level placement is part of the spec but
    /// blocked on the module-attribute AST surface.
    fn reject_profile_attr(&mut self, attrs: &[Attribute], target_kind: &str) {
        for attr in attrs {
            if attr.is_bare("profile") {
                self.errors.push(ResolveError {
                    message: format!(
                        "error[E_PROFILE_INVALID_TARGET]: \
                         `#[profile(...)]` is not valid on {target_kind}; \
                         the attribute asserts per-function profile \
                         compatibility and only applies to `fn` declarations",
                    ),
                    span: attr.span,
                    kind: ResolveErrorKind::ProfileInvalidTarget,
                    suggestion: None,
                    replacement: None,
                    stub_hint: None,
                });
            }
        }
    }

    /// Reject `#[no_effect(...)]` on an item kind that doesn't support it.
    /// Same rule and same reason as `reject_profile_attr` one function up:
    /// the attribute constrains a function's effect set, and only a `fn` has
    /// one. Shares `ProfileInvalidTarget` because the placement rule is
    /// literally the same rule — a distinct error kind would imply the two
    /// can diverge, and nothing here should let them.
    fn reject_no_effect_attr(&mut self, attrs: &[Attribute], target_kind: &str) {
        for attr in attrs {
            if attr.is_bare("no_effect") {
                self.errors.push(ResolveError {
                    message: format!(
                        "error[E_NO_EFFECT_INVALID_TARGET]: \
                         `#[no_effect(...)]` is not valid on {target_kind}; \
                         the attribute asserts that a function's effect set \
                         excludes the named effects and only applies to `fn` \
                         declarations",
                    ),
                    span: attr.span,
                    kind: ResolveErrorKind::ProfileInvalidTarget,
                    suggestion: None,
                    replacement: None,
                    stub_hint: None,
                });
            }
        }
    }

    /// Validate every `#[profile(...)]` name on a `fn` against the
    /// closed v1 set (`default` / `embedded` / `kernel`, mirroring
    /// `CompileProfile` from `src/manifest.rs`). Unknown names emit
    /// `error[E_UNKNOWN_PROFILE]`. Empty `profile_compat` (the
    /// no-attribute case) short-circuits.
    fn validate_profile_names(&mut self, f: &crate::ast::Function) {
        const KNOWN: &[&str] = &["default", "embedded", "kernel"];
        for name in &f.profile_compat {
            if !KNOWN.iter().any(|k| k == name) {
                // Anchor the diagnostic at the function's span — the
                // attribute span is the bracketed `#[profile(...)]`
                // outer, not the offending arg; pointing at the fn
                // gets the user to the right neighbourhood. A future
                // refinement could thread per-arg spans through the
                // scan helper for precise underlines.
                self.errors.push(ResolveError {
                    message: format!(
                        "error[E_UNKNOWN_PROFILE]: \
                         unknown profile `{name}` in `#[profile(...)]` on \
                         `fn {fn_name}` — the profiles are {known}",
                        fn_name = f.name,
                        known = KNOWN.join(", "),
                    ),
                    span: f.span,
                    kind: ResolveErrorKind::UnknownProfile,
                    suggestion: None,
                    replacement: None,
                    stub_hint: None,
                });
            }
        }
    }

    /// Reject `#[non_exhaustive]` placed on an item kind that does
    /// not support it. Per design.md § `#[non_exhaustive]` for
    /// Evolvable Public Types, the attribute is meaningful only on
    /// `pub struct` and `pub enum` declarations — no cross-package
    /// boundary exists for private types, traits / fns / impls have
    /// no field or variant surface to evolve, and individual enum
    /// variants are v1-out-of-scope (Rust accepts variant-level; we
    /// ship type-level only, additive later if real use surfaces).
    ///
    /// `target_kind` is the human-readable role name surfaced in the
    /// diagnostic message — `"trait"`, `"function"`, `"impl block"`,
    /// `"private struct"`, etc. The two callers that *do* accept the
    /// attribute (`collect_struct` / `collect_enum` on `pub` types)
    /// skip this helper entirely; everyone else calls through with
    /// the kind name that fits their item.
    fn reject_non_exhaustive_attr(&mut self, attrs: &[Attribute], target_kind: &str) {
        for attr in attrs {
            if attr.is_bare("non_exhaustive") {
                self.errors.push(ResolveError {
                    message: format!(
                        "error[E_NON_EXHAUSTIVE_INVALID_TARGET]: \
                         `#[non_exhaustive]` is not valid on {target_kind}; \
                         the attribute applies only to `pub struct` and \
                         `pub enum` declarations — see design.md § \
                         `#[non_exhaustive]` for Evolvable Public Types",
                    ),
                    span: attr.span,
                    kind: ResolveErrorKind::NonExhaustiveInvalidTarget,
                    suggestion: None,
                    replacement: None,
                    stub_hint: None,
                });
            }
        }
    }

    fn check_compiler_builtin_attr(&mut self, attrs: &[Attribute], item_stdlib_origin: bool) {
        // The gate bypasses (a) when the whole resolver session is in
        // stdlib-source mode (CR-202 slice 1's `with_stdlib_source(true)`
        // builder), or (b) when the individual item carries the per-item
        // stdlib-origin tag (CR-202 slice 3b). The per-item tag is what
        // 3c uses to flip baked stdlib items spliced into a user-mode
        // program tree — the resolver session for that tree stays
        // user-mode, but individual baked items get an exemption.
        if self.is_stdlib_source || item_stdlib_origin {
            return;
        }
        for attr in attrs {
            // `#[observability]` marks the library's logging as transparent
            // to effects (design.md § Observability); user code cannot.
            let name = if attr.is_bare("compiler_builtin") {
                "compiler_builtin"
            } else if attr.is_bare("observability") {
                "observability"
            } else {
                continue;
            };
            {
                self.errors.push(ResolveError {
                    message: format!(
                        "`#[{name}]` is reserved for stdlib source baked into the compiler binary"
                    ),
                    span: attr.span,
                    kind: ResolveErrorKind::CompilerBuiltinReserved,
                    suggestion: None,
                    replacement: None,
                    stub_hint: None,
                });
            }
        }
    }

    fn collect_function(&mut self, f: &Function) {
        self.check_compiler_builtin_attr(&f.attributes, f.stdlib_origin);
        self.reject_non_exhaustive_attr(&f.attributes, "function");
        self.validate_profile_names(f);
        let param_names: Vec<String> = f
            .params
            .iter()
            .flat_map(|p| p.pattern.binding_names())
            .collect();
        match self.table.define(
            f.name.clone(),
            SymbolKind::Function { param_names },
            f.span,
            f.is_pub,
        ) {
            Ok(id) => {
                self.record_deprecation_if_present(id, &f.deprecation);
                self.record_unstable_if_present(id, &f.unstable);
            }
            Err(e) => self.errors.push(e),
        }
    }

    fn collect_struct(&mut self, s: &StructDef) {
        self.check_compiler_builtin_attr(&s.attributes, s.stdlib_origin);
        // `#[non_exhaustive]` is only meaningful at the cross-package
        // boundary — a private struct has no consumers outside its own
        // package, so the attribute is rejected with the kind-named
        // diagnostic. `pub struct` consumes the attribute via the
        // parser-set `is_non_exhaustive` flag and no rejection fires.
        if s.is_non_exhaustive && !s.is_pub {
            self.reject_non_exhaustive_attr(&s.attributes, "private struct");
        }
        self.reject_track_caller_attr(&s.attributes, "struct");
        self.reject_gpu_attr(&s.attributes, "struct");
        self.reject_codegen_hint_attrs(&s.attributes, "struct");
        self.reject_profile_attr(&s.attributes, "struct");
        self.reject_no_effect_attr(&s.attributes, "struct");

        // B-2026-08-21-2 — `float_in_serialized_type`. design.md § Floating
        // point > Serialization: "`f32`/`f64` fields in `#[derive(Serialize,
        // Deserialize)]` types trigger the `float_in_serialized_type` lint —
        // JSON has no NaN encoding, MessagePack/Protobuf carry IEEE bits but
        // consumers may diverge. The lint is a warning, suppressible per-field
        // with `#[allow(float_in_serialized_type)]`."
        //
        // PER-FIELD suppression, which is why this walks fields rather than
        // reporting once on the struct: a type may carry one float the author
        // has reasoned about beside another they have not, and the annotation
        // has to land where the reasoning is.
        //
        // The derive MACHINERY is deferred post-v1, but the attribute parses
        // and typechecks today, so the trigger is decidable now — the lint is
        // not vacuous, it was simply never written.
        let derives = crate::typechecker::extract_derived_traits(&s.attributes);
        if derives.contains("Serialize") || derives.contains("Deserialize") {
            for f in &s.fields {
                let is_float = matches!(
                    &f.ty.kind,
                    TypeKind::Path(p)
                        if p.generic_args.is_none()
                            && matches!(
                                p.segments.first().map(|x| x.as_str()),
                                Some("f32") | Some("f64")
                            )
                );
                if !is_float {
                    continue;
                }
                let allowed = f.attributes.iter().any(|a| {
                    a.is_bare("allow")
                        && a.args.iter().any(|arg| {
                            matches!(
                                &arg.value,
                                Some(crate::ast::Expr {
                                    kind: crate::ast::ExprKind::Identifier(name),
                                    ..
                                }) if name == "float_in_serialized_type"
                            )
                        })
                });
                if allowed {
                    continue;
                }
                let ty_name = match &f.ty.kind {
                    TypeKind::Path(p) => p.segments.first().cloned().unwrap_or_default(),
                    _ => String::new(),
                };
                self.errors.push(ResolveError {
                    message: format!(
                        concat!(
                            "field '{}.{}' is `{}` in a type deriving Serialize/",
                            "Deserialize: JSON has no NaN encoding, and MessagePack/",
                            "Protobuf carry IEEE bits that consumers may decode ",
                            "differently, so a round-trip through this field is not ",
                            "guaranteed to preserve NaN or signed zero."
                        ),
                        s.name, f.name, ty_name
                    ),
                    span: f.span,
                    kind: ResolveErrorKind::FloatInSerializedType,
                    suggestion: Some(
                        concat!(
                            "store a lossless encoding (integer bits, or a string) if ",
                            "round-trip fidelity matters, or suppress with ",
                            "#[allow(float_in_serialized_type)] on the field"
                        )
                        .to_string(),
                    ),
                    replacement: None,
                    stub_hint: None,
                });
            }
        }
        // Field-level `#[non_exhaustive]` is post-v1 (Rust accepts it
        // on fields too; we ship type-level only). Reject so users get
        // a focused message instead of a silent acceptance that does
        // nothing — the attribute presence on a field would otherwise
        // be ignored, which is worse than the diagnostic.
        for field in &s.fields {
            self.reject_non_exhaustive_attr(&field.attributes, "struct field");
            self.reject_track_caller_attr(&field.attributes, "struct field");
            self.reject_gpu_attr(&field.attributes, "struct field");
            self.reject_codegen_hint_attrs(&field.attributes, "struct field");
            self.reject_profile_attr(&field.attributes, "struct field");
            self.reject_no_effect_attr(&field.attributes, "struct field");
            self.reject_deprecated_on_field(&field.attributes);
        }
        let field_names: Vec<String> = s.fields.iter().map(|f| f.name.clone()).collect();
        match self.table.define(
            s.name.clone(),
            SymbolKind::Struct { field_names },
            s.span,
            s.is_pub,
        ) {
            Ok(id) => {
                self.record_deprecation_if_present(id, &s.deprecation);
                self.record_unstable_if_present(id, &s.unstable);
            }
            Err(e) => self.errors.push(e),
        }
    }

    fn collect_union(&mut self, u: &UnionDef) {
        self.check_compiler_builtin_attr(&u.attributes, u.stdlib_origin);
        // Per-attribute placement rejections that mirror struct treatment.
        // `#[track_caller]` / `#[profile]` are not meaningful on unions
        // (the FFI shape carries no body and no effect set), so they
        // route through the generic helpers.
        //
        // Phase-5 FFI unions slice 3b: `#[non_exhaustive]` on a union
        // gets its own focused code (`E_UNION_NON_EXHAUSTIVE_FORBIDDEN`)
        // because the reason it is meaningless is union-specific —
        // unions are an FFI boundary shape, not a versioned Kāra-owned
        // aggregate; the field list is determined by the C side and
        // cannot be extended in a backwards-compatible way the way
        // `pub struct` / `pub enum` can. The dedicated kind lets
        // downstream consumers (CLI E-code mapping, IDE quick-fix UIs)
        // distinguish the union case from the generic one without
        // parsing the message body. Field-level `#[non_exhaustive]`
        // still routes through the generic helper below — the focused
        // code is type-level only.
        for attr in &u.attributes {
            if attr.is_bare("non_exhaustive") {
                self.errors.push(ResolveError {
                    message: format!(
                        "error[E_UNION_NON_EXHAUSTIVE_FORBIDDEN]: \
                         `#[non_exhaustive]` is not valid on union `{}` \
                         — unions are an FFI boundary shape, not a \
                         versioned Kāra-owned aggregate; their field \
                         list is determined by the C side and cannot \
                         be extended in a backwards-compatible way the \
                         way `pub struct` / `pub enum` can. Remove the \
                         attribute; if the C contract changes, the \
                         union definition changes in lockstep",
                        u.name,
                    ),
                    span: attr.span,
                    kind: ResolveErrorKind::UnionNonExhaustiveForbidden,
                    suggestion: None,
                    // B-2026-08-10-2's sweep. "Remove the attribute" is
                    // unconditional here — `#[non_exhaustive]` is never valid
                    // on a union — and the diagnostic is already anchored at
                    // the whole `#[non_exhaustive]`, so the edit is that span
                    // deleted. Unlike the call-site markers this cannot absorb
                    // the trailing newline (the resolver sees spans, not
                    // source), so it leaves the attribute's line blank; that is
                    // ordinary between-item whitespace in Kāra, not the
                    // mid-expression debris that made the marker deletions
                    // extend past the token.
                    replacement: Some(Box::new(crate::resolver::TextEdit {
                        offset: attr.span.offset,
                        length: attr.span.length,
                        replacement: String::new(),
                    })),
                    stub_hint: None,
                });
            }
        }
        self.reject_track_caller_attr(&u.attributes, "union");
        self.reject_gpu_attr(&u.attributes, "union");
        self.reject_codegen_hint_attrs(&u.attributes, "union");
        self.reject_profile_attr(&u.attributes, "union");
        self.reject_no_effect_attr(&u.attributes, "union");
        for field in &u.fields {
            self.reject_non_exhaustive_attr(&field.attributes, "union field");
            self.reject_track_caller_attr(&field.attributes, "union field");
            self.reject_gpu_attr(&field.attributes, "union field");
            self.reject_codegen_hint_attrs(&field.attributes, "union field");
            self.reject_profile_attr(&field.attributes, "union field");
            self.reject_no_effect_attr(&field.attributes, "union field");
            self.reject_deprecated_on_field(&field.attributes);
        }
        let field_names: Vec<String> = u.fields.iter().map(|f| f.name.clone()).collect();
        match self.table.define(
            u.name.clone(),
            SymbolKind::Union { field_names },
            u.span,
            u.is_pub,
        ) {
            Ok(id) => {
                self.record_deprecation_if_present(id, &u.deprecation);
                self.record_unstable_if_present(id, &u.unstable);
            }
            Err(e) => self.errors.push(e),
        }
    }

    fn collect_enum(&mut self, e: &EnumDef) {
        self.check_compiler_builtin_attr(&e.attributes, e.stdlib_origin);
        if e.is_non_exhaustive && !e.is_pub {
            self.reject_non_exhaustive_attr(&e.attributes, "private enum");
        }
        self.reject_track_caller_attr(&e.attributes, "enum");
        self.reject_gpu_attr(&e.attributes, "enum");
        self.reject_codegen_hint_attrs(&e.attributes, "enum");
        self.reject_profile_attr(&e.attributes, "enum");
        self.reject_no_effect_attr(&e.attributes, "enum");
        // Variant-level attribute placement validation —
        // `#[track_caller]` and `#[non_exhaustive]` are rejected on
        // individual variants (the spec scopes both at type-level
        // only). `#[deprecated]` IS legal on variants per design.md
        // and so is not rejected here.
        for variant in &e.variants {
            self.reject_track_caller_attr(&variant.attributes, "enum variant");
            self.reject_gpu_attr(&variant.attributes, "enum variant");
            self.reject_codegen_hint_attrs(&variant.attributes, "enum variant");
            self.reject_profile_attr(&variant.attributes, "enum variant");
            self.reject_no_effect_attr(&variant.attributes, "enum variant");
            self.reject_non_exhaustive_attr(&variant.attributes, "enum variant");
        }
        let variant_names: Vec<String> = e.variants.iter().map(|v| v.name.clone()).collect();
        let enum_id = match self.table.define(
            e.name.clone(),
            SymbolKind::Enum { variant_names },
            e.span,
            e.is_pub,
        ) {
            Ok(id) => {
                self.record_deprecation_if_present(id, &e.deprecation);
                self.record_unstable_if_present(id, &e.unstable);
                id
            }
            Err(err) => {
                self.errors.push(err);
                return;
            }
        };

        // Register each variant in global scope
        for variant in &e.variants {
            let variant_kind = match &variant.kind {
                VariantKind::Unit => VariantSymbolKind::Unit,
                VariantKind::Tuple(types) => VariantSymbolKind::Tuple(types.len()),
                VariantKind::Struct(fields) => {
                    VariantSymbolKind::Struct(fields.iter().map(|f| f.name.clone()).collect())
                }
            };
            // Try to register variant name directly; if collision, that's ok —
            // user must use qualified path. Variant-level `#[deprecated]`
            // (allowed by the spec; AST enabling change at phase-5 line 431)
            // is recorded against the variant's own SymbolId.
            if let Ok(variant_id) = self.table.define(
                variant.name.clone(),
                SymbolKind::EnumVariant {
                    parent_enum: enum_id,
                    variant_kind,
                },
                variant.span,
                e.is_pub,
            ) {
                self.record_deprecation_if_present(variant_id, &variant.deprecation);
                self.record_unstable_if_present(variant_id, &variant.unstable);
            }
        }
    }

    fn collect_trait(&mut self, t: &TraitDef) {
        self.check_compiler_builtin_attr(&t.attributes, t.stdlib_origin);
        self.reject_non_exhaustive_attr(&t.attributes, "trait");
        self.reject_track_caller_attr(&t.attributes, "trait");
        self.reject_gpu_attr(&t.attributes, "trait");
        self.reject_codegen_hint_attrs(&t.attributes, "trait");
        self.reject_profile_attr(&t.attributes, "trait");
        self.reject_no_effect_attr(&t.attributes, "trait");
        // Trait-method-level attribute placement validation —
        // `#[track_caller]` IS legal (propagates to impls), so the
        // helper is not called. `#[deprecated]` IS legal. But
        // `#[non_exhaustive]` is rejected (the spec scopes it to
        // pub struct / pub enum types only).
        for item in &t.items {
            if let TraitItem::Method(m) = item {
                self.reject_non_exhaustive_attr(&m.attributes, "trait method");
            }
        }
        let method_names: Vec<String> = t
            .items
            .iter()
            .filter_map(|item| match item {
                TraitItem::Method(m) => Some(m.name.clone()),
                TraitItem::AssocType(_) => None,
            })
            .collect();
        let trait_id = match self.table.define(
            t.name.clone(),
            SymbolKind::Trait { method_names },
            t.span,
            t.is_pub,
        ) {
            Ok(id) => {
                self.record_deprecation_if_present(id, &t.deprecation);
                self.record_unstable_if_present(id, &t.unstable);
                Some(id)
            }
            Err(e) => {
                self.errors.push(e);
                None
            }
        };

        // Trait-method-level `#[deprecated]` (legal per spec; AST
        // enabling change at phase-5 line 431) is recorded against the
        // trait-method's symbol when the slice-4 use-site lookup pass
        // lands. At v1 trait methods are not registered as top-level
        // symbols themselves — they're looked up via the trait's
        // method-names list — so we record the per-method payload
        // against a synthetic id derived from the trait id by name.
        // Until slice 4 lands a real lookup path, we walk the items
        // here to surface the *placement* validation only; the actual
        // payload-recording site for trait methods will need a parallel
        // symbol-table entry that slice 4 of the lint-level work
        // introduces. Tracked as a slice-3b carry-forward below.
        let _ = trait_id;
    }

    fn collect_trait_alias(&mut self, t: &TraitAliasDef) {
        self.reject_non_exhaustive_attr(&t.attributes, "trait alias");
        self.reject_track_caller_attr(&t.attributes, "trait alias");
        self.reject_gpu_attr(&t.attributes, "trait alias");
        self.reject_codegen_hint_attrs(&t.attributes, "trait alias");
        self.reject_profile_attr(&t.attributes, "trait alias");
        self.reject_no_effect_attr(&t.attributes, "trait alias");
        match self
            .table
            .define(t.name.clone(), SymbolKind::TraitAlias, t.span, t.is_pub)
        {
            Ok(id) => {
                self.record_deprecation_if_present(id, &t.deprecation);
                self.record_unstable_if_present(id, &t.unstable);
            }
            Err(e) => self.errors.push(e),
        }
    }

    fn collect_marker_trait(&mut self, t: &MarkerTraitDef) {
        self.reject_non_exhaustive_attr(&t.attributes, "marker trait");
        self.reject_track_caller_attr(&t.attributes, "marker trait");
        self.reject_gpu_attr(&t.attributes, "marker trait");
        self.reject_codegen_hint_attrs(&t.attributes, "marker trait");
        self.reject_profile_attr(&t.attributes, "marker trait");
        self.reject_no_effect_attr(&t.attributes, "marker trait");
        // Marker traits register in the trait namespace alongside ordinary
        // traits; no methods to track, so the symbol carries an empty
        // method list. Trait-bound resolution and impl coherence treat
        // markers identically to ordinary traits — the marker-ness is a
        // definition-site property, not a use-site property.
        match self.table.define(
            t.name.clone(),
            SymbolKind::Trait {
                method_names: Vec::new(),
            },
            t.span,
            t.is_pub,
        ) {
            Ok(id) => {
                self.record_deprecation_if_present(id, &t.deprecation);
                self.record_unstable_if_present(id, &t.unstable);
            }
            Err(e) => self.errors.push(e),
        }
    }

    fn collect_impl(&mut self, imp: &ImplBlock) {
        // `#[compiler_builtin]` is reserved for stdlib source — user code
        // is rejected (E0115). The other `collect_*` callers gate top-level
        // items; impl blocks need the same gate at two levels: the block
        // itself, and each method. ImplBlock has no `stdlib_origin` field
        // because baked stdlib impls live in `STDLIB_PROGRAMS` and are
        // walked by the typechecker/interpreter directly — they never
        // reach this resolver path. So `false` here is correct: any impl
        // block walked by `collect_impl` is user-authored, and a
        // session-wide stdlib-source resolver bypasses via `is_stdlib_source`.
        // Methods carry their own `stdlib_origin` (inherited from the
        // `Function` field) and pass it through for the per-item exemption.
        self.check_compiler_builtin_attr(&imp.attributes, false);
        self.reject_non_exhaustive_attr(&imp.attributes, "impl block");
        self.reject_track_caller_attr(&imp.attributes, "impl block");
        self.reject_gpu_attr(&imp.attributes, "impl block");
        self.reject_codegen_hint_attrs(&imp.attributes, "impl block");
        self.reject_profile_attr(&imp.attributes, "impl block");
        self.reject_no_effect_attr(&imp.attributes, "impl block");
        self.reject_deprecated_on_impl(&imp.attributes);
        // Methods are registered in type_methods, not global scope.
        // We need the type name from the target_type.
        let type_name = self.type_expr_name(&imp.target_type);
        if let Some(type_name) = type_name {
            for item in &imp.items {
                let method = match item {
                    ImplItem::Method(m) => m,
                    ImplItem::AssocType(_) => continue,
                };
                self.check_compiler_builtin_attr(&method.attributes, method.stdlib_origin);
                self.reject_non_exhaustive_attr(&method.attributes, "impl method");
                let param_names: Vec<String> = method
                    .params
                    .iter()
                    .flat_map(|p| p.pattern.binding_names())
                    .collect();
                let method_id = SymbolId(self.table.symbols.len());
                self.table.symbols.push(Symbol {
                    id: method_id,
                    name: method.name.clone(),
                    kind: SymbolKind::Function { param_names },
                    span: method.span,
                    is_pub: method.is_pub,
                    scope: self.table.current_scope,
                });
                self.table.register_method(&type_name, method_id);
                self.record_deprecation_if_present(method_id, &method.deprecation);
                self.record_unstable_if_present(method_id, &method.unstable);
            }
        }
    }

    fn collect_effect_resource(&mut self, e: &EffectResourceDecl) {
        const RESERVED: &[&str] = &["CompileTimeEnv", "CompileTimeHeap"];
        if RESERVED.contains(&e.name.as_str()) {
            self.errors.push(ResolveError {
                message: format!(
                    "resource name '{}' is reserved for the deferred comptime feature; \
                     see deferred.md § Comptime Effect Defaults",
                    e.name
                ),
                span: e.span,
                kind: ResolveErrorKind::ReservedEffectResource,
                suggestion: None,
                replacement: None,
                stub_hint: None,
            });
            return;
        }
        if let Err(err) = self.table.define(
            e.name.clone(),
            SymbolKind::EffectResource,
            e.span,
            true, // effect resources are always accessible
        ) {
            self.errors.push(err);
        }
    }

    fn collect_effect_group(&mut self, e: &EffectGroupDecl) {
        if let Err(err) =
            self.table
                .define(e.name.clone(), SymbolKind::EffectGroup, e.span, e.is_pub)
        {
            self.errors.push(err);
        }
    }

    fn collect_effect_verb(&mut self, e: &EffectVerbDecl) {
        if let Err(err) =
            self.table
                .define(e.verb_name.clone(), SymbolKind::EffectVerb, e.span, true)
        {
            self.errors.push(err);
        }
    }

    fn collect_const(&mut self, c: &ConstDecl) {
        // `#[non_exhaustive]` and `#[track_caller]` are rejected
        // on module-level consts — `#[deprecated]` is the only
        // attribute the spec lists as valid here. The other two
        // produce placement diagnostics; the helper functions
        // share the same "target kind" message shape.
        self.reject_non_exhaustive_attr(&c.attributes, "module const");
        self.reject_track_caller_attr(&c.attributes, "module const");
        self.reject_gpu_attr(&c.attributes, "module const");
        self.reject_codegen_hint_attrs(&c.attributes, "module const");
        self.reject_profile_attr(&c.attributes, "module const");
        self.reject_no_effect_attr(&c.attributes, "module const");
        match self
            .table
            .define(c.name.clone(), SymbolKind::Constant, c.span, c.is_pub)
        {
            Ok(id) => {
                self.record_deprecation_if_present(id, &c.deprecation);
                self.record_unstable_if_present(id, &c.unstable);
            }
            Err(err) => self.errors.push(err),
        }
    }

    /// Slice 3 of design.md § Module-Level Bindings: enforce the
    /// Const-class naming convention on the binding identifier, then
    /// register the name in the same Const-class namespace that
    /// `collect_const` populates. Slices 5-9 layer type / mutability /
    /// effect / codegen on top of the registered symbol; the
    /// compile-time-constant initializer rule (slice 4) is the next
    /// typechecker-side gate.
    ///
    /// The registration uses `SymbolKind::Constant` because design.md
    /// §273 puts module-level bindings in the Const-class namespace
    /// alongside `const NAME` decls. Downstream phases that need the
    /// mutability bit re-read it from the `Item::ModuleBinding` AST
    /// node (the typechecker, effect checker, and codegen all walk
    /// `program.items` separately from the symbol table).
    fn collect_module_binding(&mut self, b: &ModuleBinding) {
        // Const-class naming: parallels `parse_const_decl`'s
        // `check_ident_class(.., IdentClass::Const, "const", ..)`
        // call, but at the resolver per the slice-3 spec so the
        // diagnostic carries the named `E_MODULE_BINDING_NAMING`
        // code rather than the generic parser-side message shape.
        let actual = crate::lexer::classify_ident(&b.name);
        // Set when the naming diagnostic fires with a usable candidate;
        // consumed after `define` below, once the symbol id exists.
        let mut pending_rename: Option<(Option<usize>, String)> = None;
        if actual != crate::lexer::IdentClass::Const {
            // B-2026-07-31-32 — only advertise a rename that actually lands in
            // Const-class. `suggest_const_name` is the identity on a name whose
            // sole letter is already uppercase (`M`, `M2`), which produced
            // "consider renaming to `M`" for a binding named `M` AND a no-op
            // machine-applicable edit: `karac fix` reported "applied 1 fix(es)"
            // on every pass while the file stayed byte-identical and the error
            // persisted, so an automated Mend loop never terminated.
            let suggestion =
                crate::lexer::suggest_name_for_class(&b.name, crate::lexer::IdentClass::Const);
            let actual_desc = match actual {
                crate::lexer::IdentClass::Type => "Type-class",
                crate::lexer::IdentClass::Value => "Value-class",
                crate::lexer::IdentClass::Const => unreachable!(),
            };
            // When no case transform can reach Const-class, say WHY and what to
            // do instead of naming a replacement. A fabricated distinct name
            // (`M` -> `M_VALUE`) would be inventing the programmer's intent, so
            // the diagnostic states the constraint and leaves the choice.
            let advice = match &suggestion {
                Some(s) => format!("consider renaming to `{s}`"),
                None if crate::lexer::has_single_letter(&b.name) => {
                    "a name with a single letter is always Type-class (so `T` / `K` / `V` read \
                     as type parameters), and no capitalization change can make it Const-class \
                     — use a name with more than one letter"
                        .to_string()
                }
                None => "rename it to SCREAMING_SNAKE_CASE".to_string(),
            };
            // Under the core rules every module `let` is refused with
            // E_MODULE_LET, whose `const` fix carries the rename; a naming
            // error first would only lead to it (B-2026-11-17-125).
            if !self.module_let_refused {
                self.errors.push(ResolveError {
                    message: format!(
                        "error[E_MODULE_BINDING_NAMING]: module-level binding name \
                         `{}` is {actual_desc} but module-level `let` / `let mut` \
                         bindings introduce Const-class identifiers \
                         (SCREAMING_SNAKE_CASE); {advice}",
                        b.name,
                    ),
                    span: b.span,
                    kind: ResolveErrorKind::UndefinedName,
                    suggestion: suggestion.as_ref().map(|s| format!("rename to `{s}`")),
                    // Machine-applicable rename (B-2026-07-06-3), deferred.
                    // The candidate is known here, but the binding's USE SITES
                    // are not — collection runs before any body resolves — and
                    // a rename that misses them breaks the program
                    // (B-2026-07-31-33). `attach_module_binding_renames` fills
                    // this in after pass 2, or leaves it empty when it cannot
                    // rewrite every reference.
                    replacement: None,
                    stub_hint: None,
                });
            }
            // A `pub` binding belonging to a package can be referenced from
            // modules this session cannot see — each module resolves in its
            // own session, with its own symbol ids, so an importer's use
            // sites are structurally invisible from here. Renaming under
            // those conditions fixes this file and breaks the importer, so
            // offer the advice without the edit. A standalone file has no
            // importers and keeps the fix.
            let escapes_this_module = b.is_pub && self.pub_refs_may_be_external;
            if let (Some(s), false) = (suggestion, escapes_this_module) {
                let error_index = (!self.module_let_refused).then(|| self.errors.len() - 1);
                pending_rename = Some((error_index, s));
            }
            // Continue and still attempt registration under the
            // offending name so use-site references don't double-up
            // with cascading "undefined name" diagnostics.
        }
        match self
            .table
            .define(b.name.clone(), SymbolKind::Constant, b.span, b.is_pub)
        {
            Ok(id) => {
                self.record_deprecation_if_present(id, &b.deprecation);
                self.record_unstable_if_present(id, &b.unstable);
                if let Some((error_index, new_name)) = pending_rename {
                    self.pending_binding_renames
                        .push(crate::resolver::PendingBindingRename {
                            error_index,
                            symbol: id,
                            new_name,
                            name_span: b.name_span,
                            diag_span: b.span,
                            name_len: b.name.len(),
                        });
                }
            }
            Err(mut err) => {
                if matches!(err.kind, ResolveErrorKind::DuplicateDefinition) {
                    err.message = format!("error[E_DUPLICATE_MODULE_BINDING]: {}", err.message,);
                }
                self.errors.push(err);
            }
        }
    }

    fn collect_type_alias(&mut self, t: &TypeAliasDef) {
        // Same rejection pattern as `collect_const` — type
        // aliases are not fns (track_caller invalid) and aren't
        // public types that grow new variants/fields
        // (non_exhaustive invalid).
        self.reject_non_exhaustive_attr(&t.attributes, "type alias");
        self.reject_track_caller_attr(&t.attributes, "type alias");
        self.reject_gpu_attr(&t.attributes, "type alias");
        self.reject_codegen_hint_attrs(&t.attributes, "type alias");
        self.reject_profile_attr(&t.attributes, "type alias");
        self.reject_no_effect_attr(&t.attributes, "type alias");
        match self
            .table
            .define(t.name.clone(), SymbolKind::TypeAlias, t.span, t.is_pub)
        {
            Ok(id) => {
                self.record_deprecation_if_present(id, &t.deprecation);
                self.record_unstable_if_present(id, &t.unstable);
            }
            Err(err) => self.errors.push(err),
        }
    }

    fn collect_distinct_type(&mut self, d: &crate::ast::DistinctTypeDef) {
        match self
            .table
            .define(d.name.clone(), SymbolKind::DistinctType, d.span, d.is_pub)
        {
            Ok(id) => {
                self.record_deprecation_if_present(id, &d.deprecation);
                self.record_unstable_if_present(id, &d.unstable);
            }
            Err(err) => self.errors.push(err),
        }
    }

    /// Slice 3b plumbing — record a `#[deprecated]` payload against
    /// the freshly-defined symbol when the parser captured one.
    /// Centralises the `Option<Deprecation>` dispatch so each
    /// `collect_*` call-site stays uniform: define → on Ok, record.
    fn record_deprecation_if_present(&mut self, id: SymbolId, dep: &Option<Deprecation>) {
        if let Some(d) = dep {
            self.table.record_deprecation(id, d.clone());
        }
    }

    /// Phase-8 line 49 mirror of `record_deprecation_if_present` —
    /// record a `#[unstable]` payload against the freshly-defined
    /// symbol when the parser captured one. Use-site `unstable_api`
    /// lint emission (`TypeChecker::check_unstable_use_at`) reads
    /// the payload back from the symbol table.
    fn record_unstable_if_present(&mut self, id: SymbolId, payload: &Option<Unstable>) {
        if let Some(p) = payload {
            self.table.record_unstable(id, p.clone());
        }
    }

    /// Resolve a CR-24 `import` declaration against the `ProgramTree`:
    ///
    /// 1. Ensure the dotted prefix in `imp.path` names a module in the graph
    ///    (`E0112 UnknownModule` on miss, with a Levenshtein suggestion over
    ///    all known module paths).
    /// 2. For each brace-listed item, bind `alias.or(name)` in the current
    ///    scope as `SymbolKind::Import { path }`. If the `path + name` is
    ///    itself a submodule, the binding is a module reference; otherwise
    ///    the target module must expose a matching top-level item
    ///    (`E0113 UnknownItemInModule` on miss).
    ///
    /// Single-file mode (no tree attached) skips cross-module validation and
    /// just registers the symbol so downstream passes see the name in scope.
    /// This keeps `karac run file.kara` unchanged.
    /// Single-file mode only: emit `UnknownModule` for a std-rooted import
    /// that names no baked stdlib module. `std` is the compiler's own
    /// namespace and fully known at compile time, so it can be validated even
    /// without a `ProgramTree` (unlike a user cross-file import, which stays
    /// trusted). Valid gated imports were already spliced out upstream by
    /// `expand_gated_stdlib_imports`, so this only ever fires on a genuine
    /// typo / nonexistent `std.*` module. See the `collect_import` single-file
    /// call site for the full rationale (B-2026-07-18-25).
    fn report_bogus_single_file_std_import(&mut self, imp: &ImportDecl) {
        if imp.path.first().map(String::as_str) != Some("std") {
            return; // user cross-file import — not our namespace, trusted
        }
        // A braced import (`import std.web.{X}`) references the module
        // `imp.path`; a bare import (`import std.web;` / `import std.web.net;`)
        // references `imp.path ++ [item]`. Accept if EITHER shape resolves to
        // an importable module.
        // `import std.path;` binding the module as `path` is desugared to
        // item imports before resolution
        // (`module_binding::expand_single_file_std_module_bindings`), except
        // when the program declares an item of the bound name itself.
        for it in &imp.items {
            let mut full = imp.path.clone();
            full.push(it.name.clone());
            if crate::prelude::is_gated_stdlib_module(&full) {
                let bound = it.alias.as_deref().unwrap_or(&it.name);
                self.errors.push(ResolveError {
                    message: format!(
                        "`import {m};` binds `{bound}`, which this program also declares: \
                         import from the module by item, `import {m}.{{..}};` or `import {m}.*;`",
                        m = full.join(".")
                    ),
                    span: imp.span,
                    kind: ResolveErrorKind::UnknownModule,
                    suggestion: None,
                    replacement: None,
                    stub_hint: None,
                });
                return;
            }
        }
        // A library module whose items are globals (B-2026-10-14-48): the
        // items that resolve were dropped before resolution, so each one
        // left is unknown, specified but unimplemented, or renamed.
        if self.report_library_global_items(imp) {
            return;
        }
        // A gated module's item the module does not define (B-2026-10-21-7).
        // A pipeline that splices imports has dropped the items that resolve;
        // one that resolves without splicing still carries them.
        if crate::prelude::is_gated_stdlib_module(&imp.path) && !imp.is_wildcard {
            for it in &imp.items {
                if let Some(alias) = &it.alias {
                    if crate::prelude::gated_module_trait(&imp.path, &it.name) {
                        self.errors.push(ResolveError {
                            message: format!(
                                "`{}.{}` is a trait, and a trait cannot be renamed (`as {alias}`) \
                                 yet; import it as `{}`",
                                imp.path.join("."),
                                it.name,
                                it.name
                            ),
                            span: it.span,
                            kind: ResolveErrorKind::UnknownItemInModule,
                            suggestion: None,
                            replacement: None,
                            stub_hint: None,
                        });
                        continue;
                    }
                }
                if crate::prelude::gated_module_defines(&imp.path, &it.name) {
                    continue;
                }
                let (message, suggestion) =
                    crate::prelude::gated_module_item_error(&imp.path, &it.name);
                // Only an item spanning exactly its name is rewritten: one
                // desugared from `path.f(..)` carries the use's span.
                let replacement = suggestion
                    .as_ref()
                    .filter(|_| it.span.length == it.name.len())
                    .map(|s| {
                        Box::new(TextEdit {
                            offset: it.span.offset,
                            length: it.span.length,
                            replacement: s.clone(),
                        })
                    });
                self.errors.push(ResolveError {
                    message,
                    span: it.span,
                    kind: ResolveErrorKind::UnknownItemInModule,
                    suggestion,
                    replacement,
                    stub_hint: None,
                });
            }
            return;
        }
        if crate::prelude::is_importable_stdlib_module(&imp.path) {
            return;
        }
        if self.report_bare_library_global_import(imp) {
            return;
        }
        let bare_ok = imp.items.iter().any(|it| {
            let mut full = imp.path.clone();
            full.push(it.name.clone());
            crate::prelude::is_importable_stdlib_module(&full)
        });
        if bare_ok {
            return;
        }
        // Bogus — report the deepest std path the user wrote (the bare form's
        // module is `path ++ [first item]`; the braced form's is `path`).
        let deepest: Vec<String> = if imp.path.len() >= 2 {
            imp.path.clone()
        } else {
            let mut p = imp.path.clone();
            if let Some(it) = imp.items.first() {
                p.push(it.name.clone());
            }
            p
        };
        let wanted = deepest.join(".");
        let names = crate::prelude::importable_stdlib_module_names();
        let candidates: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        // Rust's `std::rc` / `std::cell` have no Kāra module to be near:
        // `std.io` is no answer to `std.rc` (B-2026-11-08-65).
        let rust_rc = wanted == "std.rc"
            || wanted == "std.cell"
            || wanted.starts_with("std.rc.")
            || wanted.starts_with("std.cell.");
        let suggestion = if rust_rc {
            None
        } else {
            suggest_similar(&wanted, &candidates)
        };
        let mut message = format!("unknown module `{wanted}`");
        // `std.data` / `std.data.{DataFrame}`: the data types are the post-v1
        // `kara-data` package, not `std` and not the prelude, so neither the
        // near module nor "delete the import" is an answer.
        let data_items: Vec<String> = imp
            .items
            .iter()
            .filter(|it| crate::prelude::REMOVED_LIBRARY_TYPES.contains(&it.name.as_str()))
            .map(|it| format!("`{}`", it.name))
            .collect();
        let data_pkg = wanted == "std.data" || wanted.starts_with("std.data.");
        if !data_items.is_empty() || data_pkg {
            let what = if data_items.is_empty() {
                "data frames and columns come".to_string()
            } else if data_items.len() == 1 {
                format!("{} comes", data_items[0])
            } else {
                format!("{} come", data_items.join(", "))
            };
            message.push_str(&format!(
                ": {what} with the `kara-data` package, which is not in 0.8 and is not \
                 `std` or the prelude (docs/library/data.md, docs/deferred.md § M4c data)"
            ));
            self.errors.push(ResolveError {
                message,
                span: imp.span,
                kind: ResolveErrorKind::UnknownModule,
                suggestion: None,
                replacement: None,
                stub_hint: None,
            });
            return;
        }
        if rust_rc {
            message.push_str(
                ": `Rc` and `RefCell` are Rust's, and Kāra has neither; a `shared struct` is \
                 reference counted with `mut` fields writable through any handle (design.md \
                 §11), so `Rc<RefCell<T>>` is a `T` declared `shared struct`",
            );
        }
        let prelude = Self::prelude_import_note(imp).or_else(|| Self::rehomed_import_note(imp));
        if let Some((note, _)) = &prelude {
            message.push_str(note);
        } else if let Some(ref s) = suggestion {
            message.push_str(&format!(", did you mean `{s}`?"));
        }
        let (suggestion, replacement) = match prelude {
            Some((_, edit)) => (None, Some(edit)),
            None => (suggestion, None),
        };
        self.errors.push(ResolveError {
            message,
            span: imp.span,
            kind: ResolveErrorKind::UnknownModule,
            suggestion,
            replacement,
            stub_hint: None,
        });
    }

    /// An import from `std` whose every item is visible without one
    /// (`import std.iter.once;`, `import std.collections.VecDeque;`, Rust's
    /// `use` lines carried over): the note that says so, and the edit that
    /// deletes the import (B-2026-11-17-149). `None` when any item is not
    /// a prelude name or is renamed.
    fn prelude_import_note(imp: &ImportDecl) -> Option<(String, Box<TextEdit>)> {
        if imp.path.first().map(String::as_str) != Some("std")
            || imp.is_wildcard
            || imp.items.is_empty()
            || imp.items.iter().any(|it| it.alias.is_some())
        {
            return None;
        }
        let visible = |name: &str| {
            crate::prelude::PRELUDE_TYPES.contains(&name)
                || crate::prelude::LIBRARY_SOURCE_TYPES.contains(&name)
                || crate::prelude::PRELUDE_FUNCTIONS.contains(&name)
                || crate::prelude::PRELUDE_TRAITS.contains(&name)
                || name == "String"
                || crate::module_rename::library_function_names().contains(name)
        };
        if !imp.items.iter().all(|it| visible(&it.name)) {
            return None;
        }
        let names: Vec<String> = imp
            .items
            .iter()
            .map(|it| format!("`{}`", it.name))
            .collect();
        let (them, is) = if names.len() == 1 {
            ("it", "is")
        } else {
            ("them", "are")
        };
        let note = format!(
            ": {} {is} in the prelude, visible without an import, so the import can go \
             (delete {them})",
            names.join(", ")
        );
        Some((
            note,
            Box::new(TextEdit {
                offset: imp.span.offset,
                length: imp.span.length,
                replacement: String::new(),
            }),
        ))
    }

    /// An import from a `std` module that does not exist whose items are
    /// each either the prelude's or one real module's (Rust's
    /// `std::collections::BinaryHeap` beside `std::cmp::Reverse`, written
    /// `import std.collections.{PriorityQueue, Reverse};`): the note that
    /// says where each one is, and the edit that imports the rest from
    /// their modules (kata 1244, B-2026-11-20-80). `None` when an item is
    /// renamed, is in no module, or is in more than one.
    fn rehomed_import_note(imp: &ImportDecl) -> Option<(String, Box<TextEdit>)> {
        if imp.path.first().map(String::as_str) != Some("std")
            || imp.is_wildcard
            || imp.items.is_empty()
            || imp.items.iter().any(|it| it.alias.is_some())
        {
            return None;
        }
        let visible = |name: &str| {
            crate::prelude::PRELUDE_TYPES.contains(&name)
                || crate::prelude::LIBRARY_SOURCE_TYPES.contains(&name)
                || crate::prelude::PRELUDE_FUNCTIONS.contains(&name)
                || crate::prelude::PRELUDE_TRAITS.contains(&name)
                || name == "String"
                || crate::module_rename::library_function_names().contains(name)
        };
        let home = |name: &str| -> Option<String> {
            let mut homes: Vec<String> = crate::prelude::GATED_STDLIB_SOURCES
                .iter()
                .enumerate()
                .filter(|(_, (_, text))| text.contains(name))
                .filter(|(i, _)| {
                    crate::prelude::gated_stdlib_program(*i)
                        .items
                        .iter()
                        .any(|item| match item {
                            Item::Function(f) => f.is_pub && f.name == name,
                            Item::StructDef(s) => s.is_pub && s.name == name,
                            Item::EnumDef(e) => e.is_pub && e.name == name,
                            Item::TraitDef(t) => t.is_pub && t.name == name,
                            Item::EffectResource(r) => r.name == name,
                            _ => false,
                        })
                })
                .map(|(_, (path, _))| path.join("."))
                .collect();
            homes.extend(
                crate::prelude::LIBRARY_GLOBAL_MODULES
                    .iter()
                    .filter(|(_, items, _)| items.contains(&name))
                    .map(|(path, _, _)| path.join(".")),
            );
            homes.dedup();
            (homes.len() == 1).then(|| homes.remove(0))
        };
        let mut in_prelude: Vec<String> = Vec::new();
        // Each module with the items it gets, in the import's order.
        let mut moved: Vec<(String, Vec<String>)> = Vec::new();
        for it in &imp.items {
            if visible(&it.name) {
                in_prelude.push(format!("`{}`", it.name));
                continue;
            }
            let module = home(&it.name)?;
            if module == imp.path.join(".") {
                return None;
            }
            match moved.iter_mut().find(|(m, _)| *m == module) {
                Some((_, names)) => names.push(it.name.clone()),
                None => moved.push((module, vec![it.name.clone()])),
            }
        }
        if moved.is_empty() {
            return None;
        }
        let imports: Vec<String> = moved
            .iter()
            .map(|(module, names)| match names.as_slice() {
                [one] => format!("import {module}.{one};"),
                _ => format!("import {module}.{{{}}};", names.join(", ")),
            })
            .collect();
        let places: Vec<String> = moved
            .iter()
            .flat_map(|(module, names)| names.iter().map(move |n| format!("`{n}` is `{module}`'s")))
            .collect();
        let mut note = String::from(": ");
        if !in_prelude.is_empty() {
            let is = if in_prelude.len() == 1 { "is" } else { "are" };
            note.push_str(&format!(
                "{} {is} in the prelude, visible without an import, and ",
                in_prelude.join(", ")
            ));
        }
        note.push_str(&format!(
            "{}, so the import becomes `{}`",
            places.join(", "),
            imports.join(" ")
        ));
        Some((
            note,
            Box::new(TextEdit {
                offset: imp.span.offset,
                length: imp.span.length,
                replacement: imports.join("\n"),
            }),
        ))
    }

    /// B-2026-10-22-30: a file checked on its own that belongs to no package
    /// is the whole program beside the library, so a module outside `std`
    /// cannot exist. Without this the import bound a name nothing defines,
    /// check passed, and both MIR backends stopped on the call.
    fn report_lone_file_user_import(&mut self, imp: &ImportDecl) {
        if !self.lone_file || imp.path.first().map(String::as_str) == Some("std") {
            return;
        }
        // A bare `import m;` names the module `m` itself.
        let module: Vec<String> = if imp.path.is_empty() {
            imp.items
                .first()
                .map(|it| vec![it.name.clone()])
                .unwrap_or_default()
        } else {
            imp.path.clone()
        };
        if module.is_empty() {
            return;
        }
        self.errors.push(ResolveError {
            message: format!(
                "unknown module `{}`: this file belongs to no package, so the only modules \
                 it can import are the library's (`std.*`); a program of several files is a \
                 package, with a `kara.toml` beside its `src/`",
                module.join(".")
            ),
            span: imp.span,
            kind: ResolveErrorKind::UnknownModule,
            suggestion: None,
            replacement: None,
            stub_hint: None,
        });
    }

    /// For an import from a library module whose items are globals
    /// ([`crate::prelude::LIBRARY_GLOBAL_MODULES`], B-2026-10-14-48), report
    /// each item that does not resolve (unknown, specified but unimplemented,
    /// or renamed) and return true: the import binds nothing, since the items
    /// that resolve are globals already in scope. A pipeline that drops the
    /// resolving items first leaves only the bad ones; one that does not
    /// (a package's modules) still carries them, and they pass.
    fn report_library_global_items(&mut self, imp: &ImportDecl) -> bool {
        if crate::prelude::library_global_module(&imp.path).is_none() {
            return false;
        }
        // Every item a prelude name: one deletion of the whole import, on
        // the first item's error, and the note on each.
        let mut prelude = Self::prelude_import_note(imp);
        for it in &imp.items {
            if let Some(mut message) =
                crate::prelude::library_module_item_error(&imp.path, &it.name, it.alias.as_deref())
            {
                let mut replacement = None;
                if let Some((note, edit)) = prelude.take() {
                    message = format!("`{}` has no item `{}`{note}", imp.path.join("."), it.name);
                    replacement = Some(edit);
                }
                self.errors.push(ResolveError {
                    message,
                    span: it.span,
                    kind: ResolveErrorKind::UnknownItemInModule,
                    suggestion: None,
                    replacement,
                    stub_hint: None,
                });
            }
        }
        true
    }

    /// `import std.sync;` names a module of globals: there is no module
    /// value to bind, so say how to import from it, and return true.
    fn report_bare_library_global_import(&mut self, imp: &ImportDecl) -> bool {
        for it in &imp.items {
            let mut full = imp.path.clone();
            full.push(it.name.clone());
            if crate::prelude::library_global_module(&full).is_some()
                && !crate::prelude::is_importable_stdlib_module(&full)
            {
                self.errors.push(ResolveError {
                    message: format!(
                        "`{m}` is imported item by item: `import {m}.{{..}};`",
                        m = full.join(".")
                    ),
                    span: imp.span,
                    kind: ResolveErrorKind::UnknownModule,
                    suggestion: None,
                    replacement: None,
                    stub_hint: None,
                });
                return true;
            }
        }
        false
    }

    /// B-2026-11-17-38: a package imports only the packages its own
    /// manifest declares (design.md § The package name in paths: the first
    /// segment is matched against the dependencies "as listed in
    /// kara.toml"). Every resolved package is in the tree, so without this
    /// a package reached only through another resolved as if declared.
    fn report_undeclared_dependency(
        &mut self,
        tree: &crate::module::ProgramTree,
        imp: &ImportDecl,
    ) {
        let Some(first) = imp
            .path
            .first()
            .or_else(|| imp.items.first().map(|it| &it.name))
        else {
            return;
        };
        let Some(pkg) = tree.dependency_packages.get(first) else {
            return;
        };
        let importer = self
            .current_module
            .and_then(|id| tree.modules.get(id))
            .and_then(|m| m.package.clone());
        let Some(scope) = tree.dependency_scope.get(&importer) else {
            return;
        };
        if scope.contains(first) {
            return;
        }
        let manifest = match &importer {
            None => "kara.toml".to_string(),
            Some(p) => format!("`{p}`'s kara.toml"),
        };
        let mut through: Vec<&str> = tree
            .dependency_scope
            .iter()
            .filter(|(k, v)| k.is_some() && *k != &importer && v.contains(first))
            .filter_map(|(k, _)| k.as_deref())
            .filter(|k| *k != pkg.as_str())
            .collect();
        through.sort_unstable();
        let mut message = format!(
            "unknown module `{first}`: `{pkg}` is not a dependency of this package, so it \
             cannot be imported here; add `{pkg}` to `[dependencies]` in {manifest}"
        );
        if !through.is_empty() {
            message.push_str(&format!(
                " (it is a dependency of {}, which does not make it this package's)",
                through
                    .iter()
                    .map(|t| format!("`{t}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        self.errors.push(ResolveError {
            message,
            span: imp.span,
            kind: ResolveErrorKind::UnknownModule,
            suggestion: None,
            replacement: None,
            stub_hint: None,
        });
    }

    fn collect_import(&mut self, imp: &ImportDecl) {
        let Some(tree) = self.tree else {
            // Single-file mode — cross-module validation is skipped (the file
            // may be one translation unit of a larger build the single-file
            // resolver can't see), so *user* imports are trusted and bound
            // as-is. But `std` is the COMPILER'S OWN namespace, fully known at
            // compile time regardless of build shape, so a std-rooted import
            // that names no baked stdlib module is definitively bogus. Left
            // unreported it bound a dead alias (`import std.math;` → `math`)
            // that then ICE'd both backends on use ("variable 'math' not found
            // ... should be caught by resolver"). Valid gated imports are
            // spliced + removed by `expand_gated_stdlib_imports` BEFORE
            // resolution, so any std-rooted import still present here is a typo
            // / nonexistent module. Report it (mirroring the tree-mode
            // `UnknownModule`), then still bind the names below so downstream
            // passes don't cascade `UndefinedName` (B-2026-07-18-25).
            self.report_bogus_single_file_std_import(imp);
            self.report_lone_file_user_import(imp);
            if imp.is_wildcard {
                // No tree, so there is nothing to enumerate — the names this
                // wildcard would have bound stay unbound. Record that so the
                // undefined-name channel can stay quiet rather than accuse a
                // valid program (see `wildcard_hides_unresolved_names`).
                self.unexpanded_wildcard_import = true;
            }
            for item in &imp.items {
                let bound = item.alias.clone().unwrap_or_else(|| item.name.clone());
                let mut full = imp.path.clone();
                full.push(item.name.clone());
                if let Err(e) = self.table.define(
                    bound,
                    SymbolKind::Import { path: full },
                    item.span,
                    imp.is_pub,
                ) {
                    self.errors.push(e);
                }
            }
            return;
        };

        // A library module of globals is no module of the package's graph
        // (std.sync, std.net, std.json, ...): its import resolves, or is
        // refused item by item, exactly as in a lone file (B-2026-11-05-7).
        // A module that is also gated (std.time) has a synthetic module in
        // the graph and resolves below.
        if !tree.graph.by_path.contains_key(&imp.path)
            && (self.report_library_global_items(imp)
                || self.report_bare_library_global_import(imp))
        {
            return;
        }

        self.report_undeclared_dependency(tree, imp);

        // Validate the dotted prefix exists. A prefix that is only a
        // directory (`db/` with no `db.kara`) is a valid one when every name
        // the import binds is a module under it: `import db.connection;`
        // binds the module `connection` (design.md § Imports,
        // B-2026-11-04-21).
        let only_submodules = !imp.is_wildcard
            && !imp.items.is_empty()
            && imp.items.iter().all(|item| {
                let mut full = imp.path.clone();
                full.push(item.name.clone());
                tree.graph.by_path.contains_key(&full)
            });
        if !tree.graph.by_path.contains_key(&imp.path) && !imp.path.is_empty() && !only_submodules {
            let wanted = imp.path.join(".");
            let all_paths: Vec<String> = tree
                .graph
                .by_path
                .keys()
                .map(|p| {
                    if p.is_empty() {
                        "<crate>".to_string()
                    } else {
                        p.join(".")
                    }
                })
                .collect();
            let candidates: Vec<&str> = all_paths.iter().map(|s| s.as_str()).collect();
            // Only other platforms' files define it (design.md § 4, Missing
            // platforms): say which, and that this one has none.
            let elsewhere = tree.elsewhere.get(&imp.path);
            let suggestion = match elsewhere {
                Some(_) => None,
                None => suggest_similar(&wanted, &candidates),
            };
            let mut message = format!("unknown module `{wanted}`");
            if let Some(on) = elsewhere {
                let files: Vec<String> = on
                    .iter()
                    .map(|p| format!("`{}_{p}.kara`", imp.path.join("/")))
                    .collect();
                message.push_str(&format!(
                    ": it is defined only for {} ({}), and no file defines it for this \
                     platform; add a shared `{}.kara` or a file for this platform",
                    on.join(", "),
                    files.join(", "),
                    imp.path.join("/")
                ));
            }
            if let Some(ref s) = suggestion {
                message.push_str(&format!(", did you mean `{s}`?"));
            }
            // The replacement covers exactly the dotted prefix tokens in
            // source — `imp.path_spans` is the per-segment span vector
            // from the parser, paired with `imp.path`. A non-empty
            // `imp.path` (guarded above) therefore implies a non-empty
            // `path_spans`, and the contiguous span runs from the first
            // segment's offset to the last segment's end.
            let replacement = suggestion.as_ref().and_then(|s| {
                let first = imp.path_spans.first()?;
                let last = imp.path_spans.last()?;
                let offset = first.offset;
                let length = (last.offset + last.length).saturating_sub(offset);
                Some(Box::new(TextEdit {
                    offset,
                    length,
                    replacement: s.clone(),
                }))
            });
            self.errors.push(ResolveError {
                message,
                span: imp.span,
                kind: ResolveErrorKind::UnknownModule,
                suggestion,
                replacement,
                stub_hint: None,
            });
            // Still register names locally so downstream passes do not
            // compound with cascading UndefinedName errors.
            for item in &imp.items {
                let bound = item.alias.clone().unwrap_or_else(|| item.name.clone());
                let mut full = imp.path.clone();
                full.push(item.name.clone());
                if let Err(e) = self.table.define(
                    bound,
                    SymbolKind::Import { path: full },
                    item.span,
                    imp.is_pub,
                ) {
                    self.errors.push(e);
                }
            }
            return;
        }

        // Prefix exists. Look up each brace-listed item.
        let importer_path = self
            .current_module
            .map(|id| tree.module(id).path.clone())
            .unwrap_or_default();
        for item in &imp.items {
            let mut full = imp.path.clone();
            full.push(item.name.clone());

            // A module of Kāra items that also has globals (`std.time.{Clock}`):
            // a global is in scope already and binds nothing, as in a lone file,
            // whose pipeline drops it before resolution (B-2026-11-05-7).
            if let Some((have, _)) = crate::prelude::library_global_module(&imp.path) {
                if have.contains(&item.name.as_str()) {
                    if let Some(message) = crate::prelude::library_module_item_error(
                        &imp.path,
                        &item.name,
                        item.alias.as_deref(),
                    ) {
                        self.errors.push(ResolveError {
                            message,
                            span: item.span,
                            kind: ResolveErrorKind::UnknownItemInModule,
                            suggestion: None,
                            replacement: None,
                            stub_hint: None,
                        });
                    }
                    continue;
                }
            }

            let binds_submodule = tree.graph.by_path.contains_key(&full);
            let binds_item = !binds_submodule && module_exposes_name(tree, &imp.path, &item.name);
            // A package checked against the Kāra library imports the
            // library's items as a lone file does (B-2026-10-24-43).
            let binds_library = !binds_submodule
                && !binds_item
                && imp.path.first().is_some_and(|s| s == "std")
                && self.library_declares(&item.name);

            if !binds_submodule && !binds_item && !binds_library {
                // Phase-10 `#[target(...)]`: the item exists in source but
                // was filtered for the current compilation target — answer
                // with the targeted diagnostic instead of unknown-item +
                // available-list.
                if let Some(spec) = self.target_tombstones.get(&item.name) {
                    self.errors.push(ResolveError {
                        message: format!(
                            "'{}' is not available on target `{}` — it is gated to \
                             `#[target({})]`",
                            item.name,
                            crate::target::active_target(),
                            spec,
                        ),
                        span: item.span,
                        kind: ResolveErrorKind::UndefinedName,
                        suggestion: None,
                        replacement: None,
                        stub_hint: None,
                    });
                    continue;
                }
                // Look at target module's top-level items for suggestions,
                // plus any submodule siblings.
                // Only names this importer could import: across packages a
                // name that is not `pub` is E0111, so it is not "available"
                // (B-2026-11-08-35); neither is a `private` one out of its
                // directory.
                let importer_pkg = self
                    .current_module
                    .and_then(|id| tree.module(id).package.clone());
                let importable = |name: &str| {
                    let Some(vis) = module_item_visibility(tree, &imp.path, name) else {
                        return true;
                    };
                    let (def_path, _) = module::canonical_origin(tree, &imp.path, name)
                        .unwrap_or_else(|| (imp.path.clone(), name.to_string()));
                    if !visibility_allows_access(vis, &def_path, &importer_path) {
                        return false;
                    }
                    let def_pkg = tree
                        .graph
                        .lookup(&def_path)
                        .and_then(|id| tree.module(id).package.clone());
                    vis == Visibility::Pub || def_pkg == importer_pkg
                };
                let mut candidates_owned: Vec<String> = module_top_level_names(tree, &imp.path)
                    .into_iter()
                    .filter(|n| importable(n))
                    .collect();
                // B-2026-08-14-34 leg A — `by_path` is a `HashMap`, and
                // `suggest_similar` breaks an equal-distance tie on candidate
                // order, so two sibling submodules equidistant from a typo would
                // be separated by per-process hash order. Sorted for the same
                // reason as the scope and impl producers; the top-level names
                // above keep their position ahead of the siblings.
                let mut siblings: Vec<String> = tree
                    .graph
                    .by_path
                    .keys()
                    .filter(|p| p.len() == imp.path.len() + 1 && p.starts_with(&imp.path))
                    .map(|p| p.last().cloned().unwrap_or_default())
                    .collect();
                siblings.sort_unstable();
                candidates_owned.extend(siblings);
                let candidates: Vec<&str> = candidates_owned.iter().map(|s| s.as_str()).collect();
                let mut suggestion = suggest_similar(&item.name, &candidates);

                let module_label = if imp.path.is_empty() {
                    "(package root)".to_string()
                } else {
                    imp.path.join(".")
                };
                let mut message =
                    format!("unknown item `{}` in module `{module_label}`", item.name);
                if crate::prelude::is_gated_stdlib_module(&imp.path) {
                    // A library module names the sibling that has the item
                    // (B-2026-10-21-7).
                    (message, suggestion) =
                        crate::prelude::gated_module_item_error(&imp.path, &item.name);
                } else if let Some(ref s) = suggestion {
                    message.push_str(&format!(", did you mean `{s}`?"));
                } else if candidates_owned.len() <= 10 && !candidates_owned.is_empty() {
                    // Design.md § Path resolution algorithm — for small
                    // modules, list the available exports.
                    message.push_str(&format!("; available: {}", candidates_owned.join(", ")));
                }
                let replacement = suggestion.as_ref().map(|s| {
                    Box::new(TextEdit {
                        offset: item.span.offset,
                        length: item.span.length,
                        replacement: s.clone(),
                    })
                });
                // B-2026-10-30-27: the fix renames the item; with no alias
                // it binds the corrected name, which a use may already say.
                if let (Some(_), Some(s), None) = (&replacement, &suggestion, &item.alias) {
                    self.failed_import_fixes
                        .insert((self.current_module, s.clone()), item.name.clone());
                }
                self.errors.push(ResolveError {
                    message,
                    span: item.span,
                    kind: ResolveErrorKind::UnknownItemInModule,
                    suggestion,
                    replacement,
                    stub_hint: None,
                });
            } else if binds_item {
                // Slice 6 + 7: enforce three-level visibility against the
                // canonical defining item. Following the `pub import` chain
                // ensures re-exports are transparent — E0111 fires based on
                // where the item is really defined, not the re-exporter's
                // location. Submodule bindings (`binds_submodule`) have no
                // item visibility to check.
                let (def_path, def_name) =
                    match module::canonical_origin(tree, &imp.path, &item.name) {
                        Some(p) => p,
                        None => (imp.path.clone(), item.name.clone()),
                    };
                if let Some(vis) = module_item_visibility(tree, &imp.path, &item.name) {
                    if !visibility_allows_access(vis, &def_path, &importer_path) {
                        let def_label = if def_path.is_empty() {
                            "(package root)".to_string()
                        } else {
                            def_path.join(".")
                        };
                        let message = format!(
                            "`{}` in module `{}` is `private` — visible only to files in the same directory",
                            def_name, def_label,
                        );
                        self.errors.push(ResolveError {
                            message,
                            span: item.span,
                            kind: ResolveErrorKind::PrivateItemAccess,
                            suggestion: Some(format!(
                                "mark `{}` as `pub` or move the caller into the same directory",
                                def_name
                            )),
                            replacement: None,
                            stub_hint: None,
                        });
                    } else if vis != Visibility::Pub {
                        // Cross-package module loading (phase-5 line 898):
                        // `Default` visibility is package-internal. When the
                        // canonical defining module belongs to a different
                        // package than the importer, only `pub` items are
                        // importable — design.md § Package name in paths.
                        let importer_pkg = self
                            .current_module
                            .and_then(|id| tree.module(id).package.as_deref());
                        let def_pkg = tree
                            .graph
                            .lookup(&def_path)
                            .and_then(|id| tree.module(id).package.as_deref());
                        if importer_pkg != def_pkg {
                            let def_label = def_pkg.unwrap_or("<root package>");
                            self.errors.push(ResolveError {
                                message: format!(
                                    "`{}` in package `{}` is not `pub` — only `pub` items can be imported across packages",
                                    def_name, def_label,
                                ),
                                span: item.span,
                                kind: ResolveErrorKind::PrivateItemAccess,
                                suggestion: Some(format!(
                                    "mark `{}` as `pub` in package `{}`",
                                    def_name, def_label,
                                )),
                                replacement: None,
                                stub_hint: None,
                            });
                        }
                    }
                }
            }

            // Slice 7: the bound symbol records the *canonical* path so that
            // re-exports are transparent to downstream phases — method
            // resolution, trait coherence, and typechecker cross-module
            // lookups all see a single identity regardless of which alias
            // the name travelled through.
            let canonical_full = if binds_item {
                module::canonical_origin(tree, &imp.path, &item.name)
                    .map(|(mut path, name)| {
                        path.push(name);
                        path
                    })
                    .unwrap_or_else(|| full.clone())
            } else {
                full.clone()
            };
            let bound = item.alias.clone().unwrap_or_else(|| item.name.clone());
            match self.table.define(
                bound,
                SymbolKind::Import {
                    path: canonical_full.clone(),
                },
                item.span,
                imp.is_pub,
            ) {
                Ok(import_id) => {
                    // If the imported item is an enum, also bring its variant
                    // names into the importer's scope — exactly as `collect_enum`
                    // does for a locally-defined enum. Without this, an imported
                    // enum's *unqualified* variant patterns and constructors
                    // (`B(x) =>`, `C { y } =>`, `B(3)`) fail name resolution
                    // while the identical code against a *local* enum resolves,
                    // because only the enum's type name is imported, not its
                    // variants. That asymmetry was surfaced by the self-hosting
                    // lexer's cross-module split. Submodule bindings have no
                    // variants to register.
                    if binds_item {
                        self.register_imported_enum_variants(tree, &canonical_full, import_id);
                    }
                }
                Err(e) => self.errors.push(e),
            }
        }
    }

    /// Register the variants of an imported enum into the current scope,
    /// mirroring [`Self::collect_enum`] for locally-defined enums. The enum's
    /// `EnumDef` is located in its defining module via the program tree, keyed
    /// off the canonical `[module.., EnumName]` import path. A no-op when the
    /// path doesn't name an enum (structs, functions, type aliases, …). Name
    /// collisions (another enum's variant, a local definition) are tolerated
    /// the same way `collect_enum` tolerates them — the variant simply isn't
    /// rebound, and the user must qualify it as `Enum.Variant`.
    fn register_imported_enum_variants(
        &mut self,
        tree: &module::ProgramTree,
        canonical_full: &[String],
        parent_enum: SymbolId,
    ) {
        let Some((enum_name, mod_path)) = canonical_full.split_last() else {
            return;
        };
        let Some(module_id) = tree.graph.by_path.get(mod_path).copied() else {
            return;
        };
        let module = tree.module(module_id);
        let Some(enum_def) = module.items.iter().find_map(|it| match it {
            Item::EnumDef(e) if &e.name == enum_name => Some(e),
            _ => None,
        }) else {
            return;
        };
        for variant in &enum_def.variants {
            let variant_kind = match &variant.kind {
                VariantKind::Unit => VariantSymbolKind::Unit,
                VariantKind::Tuple(types) => VariantSymbolKind::Tuple(types.len()),
                VariantKind::Struct(fields) => {
                    VariantSymbolKind::Struct(fields.iter().map(|f| f.name.clone()).collect())
                }
            };
            // Collision → leave the existing binding in place (qualify to
            // disambiguate). Mirrors collect_enum's `if let Ok` tolerance.
            let _ = self.table.define(
                variant.name.clone(),
                SymbolKind::EnumVariant {
                    parent_enum,
                    variant_kind,
                },
                variant.span,
                enum_def.is_pub,
            );
        }
    }

    fn collect_extern_function(&mut self, e: &ExternFunction) {
        // Codegen hints have no Kāra-side body to attach to on a foreign
        // import — reject them with the extern-specific diagnostic.
        self.reject_codegen_hint_on_extern(&e.attributes);
        if let Err(err) =
            self.table
                .define(e.name.clone(), SymbolKind::ExternFunction, e.span, true)
        {
            self.errors.push(err);
        }
    }

    fn collect_opaque_foreign_type(&mut self, o: &OpaqueTypeDecl) {
        // A redeclared opaque type is a warning, and the first declaration
        // stands (design.md §15, B-2026-11-16-239).
        let scope = self.table.current_scope_id();
        if let Some(first) = self
            .table
            .lookup_in_scope(scope, &o.name)
            .filter(|s| matches!(s.kind, SymbolKind::OpaqueForeignType))
        {
            let message = format!(
                "opaque foreign type '{}' is already declared at {}:{}; both declarations \
                 must name the same C type",
                o.name, first.span.line, first.span.column
            );
            self.errors.push(ResolveError {
                message,
                span: o.span,
                kind: ResolveErrorKind::OpaqueTypeRedeclared,
                suggestion: None,
                replacement: None,
                stub_hint: None,
            });
            return;
        }
        if let Err(err) =
            self.table
                .define(o.name.clone(), SymbolKind::OpaqueForeignType, o.span, true)
        {
            self.errors.push(err);
        }
    }

    /// Extract a simple name from a type expression (for impl block target types).
    pub(crate) fn type_expr_name(&self, ty: &TypeExpr) -> Option<String> {
        match &ty.kind {
            TypeKind::Path(path) => path.segments.last().cloned(),
            _ => None,
        }
    }

    /// Whether `name` is declared as an associated function on at least one
    /// visible trait. Used to suppress the undefined-name error for bare
    /// identifier callees that the typechecker can dispatch via expected-type
    /// inference (e.g. `let x: T = default()` where `T: Default`).
    pub(crate) fn is_trait_assoc_fn_name(&self, name: &str) -> bool {
        for item in &self.program.items {
            if let Item::TraitDef(t) = item {
                for ti in &t.items {
                    if let TraitItem::Method(m) = ti {
                        if m.name == name && m.self_param.is_none() {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }
}
