//! Attribute checker — the central registry of recognised bare-name
//! attributes and compiler-reserved namespaces, plus a one-shot pass
//! that emits `error[E_UNKNOWN_ATTRIBUTE]` on every unrecognised
//! bare-name attribute found in a program.
//!
//! Slice 2 of the `#[diagnostic::*]` attribute namespace entry
//! (`docs/implementation_checklist/phase-5-diagnostics.md` § item 36).
//! Slice 1 landed the lexer/parser/AST surface (`Attribute.path:
//! Vec<String>` + `Token::ColonColon`); slice 2 lands the dispatcher
//! that recognises namespaced paths and turns unknown bare names into
//! a hard error.
//!
//! ## Behaviour summary
//!
//! - **Bare-name path** (`#[derive]`, `#[no_such_thing]`): looked up
//!   against [`RECOGNIZED_BARE_ATTRIBUTES`]. Unknown names emit
//!   `error[E_UNKNOWN_ATTRIBUTE]` anchored at the attribute's span.
//! - **Multi-segment path** with a known compiler-reserved first
//!   segment (`#[diagnostic::*]`): silently accepted at slice 2 —
//!   per-member shape validation lives in slices 3, 4 (which add
//!   `on_unimplemented` / `do_not_recommend` handling) and slice 5
//!   (which registers the `malformed_diagnostic_attribute` lint).
//! - **Multi-segment path** with any other first segment
//!   (`#[karafmt::skip]`, `#[acmecorp_security::audit]`): silently
//!   accepted. Item 37 (`Tool-Namespaced Attributes`) will formalise
//!   the catch-all rule in the registry, but the slice-2 surface
//!   already accepts the shape — the only thing item 37 changes here
//!   is the absence of the "the namespace is in [`KnownNamespace`]"
//!   guard (slice 2 only knows `diagnostic`).
//!
//! ## What slice 2 does *not* do
//!
//! - No semantic handling of `#[diagnostic::on_unimplemented]` /
//!   `#[diagnostic::do_not_recommend]` — slices 3, 4.
//! - No `malformed_diagnostic_attribute` lint emission — slice 5
//!   registers the lint; the shape checks live with the per-member
//!   handlers in slices 3, 4.
//! - No catch-all silence for arbitrary tool namespaces — item 37 adds
//!   the rule; slice 2's behaviour (accept silently) already matches it
//!   incidentally because no validation runs.

use crate::ast::*;
use crate::resolver::{ResolveError, ResolveErrorKind};
use crate::token::Span;

