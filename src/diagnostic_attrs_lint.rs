//! `malformed_diagnostic_attribute` lint — slices 3+4 of item 36's
//! `#[diagnostic::*]` attribute namespace entry.
//!
//! Slice 2 ([`crate::attribute_validator`]) accepts every member of the
//! `diagnostic::*` namespace silently. Slices 3 and 4 wire the actual
//! shape checks for the two compiler-known members,
//! `#[diagnostic::on_unimplemented(...)]` (slice 3, trait-only) and
//! `#[diagnostic::do_not_recommend]` (slice 4, impl-block-only, argument-
//! less). Both produce `warning[malformed_diagnostic_attribute]` for:
//!
//! 1. **Off-target** — the attribute is allowed on `trait` declarations
//!    only; on a function / struct / enum / impl / type-alias / const /
//!    extern-fn / variant / trait-method / etc. the diagnostic fires and
//!    the attribute is ignored.
//! 2. **Duplicate** — multiple `#[diagnostic::on_unimplemented]` on the
//!    same trait. The first occurrence wins (matching the parser scan in
//!    [`crate::parser::Parser::scan_on_unimplemented_attr`]); each
//!    subsequent occurrence gets its own warning.
//! 3. **Bad argument shape** — positional argument (no `name:`), unknown
//!    field name (anything other than `message` / `label` / `note`),
//!    non-string-literal value, or the `#[diagnostic::on_unimplemented =
//!    "..."]` shorthand (only the parenthesised long form is accepted).
//! 4. **Unknown placeholder** — a `{NAME}` placeholder in the message /
//!    label / note that is neither `{Self}` nor `{T0}` / `{T1}` / ... up
//!    to the trait's generic-arity. Renders literally at emit time
//!    (slice 6); the warning here fires at the trait declaration site so
//!    the author sees it once at compile time rather than every use site.
//!
//! The lint is `warn`-by-default; the registry entry is already in
//! [`crate::lints::STARTER_LINTS`] (registered in item 35 slice 1).
//! Suppression via `#[allow(malformed_diagnostic_attribute)]` works
//! through the slice-4b cross-cutting cascade (CLI `-A` / source-allow
//! on the bearing item).
//!
//! The substitution semantics for the recognised placeholders live with
//! slice 6 (failed-bound diagnostic integration); slice 3 only validates
//! that every placeholder in the trait-decl-site strings is one of the
//! known names.

