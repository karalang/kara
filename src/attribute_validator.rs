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

/// The closed list of bare-name attributes 0.8 accepts. Every entry is a
/// row of design.md § Attributes' table, the experimental ones marked so
/// (B-2026-11-25-61: listed only when `karac check` accepts it and a
/// program that uses it runs); `attribute_registry_matches_design_md`
/// holds the two together. A single-segment attribute not here is
/// `error[E_UNKNOWN_ATTRIBUTE]`, or `error[E_ATTRIBUTE_DEFERRED]` when
/// [`DEFERRED_ATTRIBUTES`] names it.
///
/// The `is_bare("...")` lookups across the pipeline are the consumers; a
/// new attribute needs an entry here, and a row in the table.
const RECOGNIZED_BARE_ATTRIBUTES: &[&str] = &[
    // Lint levels: `scan_lint_level_attrs` in the parser turns these into
    // `lint_overrides`.
    "allow",
    "warn",
    "deny",
    "expect",
    // Standard library only: `compiler_builtin` marks a declaration the
    // compiler implements; `observability` a library function transparent
    // to effects (design.md § Observability; E0115 in user code).
    "compiler_builtin",
    "observability",
    // General item annotations.
    "derive",
    "must_use",
    "deprecated",
    "non_exhaustive",
    "track_caller",
    // `#[default]` marks the unit variant a `#[derive(Default)]` enum
    // builds; placement and shape are `validate_default_attribute`'s.
    "default",
    // The custom panic handler, checked by `validate_panic_handler`.
    "panic_handler",
    // The `#[derive(Serialize, Deserialize)]` helper, read by the comptime
    // derive in `runtime/stdlib/source/serde.kara`.
    "serde",
    // Stdlib API stability marker; the `unstable_api` lint enforces it.
    "unstable",
    // FFI and linking: `kara_name`; experimental `link_name` (bind a
    // foreign symbol of another name) and `used` (keep a symbol nothing
    // references).
    "used",
    "link_name",
    "kara_name",
    // A `test "name" { }` block's configuration (`#[test(requires: ..)]`)
    // and its provider substitution (`#[with_provider(R, e)]`); on a `fn`
    // both are E_TEST_ATTR_INVALID_TARGET.
    "test",
    "with_provider",
    // ABI layout.
    "repr",
    // A GPU kernel: on the later track, its dispatch is E_ARCHIVED and the
    // kernel itself draws the later-track note (core/ok_later_track_gpu).
    "gpu",
    // Single-target gating until `#[cfg]`; shape checked in the parser.
    "target",
    // `#[no_effect(..)]` forbids effects in a function and its callees.
    "no_effect",
    // Experimental: `#[require_simd]` makes a scalar fallback of a
    // `Vector[T, N]` op a build error (`simd_report::require_simd_errors`).
    "require_simd",
    // `#![rc_budget(max: N)]` is accepted and counts nothing: the compiler
    // inserts no reference counts (B-2026-11-21-76).
    "rc_budget",
    // Experimental query answers (design.md § Compiler queries): each
    // suppresses its query's note. `#[specialize(T: i64)]` the
    // specialization query, `#[no_rc]` / `#[prefer_rc]` the RC-fallback
    // query (a pair on one item is E_QUERY_RESOLUTION_CONFLICT), and
    // `#[fork_at(n)]` the auto-concurrency fork threshold.
    "specialize",
    "no_rc",
    "prefer_rc",
    "fork_at",
    // Experimental codegen hints: checked for placement and conflicts, not
    // yet lowered to LLVM function attributes (deferred.md § Codegen Hint
    // Attributes).
    "inline",
    "cold",
    // Experimental: a function's CPU features above the baseline, and the
    // per-feature variants with a runtime dispatch (`desugar.rs`).
    "target_feature",
    "multiversion",
];