/// The closed v1 list of bare-name attributes the compiler recognises.
/// Any single-segment attribute path whose name is not in this list
/// emits `error[E_UNKNOWN_ATTRIBUTE]` during validation.
///
/// Entries fall into three groups: (1) attributes the current compiler
/// acts on (deprecated, derive, must_use, …) — the canonical source is
/// the `attr.is_bare("...")` lookups across the pipeline; (2) attributes
/// the v1 spec reserves but the current compiler does not yet wire (gpu,
/// cyclic, interrupt, thread_local, repr, …) — accepting them at the
/// attribute-check layer keeps v1-conformant code compiling while the
/// per-attribute semantics ship in their own entries; (3) the four
/// lint-level attributes (allow / warn / deny / expect) plus `forbid`,
/// which are handled by `scan_lint_level_attrs` in the parser but still
/// need to be in this list so the attribute-check pass does not flag
/// them.
///
/// Keep this list synced with `docs/book/src/appendix-d-attributes.md`
/// and the `is_bare(...)` lookup sites; adding a new compiler-recognised
/// attribute requires an entry here so the recognition layer keeps up
/// with the consumer layer.
const RECOGNIZED_BARE_ATTRIBUTES: &[&str] = &[
    // Lint-level attributes — `scan_lint_level_attrs` in the parser
    // turns these into `lint_overrides`. `forbid` is the CLI-only
    // sibling; it never appears as a source-level attribute today but
    // is reserved for symmetry with the CLI's `-F NAME`.
    "allow",
    "warn",
    "deny",
    "expect",
    "forbid",
    // Compiler-internal markers — `compiler_builtin` gates stdlib
    // source; `no_rc` opts a `shared struct` out of RC.
    "compiler_builtin",
    "no_rc",
    // Phase-6 line 155 slice 3 — flow-sensitive RAII-across-yield.
    // `#[cancel_unsafe_until(method = "<clear>")]` on an impl method
    // declares "calling this method puts the receiver into a
    // cancel-unsafe state until `<clear>` is called on the same
    // receiver". Read by `raii_check` to track per-binding state
    // across yield points; consumers outside `raii_check` accept the
    // attribute silently. v1 seeds `File.write` (clear = `flush`).
    "cancel_unsafe_until",
    // General item annotations.
    "derive",
    "must_use",
    "deprecated",
    "non_exhaustive",
    "track_caller",
    // Phase-8 line 49 — stdlib API stability marker. Use-site
    // emission of the `unstable_api` lint reads the symbol-table
    // payload populated from this attribute. At v1 the spec
    // restricts placement to stdlib items (user-side use is a
    // future RFC), but the parser/recogniser accepts it
    // anywhere so user-side experiments compile silently — the
    // lint emission is what enforces the gating.
    "unstable",
    // FFI / linker.
    "no_mangle",
    "used",
    "link_section",
    "link_name",
    "kara_name",
    "noblock",
    // Testing.
    "test",
    "with_provider",
    "property",
    "snapshot",
    // Memory layout / placement.
    "repr",
    "thread_local",
    // Embedded targets — recognised at v1 even before the embedded
    // profile's semantic handling ships.
    "interrupt",
    "max_stack",
    // GPU compute / shared types.
    "gpu",
    "cyclic",
    // Portable SIMD — `#[require_simd]` on a function makes scalar
    // fallback for any `Vector[T, N]` op in its body a hard build error
    // (phase-7-codegen.md line 308). Read by `simd_report::require_simd_errors`;
    // takes no arguments.
    "require_simd",
    // Reserved for the eventual `#[rc_budget(max: N)]` knob; the
    // parser already accepts the syntactic shape (one of the test
    // fixtures uses it as a generic two-arg attribute example).
    "rc_budget",
    // Phase-8 stdlib-floor § Compiler queries channel sub-item 4 —
    // future query-resolving attributes reserved at v1 freeze.
    // Recognized so they propagate through resolver / typechecker /
    // ownership without information loss; per-attribute semantics
    // land alongside their P1.x catalogue entries:
    //   * `prefer_rc` — P1.1 (resolution surface for the RC fallback
    //     query at `src/ownership.rs:360`). Companion to `no_rc`.
    //   * `specialize` — P1.2 (resolution surface for generic
    //     specialization on monomorphization tuple).
    //   * `fork_at` — P1.6 (resolution surface for auto-concurrency
    //     fork threshold).
    "prefer_rc",
    "specialize",
    "fork_at",
    // Phase-8 stdlib-floor § Compiler queries channel sub-item 5 —
    // P1.3 codegen-query resolution surface (phase-7-codegen.md
    // line 25). Recognized + reserved at v1; semantic enforcement
    // lands with the codegen hooks in P1.3. Pairs with `cold` in
    // the [`QUERY_RESOLUTION_CONFLICT_PAIRS`] table for the
    // multi-resolution-conflict diagnostic.
    "inline",
    "cold",
    "likely",
    "unlikely",
    "inline(never)",
];

/// Phase-8 stdlib-floor § Compiler queries channel sub-item 5.
/// Pairs of bare-name attributes that resolve the same query in
/// conflicting ways. When both appear on the same item, the resolver
/// emits `error[E_QUERY_RESOLUTION_CONFLICT]` anchored at the second
/// attribute's span. Order within each pair is canonical (`a` < `b`
/// lexicographically) so the resolver's detection walk doesn't need
/// to consider permutations.
///
/// Conflict pairs at v1:
///   * `cold` ↔ `inline` — inlining-decision query. P1.3 catalogue
///     entry, phase-7-codegen.md line 25.
///   * `no_rc` ↔ `prefer_rc` — RC-fallback query. P1.1 catalogue
///     entry, src/ownership.rs:360.
const QUERY_RESOLUTION_CONFLICT_PAIRS: &[(&str, &str)] =
    &[("cold", "inline"), ("no_rc", "prefer_rc")];

