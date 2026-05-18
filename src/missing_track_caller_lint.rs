//! `missing_track_caller` lint — slice 7 of the `#[track_caller]` for
//! stdlib panic-emitters entry
//! (`docs/implementation_checklist/phase-5-diagnostics.md` § `#[track_caller]`
//! for stdlib panic-emitters, slice 7).
//!
//! **Role.** Stdlib-hygiene lint. Warns when a baked stdlib `pub fn`
//! has `panics` in its declared or inferred effect set but does not
//! carry `#[track_caller]`. The lint surfaces the candidate set for
//! slice 6 (stdlib annotations) mechanically — every `pub fn` whose
//! panic point would surface at the stdlib frame instead of the
//! caller's site shows up here, so the slice-6 annotation pass has a
//! complete checklist.
//!
//! **Scope (stdlib only, like `missing_must_use`).** Walks every
//! `Item::Function` and every inherent-impl method
//! (`ImplItem::Method` inside an `Item::ImplBlock` whose `trait_name`
//! is `None`). Fires only when `stdlib_origin == true` and the
//! function is `pub`. User code (`stdlib_origin == false`) is silent
//! by design — the spec scopes the lint to stdlib hygiene. Trait-impl
//! methods are skipped: the `#[track_caller]` semantic on a trait
//! method belongs on the trait declaration (slice 6), not on every
//! implementation; linting impls would force every implementor to
//! re-annotate.
//!
//! **What's already excluded.** Functions carrying `#[track_caller]`
//! or `#[allow(missing_track_caller)]` are skipped. Functions with no
//! `panics` effect (neither declared nor inferred) are skipped —
//! `panics` is the trigger, and the lint stays quiet on stdlib
//! functions that don't panic.
//!
//! **Diagnostic shape** mirrors the other lint modules' three-piece
//! rustc-style (primary / `= note:` / `= help:`). The primary message
//! names the function and identifies the panic-source rule; the note
//! explains *why* `#[track_caller]` matters (panic point reporting);
//! the help suggests adding the attribute.

use crate::ast::{Attribute, EffectVerbKind, Function, ImplBlock, ImplItem, Item, Program};
use crate::effectchecker::EffectCheckResult;
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
    pub lint_name: String,
    pub help: Option<String>,
    pub note: Option<String>,
}

/// Run the `missing_track_caller` lint over the parsed program. Reads
/// `EffectCheckResult.inferred_effects` to detect the `panics` effect
/// on each function's effect set; the AST itself only carries declared
/// effects, so the effect-checker pass must have run for inferred
/// `panics` (via transitive call analysis) to flow through.
///
/// Returns an empty vec when `effects` is `None` (the effect checker
/// didn't run — typically because an earlier phase failed and the CLI
/// short-circuited) so the lint becomes a no-op rather than firing on
/// stale or absent data.
pub fn check_missing_track_caller(
    program: &Program,
    effects: Option<&EffectCheckResult>,
    cli_lint_overrides: &crate::lints::CliLintOverrides,
) -> Vec<LintDiagnostic> {
    let Some(effects) = effects else {
        return Vec::new();
    };
    let cli_severity = crate::lints::effective_level_for_module_lint(
        false,
        false,
        false,
        cli_lint_overrides,
        "missing_track_caller",
    );
    if matches!(cli_severity, crate::lints::ModuleLintSeverity::Suppress) {
        return Vec::new();
    }
    let default_level = match cli_severity {
        crate::lints::ModuleLintSeverity::Deny => LintLevel::Error,
        _ => LintLevel::Warning,
    };
    let mut diags: Vec<LintDiagnostic> = Vec::new();
    for item in &program.items {
        match item {
            Item::Function(f) => check_free_function(f, default_level, effects, &mut diags),
            Item::ImplBlock(imp) => check_impl_block(imp, default_level, effects, &mut diags),
            _ => {}
        }
    }
    diags
}

fn check_free_function(
    f: &Function,
    default_level: LintLevel,
    effects: &EffectCheckResult,
    diags: &mut Vec<LintDiagnostic>,
) {
    if !f.stdlib_origin || !f.is_pub {
        return;
    }
    check_function_with_key(f, &f.name, default_level, effects, diags);
}

fn check_impl_block(
    imp: &ImplBlock,
    default_level: LintLevel,
    effects: &EffectCheckResult,
    diags: &mut Vec<LintDiagnostic>,
) {
    // Trait-impl methods inherit `#[track_caller]` propagation from the
    // trait declaration (slice 4 of the entry: when the trait method
    // declaration is `#[track_caller]`, every impl method inherits the
    // flag unless explicitly dropped). Linting impls here would force
    // every implementor to carry the attribute separately — wrong
    // layering. Skip.
    if imp.trait_name.is_some() {
        return;
    }
    let Some(target_name) = impl_target_name(imp) else {
        return;
    };
    for it in &imp.items {
        if let ImplItem::Method(m) = it {
            if !m.stdlib_origin {
                continue;
            }
            // Same lookup-key shape as `EffectChecker` uses for
            // inherent methods: `"Target.method"`.
            let key = format!("{}.{}", target_name, m.name);
            check_function_with_key(m, &key, default_level, effects, diags);
        }
    }
}