/// Attributes that docs/deferred.md specifies and 0.8 does not have, with
/// the section that holds each. One is refused with
/// `error[E_ATTRIBUTE_DEFERRED]`, naming the section, rather than as an
/// unknown name: it is scheduled, not misspelled (B-2026-11-25-61).
pub(crate) const DEFERRED_ATTRIBUTES: &[(&str, &str)] = &[
    ("no_mangle", "Linker Control Attributes"),
    ("link_section", "Linker Control Attributes"),
    ("profile", "Project Profiles"),
    ("interrupt", "Interrupt Handler ABI (`#[interrupt]`)"),
    ("max_stack", "Static Stack Depth Analysis"),
    ("thread_local", "Module-Level Mutable State"),
    ("property", "Property tests"),
    ("snapshot", "Snapshot tests"),
    ("likely", "Codegen Hint Attributes (`#[inline]`, `#[cold]`)"),
    (
        "unlikely",
        "Codegen Hint Attributes (`#[inline]`, `#[cold]`)",
    ),
    ("tailrec", "Tail-Call Optimization (`#[tailrec]`)"),
    ("should_panic", "Expected-panic tests (`should_panic`)"),
    ("zeroize", "Generalized `#[zeroize]` Attribute"),
    ("no_std", "`#[no_std]` / Bare Metal Support"),
    ("checked", "Production Contract Checking (`#[checked]`)"),
    ("example", "Doctests as `#[example]` Blocks on `pub` Items"),
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
///   * `no_rc` ↔ `prefer_rc` — RC-fallback query. P1.1 catalogue
///     entry, src/ownership.rs:360.
///
/// The `cold` ↔ `inline` pair that once lived here was removed when the
/// codegen-hint attributes gained real semantics: `#[cold]` + `#[inline]`
/// is a legal combination (design.md § Codegen Hint Attributes), and the
/// contradictory pairs (`#[inline]`+`#[inline(never)]`,
/// `#[cold]`+`#[inline(always)]`, …) are diagnosed in the parser scan
/// with their own focused error codes.
const QUERY_RESOLUTION_CONFLICT_PAIRS: &[(&str, &str)] = &[("no_rc", "prefer_rc")];

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
    visit_inner_attrs(&program.inner_attrs, &mut errors);
    for item in &program.items {
        visit_item(item, &mut errors);
    }
    errors
}

/// `#![...]` at the top of a file, the module-level position (design.md
/// §17 Lint levels, B-2026-11-19-52): checked against the same registry as
/// an outer attribute, and only the lint levels and `rc_budget` may stand
/// there; any other attribute is refused naming the item it belongs on.
fn visit_inner_attrs(attrs: &[Attribute], errors: &mut Vec<ResolveError>) {
    for attr in attrs {
        let before = errors.len();
        visit_attrs(std::slice::from_ref(attr), errors);
        if errors.len() > before || attr.path.len() != 1 {
            continue;
        }
        let name = attr.path[0].as_str();
        if matches!(name, "allow" | "warn" | "deny" | "expect" | "rc_budget") {
            continue;
        }
        let belongs = match name {
            "derive" | "repr" | "non_exhaustive" => "the struct or enum it applies to",
            "default" => "an enum variant",
            "serde" => "the struct, enum or field it applies to",
            "track_caller" | "panic_handler" | "no_effect" | "inline" | "cold" => {
                "the function it applies to"
            }
            "kara_name" => "the foreign function or type it names",
            _ => "the item it applies to",
        };
        errors.push(ResolveError {
            message: format!(
                "error[E_UNKNOWN_ATTRIBUTE]: `#![{name}]` is not a module-level attribute: only \
                 the lint levels (`allow`, `warn`, `deny`, `expect`) and `rc_budget` stand at \
                 the top of a file; write `#[{name}]` on {belongs} (design.md §17 Lint levels)"
            ),
            span: attr.span,
            kind: ResolveErrorKind::UnknownAttribute,
            suggestion: None,
            replacement: None,
            stub_hint: None,
        });
    }
}

