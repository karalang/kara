//! Pass 1: top-level declaration collection.
//!
//! Walks the program's items once, registers every declaration with
//! the symbol table, validates layouts, and assembles the
//! cross-module use / import tables. Methods are kept thin —
//! each item form has its own dedicated `collect_<kind>` that
//! pushes its symbol(s) onto `table` and defers body resolution
//! to Pass 2.
//!
//! Houses `collect_top_level_items` (the dispatcher) + every
//! per-item-form helper, the `validate_layouts` / `validate_layout`
//! pair, the `check_compiler_builtin_attr` gate, the two name
//! helpers `type_expr_name` / `is_trait_assoc_fn_name`, and the
//! big `collect_import` body that drives cross-module visibility
//! resolution against `ProgramTree`.
//!
//! Lives in a sibling `impl<'a> super::Resolver<'a>` block.

use std::collections::HashSet;

use crate::ast::*;
use crate::edit_distance::suggest_similar;
use crate::module::{self};

use super::{
    module_exposes_name, module_item_visibility, module_top_level_names, visibility_allows_access,
    ResolveError, ResolveErrorKind, ScopeId, Symbol, SymbolId, SymbolKind, TextEdit,
    VariantSymbolKind,
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
                Item::UseDecl(u) => self.collect_use(u),
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
    }

    pub(crate) fn validate_layouts(&mut self) {
        let items: Vec<Item> = self.program.items.clone();
        for item in &items {
            if let Item::LayoutDef(layout) = item {
                self.validate_layout(layout);
            }
        }
    }

    fn validate_layout(&mut self, layout: &LayoutDef) {
        // Extract the element type name from the collection type.
        // e.g., Vec[Entity] → "Entity", Array[Entity, 100] → "Entity"
        let struct_name = match &layout.collection_type.kind {
            TypeKind::Path(path) => {
                let coll_name = path.segments.first().map(|s| s.as_str()).unwrap_or("");
                if coll_name != "Vec" && coll_name != "Array" {
                    self.errors.push(ResolveError {
                        message: format!(
                            "layout collection type must be Vec[T] or Array[T, N], found '{}'",
                            coll_name
                        ),
                        span: layout.span.clone(),
                        kind: ResolveErrorKind::UndefinedType,
                        suggestion: None,
                        replacement: None,
                        stub_hint: None,
                    });
                    return;
                }
                // Extract the element type from generic args.
                match &path.generic_args {
                    Some(args) if !args.is_empty() => {
                        if let GenericArg::Type(te) = &args[0] {
                            if let TypeKind::Path(inner) = &te.kind {
                                inner.segments.first().cloned()
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            }
            _ => None,
        };

        let struct_name = match struct_name {
            Some(n) => n,
            None => {
                self.errors.push(ResolveError {
                    message:
                        "layout collection type must specify an element type (e.g., Vec[Entity])"
                            .to_string(),
                    span: layout.span.clone(),
                    kind: ResolveErrorKind::UndefinedType,
                    suggestion: None,
                    replacement: None,
                    stub_hint: None,
                });
                return;
            }
        };

        // Look up the struct's field names.
        let struct_fields: Vec<String> = if let Some(sym) =
            self.table.lookup_in_scope(ScopeId(0), &struct_name)
        {
            if let SymbolKind::Struct { field_names } = &sym.kind {
                field_names.clone()
            } else if let SymbolKind::Enum { .. } = &sym.kind {
                // Validate split_by_variant for enums.
                for item in &layout.items {
                    if let LayoutItem::Group { name, span, .. } = item {
                        self.errors.push(ResolveError {
                                message: format!(
                                    "layout group '{}' is not allowed for enum types; use split_by_variant",
                                    name
                                ),
                                span: span.clone(),
                                kind: ResolveErrorKind::UndefinedField,
                                suggestion: Some("use split_by_variant instead of group".to_string()),
                                replacement: None,
                stub_hint: None,
                            });
                    }
                }
                return;
            } else if let SymbolKind::Union { .. } = &sym.kind {
                // Phase 5 line 569 slice 4: FFI unions reject any
                // layout-block syntax. A union's bytes are a single
                // alternation slot — there's no field-set to SoA-split,
                // grouping doesn't change the C-side ABI we're locked
                // to, and the cache-locality reasoning that motivates
                // layout blocks doesn't apply to a one-cell aggregate.
                // Fires per offending layout item so the user sees
                // every site they need to remove, not just the first.
                for item in &layout.items {
                    let (name, span) = match item {
                        LayoutItem::Group { name, span, .. } => (name.clone(), span.clone()),
                        LayoutItem::Cold { span, .. } => ("cold".to_string(), span.clone()),
                        LayoutItem::SplitByVariant(span) => {
                            ("split_by_variant".to_string(), span.clone())
                        }
                    };
                    self.errors.push(ResolveError {
                        message: format!(
                            "layout block on union '{}' is not allowed — unions are an FFI alternation, not a struct shape with a field-set to lay out",
                            struct_name
                        ),
                        span,
                        kind: ResolveErrorKind::UndefinedField,
                        suggestion: Some(format!(
                            "remove the '{}' layout item; FFI unions inherit their layout from the C side",
                            name
                        )),
                        replacement: None,
                stub_hint: None,
                    });
                }
                return;
            } else {
                self.errors.push(ResolveError {
                    message: format!("'{}' is not a struct", struct_name),
                    span: layout.span.clone(),
                    kind: ResolveErrorKind::UndefinedType,
                    suggestion: None,
                    replacement: None,
                    stub_hint: None,
                });
                return;
            }
        } else {
            self.errors.push(ResolveError {
                message: format!("undefined struct '{}' in layout definition", struct_name),
                span: layout.span.clone(),
                kind: ResolveErrorKind::UndefinedType,
                suggestion: None,
                replacement: None,
                stub_hint: None,
            });
            return;
        };

        // Build a lookup of field-name → field type expression by
        // scanning the program's items for the matching StructDef.
        // Used to reject heap-owning field types in any group / cold
        // section — SoA push, materialize, and per-element drop all
        // need a coordinated move/clone/borrow story for heap-owning
        // fields (currently unresolved); rejecting at the layout site
        // produces a focused diagnostic instead of silent leak or
        // double-free further downstream. See `phase-7-codegen.md`
        // § *SoA drop semantics > Per-element destructor calls for
        // heap-bearing element fields* for the open design issue.
        let mut field_types: std::collections::HashMap<&str, &TypeExpr> =
            std::collections::HashMap::new();
        for item in &self.program.items {
            if let Item::StructDef(s) = item {
                if s.name == struct_name {
                    for f in &s.fields {
                        field_types.insert(f.name.as_str(), &f.ty);
                    }
                    break;
                }
            }
        }

        // Validate layout items: field existence, uniqueness, cold constraints, align(N).
        let mut assigned: HashSet<String> = HashSet::new();
        let mut cold_count = 0usize;
        for item in &layout.items {
            match item {
                LayoutItem::Group {
                    name,
                    fields,
                    align,
                    span,
                } => {
                    for field in fields {
                        if !struct_fields.contains(field) {
                            self.errors.push(ResolveError {
                                message: format!(
                                    "field '{}' does not exist on struct '{}' (in group '{}')",
                                    field, struct_name, name
                                ),
                                span: span.clone(),
                                kind: ResolveErrorKind::UndefinedField,
                                suggestion: None,
                                replacement: None,
                                stub_hint: None,
                            });
                        } else if !assigned.insert(field.clone()) {
                            self.errors.push(ResolveError {
                                message: format!(
                                    "field '{}' appears in multiple sections in layout '{}'",
                                    field, layout.name
                                ),
                                span: span.clone(),
                                kind: ResolveErrorKind::DuplicateDefinition,
                                suggestion: None,
                                replacement: None,
                                stub_hint: None,
                            });
                        } else if let Some(ty) = field_types.get(field.as_str()) {
                            if let Some(why) = self.layout_field_heap_reason(ty) {
                                self.errors.push(ResolveError {
                                    message: format!(
                                        "layout '{}' group '{}': field '{}' has heap-owning type ({}), \
                                         which is not yet supported in SoA layouts",
                                        layout.name, name, field, why
                                    ),
                                    span: span.clone(),
                                    kind: ResolveErrorKind::UndefinedField,
                                    suggestion: Some(
                                        "move the field outside the layout block (it will fall back to AoS) \
                                         or store it via a separate Vec; SoA push / materialize / drop for \
                                         heap-owning fields is tracked under 'SoA drop semantics' in the \
                                         phase-7 codegen tracker"
                                            .to_string(),
                                    ),
                                    replacement: None,
                                    stub_hint: None,
                                });
                            }
                        }
                    }
                    if let Some(n) = align {
                        if *n == 0 || (*n & (*n - 1)) != 0 {
                            self.errors.push(ResolveError {
                                message: format!(
                                    "align({}) is not a power of two in layout '{}' group '{}'",
                                    n, layout.name, name
                                ),
                                span: span.clone(),
                                kind: ResolveErrorKind::UndefinedField,
                                suggestion: Some(
                                    "common values: 8, 16, 32, 64 (cache line), 128 (Apple Silicon cache line)".to_string(),
                                ),
                                replacement: None,
                stub_hint: None,
                            });
                        }
                    }
                }
                LayoutItem::Cold { fields, span } => {
                    cold_count += 1;
                    if cold_count > 1 {
                        self.errors.push(ResolveError {
                            message: format!(
                                "layout '{}' has more than one cold section; at most one is allowed",
                                layout.name
                            ),
                            span: span.clone(),
                            kind: ResolveErrorKind::DuplicateDefinition,
                            suggestion: None,
                            replacement: None,
                stub_hint: None,
                        });
                    }
                    for field in fields {
                        if !struct_fields.contains(field) {
                            self.errors.push(ResolveError {
                                message: format!(
                                    "field '{}' does not exist on struct '{}' (in cold section)",
                                    field, struct_name
                                ),
                                span: span.clone(),
                                kind: ResolveErrorKind::UndefinedField,
                                suggestion: None,
                                replacement: None,
                                stub_hint: None,
                            });
                        } else if !assigned.insert(field.clone()) {
                            self.errors.push(ResolveError {
                                message: format!(
                                    "field '{}' appears in multiple sections in layout '{}'",
                                    field, layout.name
                                ),
                                span: span.clone(),
                                kind: ResolveErrorKind::DuplicateDefinition,
                                suggestion: None,
                                replacement: None,
                                stub_hint: None,
                            });
                        } else if let Some(ty) = field_types.get(field.as_str()) {
                            if let Some(why) = self.layout_field_heap_reason(ty) {
                                self.errors.push(ResolveError {
                                    message: format!(
                                        "layout '{}' cold section: field '{}' has heap-owning type ({}), \
                                         which is not yet supported in SoA layouts",
                                        layout.name, field, why
                                    ),
                                    span: span.clone(),
                                    kind: ResolveErrorKind::UndefinedField,
                                    suggestion: Some(
                                        "move the field outside the layout block (it will fall back to AoS) \
                                         or store it via a separate Vec; SoA push / materialize / drop for \
                                         heap-owning fields is tracked under 'SoA drop semantics' in the \
                                         phase-7 codegen tracker"
                                            .to_string(),
                                    ),
                                    replacement: None,
                                    stub_hint: None,
                                });
                            }
                        }
                    }
                }
                LayoutItem::SplitByVariant(span) => {
                    self.errors.push(ResolveError {
                        message: "split_by_variant is only valid for enum layout blocks"
                            .to_string(),
                        span: span.clone(),
                        kind: ResolveErrorKind::UndefinedField,
                        suggestion: None,
                        replacement: None,
                        stub_hint: None,
                    });
                }
            }
        }

        // Warn about unassigned fields (fields not in any group or cold section).
        let unassigned: Vec<&String> = struct_fields
            .iter()
            .filter(|f| !assigned.contains(*f))
            .collect();
        if !unassigned.is_empty() {
            // TODO: Implement proper warning severity and #[allow(layout_unassigned_fields)].
            let field_list = unassigned
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            self.errors.push(ResolveError {
                message: format!(
                    "layout '{}': fields not assigned to any group or cold section: {}. These will be placed in an implicit trailing hot group.",
                    layout.name, field_list
                ),
                span: layout.span.clone(),
                kind: ResolveErrorKind::UndefinedField,
                suggestion: Some("assign all fields to groups, or suppress with #[allow(layout_unassigned_fields)]".to_string()),
                replacement: None,
                stub_hint: None,
            });
        }
    }

    /// Return a human-readable description of why a layout-group field's
    /// type is heap-owning, or `None` if the type is safe (primitive,
    /// inline struct of primitives, etc.) for SoA storage.
    ///
    /// Drives the diagnostic surface emitted by `validate_layout`: SoA
    /// push currently `memcpy`s field bits into per-group buffers, the
    /// read-side `compile_soa_index_read` (`src/codegen/collections.rs`)
    /// memcpys them back, and per-element drop for heap-owning fields
    /// is not implemented. Allowing a `String` / `Vec` / `Map` / `Set`
    /// field in a group today silently produces either a leak (if push
    /// suppression fires) or a double-free (if it doesn't), neither of
    /// which has a clean diagnostic at the use site. Rejecting at
    /// layout-validation time gives the user one focused error pointing
    /// at the tracker entry where the design question is being worked.
    ///
    /// The check is syntactic — it looks at the `TypeExpr` shape, not
    /// the resolved `Type`. That keeps it fast (no typecheck dependency)
    /// and is sound for the names it recognizes: `Vec` / `String` /
    /// `Map` / `Set` / `VecDeque` / `TreeMap` / `SortedSet` are
    /// reserved type names in the prelude (never user-overridable), so
    /// a path whose first segment is one of those *always* refers to
    /// the heap-owning stdlib type. Tuples / Arrays recurse so e.g.
    /// `(i64, String)` and `Array[String, 4]` also get flagged.
    /// `shared struct` / `shared enum` field types are flagged via a
    /// `program.items` lookup against the AST.
    fn layout_field_heap_reason(&self, ty: &TypeExpr) -> Option<String> {
        match &ty.kind {
            TypeKind::Path(p) => {
                let seg = p.segments.first()?.as_str();
                match seg {
                    "String" => Some("String".to_string()),
                    "Vec" => Some("Vec[…]".to_string()),
                    "Map" => Some("Map[…, …]".to_string()),
                    "Set" => Some("Set[…]".to_string()),
                    "VecDeque" => Some("VecDeque[…]".to_string()),
                    "TreeMap" => Some("TreeMap[…, …]".to_string()),
                    "SortedSet" => Some("SortedSet[…]".to_string()),
                    _ => {
                        for item in &self.program.items {
                            match item {
                                Item::StructDef(s) if s.name == seg && s.is_shared => {
                                    return Some(format!("shared struct {seg}"));
                                }
                                Item::EnumDef(e) if e.name == seg && e.is_shared => {
                                    return Some(format!("shared enum {seg}"));
                                }
                                _ => {}
                            }
                        }
                        None
                    }
                }
            }
            TypeKind::Tuple(elems) => {
                for el in elems {
                    if let Some(reason) = self.layout_field_heap_reason(el) {
                        return Some(format!("tuple containing {reason}"));
                    }
                }
                None
            }
            TypeKind::Array { element, .. } => self
                .layout_field_heap_reason(element)
                .map(|r| format!("Array of {r}")),
            _ => None,
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
                    span: attr.span.clone(),
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
                              is post-v1; only item-level deprecation is \
                              supported. Use-site detection for field \
                              reads/writes is bundled with the post-v1 \
                              lint expansion."
                        .to_string(),
                    span: attr.span.clone(),
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
                         — see design.md § Error Handling > \"Stdlib \
                         panic-emitters report the caller's source location\"",
                    ),
                    span: attr.span.clone(),
                    kind: ResolveErrorKind::TrackCallerInvalidTarget,
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
                    span: attr.span.clone(),
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
                         `fn {fn_name}` — the v1 profiles are {known}",
                        fn_name = f.name,
                        known = KNOWN.join(", "),
                    ),
                    span: f.span.clone(),
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
                    span: attr.span.clone(),
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
            if attr.is_bare("compiler_builtin") {
                self.errors.push(ResolveError {
                    message: "`#[compiler_builtin]` is reserved for stdlib source baked into the compiler binary"
                        .to_string(),
                    span: attr.span.clone(),
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
            f.span.clone(),
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
        self.reject_profile_attr(&s.attributes, "struct");
        // Field-level `#[non_exhaustive]` is post-v1 (Rust accepts it
        // on fields too; we ship type-level only). Reject so users get
        // a focused message instead of a silent acceptance that does
        // nothing — the attribute presence on a field would otherwise
        // be ignored, which is worse than the diagnostic.
        for field in &s.fields {
            self.reject_non_exhaustive_attr(&field.attributes, "struct field");
            self.reject_track_caller_attr(&field.attributes, "struct field");
            self.reject_profile_attr(&field.attributes, "struct field");
            self.reject_deprecated_on_field(&field.attributes);
        }
        let field_names: Vec<String> = s.fields.iter().map(|f| f.name.clone()).collect();
        match self.table.define(
            s.name.clone(),
            SymbolKind::Struct { field_names },
            s.span.clone(),
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
                    span: attr.span.clone(),
                    kind: ResolveErrorKind::UnionNonExhaustiveForbidden,
                    suggestion: None,
                    replacement: None,
                    stub_hint: None,
                });
            }
        }
        self.reject_track_caller_attr(&u.attributes, "union");
        self.reject_profile_attr(&u.attributes, "union");
        for field in &u.fields {
            self.reject_non_exhaustive_attr(&field.attributes, "union field");
            self.reject_track_caller_attr(&field.attributes, "union field");
            self.reject_profile_attr(&field.attributes, "union field");
            self.reject_deprecated_on_field(&field.attributes);
        }
        let field_names: Vec<String> = u.fields.iter().map(|f| f.name.clone()).collect();
        match self.table.define(
            u.name.clone(),
            SymbolKind::Union { field_names },
            u.span.clone(),
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
        self.reject_profile_attr(&e.attributes, "enum");
        // Variant-level attribute placement validation —
        // `#[track_caller]` and `#[non_exhaustive]` are rejected on
        // individual variants (the spec scopes both at type-level
        // only). `#[deprecated]` IS legal on variants per design.md
        // and so is not rejected here.
        for variant in &e.variants {
            self.reject_track_caller_attr(&variant.attributes, "enum variant");
            self.reject_profile_attr(&variant.attributes, "enum variant");
            self.reject_non_exhaustive_attr(&variant.attributes, "enum variant");
        }
        let variant_names: Vec<String> = e.variants.iter().map(|v| v.name.clone()).collect();
        let enum_id = match self.table.define(
            e.name.clone(),
            SymbolKind::Enum { variant_names },
            e.span.clone(),
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
                variant.span.clone(),
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
        self.reject_profile_attr(&t.attributes, "trait");
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
            t.span.clone(),
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
        self.reject_profile_attr(&t.attributes, "trait alias");
        match self.table.define(
            t.name.clone(),
            SymbolKind::TraitAlias,
            t.span.clone(),
            t.is_pub,
        ) {
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
        self.reject_profile_attr(&t.attributes, "marker trait");
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
            t.span.clone(),
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
        // is rejected (E0237). The other `collect_*` callers gate top-level
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
        self.reject_profile_attr(&imp.attributes, "impl block");
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
                    span: method.span.clone(),
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
                span: e.span.clone(),
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
            e.span.clone(),
            true, // effect resources are always accessible
        ) {
            self.errors.push(err);
        }
    }

    fn collect_effect_group(&mut self, e: &EffectGroupDecl) {
        if let Err(err) = self.table.define(
            e.name.clone(),
            SymbolKind::EffectGroup,
            e.span.clone(),
            e.is_pub,
        ) {
            self.errors.push(err);
        }
    }

    fn collect_effect_verb(&mut self, e: &EffectVerbDecl) {
        if let Err(err) = self.table.define(
            e.verb_name.clone(),
            SymbolKind::EffectVerb,
            e.span.clone(),
            true,
        ) {
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
        self.reject_profile_attr(&c.attributes, "module const");
        match self.table.define(
            c.name.clone(),
            SymbolKind::Constant,
            c.span.clone(),
            c.is_pub,
        ) {
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
        if actual != crate::lexer::IdentClass::Const {
            let suggestion = crate::lexer::suggest_const_name(&b.name);
            let actual_desc = match actual {
                crate::lexer::IdentClass::Type => "Type-class",
                crate::lexer::IdentClass::Value => "Value-class",
                crate::lexer::IdentClass::Const => unreachable!(),
            };
            self.errors.push(ResolveError {
                message: format!(
                    "error[E_MODULE_BINDING_NAMING]: module-level binding name \
                     `{}` is {actual_desc} but module-level `let` / `let mut` \
                     bindings introduce Const-class identifiers \
                     (SCREAMING_SNAKE_CASE); consider renaming to `{}`",
                    b.name, suggestion,
                ),
                span: b.span.clone(),
                kind: ResolveErrorKind::UndefinedName,
                suggestion: Some(format!("rename to `{}`", suggestion)),
                replacement: None,
                stub_hint: None,
            });
            // Continue and still attempt registration under the
            // offending name so use-site references don't double-up
            // with cascading "undefined name" diagnostics.
        }
        match self.table.define(
            b.name.clone(),
            SymbolKind::Constant,
            b.span.clone(),
            b.is_pub,
        ) {
            Ok(id) => {
                self.record_deprecation_if_present(id, &b.deprecation);
                self.record_unstable_if_present(id, &b.unstable);
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
        self.reject_profile_attr(&t.attributes, "type alias");
        match self.table.define(
            t.name.clone(),
            SymbolKind::TypeAlias,
            t.span.clone(),
            t.is_pub,
        ) {
            Ok(id) => {
                self.record_deprecation_if_present(id, &t.deprecation);
                self.record_unstable_if_present(id, &t.unstable);
            }
            Err(err) => self.errors.push(err),
        }
    }

    fn collect_distinct_type(&mut self, d: &crate::ast::DistinctTypeDef) {
        match self.table.define(
            d.name.clone(),
            SymbolKind::DistinctType,
            d.span.clone(),
            d.is_pub,
        ) {
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

    fn collect_use(&mut self, u: &UseDecl) {
        if let Some(last) = u.path.last() {
            if let Err(err) = self.table.define(
                last.clone(),
                SymbolKind::Import {
                    path: u.path.clone(),
                },
                u.span.clone(),
                u.is_pub,
            ) {
                self.errors.push(err);
            }
        }
    }

    /// Resolve a CR-24 `import` declaration against the `ProgramTree`:
    ///
    /// 1. Ensure the dotted prefix in `imp.path` names a module in the graph
    ///    (`E0224 UnknownModule` on miss, with a Levenshtein suggestion over
    ///    all known module paths).
    /// 2. For each brace-listed item, bind `alias.or(name)` in the current
    ///    scope as `SymbolKind::Import { path }`. If the `path + name` is
    ///    itself a submodule, the binding is a module reference; otherwise
    ///    the target module must expose a matching top-level item
    ///    (`E0225 UnknownItemInModule` on miss).
    ///
    /// Single-file mode (no tree attached) skips cross-module validation and
    /// just registers the symbol so downstream passes see the name in scope.
    /// This keeps `karac run file.kara` unchanged.
    fn collect_import(&mut self, imp: &ImportDecl) {
        let Some(tree) = self.tree else {
            // Single-file mode — bind without validation.
            for item in &imp.items {
                let bound = item.alias.clone().unwrap_or_else(|| item.name.clone());
                let mut full = imp.path.clone();
                full.push(item.name.clone());
                if let Err(e) = self.table.define(
                    bound,
                    SymbolKind::Import { path: full },
                    item.span.clone(),
                    imp.is_pub,
                ) {
                    self.errors.push(e);
                }
            }
            return;
        };

        // Validate the dotted prefix exists.
        if !tree.graph.by_path.contains_key(&imp.path) && !imp.path.is_empty() {
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
            let suggestion = suggest_similar(&wanted, &candidates);
            let mut message = format!("unknown module `{wanted}`");
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
                span: imp.span.clone(),
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
                    item.span.clone(),
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

            let binds_submodule = tree.graph.by_path.contains_key(&full);
            let binds_item = !binds_submodule && module_exposes_name(tree, &imp.path, &item.name);

            if !binds_submodule && !binds_item {
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
                        span: item.span.clone(),
                        kind: ResolveErrorKind::UndefinedName,
                        suggestion: None,
                        replacement: None,
                        stub_hint: None,
                    });
                    continue;
                }
                // Look at target module's top-level items for suggestions,
                // plus any submodule siblings.
                let mut candidates_owned: Vec<String> = module_top_level_names(tree, &imp.path);
                for p in tree.graph.by_path.keys() {
                    if p.len() == imp.path.len() + 1 && p.starts_with(&imp.path) {
                        candidates_owned.push(p.last().cloned().unwrap_or_default());
                    }
                }
                let candidates: Vec<&str> = candidates_owned.iter().map(|s| s.as_str()).collect();
                let suggestion = suggest_similar(&item.name, &candidates);

                let module_label = if imp.path.is_empty() {
                    "<crate root>".to_string()
                } else {
                    imp.path.join(".")
                };
                let mut message =
                    format!("unknown item `{}` in module `{module_label}`", item.name);
                if let Some(ref s) = suggestion {
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
                self.errors.push(ResolveError {
                    message,
                    span: item.span.clone(),
                    kind: ResolveErrorKind::UnknownItemInModule,
                    suggestion,
                    replacement,
                    stub_hint: None,
                });
            } else if binds_item {
                // Slice 6 + 7: enforce three-level visibility against the
                // canonical defining item. Following the `pub import` chain
                // ensures re-exports are transparent — E0222 fires based on
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
                            "<crate root>".to_string()
                        } else {
                            def_path.join(".")
                        };
                        let message = format!(
                            "`{}` in module `{}` is `private` — visible only to files in the same directory",
                            def_name, def_label,
                        );
                        self.errors.push(ResolveError {
                            message,
                            span: item.span.clone(),
                            kind: ResolveErrorKind::PrivateItemAccess,
                            suggestion: Some(format!(
                                "mark `{}` as `pub` or move the caller into the same directory",
                                def_name
                            )),
                            replacement: None,
                            stub_hint: None,
                        });
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
                item.span.clone(),
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
                variant.span.clone(),
                enum_def.is_pub,
            );
        }
    }

    fn collect_extern_function(&mut self, e: &ExternFunction) {
        if let Err(err) = self.table.define(
            e.name.clone(),
            SymbolKind::ExternFunction,
            e.span.clone(),
            true,
        ) {
            self.errors.push(err);
        }
    }

    fn collect_opaque_foreign_type(&mut self, o: &OpaqueTypeDecl) {
        if let Err(err) = self.table.define(
            o.name.clone(),
            SymbolKind::OpaqueForeignType,
            o.span.clone(),
            true,
        ) {
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