/// Render the impl target as a string name (e.g. `"Command"` for
/// `impl Command { … }`). Returns `None` for impl targets that don't
/// reduce to a simple named path — those are exotic and not part of
/// the stdlib-hygiene surface.
fn impl_target_name(imp: &ImplBlock) -> Option<String> {
    use crate::ast::TypeKind;
    match &imp.target_type.kind {
        TypeKind::Path(p) => p.segments.last().cloned(),
        _ => None,
    }
}

fn check_function_with_key(
    f: &Function,
    key: &str,
    default_level: LintLevel,
    effects: &EffectCheckResult,
    diags: &mut Vec<LintDiagnostic>,
) {
    if f.is_track_caller {
        return;
    }
    if has_allow_missing_track_caller(&f.attributes) {
        return;
    }
    if !has_panics_effect(f, key, effects) {
        return;
    }
    let resource = panic_resource_name(f, key, effects).unwrap_or_default();
    let qualified = if key == f.name {
        f.name.clone()
    } else {
        key.to_string()
    };
    let message = format!(
        "stdlib `pub fn {qualified}` has `panics{}` in its effect set but lacks `#[track_caller]`",
        if resource.is_empty() {
            String::new()
        } else {
            format!("({resource})")
        },
    );
    let note = Some(
        "without `#[track_caller]`, a panic from this function reports the stdlib frame as \
         the panic site — callers see the implementation line, not their own call site"
            .to_string(),
    );
    let help = Some(
        "add `#[track_caller]` above the function declaration so the panic-site fields \
         (file/line/col) point at the caller"
            .to_string(),
    );
    diags.push(LintDiagnostic {
        level: default_level,
        span: f.span.clone(),
        message,
        lint_name: "missing_track_caller".to_string(),
        help,
        note,
    });
}

fn has_panics_effect(f: &Function, key: &str, effects: &EffectCheckResult) -> bool {
    // Declared effects on the function signature take priority — they
    // are the author's stated contract. Inferred effects fill in for
    // private fns / unannotated fns where the effect checker derived
    // the set from the body.
    if declared_has_panics(f) {
        return true;
    }
    if let Some(set) = effects.inferred_effects.get(key) {
        return set
            .effects
            .iter()
            .any(|t| matches!(t.effect.verb, EffectVerbKind::Panics));
    }
    false
}

fn declared_has_panics(f: &Function) -> bool {
    let Some(declared) = &f.effects else {
        return false;
    };
    use crate::ast::EffectItem;
    declared.items.iter().any(|i| match i {
        EffectItem::Verb(v) => matches!(v.kind, EffectVerbKind::Panics),
        _ => false,
    })
}

/// Extract the panic-effect's resource name when present (for the
/// diagnostic message). Falls back to the declared effect resource if
/// declared; otherwise reads from the inferred set. Returns the empty
/// string when the panic effect carries no resource (the `panics`
/// execution verb takes no resource per design.md).
fn panic_resource_name(f: &Function, key: &str, effects: &EffectCheckResult) -> Option<String> {
    use crate::ast::EffectItem;
    if let Some(declared) = &f.effects {
        for i in &declared.items {
            if let EffectItem::Verb(v) = i {
                if matches!(v.kind, EffectVerbKind::Panics) {
                    // `panics` is an execution verb — takes no
                    // resource per design.md. Return None so the
                    // caller renders the bare `panics` form.
                    if v.resources.is_empty() {
                        return None;
                    }
                    return Some(v.resources[0].path.join("."));
                }
            }
        }
    }
    if let Some(set) = effects.inferred_effects.get(key) {
        for t in &set.effects {
            if matches!(t.effect.verb, EffectVerbKind::Panics) {
                if t.effect.resource.is_empty() {
                    return None;
                }
                return Some(t.effect.resource.clone());
            }
        }
    }
    None
}

fn has_allow_missing_track_caller(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.is_bare("allow")
            && attr.args.iter().any(|arg| {
                use crate::ast::ExprKind;
                let name_matches = arg
                    .name
                    .as_deref()
                    .map(|n| n == "missing_track_caller")
                    .unwrap_or(false);
                if name_matches {
                    return true;
                }
                if let Some(v) = &arg.value {
                    if let ExprKind::Identifier(id) = &v.kind {
                        return id == "missing_track_caller";
                    }
                    if let ExprKind::Path { segments, .. } = &v.kind {
                        if segments.len() == 1 && segments[0] == "missing_track_caller" {
                            return true;
                        }
                    }
                }
                false
            })
    })
}