use crate::ast::{
    Attribute, ExprKind, GenericParams, ImplBlock, ImplItem, Item, Program, TraitDef, TraitItem,
};
use crate::token::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LintLevel {
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct LintDiagnostic {
    pub level: LintLevel,
    pub span: Span,
    pub message: String,
}

/// Walk `program` and produce `malformed_diagnostic_attribute` warnings
/// for every misshape of `#[diagnostic::*]` the slice-3 scope covers.
///
/// CLI lint overrides flow through the standard cascade helper — `-A
/// malformed_diagnostic_attribute` suppresses; `-D
/// malformed_diagnostic_attribute` (or `-D warnings`) promotes to error.
/// Per-frame source-level cascade (`#[allow]` on the bearing item or an
/// enclosing scope) is not consulted at slice 3 — the same deferral
/// `logical_lint` and `must_use_lint` take, and tracked under the
/// per-frame-cascade `[->]` sub-bullet of slice 4b cross-cutting (line
/// 453).
pub fn check_diagnostic_attributes(
    program: &Program,
    cli_lint_overrides: &crate::lints::CliLintOverrides,
) -> Vec<LintDiagnostic> {
    let severity = crate::lints::effective_level_for_module_lint(
        false,
        false,
        false,
        cli_lint_overrides,
        "malformed_diagnostic_attribute",
    );
    if matches!(severity, crate::lints::ModuleLintSeverity::Suppress) {
        return Vec::new();
    }
    let level = match severity {
        crate::lints::ModuleLintSeverity::Deny => LintLevel::Error,
        _ => LintLevel::Warning,
    };
    let mut diags = Vec::new();
    for item in &program.items {
        walk_item(item, level, &mut diags);
    }
    diags
}

fn is_on_unimplemented_path(attr: &Attribute) -> bool {
    attr.path.len() == 2 && attr.path[0] == "diagnostic" && attr.path[1] == "on_unimplemented"
}

fn is_do_not_recommend_path(attr: &Attribute) -> bool {
    attr.path.len() == 2 && attr.path[0] == "diagnostic" && attr.path[1] == "do_not_recommend"
}

/// Walk one top-level item: dispatch the legal-target paths (trait
/// declarations for `on_unimplemented`; impl blocks for
/// `do_not_recommend`) and emit off-target warnings for every other
/// site that carries one of the slice-3/4 diagnostic attributes.
fn walk_item(item: &Item, level: LintLevel, diags: &mut Vec<LintDiagnostic>) {
    match item {
        Item::TraitDef(t) => {
            // on_unimplemented is legal here; do_not_recommend is not.
            check_trait_on_unimplemented(t, level, diags);
            emit_off_target_for(&t.attributes, "trait", &["on_unimplemented"], level, diags);
            // Trait-method declarations cannot legally carry either
            // attribute — off-target warning per method.
            for ti in &t.items {
                if let TraitItem::Method(m) = ti {
                    emit_off_target_for(&m.attributes, "trait method", &[], level, diags);
                }
            }
        }
        Item::ImplBlock(i) => {
            // do_not_recommend is legal here; on_unimplemented is not.
            check_impl_do_not_recommend(i, level, diags);
            emit_off_target_for(
                &i.attributes,
                "impl block",
                &["do_not_recommend"],
                level,
                diags,
            );
            // Impl methods do not accept either attribute — the spec
            // scopes `do_not_recommend` to impl *blocks* (the whole
            // impl is suppressed or not), not individual methods.
            for ii in &i.items {
                if let ImplItem::Method(m) = ii {
                    emit_off_target_for(&m.attributes, "impl method", &[], level, diags);
                }
            }
        }
        Item::Function(f) => emit_off_target_for(&f.attributes, "function", &[], level, diags),
        Item::StructDef(s) => {
            emit_off_target_for(&s.attributes, "struct", &[], level, diags);
            for field in &s.fields {
                emit_off_target_for(&field.attributes, "struct field", &[], level, diags);
            }
        }
        Item::UnionDef(u) => {
            emit_off_target_for(&u.attributes, "union", &[], level, diags);
            for field in &u.fields {
                emit_off_target_for(&field.attributes, "union field", &[], level, diags);
            }
        }
        Item::EnumDef(e) => {
            emit_off_target_for(&e.attributes, "enum", &[], level, diags);
            for v in &e.variants {
                emit_off_target_for(&v.attributes, "enum variant", &[], level, diags);
            }
        }
        Item::TraitAlias(t) => emit_off_target_for(&t.attributes, "trait alias", &[], level, diags),
        Item::MarkerTrait(t) => {
            emit_off_target_for(&t.attributes, "marker trait", &[], level, diags)
        }
        Item::ConstDecl(c) => emit_off_target_for(&c.attributes, "module const", &[], level, diags),
        Item::ModuleBinding(b) => {
            emit_off_target_for(&b.attributes, "module-level binding", &[], level, diags)
        }
        Item::TypeAlias(t) => emit_off_target_for(&t.attributes, "type alias", &[], level, diags),
        Item::DistinctType(d) => {
            emit_off_target_for(&d.attributes, "distinct type", &[], level, diags)
        }
        Item::ExternFunction(f) => {
            emit_off_target_for(&f.attributes, "extern function", &[], level, diags)
        }
        Item::ExternBlock(b) => {
            emit_off_target_for(&b.attributes, "extern block", &[], level, diags);
            for it in &b.items {
                use crate::ast::ExternItem;
                match it {
                    ExternItem::Function(f) => {
                        emit_off_target_for(&f.attributes, "extern function", &[], level, diags);
                    }
                    ExternItem::OpaqueType(o) => {
                        emit_off_target_for(&o.attributes, "extern opaque type", &[], level, diags);
                    }
                }
            }
        }
        Item::LayoutDef(l) => emit_off_target_for(&l.attributes, "layout block", &[], level, diags),
        Item::TestCase(t) => emit_off_target_for(&t.attributes, "test case", &[], level, diags),
        // Effect / use / import / alias / independent decls carry no
        // attributes at the AST level (slice 2's namespace dispatch
        // walks the same set of kinds), so there is no surface for an
        // `on_unimplemented` attribute to attach to.
        Item::EffectResource(_)
        | Item::EffectGroup(_)
        | Item::EffectVerbDecl(_)
        | Item::UseDecl(_)
        | Item::Import(_)
        | Item::AliasDecl(_)
        | Item::IndependentDecl(_) => {}
    }
}

/// Emit off-target warnings for every `#[diagnostic::on_unimplemented]`
/// or `#[diagnostic::do_not_recommend]` on `attrs` whose member name is
/// not in `legal_here`. Callers pass `&["on_unimplemented"]` on traits
/// (so the legal-target on_unimplemented gets validated separately by
/// [`check_trait_on_unimplemented`] without producing a spurious
/// off-target warning), `&["do_not_recommend"]` on impl blocks (similar
/// pairing with [`check_impl_do_not_recommend`]), and `&[]` everywhere
/// else.
fn emit_off_target_for(
    attrs: &[Attribute],
    target_kind: &str,
    legal_here: &[&str],
    level: LintLevel,
    diags: &mut Vec<LintDiagnostic>,
) {
    for attr in attrs {
        let member = if is_on_unimplemented_path(attr) {
            "on_unimplemented"
        } else if is_do_not_recommend_path(attr) {
            "do_not_recommend"
        } else {
            continue;
        };
        if legal_here.contains(&member) {
            continue;
        }
        let only_valid_on = match member {
            "on_unimplemented" => "`trait` declarations",
            "do_not_recommend" => "`impl` blocks",
            _ => unreachable!(),
        };
        diags.push(LintDiagnostic {
            level,
            span: attr.span.clone(),
            message: format!(
                "warning[malformed_diagnostic_attribute]: \
                 `#[diagnostic::{member}]` is only valid on \
                 {only_valid_on}; applied here to a {target_kind} \
                 — attribute ignored"
            ),
        });
    }
}

/// Run validation on every `#[diagnostic::do_not_recommend]` attached
/// to an impl block. First-occurrence-wins matches the parser scan's
/// boolean flag; each subsequent occurrence gets a duplicate warning.
/// The attribute is spec'd as argument-less; any argument-bearing form
/// (parens or `= "..."`) emits a separate shape warning.
fn check_impl_do_not_recommend(i: &ImplBlock, level: LintLevel, diags: &mut Vec<LintDiagnostic>) {
    let mut seen_first = false;
    for attr in &i.attributes {
        if !is_do_not_recommend_path(attr) {
            continue;
        }
        if seen_first {
            diags.push(LintDiagnostic {
                level,
                span: attr.span.clone(),
                message: "warning[malformed_diagnostic_attribute]: \
                     duplicate `#[diagnostic::do_not_recommend]` on the same \
                     impl — only the first attribute is observed; remove the \
                     duplicates"
                    .to_string(),
            });
            continue;
        }
        seen_first = true;
        if !attr.args.is_empty() || attr.string_value.is_some() {
            diags.push(LintDiagnostic {
                level,
                span: attr.span.clone(),
                message: "warning[malformed_diagnostic_attribute]: \
                     `#[diagnostic::do_not_recommend]` takes no arguments — \
                     drop the parenthesised form and write the bare \
                     `#[diagnostic::do_not_recommend]`"
                    .to_string(),
            });
        }
    }
}

/// Run shape + placeholder validation on every
/// `#[diagnostic::on_unimplemented]` attached to a trait declaration.
/// First-occurrence-wins matches the parser scan; each subsequent
/// occurrence gets a duplicate-warning so the author sees both spans.
fn check_trait_on_unimplemented(t: &TraitDef, level: LintLevel, diags: &mut Vec<LintDiagnostic>) {
    let mut seen_first = false;
    for attr in &t.attributes {
        if !is_on_unimplemented_path(attr) {
            continue;
        }
        if seen_first {
            diags.push(LintDiagnostic {
                level,
                span: attr.span.clone(),
                message: "warning[malformed_diagnostic_attribute]: \
                     duplicate `#[diagnostic::on_unimplemented]` on the same \
                     trait — only the first attribute is used; remove the \
                     duplicates"
                    .to_string(),
            });
            continue;
        }
        seen_first = true;
        validate_attr_shape(attr, &t.generic_params, level, diags);
    }
}

/// Validate the argument shape of a single
/// `#[diagnostic::on_unimplemented(...)]` and the placeholders in any
/// string values it carries.
fn validate_attr_shape(
    attr: &Attribute,
    generics: &Option<GenericParams>,
    level: LintLevel,
    diags: &mut Vec<LintDiagnostic>,
) {
    // The `#[diagnostic::on_unimplemented = "..."]` shorthand is not
    // accepted — only the parenthesised long form. This mirrors how
    // `#[derive = "..."]` would be rejected too: the shorthand is a
    // per-attribute design choice, and on_unimplemented uses three
    // distinct optional fields where a single string would be
    // ambiguous (is it the message? the label? the note?).
    if attr.string_value.is_some() {
        diags.push(LintDiagnostic {
            level,
            span: attr.span.clone(),
            message: "warning[malformed_diagnostic_attribute]: \
                 `#[diagnostic::on_unimplemented = \"...\"]` is not a \
                 recognised shape; use the parenthesised form with named \
                 fields, e.g. `#[diagnostic::on_unimplemented(message: \
                 \"...\")]`"
                .to_string(),
        });
        return;
    }
    let mut seen_message = false;
    let mut seen_label = false;
    let mut seen_note = false;
    for arg in &attr.args {
        let Some(name) = &arg.name else {
            diags.push(LintDiagnostic {
                level,
                span: arg.span.clone(),
                message: "warning[malformed_diagnostic_attribute]: \
                     `#[diagnostic::on_unimplemented]` requires named \
                     arguments — `message: \"...\"`, `label: \"...\"`, \
                     and/or `note: \"...\"`"
                    .to_string(),
            });
            continue;
        };
        let seen_slot = match name.as_str() {
            "message" => &mut seen_message,
            "label" => &mut seen_label,
            "note" => &mut seen_note,
            other => {
                diags.push(LintDiagnostic {
                    level,
                    span: arg.span.clone(),
                    message: format!(
                        "warning[malformed_diagnostic_attribute]: \
                         `#[diagnostic::on_unimplemented]` does not accept \
                         field `{other}`; the accepted fields are \
                         `message`, `label`, `note`"
                    ),
                });
                continue;
            }
        };
        if *seen_slot {
            diags.push(LintDiagnostic {
                level,
                span: arg.span.clone(),
                message: format!(
                    "warning[malformed_diagnostic_attribute]: \
                     `#[diagnostic::on_unimplemented]` field `{name}` \
                     specified more than once — first occurrence wins"
                ),
            });
            continue;
        }
        let Some(value_expr) = &arg.value else {
            diags.push(LintDiagnostic {
                level,
                span: arg.span.clone(),
                message: format!(
                    "warning[malformed_diagnostic_attribute]: \
                     `#[diagnostic::on_unimplemented]` field `{name}` \
                     requires a string-literal value"
                ),
            });
            continue;
        };
        let ExprKind::StringLit(s) = &value_expr.kind else {
            diags.push(LintDiagnostic {
                level,
                span: arg.span.clone(),
                message: format!(
                    "warning[malformed_diagnostic_attribute]: \
                     `#[diagnostic::on_unimplemented]` field `{name}` \
                     requires a string-literal value"
                ),
            });
            continue;
        };
        *seen_slot = true;
        validate_placeholders(s, &arg.span, generics, level, diags);
    }
}

/// Walk a recognised string and emit an `unknown placeholder` warning
/// for every `{NAME}` that is neither `{Self}` nor `{T0}` / `{T1}` /
/// ... within the trait's generic-arity. The substitution itself
/// happens at the failed-bound emit site (slice 6) using the solved
/// metavariable map.
///
/// Unbalanced `{`/`}` are silently tolerated — the message renders
/// literally and the slice-6 emit path will print the raw `{` if it
/// reaches a malformed brace pair. The lint focuses on misspelled
/// names like `{NotAParam}` rather than syntactic typos that the
/// failed-bound formatter will handle.
fn validate_placeholders(
    s: &str,
    arg_span: &Span,
    generics: &Option<GenericParams>,
    level: LintLevel,
    diags: &mut Vec<LintDiagnostic>,
) {
    let arity = generics.as_ref().map(|g| g.params.len()).unwrap_or(0);
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'{' {
            i += 1;
            continue;
        }
        // Find the matching `}` on the same name run. The placeholder
        // shape is `{IDENT}`; anything that isn't a valid placeholder
        // ident-shape is left for the emit-time formatter to handle.
        let start = i + 1;
        let mut end = start;
        while end < bytes.len() && bytes[end] != b'}' {
            end += 1;
        }
        if end >= bytes.len() {
            // No closing `}` — bail; the message will render literally.
            break;
        }
        let name = &s[start..end];
        if !is_known_placeholder(name, arity) {
            diags.push(LintDiagnostic {
                level,
                span: arg_span.clone(),
                message: format!(
                    "warning[malformed_diagnostic_attribute]: \
                     unknown placeholder `{{{name}}}` in \
                     `#[diagnostic::on_unimplemented]` — the recognised \
                     placeholders are `{{Self}}` and `{{T0}}` … `{{T{}}}` \
                     for this trait's generic parameters; unrecognised \
                     placeholders render literally at the use site",
                    arity.saturating_sub(1),
                ),
            });
        }
        i = end + 1;
    }
}