/// `#[panic_handler]` (design.md § Panic handler): at most one in a
/// program, on a free function of one `PanicInfo` parameter that returns
/// nothing, with no type parameters and no arguments to the attribute.
/// Each violation is `E_PANIC_HANDLER`, at the attribute.
pub fn validate_panic_handler(program: &Program) -> Vec<ResolveError> {
    let mut errors = Vec::new();
    let mut first: Option<String> = None;
    let mut report = |span: Span, message: String| {
        errors.push(ResolveError {
            message: format!("error[E_PANIC_HANDLER]: {message}"),
            span,
            kind: ResolveErrorKind::PanicHandlerInvalid,
            suggestion: None,
            replacement: None,
            stub_hint: None,
        });
    };
    let marker = |attrs: &[Attribute]| attrs.iter().find(|a| a.is_bare("panic_handler")).cloned();
    for item in &program.items {
        match item {
            Item::Function(f) => {
                let Some(attr) = marker(&f.attributes) else {
                    continue;
                };
                if !attr.args.is_empty() || attr.string_value.is_some() {
                    report(
                        attr.span,
                        "`#[panic_handler]` takes no arguments".to_string(),
                    );
                }
                if let Some(prev) = &first {
                    report(
                        attr.span,
                        format!(
                            "a program has at most one panic handler, and `{prev}` is \
                             already marked `#[panic_handler]`"
                        ),
                    );
                    continue;
                }
                first = Some(f.name.clone());
                let param_ok = match f.params.as_slice() {
                    [p] => {
                        let ty = match &p.ty.kind {
                            TypeKind::Ref(inner) => inner.as_ref(),
                            _ => &p.ty,
                        };
                        matches!(&ty.kind, TypeKind::Path(path)
                            if path.segments.len() == 1
                                && path.segments[0] == "PanicInfo"
                                && path.generic_args.is_none())
                    }
                    _ => false,
                };
                let returns_unit = match &f.return_type {
                    None => true,
                    Some(t) => matches!(&t.kind, TypeKind::Tuple(ts) if ts.is_empty()),
                };
                if f.generic_params.is_some() || !param_ok || !returns_unit {
                    report(
                        attr.span,
                        format!(
                            "the panic handler `{}` must be `fn {}(info: PanicInfo)`: one \
                             `PanicInfo` parameter, no type parameters and no return value",
                            f.name, f.name
                        ),
                    );
                }
            }
            Item::ImplBlock(b) => {
                for it in &b.items {
                    if let ImplItem::Method(m) = it {
                        if let Some(attr) = marker(&m.attributes) {
                            report(
                                attr.span,
                                format!(
                                    "`#[panic_handler]` marks a free function, not the method `{}`",
                                    m.name
                                ),
                            );
                        }
                    }
                }
            }
            Item::StructDef(s) => {
                if let Some(attr) = marker(&s.attributes) {
                    report(
                        attr.span,
                        "`#[panic_handler]` marks a free function".to_string(),
                    );
                }
            }
            Item::EnumDef(e) => {
                if let Some(attr) = marker(&e.attributes) {
                    report(
                        attr.span,
                        "`#[panic_handler]` marks a free function".to_string(),
                    );
                }
            }
            _ => {}
        }
    }
    errors
}

/// Phase-8 stdlib-floor — `#[default]` placement + arg-shape checker.
///
/// `#[default]` is legal **only** on a unit enum variant under
/// `#[derive(Default)]`. This pass walks every attribute-bearing item
/// and emits:
///   * `E_MALFORMED_ATTRIBUTE_ARGS` for `#[default(...)]` / `#[default
///     = ...]` (the attribute takes no arguments) — checked first, at
///     every site, so a malformed-and-misplaced marker reports the
///     shape problem rather than the position one;
///   * `E_DEFAULT_ATTRIBUTE_INVALID_POSITION` for a well-formed
///     `#[default]` anywhere other than an enum variant;
///   * `E_DEFAULT_ATTRIBUTE_WITHOUT_DERIVE` for a well-formed
///     `#[default]` on a variant whose enclosing enum does not carry
///     `#[derive(Default)]`.
///
/// The "marked variant is field-less" and "exactly one marker" checks
/// are derive-semantic and live in the typechecker's
/// `validate_derive_default`; this pass is purely about where the
/// attribute may appear and whether it is well-formed.
pub fn validate_default_attribute(program: &Program) -> Vec<ResolveError> {
    let mut errors = Vec::new();
    for item in &program.items {
        check_item_default(item, &mut errors);
        check_item_helpers(item, &mut errors);
    }
    errors
}

/// Where a derive helper may stand.
#[derive(Clone, Copy, PartialEq)]
enum HelperSite {
    /// A struct or enum declaration, or an enum variant: `#[serde(...)]`.
    TypeOrVariant,
    /// A struct field or a struct variant's field: `#[serde(...)]` and the
    /// protobuf derive's `#[karac::proto(...)]` / `#[karac::field(...)]`.
    Field,
    /// Anywhere else: no derive reads a helper there.
    Elsewhere,
}

/// Refuse a derive helper where its derive does not look: it would be dropped
/// silently, as `#[serde(rename: "x")]` on a function was (B-2026-11-05-17
/// follow-up). `#[default]` has its own position check above.
fn report_helpers(attrs: &[Attribute], site: HelperSite, errors: &mut Vec<ResolveError>) {
    for attr in attrs {
        let (name, valid_at) = if attr.path.len() == 1 && attr.path[0] == "serde" {
            ("#[serde(...)]", "a struct, an enum, a field or a variant")
        } else if attr.path.len() == 2
            && attr.path[0] == "karac"
            && KARAC_NAMESPACE_ATTRIBUTES.contains(&attr.path[1].as_str())
        {
            if attr.path[1] == "proto" {
                ("#[karac::proto(...)]", "a field")
            } else {
                ("#[karac::field(...)]", "a field")
            }
        } else {
            continue;
        };
        let ok = match site {
            HelperSite::Field => true,
            HelperSite::TypeOrVariant => attr.path.len() == 1,
            HelperSite::Elsewhere => false,
        };
        if ok {
            continue;
        }
        errors.push(ResolveError {
            message: format!(
                "error[E_HELPER_ATTRIBUTE_INVALID_POSITION]: `{name}` is a derive helper, read \
                 only on {valid_at}; here no derive reads it, so it would do nothing"
            ),
            span: attr.span,
            kind: ResolveErrorKind::HelperAttributeInvalidPosition,
            // It does nothing here, so deleting it changes nothing.
            suggestion: Some(format!(
                "remove the attribute, or move it to where its derive reads it: {valid_at}"
            )),
            replacement: Some(Box::new(crate::resolver::TextEdit {
                offset: attr.span.offset,
                length: attr.span.length,
                replacement: String::new(),
            })),
            stub_hint: None,
        });
    }
}