/// Compiler-reserved namespaces — the *first segment* of a multi-segment
/// attribute path that the compiler claims for its own use. Members of
/// these namespaces have compiler-defined semantics (set per-namespace);
/// every other multi-segment path is a tool-namespaced attribute
/// (item 37, accepted silently and exposed to external tools via
/// `karac query attributes`).
///
/// At v1 only `diagnostic` qualifies. Slice 2's only behavioural use of
/// this list is to *not* error on members of a reserved namespace — the
/// per-namespace validation lives with each namespace's own slices
/// (slices 3, 4 for `diagnostic::on_unimplemented` /
/// `diagnostic::do_not_recommend`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnownAttributeNamespace {
    /// `#[diagnostic::*]` — the compiler-reserved diagnostics namespace.
    /// Unknown members are silently accepted (per design.md § Diagnostic
    /// Namespace Attributes); malformed members emit the
    /// `malformed_diagnostic_attribute` lint (slice 5).
    Diagnostic,
}

impl KnownAttributeNamespace {
    /// Resolve the namespace from the first segment of an attribute
    /// path. Returns `None` for paths whose first segment is not a
    /// compiler-reserved namespace — those are either bare-name
    /// attributes (validated against [`is_recognized_bare_attribute`])
    /// or tool-namespaced attributes (silently accepted at v1).
    pub fn from_first_segment(segment: &str) -> Option<Self> {
        match segment {
            "diagnostic" => Some(Self::Diagnostic),
            _ => None,
        }
    }
}

/// True iff `name` is a recognised bare-name attribute the compiler
/// will not flag as unknown. Used both by the slice-2 validator and by
/// external callers that want to mirror the compiler's recognition
/// rules.
pub fn is_recognized_bare_attribute(name: &str) -> bool {
    RECOGNIZED_BARE_ATTRIBUTES.contains(&name)
}

/// Walk every attribute-bearing item in `program` once and produce
/// `error[E_UNKNOWN_ATTRIBUTE]` for each bare-name attribute whose name
/// is not in [`RECOGNIZED_BARE_ATTRIBUTES`]. Multi-segment paths are
/// accepted silently — per-namespace validation belongs to the
/// per-namespace slices, and tool namespaces (item 37) carry no
/// compiler-visible semantics.
///
/// Called from [`crate::resolver::Resolver::resolve`] after
/// `collect_top_level_items`; the produced errors append to the
/// resolver's error vector so the CLI surfaces them with the rest of
/// the resolve-phase diagnostics.
pub fn validate_program_attributes(program: &Program) -> Vec<ResolveError> {
    let mut errors = Vec::new();
    for item in &program.items {
        visit_item(item, &mut errors);
    }
    errors
}

fn visit_attrs(attrs: &[Attribute], errors: &mut Vec<ResolveError>) {
    for attr in attrs {
        if attr.path.len() == 1 {
            let name = &attr.path[0];
            if !is_recognized_bare_attribute(name) {
                errors.push(ResolveError {
                    message: format!(
                        "error[E_UNKNOWN_ATTRIBUTE]: unknown attribute `{name}` — the \
                         compiler does not recognise this bare-name attribute. If you \
                         intended a diagnostic hint, write `#[diagnostic::{name}]`; if \
                         you intended a tool attribute, use a namespaced form like \
                         `#[your_tool::{name}]`."
                    ),
                    span: attr.span.clone(),
                    kind: ResolveErrorKind::UnknownAttribute,
                    suggestion: None,
                    replacement: None,
                    stub_hint: None,
                });
            }
        } else if KnownAttributeNamespace::from_first_segment(&attr.path[0]).is_some() {
            // Compiler-reserved namespace (currently only `diagnostic::*`).
            // Per-member validation lives with the per-member slices
            // (slices 3, 4 for `diagnostic::on_unimplemented` and
            // `diagnostic::do_not_recommend`); slice 2 silently accepts
            // every member so the namespace's "unknown member is accepted
            // silently" rule is honoured.
        } else {
            // Tool-namespaced path (`#[karafmt::skip]`, …). Silently
            // accepted; item 37 formalises the rule + the
            // `karac query attributes` read surface.
        }
    }

    // Phase-8 stdlib-floor § Compiler queries channel sub-item 5.
    // Detect any conflict pair from QUERY_RESOLUTION_CONFLICT_PAIRS
    // co-occurring on this item. Anchored at the second attribute's
    // span (the one that introduced the conflict). One error per pair
    // per item — if the same pair appears multiple times (which
    // shouldn't, but parse-error recovery can produce duplicates),
    // each later occurrence emits its own diagnostic.
    let names_on_item: Vec<(&str, &Span)> = attrs
        .iter()
        .filter_map(|a| {
            if a.path.len() == 1 {
                Some((a.path[0].as_str(), &a.span))
            } else {
                None
            }
        })
        .collect();
    for (a, b) in QUERY_RESOLUTION_CONFLICT_PAIRS {
        let has_a = names_on_item.iter().any(|(n, _)| n == a);
        let has_b = names_on_item.iter().any(|(n, _)| n == b);
        if has_a && has_b {
            // Pick the later span — whichever attribute appears
            // second in source order. attrs are in source order
            // already, so the second-occurring one in the filtered
            // list is what we want.
            let conflict_span = names_on_item
                .iter()
                .filter(|(n, _)| n == a || n == b)
                .nth(1)
                .map(|(_, s)| (*s).clone())
                .unwrap_or_else(|| names_on_item[0].1.clone());
            errors.push(ResolveError {
                message: format!(
                    "error[E_QUERY_RESOLUTION_CONFLICT]: `#[{a}]` and `#[{b}]` resolve \
                     the same compiler query in conflicting ways — only one may apply \
                     to this item. Drop whichever does not match the intent."
                ),
                span: conflict_span,
                kind: ResolveErrorKind::QueryResolutionConflict,
                suggestion: None,
                replacement: None,
                stub_hint: None,
            });
        }
    }
}