fn is_known_placeholder(name: &str, arity: usize) -> bool {
    if name == "Self" {
        return true;
    }
    if let Some(rest) = name.strip_prefix('T') {
        if let Ok(idx) = rest.parse::<usize>() {
            return idx < arity;
        }
    }
    false
}

/// Substitute the slice-6 placeholders `{Self}` / `{T0}` / `{T1}` / ...
/// in `template` against the call-site's resolved metavariables.
///
/// - `self_render` replaces every `{Self}`.
/// - `generic_arg_renders[i]` replaces every `{T<i>}` when the slot is
///   present; missing or `None` slots leave the placeholder literal.
/// - Unknown placeholder names (and unbalanced braces) render literally
///   — the slice-3 lint already warned at the trait declaration site so
///   the author saw the diagnostic once at definition; literal-render at
///   the use site is the documented fallback.
///
/// Shared placeholder grammar with [`validate_placeholders`]: the lint
/// pass and the substituter must recognise the same names, otherwise an
/// author could write a placeholder the validator accepts but the
/// substituter ignores (or vice-versa). Keeping both helpers in this
/// module enforces the grammar in one place.
pub fn substitute_placeholders(
    template: &str,
    self_render: &str,
    generic_arg_renders: &[Option<String>],
) -> String {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'{' {
            out.push(bytes[i] as char);
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut end = start;
        while end < bytes.len() && bytes[end] != b'}' {
            end += 1;
        }
        if end >= bytes.len() {
            // No closing brace — emit the rest literally.
            out.push_str(&template[i..]);
            break;
        }
        let name = &template[start..end];
        if name == "Self" {
            out.push_str(self_render);
        } else if let Some(idx) = name.strip_prefix('T').and_then(|r| r.parse::<usize>().ok()) {
            match generic_arg_renders.get(idx).and_then(|x| x.as_ref()) {
                Some(rendered) => out.push_str(rendered),
                // Unknown / unsolved slot — render literally so the
                // author can see which placeholder didn't substitute.
                None => out.push_str(&template[i..=end]),
            }
        } else {
            // Unknown placeholder name — render literally.
            out.push_str(&template[i..=end]);
        }
        i = end + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitute_self_only() {
        let s = substitute_placeholders("{Self} is not Foo", "i64", &[]);
        assert_eq!(s, "i64 is not Foo");
    }

    #[test]
    fn substitute_with_generics() {
        let s = substitute_placeholders(
            "{Self} cannot map ({T0}, {T1})",
            "MyType",
            &[Some("K".into()), Some("V".into())],
        );
        assert_eq!(s, "MyType cannot map (K, V)");
    }

    #[test]
    fn substitute_missing_slot_renders_literal() {
        let s = substitute_placeholders("{T0} and {T1}", "X", &[Some("A".into()), None]);
        assert_eq!(s, "A and {T1}");
    }

    #[test]
    fn substitute_unknown_name_renders_literal() {
        let s = substitute_placeholders("hello {NotAParam}", "X", &[]);
        assert_eq!(s, "hello {NotAParam}");
    }

    #[test]
    fn substitute_unbalanced_brace_renders_literal() {
        let s = substitute_placeholders("oops {Self {", "X", &[]);
        assert_eq!(s, "oops {Self {");
    }

    #[test]
    fn substitute_multiple_self() {
        let s = substitute_placeholders("{Self} or {Self}", "i64", &[]);
        assert_eq!(s, "i64 or i64");
    }

    #[test]
    fn substitute_empty_template() {
        let s = substitute_placeholders("", "X", &[]);
        assert_eq!(s, "");
    }
}