fn check_item_helpers(item: &Item, errors: &mut Vec<ResolveError>) {
    use HelperSite::*;
    let fields = |fs: &[StructField], place: SerdePlace, errors: &mut Vec<ResolveError>| {
        for f in fs {
            report_helpers(&f.attributes, Field, errors);
            check_serde_keys(&f.attributes, place, errors);
        }
    };
    match item {
        Item::StructDef(s) => {
            report_helpers(&s.attributes, TypeOrVariant, errors);
            check_serde_keys(&s.attributes, SerdePlace::Struct, errors);
            fields(&s.fields, SerdePlace::Field, errors);
        }
        Item::EnumDef(e) => {
            report_helpers(&e.attributes, TypeOrVariant, errors);
            check_serde_keys(&e.attributes, SerdePlace::Enum, errors);
            for v in &e.variants {
                report_helpers(&v.attributes, TypeOrVariant, errors);
                check_serde_keys(&v.attributes, SerdePlace::Variant, errors);
                if let VariantKind::Struct(fs) = &v.kind {
                    fields(fs, SerdePlace::VariantField, errors);
                }
            }
        }
        Item::UnionDef(u) => {
            report_helpers(&u.attributes, Elsewhere, errors);
            for f in &u.fields {
                report_helpers(&f.attributes, Elsewhere, errors);
            }
        }
        Item::TraitDef(t) => {
            report_helpers(&t.attributes, Elsewhere, errors);
            for ti in &t.items {
                if let TraitItem::Method(m) = ti {
                    report_helpers(&m.attributes, Elsewhere, errors);
                }
            }
        }
        Item::ImplBlock(i) => {
            report_helpers(&i.attributes, Elsewhere, errors);
            for ii in &i.items {
                if let ImplItem::Method(m) = ii {
                    report_helpers(&m.attributes, Elsewhere, errors);
                }
            }
        }
        Item::ExternBlock(b) => {
            report_helpers(&b.attributes, Elsewhere, errors);
            for it in &b.items {
                match it {
                    ExternItem::Function(f) => report_helpers(&f.attributes, Elsewhere, errors),
                    ExternItem::OpaqueType(o) => report_helpers(&o.attributes, Elsewhere, errors),
                }
            }
        }
        Item::Function(f) => report_helpers(&f.attributes, Elsewhere, errors),
        Item::TraitAlias(t) => report_helpers(&t.attributes, Elsewhere, errors),
        Item::MarkerTrait(t) => report_helpers(&t.attributes, Elsewhere, errors),
        Item::ConstDecl(c) => report_helpers(&c.attributes, Elsewhere, errors),
        Item::ModuleBinding(b) => report_helpers(&b.attributes, Elsewhere, errors),
        Item::TestCase(t) => report_helpers(&t.attributes, Elsewhere, errors),
        Item::TypeAlias(t) => report_helpers(&t.attributes, Elsewhere, errors),
        Item::DistinctType(d) => report_helpers(&d.attributes, Elsewhere, errors),
        Item::ExternFunction(f) => report_helpers(&f.attributes, Elsewhere, errors),
        Item::LayoutDef(l) => report_helpers(&l.attributes, Elsewhere, errors),
        Item::EffectResource(_)
        | Item::EffectGroup(_)
        | Item::EffectVerbDecl(_)
        | Item::Import(_)
        | Item::AliasDecl(_)
        | Item::IndependentDecl(_) => {}
    }
}

/// Where a `#[serde(...)]` stands, for the keys the derives read there
/// (runtime/stdlib/source/serde.kara).
#[derive(Clone, Copy, PartialEq)]
enum SerdePlace {
    Struct,
    Enum,
    Variant,
    Field,
    /// A struct variant's field: the derives read no key there.
    VariantField,
}