fn visit_item(item: &Item, errors: &mut Vec<ResolveError>) {
    match item {
        Item::Function(f) => visit_attrs(&f.attributes, errors),
        Item::StructDef(s) => {
            visit_attrs(&s.attributes, errors);
            for field in &s.fields {
                visit_attrs(&field.attributes, errors);
            }
        }
        Item::UnionDef(u) => {
            visit_attrs(&u.attributes, errors);
            for field in &u.fields {
                visit_attrs(&field.attributes, errors);
            }
        }
        Item::EnumDef(e) => {
            visit_attrs(&e.attributes, errors);
            for variant in &e.variants {
                visit_attrs(&variant.attributes, errors);
            }
        }
        Item::TraitDef(t) => {
            visit_attrs(&t.attributes, errors);
            for ti in &t.items {
                if let TraitItem::Method(m) = ti {
                    visit_attrs(&m.attributes, errors);
                }
            }
        }
        Item::TraitAlias(t) => visit_attrs(&t.attributes, errors),
        Item::MarkerTrait(t) => visit_attrs(&t.attributes, errors),
        Item::ImplBlock(i) => {
            visit_attrs(&i.attributes, errors);
            for ii in &i.items {
                if let ImplItem::Method(m) = ii {
                    visit_attrs(&m.attributes, errors);
                }
            }
        }
        Item::ConstDecl(c) => visit_attrs(&c.attributes, errors),
        Item::ModuleBinding(b) => visit_attrs(&b.attributes, errors),
        Item::TestCase(t) => visit_attrs(&t.attributes, errors),
        Item::TypeAlias(t) => visit_attrs(&t.attributes, errors),
        Item::DistinctType(d) => visit_attrs(&d.attributes, errors),
        Item::ExternFunction(f) => visit_attrs(&f.attributes, errors),
        Item::ExternBlock(b) => {
            visit_attrs(&b.attributes, errors);
            for it in &b.items {
                match it {
                    ExternItem::Function(f) => visit_attrs(&f.attributes, errors),
                    ExternItem::OpaqueType(o) => visit_attrs(&o.attributes, errors),
                }
            }
        }
        Item::LayoutDef(l) => visit_attrs(&l.attributes, errors),
        // EffectResource / EffectGroup / EffectVerbDecl / UseDecl /
        // Import / AliasDecl / IndependentDecl carry no attribute fields
        // at the AST level.
        Item::EffectResource(_)
        | Item::EffectGroup(_)
        | Item::EffectVerbDecl(_)
        | Item::UseDecl(_)
        | Item::Import(_)
        | Item::AliasDecl(_)
        | Item::IndependentDecl(_) => {}
    }
}
