//! `[package].public_effects` in a program of several packages (design.md
//! § Inference and declarations): each package's public functions are held
//! to the setting of its own manifest, and a package that depends on one
//! whose public effects are inferred is told so
//! (`inferred_dependency_effects`), because those effects are not part of
//! the dependency's API contract.

use std::collections::HashMap;

use crate::effectchecker::PublicEffectsPolicy;
use crate::manifest::{PackageEffects, ProfileConfig};
use crate::module::ProgramTree;

/// The lint a dependency in inferred mode is reported under.
pub(crate) const LINT_NAME: &str = "inferred_dependency_effects";

/// Each dependency's `public_effects`, by the name its modules are
/// registered under: what the manifest of the package each module's file
/// belongs to sets, `declared` where none can be read.
pub(super) fn dependency_policies(tree: &ProgramTree) -> HashMap<String, PublicEffectsPolicy> {
    let mut out = HashMap::new();
    for m in &tree.modules {
        let Some(package) = &m.package else { continue };
        if m.is_synthetic || out.contains_key(package) {
            continue;
        }
        let policy = m
            .file
            .parent()
            .and_then(crate::manifest::discover_project_root)
            .and_then(|root| crate::manifest::load_from_root(&root).ok())
            .map_or(PublicEffectsPolicy::Declared, |mf| {
                mf.profile_config.public_effects
            });
        out.insert(package.clone(), policy);
    }
    out
}

/// The dependencies' code in the module tree's own span space, where the
/// flattened late phases check it: each module's spans run from its
/// `span_base` to the next module's, and the last one's to `end`.
pub(super) fn of_tree(tree: &ProgramTree, end: usize) -> Vec<PackageEffects> {
    let policies = dependency_policies(tree);
    let mut modules: Vec<&crate::module::Module> =
        tree.modules.iter().filter(|m| !m.is_synthetic).collect();
    modules.sort_by_key(|m| m.span_base);
    let mut out = Vec::new();
    for (i, m) in modules.iter().enumerate() {
        let Some(package) = &m.package else { continue };
        out.push(PackageEffects {
            start: m.span_base,
            end: modules.get(i + 1).map_or(end, |next| next.span_base),
            package: package.clone(),
            policy: policies.get(package).copied().unwrap_or_default(),
        });
    }
    out
}

/// Hold the code from `start` on, the standard library's, to `declared`
/// whatever the root package sets: the setting is the package's own.
pub(crate) fn library_declared(config: &mut ProfileConfig, start: usize) {
    if config.public_effects == PublicEffectsPolicy::Declared
        || config.package_effects.iter().any(|p| p.package.is_empty())
    {
        return;
    }
    config.package_effects.push(PackageEffects {
        start,
        end: usize::MAX,
        package: String::new(),
        policy: PublicEffectsPolicy::Declared,
    });
}

/// `inferred_dependency_effects`: one warning for each dependency the
/// root package imports whose manifest sets `public_effects = "inferred"`,
/// at the first import of it.
pub(super) fn report_inferred_dependencies(pipeline: &mut super::Pipeline) {
    use crate::lints::LintLevel;
    let Some(typed) = pipeline.typed.as_mut() else {
        return;
    };
    let level = pipeline
        .lint_overrides
        .level_for(LINT_NAME, LintLevel::Warn)
        .unwrap_or(LintLevel::Warn);
    if matches!(level, LintLevel::Allow | LintLevel::Expect) {
        return;
    }
    for import in &pipeline.profile_config.inferred_imports {
        let entry = crate::typechecker::TypeError {
            message: format!(
                "package `{0}` sets `public_effects = \"inferred\"`: the effects of its public \
                 functions are inferred from their bodies and are not part of its API contract, \
                 so a release of `{0}` can change them without notice",
                import.package
            ),
            span: crate::token::Span {
                line: import.line,
                column: import.column,
                offset: import.offset,
                length: import.length,
            },
            kind: crate::typechecker::TypeErrorKind::TypeMismatch,
            lint_name: Some(LINT_NAME.to_string()),
            fix_it: None,
            class: Some(crate::diagnostic_class::DiagnosticClass::LintWarning),
            expected: None,
            got: None,
            help: None,
        };
        if level == LintLevel::Deny {
            typed.errors.push(entry);
        } else {
            typed.warnings.push(entry);
        }
    }
}

/// What `karac build` prints, under `public_effects = "inferred"`, for the
/// package's own public functions: each one's inferred set, one to a line.
/// `None` under `declared`, and for a package with no public function.
pub(super) fn inferred_summary(pipeline: &super::Pipeline) -> Option<String> {
    let effects = pipeline.effects.as_ref()?;
    // The prelude's trait methods have no place in the text to tell them by.
    let own: Vec<&String> = effects
        .inferred_public
        .iter()
        .filter(|name| !pipeline.library_fns.contains(*name))
        .collect();
    if own.is_empty() {
        return None;
    }
    let mut out = String::from("Public effects (inferred):\n");
    for name in own {
        let set: Vec<String> = effects
            .inferred_effects
            .get(name)
            .map(|set| set.effects.iter().map(|te| te.effect.to_string()).collect())
            .unwrap_or_default();
        let shown = crate::binding_rename::display_fn_names(name);
        out.push_str(&format!(
            "  {shown}: {}\n",
            if set.is_empty() {
                "none".to_string()
            } else {
                set.join(", ")
            }
        ));
    }
    Some(out)
}