impl SerdePlace {
    /// The keys read here: those that take a value, then the bare flags.
    /// `default` is both (`default` or `default: expr`).
    fn keys(self) -> (&'static [&'static str], &'static [&'static str]) {
        match self {
            SerdePlace::Struct | SerdePlace::VariantField => (&[], &[]),
            SerdePlace::Enum => (&["tag"], &["untagged"]),
            SerdePlace::Variant => (&["rename"], &[]),
            SerdePlace::Field => (
                &["rename", "default"],
                &["skip", "skip_serializing", "skip_deserializing", "default"],
            ),
        }
    }

    fn noun(self) -> &'static str {
        match self {
            SerdePlace::Struct => "a struct",
            SerdePlace::Enum => "an enum",
            SerdePlace::Variant => "a variant",
            SerdePlace::Field => "a field",
            SerdePlace::VariantField => "a variant's field",
        }
    }
}

/// Refuse a `#[serde(...)]` key the derives do not read where it stands: it
/// would do nothing, and a misspelt `skip_serializing` writes the field it
/// was meant to hide (B-2026-11-20-24). A key within two edits of one that
/// is read gets a machine-applicable rename.
fn check_serde_keys(attrs: &[Attribute], place: SerdePlace, errors: &mut Vec<ResolveError>) {
    let (valued, flags) = place.keys();
    let mut known: Vec<&str> = Vec::new();
    for k in valued.iter().chain(flags.iter()) {
        if !known.contains(k) {
            known.push(k);
        }
    }
    let accepted = if known.is_empty() {
        format!("The derives read no `serde` key on {}", place.noun())
    } else {
        let list: Vec<String> = known.iter().map(|k| format!("`{k}`")).collect();
        let list = match list.split_last() {
            Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
            _ => list.join(""),
        };
        let noun = place.noun();
        format!("On {noun} the derives read {list}")
    };
    for attr in attrs.iter().filter(|a| a.is_bare("serde")) {
        for arg in &attr.args {
            // `key: value`, or a bare `key`.
            let (key, key_span, has_value) = match (&arg.name, &arg.value) {
                (Some(k), _) => (k.clone(), arg.span, arg.value.is_some()),
                (
                    None,
                    Some(Expr {
                        kind: ExprKind::Identifier(k),
                        span,
                        ..
                    }),
                ) => (k.clone(), *span, false),
                _ => continue,
            };
            let message = if valued.contains(&key.as_str()) || flags.contains(&key.as_str()) {
                if has_value && !valued.contains(&key.as_str()) {
                    format!("`serde({key})` takes no value; write `{key}`")
                } else if !has_value && !flags.contains(&key.as_str()) {
                    format!("`serde({key}: ...)` needs a value, as `{key}: \"name\"`")
                } else {
                    continue;
                }
            } else {
                let close = crate::edit_distance::suggest_similar(&key, &known);
                if let Some(c) = &close {
                    errors.push(ResolveError {
                        message: format!(
                            "error[E_UNKNOWN_ATTRIBUTE]: unknown `serde` key `{key}`; did you \
                             mean `{c}`? {accepted} (design.md § Serialize and Deserialize)"
                        ),
                        span: key_span,
                        kind: ResolveErrorKind::UnknownAttribute,
                        suggestion: Some(format!("write `{c}`")),
                        replacement: Some(Box::new(crate::resolver::TextEdit {
                            offset: key_span.offset,
                            length: key.len(),
                            replacement: c.clone(),
                        })),
                        stub_hint: None,
                    });
                    continue;
                }
                format!("unknown `serde` key `{key}`")
            };
            errors.push(ResolveError {
                message: format!(
                    "error[E_UNKNOWN_ATTRIBUTE]: {message}. {accepted} (design.md § Serialize \
                     and Deserialize)"
                ),
                span: key_span,
                kind: ResolveErrorKind::UnknownAttribute,
                suggestion: None,
                replacement: None,
                stub_hint: None,
            });
        }
    }
}

/// Emit `E_MALFORMED_ATTRIBUTE_ARGS` if `#[default]` carries arguments.
/// Returns `true` when the attribute was malformed (so callers skip the
/// position/derive checks — one diagnostic per marker).
fn default_attr_malformed(attr: &Attribute, errors: &mut Vec<ResolveError>) -> bool {
    if attr.args.is_empty() && attr.string_value.is_none() {
        return false;
    }
    errors.push(ResolveError {
        message: "error[E_MALFORMED_ATTRIBUTE_ARGS]: `#[default]` accepts no arguments — \
                  write it as a bare `#[default]` on the unit enum variant that should be \
                  the default."
            .to_string(),
        span: attr.span,
        kind: ResolveErrorKind::MalformedAttributeArgs,
        suggestion: None,
        replacement: None,
        stub_hint: None,
    });
    true
}

/// Report every well-formed `#[default]` in `attrs` as occupying an
/// invalid position (any site that is not an enum variant). Malformed
/// markers report the shape problem instead.
fn report_default_invalid_position(attrs: &[Attribute], errors: &mut Vec<ResolveError>) {
    for attr in attrs {
        if !attr.is_bare("default") {
            continue;
        }
        if default_attr_malformed(attr, errors) {
            continue;
        }
        errors.push(ResolveError {
            message: "error[E_DEFAULT_ATTRIBUTE_INVALID_POSITION]: `#[default]` is only valid \
                      on a unit enum variant under `#[derive(Default)]`."
                .to_string(),
            span: attr.span,
            kind: ResolveErrorKind::DefaultAttributeInvalidPosition,
            suggestion: None,
            replacement: None,
            stub_hint: None,
        });
    }
}

fn check_item_default(item: &Item, errors: &mut Vec<ResolveError>) {
    match item {
        Item::EnumDef(e) => {
            // `#[default]` on the enum decl itself is misplaced — it
            // belongs on one of the variants.
            report_default_invalid_position(&e.attributes, errors);
            let enum_derives_default =
                crate::typechecker::extract_derived_traits(&e.attributes).contains("Default");
            for variant in &e.variants {
                for attr in &variant.attributes {
                    if !attr.is_bare("default") {
                        continue;
                    }
                    if default_attr_malformed(attr, errors) {
                        continue;
                    }
                    if !enum_derives_default {
                        errors.push(ResolveError {
                            message: format!(
                                "error[E_DEFAULT_ATTRIBUTE_WITHOUT_DERIVE]: `#[default]` on \
                                 variant `{}` requires the enclosing enum `{}` to carry \
                                 `#[derive(Default)]` — add `#[derive(Default)]` to enum `{}`.",
                                variant.name, e.name, e.name
                            ),
                            span: attr.span,
                            kind: ResolveErrorKind::DefaultAttributeWithoutDerive,
                            suggestion: None,
                            // The fix adds the derive above the enum
                            // (design.md § `Default` and `#[default]`,
                            // B-2026-11-20-127), where its line starts with
                            // it: a `pub enum`'s span starts past the `pub `.
                            replacement: {
                                let pub_len = if e.is_pub { "pub ".len() } else { 0 };
                                (e.span.column == 1 + pub_len).then_some(pub_len)
                            }
                            .map(|pub_len| {
                                Box::new(crate::resolver::TextEdit {
                                    offset: e.span.offset - pub_len,
                                    length: 0,
                                    replacement: "#[derive(Default)]\n".to_string(),
                                })
                            }),
                            stub_hint: None,
                        });
                    }
                }
                // A `#[default]` on a struct-variant *field* is misplaced.
                if let VariantKind::Struct(fields) = &variant.kind {
                    for f in fields {
                        report_default_invalid_position(&f.attributes, errors);
                    }
                }
            }
        }
        Item::StructDef(s) => {
            report_default_invalid_position(&s.attributes, errors);
            for field in &s.fields {
                report_default_invalid_position(&field.attributes, errors);
            }
        }
        Item::UnionDef(u) => {
            report_default_invalid_position(&u.attributes, errors);
            for field in &u.fields {
                report_default_invalid_position(&field.attributes, errors);
            }
        }
        Item::TraitDef(t) => {
            report_default_invalid_position(&t.attributes, errors);
            for ti in &t.items {
                if let TraitItem::Method(m) = ti {
                    report_default_invalid_position(&m.attributes, errors);
                }
            }
        }
        Item::ImplBlock(i) => {
            report_default_invalid_position(&i.attributes, errors);
            for ii in &i.items {
                if let ImplItem::Method(m) = ii {
                    report_default_invalid_position(&m.attributes, errors);
                }
            }
        }
        Item::ExternBlock(b) => {
            report_default_invalid_position(&b.attributes, errors);
            for it in &b.items {
                match it {
                    ExternItem::Function(f) => {
                        report_default_invalid_position(&f.attributes, errors)
                    }
                    ExternItem::OpaqueType(o) => {
                        report_default_invalid_position(&o.attributes, errors)
                    }
                }
            }
        }
        Item::Function(f) => report_default_invalid_position(&f.attributes, errors),
        Item::TraitAlias(t) => report_default_invalid_position(&t.attributes, errors),
        Item::MarkerTrait(t) => report_default_invalid_position(&t.attributes, errors),
        Item::ConstDecl(c) => report_default_invalid_position(&c.attributes, errors),
        Item::ModuleBinding(b) => report_default_invalid_position(&b.attributes, errors),
        Item::TestCase(t) => report_default_invalid_position(&t.attributes, errors),
        Item::TypeAlias(t) => report_default_invalid_position(&t.attributes, errors),
        Item::DistinctType(d) => report_default_invalid_position(&d.attributes, errors),
        Item::ExternFunction(f) => report_default_invalid_position(&f.attributes, errors),
        Item::LayoutDef(l) => report_default_invalid_position(&l.attributes, errors),
        Item::EffectResource(_)
        | Item::EffectGroup(_)
        | Item::EffectVerbDecl(_)
        | Item::Import(_)
        | Item::AliasDecl(_)
        | Item::IndependentDecl(_) => {}
    }
}

/// The `karac::` namespace's members: the protobuf derive's field and type
/// helpers (`runtime/stdlib/protobuf.kara`).
const KARAC_NAMESPACE_ATTRIBUTES: &[&str] = &["proto", "field"];

fn unknown_path_attribute(attr: &Attribute, message: String) -> ResolveError {
    ResolveError {
        message,
        span: attr.span,
        kind: ResolveErrorKind::UnknownAttribute,
        suggestion: None,
        replacement: None,
        stub_hint: None,
    }
}

fn visit_attrs(attrs: &[Attribute], errors: &mut Vec<ResolveError>) {
    for attr in attrs {
        if attr.path.len() == 1 {
            let name = &attr.path[0];
            // `#[cfg]` is refused by the parser, with its fix.
            if let Some((_, section)) = DEFERRED_ATTRIBUTES.iter().find(|(a, _)| a == name) {
                errors.push(ResolveError {
                    message: format!(
                        "error[E_ATTRIBUTE_DEFERRED]: `#[{name}]` is deferred (docs/deferred.md \
                         § {section}) and not in 0.8"
                    ),
                    span: attr.span,
                    kind: ResolveErrorKind::UnknownAttribute,
                    suggestion: None,
                    replacement: None,
                    stub_hint: None,
                });
            } else if name != "cfg" && !is_recognized_bare_attribute(name) {
                errors.push(ResolveError {
                    message: format!(
                        "error[E_UNKNOWN_ATTRIBUTE]: unknown attribute `{name}` — the \
                         compiler does not recognise this bare-name attribute (design.md \
                         § Attributes lists them). If you intended a diagnostic hint, \
                         write `#[diagnostic::{name}]`."
                    ),
                    span: attr.span,
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
        } else if attr.path[0] == "karac" {
            // The compiler's own namespace (design.md § Attributes, 219b88a4a2):
            // the protobuf derive's helpers. A name it does not define is an
            // error, as an unknown bare name is.
            let member = attr.path[1..].join("::");
            if !KARAC_NAMESPACE_ATTRIBUTES.contains(&member.as_str()) {
                errors.push(unknown_path_attribute(
                    attr,
                    format!(
                        "error[E_UNKNOWN_ATTRIBUTE]: unknown attribute `#[karac::{member}]` — \
                         the `karac::` namespace holds the compiler's own attributes, {}",
                        KARAC_NAMESPACE_ATTRIBUTES
                            .iter()
                            .map(|n| format!("`#[karac::{n}]`"))
                            .collect::<Vec<_>>()
                            .join(" and "),
                    ),
                ));
            }
        } else {
            // A tool-namespaced path (`#[karafmt::skip]`). Attribute paths have
            // one segment in v1 and tool namespaces are deferred (design.md
            // § Attributes), so it is refused rather than silently dropped
            // (B-2026-11-05-17).
            let path = attr.path.join("::");
            errors.push(unknown_path_attribute(
                attr,
                format!(
                    "error[E_UNKNOWN_ATTRIBUTE]: `#[{path}]` has a multi-segment path — \
                     attribute paths have one segment in v1 (tool namespaces such as \
                     `#[{}::…]` come later), apart from the compiler's `karac::` and \
                     `diagnostic::` namespaces",
                    attr.path[0],
                ),
            ));
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
                .map(|(_, s)| *(*s))
                .unwrap_or_else(|| *names_on_item[0].1);
            errors.push(ResolveError {
                message: format!(
                    "error[E_QUERY_RESOLUTION_CONFLICT]: `#[{a}]` and `#[{b}]` resolve \
                     the same compiler query in conflicting ways — only one may apply \
                     to this item. Drop whichever does not match the intent."
                ),
                span: conflict_span,
                kind: ResolveErrorKind::QueryResolutionConflict,
                suggestion: Some(format!("remove `#[{a}]` or `#[{b}]`")),
                replacement: None,
                stub_hint: None,
            });
        }
    }
}

/// A derive's argument list (`#[derive(Display(snake_case))]`): the one
/// argument design.md §9 defines is `snake_case` on an enum's `Display`
/// ("Casing"). Any other was accepted and did nothing (B-2026-11-16-262):
/// `Display(all_fields)`, which appendix C described, changed nothing,
/// since a derived struct `Display` already renders every field.
fn check_derive_arguments(attrs: &[Attribute], is_enum: bool, errors: &mut Vec<ResolveError>) {
    for attr in attrs.iter().filter(|a| a.is_bare("derive")) {
        for arg in &attr.args {
            let Some(Expr {
                kind: ExprKind::Call { callee, args },
                span,
                ..
            }) = &arg.value
            else {
                continue;
            };
            let ExprKind::Identifier(trait_name) = &callee.kind else {
                continue;
            };
            let snake_case = matches!(
                args.as_slice(),
                [one] if one.label.is_none()
                    && matches!(&one.value.kind, ExprKind::Identifier(f) if f == "snake_case")
            );
            let message = match (trait_name.as_str(), snake_case, is_enum) {
                ("Display", true, true) => continue,
                ("Display", true, false) => {
                    "`Display(snake_case)` writes an enum's variant names in snake_case, and a \
                     struct has no variants; write `Display` (design.md §9 Derived `Display`)"
                        .to_string()
                }
                ("Display", false, _) => {
                    "`Display` takes one argument in a derive, `snake_case`, on an enum \
                     (design.md §9 Derived `Display`); a derived struct `Display` renders every \
                     field already, so remove the argument"
                        .to_string()
                }
                (other, _, _) => format!(
                    "`{other}` takes no arguments in a derive: write `{other}` (design.md §9 \
                     Derive)"
                ),
            };
            errors.push(ResolveError {
                message: format!("error[E_UNKNOWN_ATTRIBUTE]: {message}"),
                span: *span,
                kind: ResolveErrorKind::UnknownAttribute,
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
            check_derive_arguments(&s.attributes, false, errors);
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
            check_derive_arguments(&e.attributes, true, errors);
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
        // EffectResource / EffectGroup / EffectVerbDecl /
        // Import / AliasDecl / IndependentDecl carry no attribute fields
        // at the AST level.
        Item::EffectResource(_)
        | Item::EffectGroup(_)
        | Item::EffectVerbDecl(_)
        | Item::Import(_)
        | Item::AliasDecl(_)
        | Item::IndependentDecl(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// design.md § Attributes, through the start of the `diagnostic::`
    /// namespace: the tables and the deferred list.
    fn design_attributes_section() -> &'static str {
        let design = include_str!("../docs/design.md");
        let start = design
            .find("\n### Attributes\n")
            .expect("design.md § Attributes");
        let end = design[start..]
            .find("#### The `diagnostic::` namespace")
            .expect("the diagnostic:: subsection");
        &design[start..start + end]
    }

    fn table_attribute_names(section: &str) -> Vec<String> {
        let mut out = Vec::new();
        for line in section.lines().filter(|l| l.starts_with("| `#")) {
            let cell = line.split('|').nth(1).unwrap_or("");
            for part in cell.split('`').skip(1).step_by(2) {
                let name = part
                    .trim_start_matches("#!")
                    .trim_start_matches('#')
                    .trim_start_matches('[');
                let name: String = name
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    out.push(name);
                }
            }
        }
        out
    }

    /// The "cannot drift" claim of design.md § Attributes, enforced
    /// (B-2026-11-25-61): every registry entry is a row of its tables,
    /// every row names a registry entry, and every deferred attribute is
    /// in its deferred list and names a deferred.md heading.
    #[test]
    fn attribute_registry_matches_design_md() {
        let section = design_attributes_section();
        let listed = table_attribute_names(section);
        for name in RECOGNIZED_BARE_ATTRIBUTES {
            assert!(
                listed.iter().any(|l| l == name),
                "`#[{name}]` is in the registry but not in design.md § Attributes' tables"
            );
        }
        for name in &listed {
            // `#[cfg]` is listed with its refusal; the parser refuses it.
            assert!(
                name == "cfg" || is_recognized_bare_attribute(name),
                "design.md § Attributes lists `#[{name}]`, which the registry does not have"
            );
        }
        let deferred = include_str!("../docs/deferred.md");
        let refused = section
            .split("**Deferred attributes are refused.**")
            .nth(1)
            .expect("the deferred-attributes paragraph");
        for (name, heading) in DEFERRED_ATTRIBUTES {
            assert!(
                !is_recognized_bare_attribute(name),
                "`#[{name}]` is both registered and deferred"
            );
            assert!(
                refused.contains(&format!("`#[{name}")) || refused.contains(&format!("({name}")),
                "`#[{name}]` is refused but design.md § Attributes does not list it as deferred"
            );
            assert!(
                deferred
                    .lines()
                    .any(|l| l.starts_with('#') && l.trim_start_matches('#').trim() == *heading),
                "deferred.md has no heading `{heading}` for `#[{name}]`"
            );
        }
    }
}
