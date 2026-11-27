//! `karac build` / `build-project` / `doc` — AOT artifact production.
//!
//! Extracted verbatim from `cli.rs` (structural-debt extraction, slice 3).

use super::*;
use rustc_hash::FxHashMap;

/// Project-mode build entry point.
///
/// Discovers the project root via `kara.toml` walk-up, loads the manifest,
/// walks `src/` to map each `.kara` file to a module path (CR-24 slice 3),
/// parses every file into its own `Program`, assembles the module graph
/// Render documentation for the current project. v1 MVP — walks the
/// project tree, parses every module, and emits one HTML page per
/// documented item under `dist/doc/`. Items without `///` doc comments
/// are skipped silently. Resolver / typechecker passes are intentionally
/// not run: doc rendering only needs the AST surface, and producing
/// docs against a project that doesn't fully type-check is useful for
/// a programmer trying to understand half-finished code.
pub(super) fn cmd_doc() {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            process::exit(1);
        }
    };

    let (root, mf) = match manifest::load_from_cwd(&cwd) {
        Ok(ok) => ok,
        Err(e) => {
            emit_manifest_error(&e, OutputMode::Text);
            process::exit(1);
        }
    };

    let walked = match walker::walk_project(&root, WalkerOpts::default()) {
        Ok(w) => w,
        Err(e) => {
            emit_walker_error(&e, OutputMode::Text);
            process::exit(1);
        }
    };

    let built = match module::build_program_tree(&walked) {
        Ok(ok) => ok,
        Err(e) => {
            emit_build_tree_error(&e, OutputMode::Text);
            process::exit(1);
        }
    };

    let BuildTreeOk { tree, parse_errors } = built;
    if !parse_errors.is_empty() {
        // Surface parse errors but keep going — render docs for what
        // parsed cleanly. The user can iterate. The run still fails (DC4):
        // a script or CI step must not read the partial docs as a success.
        print_parse_errors_text(&parse_errors);
    }

    // Run effectcheck once on a merged Program containing every
    // non-synthetic module's items so cross-module callee resolution
    // works at the bare-name level the effectchecker indexes by. See
    // `build_doc_effects_table` for the trade-offs.
    let effects = build_doc_effects_table(&tree);

    // A workspace member's docs go under the shared `dist/` of its
    // workspace root, one directory per member (B-2026-11-13-60).
    let out_root = output_root(&root);
    let output_dir = if out_root == root {
        root.join("dist").join("doc")
    } else {
        out_root.join("dist").join("doc").join(&mf.name)
    };
    match crate::doc::build_docs(
        &tree,
        &output_dir,
        Some(&effects),
        walked.entry == walker::EntryKind::Lib,
    ) {
        Ok(result) => {
            println!(
                "rendered {} doc page(s) under {}",
                result.written.len().saturating_sub(1), // minus the index
                output_dir.display()
            );
            if !parse_errors.is_empty() {
                eprintln!(
                    "error[doc]: the docs are incomplete: {} file(s) did not parse",
                    parse_errors.len()
                );
                process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("error[doc]: {e}");
            process::exit(1);
        }
    }
}

/// Build the `(module_path, fn_name) → EffectDisplay` table consumed
/// by the doc renderer.
///
/// Strategy: merge every non-synthetic module's items into a single
/// `Program` and run `effectcheck` once. The effectchecker indexes
/// functions by bare name, and cross-module call sites also resolve
/// to bare names (Kāra's `import` brings a callee into scope under
/// its bare name). A per-module check would treat every cross-module
/// call as effect-empty — `pub fn`s whose inferred set depends on a
/// callee in another module would surface incomplete `with` clauses
/// in the rendered docs.
///
/// Trade-off: when two modules define functions with the same bare
/// name, the merge keeps only one and the doc display is approximate.
/// This is doc-only; the main pipeline (`build`, `check`, `run`)
/// still runs effectcheck per-module via the regular phase wiring.
/// Effectcheck errors raised by the merged pass (e.g. duplicate
/// resource declarations across modules, missing effect declarations)
/// are deliberately ignored here — the doc renderer is best-effort.
pub(super) fn build_doc_effects_table(tree: &ProgramTree) -> crate::doc::EffectsByItem {
    use crate::ast::Item;
    use crate::doc::{EffectDisplay, EffectsByItem};
    use crate::effectchecker::{DeclaredEffects, EffectSet};

    let mut merged_items = Vec::new();
    for module in &tree.modules {
        if module.is_synthetic {
            continue;
        }
        merged_items.extend(module.items.iter().cloned());
    }
    let merged_program = Program {
        items: merged_items,
        ..Program::default()
    };
    let effects = crate::effectcheck(&merged_program);

    let mut out: EffectsByItem = std::collections::HashMap::new();
    for module in &tree.modules {
        if module.is_synthetic {
            continue;
        }
        // A pub free fn is keyed by its name; a pub inherent method by
        // `Type.method`, the key the effect checker records it under, so a
        // method page row shows the same clause a free fn does
        // (B-2026-11-24-5).
        let mut fns: Vec<(String, &crate::ast::Function)> = Vec::new();
        for item in &module.items {
            match item {
                Item::Function(f) => fns.push((f.name.clone(), f)),
                Item::ImplBlock(imp) if imp.trait_name.is_none() => {
                    let crate::ast::TypeKind::Path(p) = &imp.target_type.kind else {
                        continue;
                    };
                    let Some(ty) = p.segments.last() else {
                        continue;
                    };
                    for it in &imp.items {
                        if let crate::ast::ImplItem::Method(m) = it {
                            fns.push((format!("{ty}.{}", m.name), &**m));
                        }
                    }
                }
                _ => {}
            }
        }
        for (key, f) in fns {
            if !f.is_pub {
                continue;
            }

            // Prefer the declared annotation (the user's contract);
            // fall back to the inferred set if no explicit annotation.
            let display = match effects.declared_effects.get(&key) {
                Some(DeclaredEffects::Explicit(set)) => effect_set_to_display(set, false),
                Some(DeclaredEffects::Polymorphic) => EffectDisplay {
                    effects: Vec::new(),
                    polymorphic: true,
                },
                Some(DeclaredEffects::PolymorphicWithFixed(set)) => {
                    effect_set_to_display(set, true)
                }
                Some(DeclaredEffects::None) | None => effects
                    .inferred_effects
                    .get(&key)
                    .map(|set: &EffectSet| effect_set_to_display(set, false))
                    .unwrap_or_default(),
            };

            if !display.effects.is_empty() || display.polymorphic {
                out.insert((module.path.clone(), key), display);
            }
        }
    }

    out
}

pub(super) fn effect_set_to_display(
    set: &crate::effectchecker::EffectSet,
    polymorphic: bool,
) -> crate::doc::EffectDisplay {
    let mut effects: Vec<(crate::ast::EffectVerbKind, String)> = set
        .effects
        .iter()
        .map(|t| (t.effect.verb.clone(), t.effect.resource.clone()))
        .collect();
    // Stable order across runs: by verb name, then resource.
    effects.sort_by(|a, b| {
        let an = effect_verb_str(&a.0);
        let bn = effect_verb_str(&b.0);
        an.cmp(bn).then_with(|| a.1.cmp(&b.1))
    });
    crate::doc::EffectDisplay {
        effects,
        polymorphic,
    }
}

/// A late-phase rejection from [`run_multi_file_late_phases`]: the phase that
/// refused and the rendered diagnostics.
pub(super) struct MultiFileLateFailure {
    pub(super) phase: String,
    pub(super) message: String,
    /// The same findings, structured — what `karac check --output=json` emits
    /// so `karac fix` and the Mend loop get file/line/column rather than a
    /// pre-rendered blob (B-2026-08-27-27).
    pub(super) diags: Vec<LateDiag>,
}

/// Flatten a project into one super-program and run every pass `karac build`
/// gates on between the per-module passes and codegen: resolve, typecheck,
/// lower, EFFECT, OWNERSHIP, concurrency.
///
/// B-2026-08-27-27 — extracted from `run_multi_file_codegen` so `karac check`
/// runs the same passes. It did not: `package_check_on` stopped after the
/// per-module resolve + typecheck, so a project whose public function performed
/// an undeclared effect printed "All checks passed." and then failed to build,
/// with the same binary on the same sources. The ownership half diverged
/// identically — a use-after-move passed `check` and failed `build`.
///
/// That is not merely a missing check. `karac check --output=json` is the entry
/// point of the Mend loop (CLAUDE.md § "Developing Kāra code"), so an error
/// class `check` cannot see is one Mend cannot converge on: the loop reports
/// success and the failure appears only at build.
///
/// NOT `#[cfg(feature = "llvm")]`, unlike `run_multi_file_codegen` — `karac
/// check` has to work in the default build, and none of these passes needs a
/// backend.
///
/// The caller has already verified parse / cycles / resolve / typecheck
/// PER MODULE; this re-runs resolve and typecheck on the flattened unit because
/// the flattening is what makes cross-module effects visible at all.
/// `core_places`: a v2 command runs behind this check
/// ([`crate::lints::CliLintOverrides::core_places`]).
pub(super) fn run_multi_file_late_phases(
    tree: &ProgramTree,
    package_name: &str,
    core_places: bool,
    lib: Option<&PackageLibrary>,
    manifest: Option<&manifest::Manifest>,
) -> Result<Pipeline, MultiFileLateFailure> {
    // 1. Topological emission order — dependencies before dependents.
    let order = module::emission_order(tree);

    // 2. Concatenate items. Drop `import` declarations (their effect was
    // resolved upstream by per-module resolve) and skip synthetic
    // modules. Items keep their original spans, which downstream
    // diagnostics use for line:col reporting.
    //
    // While concatenating, build a `ModuleSpanTable`: for each non-
    // synthetic module we register its file path once, then walk every
    // appended item's spans so late-phase diagnostics can recover the
    // file-of-origin via `format_pipeline_errors`.
    let mut super_items: Vec<Item> = Vec::new();
    let mut span_table = crate::span_visitor::ModuleSpanTable::new();
    // Flattening drops every `import` declaration and puts all modules in one
    // scope, which breaks two things the tree-aware per-module passes got
    // right: an ALIAS binding names nothing once its import is gone
    // (B-2026-07-29-14), and two modules declaring the same top-level name
    // collide (B-2026-08-20-24). `module_rename` repairs both, per module, in
    // one substitution — and `karac run`'s merge calls the same helper, so the
    // two cannot answer differently again. A module that needs neither is
    // copied byte-identically.
    let renames = crate::module_rename::plan(tree);
    for &id in &order {
        let m = &tree.modules[id];
        if m.is_synthetic {
            continue;
        }
        let module_idx = span_table.register_module(m.file.clone());
        for item in crate::module_rename::flatten_module_items(tree, id, &renames) {
            span_table.record_item(module_idx, &item);
            super_items.push(item);
        }
    }

    // Phase-10 (`std.web`): gated baked stdlib modules are synthetic, so
    // the loop above never carries their items — an imported `fetch`
    // resolves and typechecks per-module (those passes chase the tree)
    // but its body would be missing here. Append the expansion of every
    // gated import found in user modules, deduplicated on the bound name
    // so two files importing the same item don't define it twice.
    {
        let mut seen: std::collections::HashSet<(Vec<String>, String)> =
            std::collections::HashSet::new();
        for m in &tree.modules {
            if m.is_synthetic {
                continue;
            }
            for imp in &m.imports {
                // `import std.path.*;` names every public item.
                let named = if imp.is_wildcard && imp.items.is_empty() {
                    crate::prelude::gated_module_public_items(&imp.path, imp.span)
                        .unwrap_or_else(|| imp.items.clone())
                } else {
                    imp.items.clone()
                };
                let deduped: Vec<crate::ast::ImportItem> = named
                    .iter()
                    .filter(|ii| {
                        let bound = ii.alias.as_ref().unwrap_or(&ii.name);
                        seen.insert((imp.path.clone(), bound.clone()))
                    })
                    .cloned()
                    .collect();
                if let Some(expansion) = crate::prelude::gated_items_for_import(&imp.path, &deduped)
                {
                    super_items.extend(expansion);
                }
            }
        }
    }
    crate::prelude::dedup_spliced_items(&mut super_items);

    let super_program = PackageLibrary::program(lib, &super_items);
    let library_fns = match lib {
        Some(lib) => library_fn_names(&super_program, lib.start),
        None => std::collections::HashSet::new(),
    };

    // 3. Drive the rest of the pipeline by hand-constructing a Pipeline
    // with the synthetic ParseResult. This mirrors what `Pipeline::new`
    // would do on a single-file source, except we skip the parse step
    // entirely (we have a pre-built Program already).
    let parsed = ParseResult {
        program: super_program,
        errors: Vec::new(),
        // Stitched from already-parsed modules; no statement ever sat at
        // this super-program's top level.
        had_top_level_statements: false,
        fix_edits: FxHashMap::default(),
        fix_diffs: FxHashMap::default(),
    };
    let mut pipeline = Pipeline {
        filename: package_name.to_string(),
        target_skipped: std::collections::HashMap::new(),
        // No single source text: `parsed` is a super-program stitched from
        // every module. `filename` is the package NAME, not a path, so it must
        // never be read from disk (B-2026-08-04-7).
        source: None,
        parsed,
        // Project mode reaches this only for codegen-bound commands
        // (`build`/`run` on a package); the interpreter-bound single-file path
        // sets it through `Pipeline::interpreter_bound`.
        codegen_bound: true,
        resolved: None,
        typed: None,
        effects: None,
        ownership: None,
        concurrency: None,
        provider_escape: None,
        simd_errors: None,
        comptime_errors: None,
        // The package's profile, as the build runs these passes under it
        // (B-2026-11-21-77).
        profile: manifest.map_or(crate::manifest::CompileProfile::Default, |m| m.profile),
        profile_config: manifest.map_or_else(crate::manifest::ProfileConfig::default, |m| {
            m.profile_config.clone()
        }),
        lint_overrides: crate::lints::CliLintOverrides {
            core_places,
            ..Default::default()
        },
        // Project mode already fails a build on any ownership error, use after
        // move included, so it needs no strict mode for C1.
        strict_core: false,
        library_len: None,
        library_fns,
        renames: Vec::new(),
        gated_imports: Vec::new(),
        user_source: None,
        library_text: None,
        mir_program: None,
        library_pruned: None,
        prune_library: true,
        library_bodies: Default::default(),
    };
    // Each package's public functions are held to its own `public_effects`,
    // the library's to `declared`.
    let library_start = lib.map_or(usize::MAX / 4, |l| l.start);
    pipeline.profile_config.package_effects = super::public_effects::of_tree(tree, library_start);
    super::public_effects::library_declared(&mut pipeline.profile_config, library_start);
    pipeline.resolve();
    PackageLibrary::drop_diagnostics(lib, &mut pipeline);
    if pipeline.has_resolve_errors() {
        return Err(MultiFileLateFailure {
            phase: "resolve".to_string(),
            message: format_pipeline_errors(&pipeline, "resolve", Some(&span_table)),
            diags: collect_pipeline_diags(&pipeline, "resolve", Some(&span_table)),
        });
    }
    pipeline.typecheck();
    PackageLibrary::drop_diagnostics(lib, &mut pipeline);
    if pipeline
        .typed
        .as_ref()
        .is_some_and(|t| !t.errors.is_empty())
    {
        return Err(MultiFileLateFailure {
            phase: "typecheck".to_string(),
            message: format_pipeline_errors(&pipeline, "typecheck", Some(&span_table)),
            diags: collect_pipeline_diags(&pipeline, "typecheck", Some(&span_table)),
        });
    }
    pipeline.lower();
    pipeline.effectcheck();
    PackageLibrary::drop_diagnostics(lib, &mut pipeline);
    // Note-severity findings must not fail a multi-file build. This gate used
    // to test `!e.errors.is_empty()` with NO filtering at all, so it would
    // have failed on an `FfiLintHint` too — latent since the hint existed,
    // unhit only because no multi-file fixture declared an `extern` that
    // triggers one. `mutual_recursion_note` surfaced it immediately: the
    // self-hosted compiler is full of legitimate mutual recursion (a
    // recursive-descent parser and a recursive emitter), so the self-host
    // oracle went red with eleven notes reported as `error[effect]`
    // (B-2026-08-21-2).
    if pipeline.effects.as_ref().is_some_and(|e| {
        e.errors
            .iter()
            .any(|err| !crate::effectchecker::kind_is_note(&err.kind))
    }) {
        return Err(MultiFileLateFailure {
            phase: "effect".to_string(),
            message: format_pipeline_errors(&pipeline, "effect", Some(&span_table)),
            diags: collect_pipeline_diags(&pipeline, "effect", Some(&span_table)),
        });
    }
    // In front of a v2 command the ownership rules are the core's, as on the
    // combined source the command then runs (B-2026-10-22-28: a returned
    // place is lent, and a bare one is the error).
    pipeline.strict_core = core_places;
    pipeline.ownershipcheck();
    PackageLibrary::drop_diagnostics(lib, &mut pipeline);
    if pipeline
        .ownership
        .as_ref()
        .is_some_and(|o| !o.errors.is_empty())
    {
        return Err(MultiFileLateFailure {
            phase: "ownership".to_string(),
            message: format_pipeline_errors(&pipeline, "ownership", Some(&span_table)),
            diags: collect_pipeline_diags(&pipeline, "ownership", Some(&span_table)),
        });
    }
    pipeline.concurrencycheck();
    PackageLibrary::drop_diagnostics(lib, &mut pipeline);
    if pipeline.has_fatal_errors() {
        return Err(MultiFileLateFailure {
            phase: "checks".to_string(),
            message: format_pipeline_errors(&pipeline, "checks", Some(&span_table)),
            diags: collect_pipeline_diags(&pipeline, "checks", Some(&span_table)),
        });
    }

    Ok(pipeline)
}

/// Render a structured error list across the post-typecheck pipeline
/// phases for the multi-file project-mode build path. `table` is the
/// per-module span lookup built at concat time in
/// `run_multi_file_codegen` — when present and the span resolves to
/// exactly one module, the diagnostic line is prefixed with
/// `file:line:col`; otherwise it falls back to bare `line:col` so
/// callers without a table (or with a span absent from the table /
/// shared across modules) still get a useful location.
// B-2026-08-27-27 — un-gated: `karac check` renders the same late-phase
// diagnostics through this formatter in the default build, where there is no
// backend. Nothing here touches one.
/// One late-phase diagnostic, structured (B-2026-08-27-27).
///
/// `format_pipeline_errors` used to render straight to a string, which was
/// enough while only `karac build` consumed it. `karac check` needs the same
/// findings as JSON — it is the Mend loop's entry point, and a blob with no
/// file/line is not something `karac fix` can act on — so the traversal now
/// produces this and the text renderer sits on top of it. One traversal, so
/// the two spellings cannot drift.
pub(super) struct LateDiag {
    /// The catalogue code and severity `--output=json` gives the finding.
    pub(super) code: &'static str,
    pub(super) severity: &'static str,
    pub(super) file: Option<String>,
    pub(super) line: usize,
    pub(super) column: usize,
    pub(super) message: String,
}

/// Collect the late-phase errors for `phase`, resolving each span's file
/// through the module span table when it maps to exactly one module.
pub(super) fn collect_pipeline_diags(
    pipeline: &Pipeline,
    phase: &str,
    table: Option<&crate::span_visitor::ModuleSpanTable>,
) -> Vec<LateDiag> {
    let mut out: Vec<LateDiag> = Vec::new();
    let file_of = |span: &crate::token::Span| -> Option<String> {
        table
            .and_then(|t| t.lookup(span))
            .map(|p| p.display().to_string())
    };
    let push_resolve = |p: &Pipeline, out: &mut Vec<LateDiag>| {
        if let Some(r) = &p.resolved {
            for e in &r.errors {
                let code = super::diag_json::resolve_error_code(&e.kind);
                let severity = if crate::resolver::resolve_kind_is_note(&e.kind) {
                    "warning"
                } else {
                    "error"
                };
                out.push(LateDiag {
                    code,
                    severity,
                    file: file_of(&e.span),
                    line: e.span.line,
                    column: e.span.column,
                    message: e.message.clone(),
                });
            }
        }
    };
    let push_typed = |p: &Pipeline, out: &mut Vec<LateDiag>| {
        if let Some(t) = &p.typed {
            for e in &t.errors {
                let (code, severity) = (type_error_code(&e.kind), "error");
                out.push(LateDiag {
                    code,
                    severity,
                    file: file_of(&e.span),
                    line: e.span.line,
                    column: e.span.column,
                    message: e.message.clone(),
                });
            }
        }
    };
    let push_effects = |p: &Pipeline, out: &mut Vec<LateDiag>| {
        if let Some(e) = &p.effects {
            for err in &e.errors {
                let (code, severity) = super::diag_json::effect_error_code(&err.kind);
                out.push(LateDiag {
                    code,
                    severity,
                    file: file_of(&err.span),
                    line: err.span.line,
                    column: err.span.column,
                    message: err.message.clone(),
                });
            }
        }
    };
    let push_ownership = |p: &Pipeline, out: &mut Vec<LateDiag>| {
        if let Some(o) = &p.ownership {
            for err in &o.errors {
                let code = super::diag_json::ownership_code(&err.kind);
                let severity = if p.is_fatal_ownership_kind(&err.kind) {
                    "error"
                } else {
                    "warning"
                };
                out.push(LateDiag {
                    code,
                    severity,
                    file: file_of(&err.span),
                    line: err.span.line,
                    column: err.span.column,
                    message: err.message.clone(),
                });
            }
        }
    };
    match phase {
        "resolve" => push_resolve(pipeline, &mut out),
        "typecheck" => push_typed(pipeline, &mut out),
        "effect" => push_effects(pipeline, &mut out),
        "ownership" => push_ownership(pipeline, &mut out),
        // The "checks" branch is reached when `has_fatal_errors` returns true
        // after a late-phase pass; today that flag is driven by parse +
        // resolve errors only (concurrency analysis emits structured
        // decisions, not errors), but we surface every accumulated error here
        // so the user gets file-context wherever a span is available rather
        // than the generic "late-phase analysis failed" stub.
        "checks" => {
            push_resolve(pipeline, &mut out);
            push_typed(pipeline, &mut out);
            push_effects(pipeline, &mut out);
            push_ownership(pipeline, &mut out);
        }
        _ => {}
    }
    out
}

/// Render [`collect_pipeline_diags`] in the text form `karac build` has always
/// printed. Byte-identical to the pre-B-2026-08-27-27 spelling.
pub(super) fn format_pipeline_errors(
    pipeline: &Pipeline,
    phase: &str,
    table: Option<&crate::span_visitor::ModuleSpanTable>,
) -> String {
    format_late_diags(phase, &collect_pipeline_diags(pipeline, phase, table))
}

pub(super) fn format_late_diags(phase: &str, diags: &[LateDiag]) -> String {
    use std::fmt::Write;
    let mut out = format!("multi-file {phase} failed:");
    for d in diags {
        let prefix = match &d.file {
            Some(f) => format!("{f}:"),
            None => String::new(),
        };
        let _ = write!(
            &mut out,
            "\n  {}{}:{}: {}",
            prefix, d.line, d.column, d.message
        );
    }
    out
}

pub(super) fn print_parse_errors_text(parse_errors: &[ModuleParseErrors]) {
    for pe in parse_errors {
        for err in &pe.errors {
            // The symbolic code a message opens with is the one printed,
            // once, as a lone file's check prints it.
            let (code, _, message) = super::specific_code(err.kind.code(), &err.message);
            eprintln!(
                "error[{code}]: {}:{}:{}: {message}",
                super::project_v2::shown(&pe.file),
                err.span.line,
                err.span.column,
            );
        }
    }
}

/// Resolver errors collected for one specific module, with the source file
/// retained so diagnostics can be printed with their original location.
pub(super) struct ModuleResolveErrors {
    file: PathBuf,
    pub(super) errors: Vec<ResolveError>,
    /// Multi-edit fix envelopes for this module's diagnostics, keyed by the
    /// owning diagnostic's span. Carried alongside `errors` so the
    /// project-mode JSON path advertises the same fixes the single-file path
    /// does — see [`crate::resolver::ResolveResult::error_fix_diffs`].
    fix_diffs: FxHashMap<crate::resolver::SpanKey, Vec<crate::resolver::TextEdit>>,
}

/// Run the resolver per module with the full `ProgramTree` attached so
/// cross-module imports can be validated. Returns only modules that produced
/// errors — a module with a clean resolve contributes nothing.
/// The Kāra library a package's modules are checked against, as
/// [`Pipeline::with_library`] appends it to a lone file (B-2026-10-24-43):
/// the items the package can reach, their bodies emptied, every one at an
/// offset from `start` on, past any module's own text. Without it a
/// package's per-module passes knew only the checker's built-in names, so a
/// v2-only item (`WsMessage`, `std.process.Command`) was undefined in a
/// package and fine in a lone file. The passes check the package against
/// the items' declarations; the check of the combined source in front of
/// `run` / `build` checks it against their methods. `None` when a module
/// cannot be read or the combined text does not parse; the passes then run
/// as before.
/// D5 (core-semantics §4.1) over a package: every module's bare parameters
/// borrow, as `Pipeline::resolve_phase` rewrites a lone file's, so the
/// per-module passes and the flattened late phases read a call the way the
/// v2 check of the combined source does (B-2026-10-25-4). The rewrite reads
/// the package whole: an impl in one module borrows where its trait in
/// another does, and a `Copy` type declared in any module, or in the
/// library, copies.
fn borrow_bare_params_of(tree: &mut ProgramTree, lib: Option<&PackageLibrary>) {
    if !crate::d5_params::on() || std::env::var_os("KARAC_D5_MIGRATE").is_some() {
        return;
    }
    let mut lens = Vec::new();
    let mut items = Vec::new();
    for m in tree.modules.iter_mut().filter(|m| !m.is_synthetic) {
        lens.push(m.items.len());
        items.append(&mut m.items);
    }
    let user = items.len();
    if let Some(lib) = lib {
        items.extend(
            lib.items
                .iter()
                .filter(|it| matches!(it, Item::StructDef(_) | Item::EnumDef(_)))
                .cloned(),
        );
    }
    let mut program = Program {
        items,
        ..Program::default()
    };
    crate::d5_params::borrow_bare_params(&mut program);
    let mut items = program.items;
    items.truncate(user);
    let mut items = items.into_iter();
    for (m, len) in tree
        .modules
        .iter_mut()
        .filter(|m| !m.is_synthetic)
        .zip(lens)
    {
        m.items = items.by_ref().take(len).collect();
    }
}

pub(super) struct PackageLibrary {
    items: Vec<Item>,
    start: usize,
    /// The library's text, from `start` on, for an editor query that maps
    /// a library span back to its file (B-2026-11-17-71).
    text: String,
}

impl PackageLibrary {
    pub(super) fn of(tree: &ProgramTree) -> Option<Self> {
        Self::of_with(tree, None)
    }

    /// [`Self::of`] with the editor's buffer in place of its file on disk.
    pub(super) fn of_with(
        tree: &ProgramTree,
        overlay: Option<(&std::path::Path, &str)>,
    ) -> Option<Self> {
        let mut src = String::new();
        for m in tree.modules.iter().filter(|m| !m.is_synthetic) {
            match overlay.filter(|(p, _)| same_file(p, &m.file)) {
                Some((_, text)) => src.push_str(text),
                None => src.push_str(&std::fs::read_to_string(&m.file).ok()?),
            }
            src.push('\n');
        }
        // The `_test.kara` companions `karac test` merges into their
        // modules name library items too (a fake's `NetworkProvider`).
        for f in tree.modules.iter().filter_map(|m| m.test_file.as_ref()) {
            src.push_str(&std::fs::read_to_string(f).ok()?);
            src.push('\n');
        }
        let user_items = crate::parse(&src).program.items.len();
        let (text, start) = crate::mir::lower::library_source(&src);
        let (mut parsed, code) = crate::parse_with_code_text(&text);
        if !parsed.errors.is_empty() || parsed.program.items.len() < user_items {
            return None;
        }
        let program = &mut parsed.program;
        if crate::mir::library_prune::enabled() {
            crate::mir::library_prune::prune_code(program, &code, start);
        }
        crate::mir::lower::mark_library_builtins(program, start);
        library_closures_take_their_callers_effects(program, start);
        let end = crate::mir::lower::library_sources_end(start);
        crate::mir::lower::library_bodies_unchecked(program, start..end);
        let mut items = std::mem::take(&mut program.items).split_off(user_items);
        // A module's own type named like one the compiler spells (`Range`)
        // keeps its name in the module, so the library's goes.
        let ours: std::collections::HashSet<String> = tree
            .modules
            .iter()
            .filter(|m| !m.is_synthetic)
            .flat_map(|m| m.items.iter().filter_map(type_item_name))
            .collect();
        items.retain(|it| match it {
            Item::ImplBlock(b) => !matches!(&b.target_type.kind,
                crate::ast::TypeKind::Path(p) if p.segments.last().is_some_and(|n| ours.contains(n))),
            _ => type_item_name(it).is_none_or(|n| !ours.contains(&n)),
        });
        let text = text.get(start..).unwrap_or("").to_string();
        Some(PackageLibrary { items, start, text })
    }

    /// One module's items with the library after them.
    pub(super) fn program(lib: Option<&Self>, items: &[Item]) -> Program {
        let mut items = items.to_vec();
        let library_start = lib.map(|lib| {
            items.extend(lib.items.iter().cloned());
            lib.start
        });
        Program {
            items,
            library_start,
            ..Program::default()
        }
    }

    /// Drop the diagnostics inside the library from a late phase. The
    /// pipeline is not put in library mode (`library_len`): a package's
    /// passes check it against the built-in methods, as before, and the v2
    /// check of its combined source in front of `run` / `build` is the one
    /// against the library's.
    fn drop_diagnostics(lib: Option<&Self>, pipeline: &mut Pipeline) {
        if let Some(lib) = lib {
            pipeline.library_len = Some(lib.start);
            pipeline.drop_library_diagnostics();
            pipeline.library_len = None;
        }
    }

    /// Whether a diagnostic at `offset` lies in the library.
    pub(super) fn holds(lib: Option<&Self>, offset: usize) -> bool {
        lib.is_some_and(|lib| offset >= lib.start)
    }
}

fn type_item_name(it: &Item) -> Option<String> {
    match it {
        Item::StructDef(s) => Some(s.name.clone()),
        Item::EnumDef(e) => Some(e.name.clone()),
        _ => None,
    }
}

pub(super) fn resolve_modules(
    tree: &ProgramTree,
    lib: Option<&PackageLibrary>,
) -> Vec<ModuleResolveErrors> {
    let mut out = Vec::new();
    for (id, m) in tree.modules.iter().enumerate() {
        // Compiler-injected modules (CR-24 slice 8: `std.prelude` placeholder)
        // skip per-module passes — their stub items only exist to surface the
        // module path to cross-module resolution.
        if m.is_synthetic {
            continue;
        }
        // Resolver still takes a `&Program`, so wrap the module's items
        // in a freshly-owned `Program` view. Clone cost is negligible next
        // to the resolver pass itself.
        let mut program = PackageLibrary::program(lib, &m.items);
        crate::desugar::desugar_multi_assigns(&mut program.items);
        // A package is checked by the v2 pipeline, which admits a user
        // operator-trait impl as a lone file's does (B-2026-11-05-21).
        // Every command refuses a module `let` with E_MODULE_LET, whose fix
        // carries the rename (B-2026-11-17-125).
        let mut result = Resolver::new(&program)
            .with_tree(tree, id as ModuleId)
            .with_test_file(m.is_test_file)
            .with_user_operator_impls(true)
            .with_module_let_refused(true)
            .resolve();
        result
            .errors
            .retain(|e| !PackageLibrary::holds(lib, e.span.offset));
        // A fix's edit is an offset into the module's FILE, which is what a
        // reader of the replacement applies it to (B-2026-10-25-26); the
        // spans it was computed from are rebased into the tree's range.
        let to_file = |e: &mut crate::resolver::TextEdit| {
            e.offset = e.offset.saturating_sub(m.span_base);
        };
        for e in &mut result.errors {
            if let Some(r) = e.replacement.as_deref_mut() {
                to_file(r);
            }
        }
        for edits in result.error_fix_diffs.values_mut() {
            edits.iter_mut().for_each(to_file);
        }
        // The fixes the help names, as a lone file's check carries them
        // (B-2026-11-21-75).
        if let Ok(text) = std::fs::read_to_string(&m.file) {
            super::source_fixes::complete_resolved_at(
                &text,
                m.span_base,
                &mut result.errors,
                &mut result.error_fix_diffs,
            );
        }
        // A merged test companion's spans sit past its sibling's
        // (`Module::test_span_base`): its errors name its own file, and
        // their edits are offsets into it.
        let test_base = m.span_base + m.test_span_base;
        let (test_errors, errors): (Vec<_>, Vec<_>) = result
            .errors
            .into_iter()
            .partition(|e| m.test_span_base > 0 && e.span.offset >= test_base);
        for (file, mut errors) in [(Some(&m.file), errors), (m.test_file.as_ref(), test_errors)] {
            let Some(file) = file.filter(|_| !errors.is_empty()) else {
                continue;
            };
            let is_test = *file != m.file;
            let mut fix_diffs = result.error_fix_diffs.clone();
            fix_diffs.retain(|k, _| (k.0 >= test_base && m.test_span_base > 0) == is_test);
            if is_test {
                let to_test_file = |e: &mut crate::resolver::TextEdit| {
                    e.offset = e.offset.saturating_sub(m.test_span_base);
                };
                for e in &mut errors {
                    if let Some(r) = e.replacement.as_deref_mut() {
                        to_test_file(r);
                    }
                }
                for edits in fix_diffs.values_mut() {
                    edits.iter_mut().for_each(to_test_file);
                }
            }
            out.push(ModuleResolveErrors {
                file: file.clone(),
                errors,
                fix_diffs,
            });
        }
    }
    out
}

pub(super) fn resolve_error_code(kind: &ResolveErrorKind) -> &'static str {
    super::diag_json::resolve_error_code(kind)
}

pub(super) fn print_resolve_errors_text(per_module: &[ModuleResolveErrors]) {
    for re in per_module {
        let file = super::project_v2::shown(&re.file);
        for err in &re.errors {
            let code = resolve_error_code(&err.kind);
            eprintln!(
                "error[{code}]: {}:{}:{}: {}",
                super::project_v2::shown(&re.file),
                err.span.line,
                err.span.column,
                err.message,
            );
            if let Some(ref s) = err.suggestion {
                // A name to put in place reads as a correction; any other
                // suggestion (E0111's "mark `x` as `pub` ...", an attribute
                // deletion's "remove the attribute") is the help.
                if err.replacement.is_some() && !s.contains(' ') {
                    eprintln!("  help: did you mean `{s}`?");
                } else {
                    eprintln!("  help: {s}");
                }
            }
            if let Some(ref stub) = err.stub_hint {
                let target_file = sibling_production_file(&file);
                eprintln!(
                    "  hint: stub `{}` in {} with inferred signature:",
                    stub.callee_name, target_file
                );
                for line in stub.render_source().lines() {
                    eprintln!("    {line}");
                }
            }
        }
    }
}

/// Render `err.replacement` as `,"replacement":{...}` JSON tail (or empty
/// string when no replacement is attached). Mirrors the single-file
/// `print_diagnostics_json` path at the top of this file so IDE quick-fix
/// consumers see the same payload regardless of how `karac check` was
/// invoked. Multi-file-only diagnostics (E0112 / E0113) reach IDEs only
/// through this path.
pub(super) fn replacement_json_tail(err: &crate::resolver::ResolveError) -> String {
    match err.replacement.as_deref() {
        Some(r) => format!(
            ",\"replacement\":{{\"offset\":{},\"length\":{},\"text\":{}}}",
            r.offset,
            r.length,
            json_string(&r.replacement),
        ),
        None => String::new(),
    }
}

/// Render a multi-edit fix envelope as a `,"fix_diff":[{...}]` JSON tail
/// (empty when the diagnostic has none). The project-mode twin of the
/// single-file `fix_diff` emission in `collect_diagnostics`; without it a
/// diagnostic whose fix spans several edits — today
/// `E_MODULE_BINDING_NAMING`'s rename — would look unfixable to an IDE
/// simply because the project was compiled as a project.
pub(super) fn fix_diff_json_tail(
    err: &crate::resolver::ResolveError,
    fix_diffs: &FxHashMap<crate::resolver::SpanKey, Vec<crate::resolver::TextEdit>>,
) -> String {
    match fix_diffs
        .get(&crate::resolver::SpanKey::from_span(&err.span))
        .filter(|v| !v.is_empty())
    {
        Some(edits) => {
            let items: Vec<String> = edits
                .iter()
                .map(|e| {
                    format!(
                        "{{\"offset\":{},\"length\":{},\"text\":{}}}",
                        e.offset,
                        e.length,
                        json_string(&e.replacement),
                    )
                })
                .collect();
            format!(",\"fix_diff\":[{}]", items.join(","))
        }
        None => String::new(),
    }
}

/// A typecheck diagnostic's fix as the `,"replacement":{..}` or
/// `,"fix_diff":[..]` tail a lone file's record carries (B-2026-11-21-75);
/// empty when it has none.
pub(super) fn typed_fix_json_tail(
    err: &crate::typechecker::TypeError,
    fix_diffs: &FxHashMap<crate::resolver::SpanKey, Vec<crate::resolver::TextEdit>>,
) -> String {
    // The help a lone file's record carries as its `suggestion`.
    let help = err
        .help
        .as_deref()
        .map(|h| format!(",\"suggestion\":{}", json_string(h)))
        .unwrap_or_default();
    help + &typed_fix_edit_json(err, fix_diffs)
}

fn typed_fix_edit_json(
    err: &crate::typechecker::TypeError,
    fix_diffs: &FxHashMap<crate::resolver::SpanKey, Vec<crate::resolver::TextEdit>>,
) -> String {
    if let Some(f) = &err.fix_it {
        return format!(
            ",\"replacement\":{{\"offset\":{},\"length\":{},\"text\":{}}}",
            f.span.offset,
            f.span.length,
            json_string(&f.replacement),
        );
    }
    match fix_diffs
        .get(&crate::resolver::SpanKey::from_span(&err.span))
        .filter(|v| !v.is_empty())
    {
        Some(edits) => {
            let items: Vec<String> = edits
                .iter()
                .map(|e| {
                    format!(
                        "{{\"offset\":{},\"length\":{},\"text\":{}}}",
                        e.offset,
                        e.length,
                        json_string(&e.replacement),
                    )
                })
                .collect();
            format!(",\"fix_diff\":[{}]", items.join(","))
        }
        None => String::new(),
    }
}

/// B-2026-08-27-27 — the flattened late-phase failure as `check` diagnostics.
/// `phase` is carried through verbatim ("effect" / "ownership" / …) so a
/// consumer can tell them apart, and each diagnostic keeps the file and
/// line:column the span table resolved.
pub(super) fn late_failure_json(f: &MultiFileLateFailure) -> Vec<String> {
    f.diags
        .iter()
        .map(|d| {
            let (code, parent_code, message) = super::specific_code(d.code, &d.message);
            let parent = parent_code
                .map(|p| format!("\"parent_code\":{},", json_string(p)))
                .unwrap_or_default();
            format!(
                "{{\"severity\":{},\"code\":{},{parent}\"phase\":{},\"concept\":{},\"file\":{},\"line\":{},\"column\":{},\"message\":{}}}",
                json_string(d.severity),
                json_string(code),
                json_string(&f.phase),
                json_string(super::explain::concept_for(&f.phase, "", None, None)),
                json_string(d.file.as_deref().unwrap_or("")),
                d.line,
                d.column,
                json_string(message),
            )
        })
        .collect()
}

pub(super) fn print_late_failure_text(f: &MultiFileLateFailure) {
    // Each finding under its catalogue code, as JSON reports it and the
    // single-file renderer prints it (review-thread ruling 2026-11-16).
    if f.diags.is_empty() {
        eprintln!("error[{}]: {}", f.phase, f.message);
    }
    for d in &f.diags {
        // Named from where the command runs, as every other package
        // diagnostic is.
        let file = d
            .file
            .as_deref()
            .map(|f| format!("{}:", super::project_v2::shown(std::path::Path::new(f))))
            .unwrap_or_default();
        eprintln!(
            "{}[{}]: {file}{}:{}: {}",
            d.severity, d.code, d.line, d.column, d.message
        );
    }
}

pub(super) fn resolve_errors_json(per_module: &[ModuleResolveErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for re in per_module {
        let file = super::project_v2::shown(&re.file);
        // The module's text, for each fix's hint diff (B-2026-11-05-30).
        let source = std::fs::read_to_string(&re.file).ok();
        let hinted = |entry: String| match source.as_deref() {
            Some(src) => super::diag_json::with_hint_diffs(&entry, src, &file),
            None => entry,
        };
        for err in &re.errors {
            let code = resolve_error_code(&err.kind);
            let suggestion = match err.suggestion.as_deref() {
                Some(s) => format!(",\"suggestion\":{}", json_string(s)),
                None => String::new(),
            };
            let replacement = replacement_json_tail(err);
            let fix_diff = fix_diff_json_tail(err, &re.fix_diffs);
            let hints = stub_hints_tail(&file, err);
            out.push(hinted(format!(
                "{{\"severity\":\"error\",\"phase\":\"resolve\",\"code\":{},\"concept\":{},\"file\":{},\"line\":{},\"column\":{},\"message\":{}{}{}{}{}}}",
                json_string(code),
                json_string(super::explain::concept_for("resolve", code, None, None)),
                json_string(&file),
                err.span.line,
                err.span.column,
                json_string(&err.message),
                suggestion,
                replacement,
                fix_diff,
                hints,
            )));
        }
    }
    out
}

pub(super) fn resolve_errors_jsonl(per_module: &[ModuleResolveErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for re in per_module {
        let file = super::project_v2::shown(&re.file);
        // The module's text, for each fix's hint diff (B-2026-11-05-30).
        let source = std::fs::read_to_string(&re.file).ok();
        let hinted = |entry: String| match source.as_deref() {
            Some(src) => super::diag_json::with_hint_diffs(&entry, src, &file),
            None => entry,
        };
        for err in &re.errors {
            let code = resolve_error_code(&err.kind);
            let suggestion = match err.suggestion.as_deref() {
                Some(s) => format!(",\"suggestion\":{}", json_string(s)),
                None => String::new(),
            };
            let replacement = replacement_json_tail(err);
            let fix_diff = fix_diff_json_tail(err, &re.fix_diffs);
            let hints = stub_hints_tail(&file, err);
            out.push(hinted(format!(
                "{{\"type\":\"resolve_error\",\"code\":{},\"file\":{},\"line\":{},\"column\":{},\"message\":{}{}{}{}{}}}",
                json_string(code),
                json_string(&file),
                err.span.line,
                err.span.column,
                json_string(&err.message),
                suggestion,
                replacement,
                fix_diff,
                hints,
            )));
        }
    }
    out
}

/// Emit the `,"hints":[…]` JSON tail when `err` carries a stub hint —
/// the multi-module resolve-error emitters' counterpart to the
/// `hints[].diff` wiring inside `DiagnosticJson::add`. Returns the
/// empty string when no stub hint is present so the JSON shape stays
/// lean for the common case.
pub(super) fn stub_hints_tail(file: &str, err: &crate::resolver::ResolveError) -> String {
    match err.stub_hint.as_ref() {
        Some(s) => format!(",\"hints\":[{}]", render_stub_hint_json(file, s)),
        None => String::new(),
    }
}

/// Typechecker errors collected for one specific module.
pub(super) struct ModuleTypeErrors {
    file: PathBuf,
    errors: Vec<crate::typechecker::TypeError>,
    /// Multi-edit fixes keyed by the owning diagnostic's span, each edit an
    /// offset into `file`, as [`ModuleResolveErrors::fix_diffs`] are.
    fix_diffs: FxHashMap<crate::resolver::SpanKey, Vec<crate::resolver::TextEdit>>,
}

/// Run the typechecker per module with the full `ProgramTree` attached so
/// the CR-24 slice-6 cross-module `E0221` + field-access rules can fire.
/// A fresh resolver pass per module provides the `ResolveResult` the
/// typechecker still consumes internally.
/// Per-module typecheck output: the errors that gate the build, and the
/// WARNINGS that do not.
///
/// The two are separated rather than merged because `type_errors.is_empty()`
/// is the project build's gate — folding warning-carrying modules into that
/// vector would abort a build over a `#[deprecated]` call. B-2026-08-18-19.
pub(super) struct ModuleTypeDiagnostics {
    pub(super) errors: Vec<ModuleTypeErrors>,
    /// Same per-file shape, carrying `TypeCheckResult::warnings` — the channel
    /// `deprecated` / `unstable_api` ride.
    pub(super) warnings: Vec<ModuleTypeErrors>,
}

pub(super) fn typecheck_modules(
    tree: &ProgramTree,
    lint_overrides: &crate::lints::CliLintOverrides,
    lib: Option<&PackageLibrary>,
) -> ModuleTypeDiagnostics {
    let mut out = Vec::new();
    let mut warn_out = Vec::new();
    for (id, m) in tree.modules.iter().enumerate() {
        // Skip the compiler-injected `std.prelude` placeholder — its stubs
        // would clash with `register_builtin_types` if pushed through the
        // typechecker's normal item-collection.
        if m.is_synthetic {
            continue;
        }
        let mut program = PackageLibrary::program(lib, &m.items);
        crate::desugar::desugar_multi_assigns(&mut program.items);
        let resolved = Resolver::new(&program)
            .with_tree(tree, id as ModuleId)
            .with_user_operator_impls(true)
            .with_module_let_refused(lint_overrides.strict_core)
            .resolve();
        let mut result = crate::typechecker::TypeChecker::new(&program, &resolved)
            .with_tree(tree, id as ModuleId)
            .with_cli_lint_overrides(lint_overrides.clone())
            .check();
        let in_lib = |e: &crate::typechecker::TypeError| PackageLibrary::holds(lib, e.span.offset);
        result.errors.retain(|e| !in_lib(e));
        result.warnings.retain(|e| !in_lib(e));
        // A fix's edit is an offset into the module's FILE, as a resolve
        // error's is (`resolve_modules`); the help's fixes are added from
        // the file's text, as a lone file's check adds them
        // (B-2026-11-21-75).
        let base = m.span_base;
        for e in result.errors.iter_mut().chain(result.warnings.iter_mut()) {
            if let Some(f) = e.fix_it.as_mut() {
                f.span.offset = f.span.offset.saturating_sub(base);
            }
        }
        for edits in result.error_fix_diffs.values_mut() {
            for e in edits.iter_mut() {
                e.offset = e.offset.saturating_sub(base);
            }
        }
        if let Ok(text) = std::fs::read_to_string(&m.file) {
            super::source_fixes::complete_typed_at(
                &text,
                base,
                &mut result.errors,
                &mut result.error_fix_diffs,
            );
            super::source_fixes::complete_typed_at(
                &text,
                base,
                &mut result.warnings,
                &mut result.error_fix_diffs,
            );
        }
        // B-2026-08-18-19 — the warnings used to be DROPPED here: `result`
        // carries them and only `.errors` was read, so a project build was
        // structurally incapable of reporting `deprecated` no matter what the
        // render path did.
        //
        // A dependency's own warnings are its author's, who sees them when
        // checking that package; the dependent cannot act on them, so its
        // check shows only its own code's, as Cargo caps a dependency's
        // lints (B-2026-11-24-54). Its errors still stop the build.
        if m.package.is_some() {
            result.warnings.clear();
        }
        if !result.warnings.is_empty() {
            // The map is keyed by span: where an error shares a warning's
            // span, the edits are the error's (B-2026-11-21-74).
            let mut fix_diffs = result.error_fix_diffs.clone();
            for e in &result.errors {
                fix_diffs.remove(&crate::resolver::SpanKey::from_span(&e.span));
            }
            warn_out.push(ModuleTypeErrors {
                file: m.file.clone(),
                errors: result.warnings,
                fix_diffs,
            });
        }
        if !result.errors.is_empty() {
            out.push(ModuleTypeErrors {
                file: m.file.clone(),
                errors: result.errors,
                fix_diffs: result.error_fix_diffs,
            });
        }
    }
    ModuleTypeDiagnostics {
        errors: out,
        warnings: warn_out,
    }
}

fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    let canon = |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(a) == canon(b)
}

/// One module of a package, checked for an editor position query with the
/// buffer in place of its file (B-2026-11-17-71): the tables of the module
/// against the rest of the package and the library, over one text in which
/// every offset of those tables lands where its source is.
pub(crate) struct ModuleTables {
    /// The module's items, the package's other modules' items (for their
    /// declarations), then the library's.
    pub(crate) program: Program,
    pub(crate) resolved: ResolveResult,
    pub(crate) typed: TypeCheckResult,
    pub(crate) effects: EffectCheckResult,
    /// The buffer at 0, each other module at its span base, the library
    /// from `lib_start`.
    pub(crate) text: String,
    pub(crate) user_len: usize,
    pub(crate) lib_start: usize,
    /// Each other module's file, span base and length in `text`.
    pub(crate) modules: Vec<(std::path::PathBuf, usize, usize)>,
}

/// [`ModuleTables`] of the buffer `source` of the file at `path`, when the
/// file is a module of a package that has other modules; `None` otherwise,
/// and when the package does not parse, so the caller checks the buffer
/// alone.
pub(crate) fn editor_module_tables(path: &std::path::Path, source: &str) -> Option<ModuleTables> {
    let root = super::run_check_cmds::package_root_of_member(&path.to_string_lossy())?;
    let walk_opts = WalkerOpts {
        target: walk_platform_for_target(crate::target::active_target()),
        ..WalkerOpts::default()
    };
    let walked = walker::walk_project(&root, walk_opts).ok()?;
    let dep_walks = super::run_check_cmds::quiet_dep_package_walks(&root);
    let BuildTreeOk {
        mut tree,
        parse_errors,
    } = module::build_program_tree_with_overlay(
        &walked,
        &dep_walks,
        module::BuildTreeOpts {
            merge_test_companions: false,
            overlay_first: true,
        },
        Some((path, source)),
    )
    .ok()?;
    if !parse_errors.is_empty() {
        return None;
    }
    let users: Vec<usize> = (0..tree.modules.len())
        .filter(|&i| !tree.modules[i].is_synthetic)
        .collect();
    let id = *users
        .iter()
        .find(|&&i| same_file(&tree.modules[i].file, path))?;
    // A lone module is a file: the buffer's own check answers it.
    if users.len() < 2 || tree.modules[id].span_base != 0 {
        return None;
    }
    let lib = PackageLibrary::of_with(&tree, Some((path, source)));
    borrow_bare_params_of(&mut tree, lib.as_ref());
    let mut program = PackageLibrary::program(lib.as_ref(), &tree.modules[id].items);
    crate::desugar::desugar_multi_assigns(&mut program.items);
    let resolved = Resolver::new(&program)
        .with_tree(&tree, id as ModuleId)
        .with_user_operator_impls(true)
        .with_module_let_refused(true)
        .resolve();
    let typed = crate::typechecker::TypeChecker::new(&program, &resolved)
        .with_tree(&tree, id as ModuleId)
        .with_cli_lint_overrides(crate::lints::CliLintOverrides {
            core_places: true,
            strict_core: true,
            ..Default::default()
        })
        .check();
    // Each package's public functions under its own `public_effects`.
    let mut config = crate::manifest::ProfileConfig::default();
    if let Ok(mf) = crate::manifest::load_from_root(&root) {
        config.public_effects = mf.profile_config.public_effects;
    }
    let lib_start = lib.as_ref().map_or(usize::MAX / 4, |l| l.start);
    config.package_effects = super::public_effects::of_tree(&tree, lib_start);
    super::public_effects::library_declared(&mut config, lib_start);
    let effects = crate::effectcheck_with_typecheck_tables(
        &program,
        config.public_effects,
        config,
        typed.method_callee_types.clone(),
        typed.call_type_subs.clone(),
        typed.node_call_subs.clone(),
        crate::typeparam_value_nodes(&typed),
        typed.user_operator_calls.clone(),
        crate::dyn_tables(&typed),
        crate::node_type_facts(&typed),
    );
    // The text: each module's source where its spans say it is.
    let user_len = source.len();
    let mut others: Vec<(std::path::PathBuf, usize, String)> = users
        .iter()
        .filter(|&&i| i != id)
        .filter_map(|&i| {
            let m = &tree.modules[i];
            Some((
                m.file.clone(),
                m.span_base,
                std::fs::read_to_string(&m.file).ok()?,
            ))
        })
        .collect();
    others.sort_by_key(|(_, base, _)| *base);
    let lib_start = lib.as_ref().map_or(usize::MAX, |l| l.start);
    let mut bytes = source.as_bytes().to_vec();
    let mut modules = Vec::new();
    for (k, (file, base, text)) in others.iter().enumerate() {
        let room = others
            .get(k + 1)
            .map_or(lib_start, |(_, next, _)| *next)
            .saturating_sub(*base);
        let mut take = text.len().min(room);
        while !text.is_char_boundary(take) {
            take -= 1;
        }
        if bytes.len() < *base {
            bytes.resize(*base, b' ');
        }
        bytes.truncate(*base);
        bytes.extend_from_slice(&text.as_bytes()[..take]);
        modules.push((file.clone(), *base, take));
    }
    let mut item_extra: Vec<Item> = users
        .iter()
        .filter(|&&i| i != id)
        .flat_map(|&i| tree.modules[i].items.iter().cloned())
        .collect();
    if let Some(lib) = &lib {
        if bytes.len() < lib.start {
            bytes.resize(lib.start, b' ');
        }
        bytes.truncate(lib.start);
        bytes.extend_from_slice(lib.text.as_bytes());
    }
    let text = String::from_utf8(bytes).ok()?;
    // The other modules' declarations sit between the module's items and
    // the library's.
    let own = tree.modules[id].items.len();
    let tail = program.items.split_off(own);
    program.items.append(&mut item_extra);
    program.items.extend(tail);
    Some(ModuleTables {
        program,
        resolved,
        typed,
        effects,
        text,
        user_len,
        lib_start: lib.as_ref().map_or(user_len, |l| l.start),
        modules,
    })
}

/// Each module's typecheck result in the package at `root`, keyed by its
/// file, every module checked with its imports resolved as [`package_check`]
/// checks it. `None` when the package cannot be walked or does not parse.
/// `karac migrate` reads binding types from these, which a module typechecked
/// on its own cannot give for `let c = make()` with `make` imported
/// (B-2026-11-16-285).
pub(super) fn package_module_type_results(
    root: &std::path::Path,
) -> Option<std::collections::HashMap<std::path::PathBuf, TypeCheckResult>> {
    let walked = walker::walk_project(root, WalkerOpts::default()).ok()?;
    let dep_walks = super::run_check_cmds::quiet_dep_package_walks(root);
    let BuildTreeOk {
        mut tree,
        parse_errors,
    } = module::build_program_tree_with_overlay(
        &walked,
        &dep_walks,
        module::BuildTreeOpts {
            merge_test_companions: false,
            overlay_first: false,
        },
        None,
    )
    .ok()?;
    if !parse_errors.is_empty() {
        return None;
    }
    let lib = PackageLibrary::of(&tree);
    borrow_bare_params_of(&mut tree, lib.as_ref());
    let lint_overrides = crate::lints::CliLintOverrides {
        core_places: true,
        strict_core: true,
        ..Default::default()
    };
    let mut out = std::collections::HashMap::new();
    for (id, m) in tree.modules.iter().enumerate() {
        if m.is_synthetic {
            continue;
        }
        let mut program = PackageLibrary::program(lib.as_ref(), &m.items);
        crate::desugar::desugar_multi_assigns(&mut program.items);
        let resolved = Resolver::new(&program)
            .with_tree(&tree, id as ModuleId)
            .with_user_operator_impls(true)
            .with_module_let_refused(lint_overrides.strict_core)
            .resolve();
        let mut result = crate::typechecker::TypeChecker::new(&program, &resolved)
            .with_tree(&tree, id as ModuleId)
            .with_cli_lint_overrides(lint_overrides.clone())
            .check();
        // The tree rebases each module's spans past the previous ones';
        // the keys go back to offsets into the module's own file.
        let base = m.span_base;
        let to_file = |map: &mut FxHashMap<crate::resolver::SpanKey, String>| {
            *map = std::mem::take(map)
                .into_iter()
                .filter(|(k, _)| k.0 >= base)
                .map(|(k, v)| (crate::resolver::SpanKey(k.0 - base, k.1), v))
                .collect();
        };
        to_file(&mut result.pattern_binding_types);
        to_file(&mut result.method_callee_types);
        out.insert(m.file.clone(), result);
    }
    Some(out)
}

pub(super) fn type_error_code(kind: &crate::typechecker::TypeErrorKind) -> &'static str {
    // The JSON feed's table, whole: this one used to list 28 kinds and code
    // every other kind `E0200` (review-thread ruling 2026-11-16).
    super::diag_json::type_error_code(kind)
}

/// The warning-severity twin of [`print_type_errors_text`]. Same layout, with
/// the severity word and the lint label a `-A <name>` invocation would take —
/// matching what `karac check` prints for the same finding.
pub(super) fn print_type_warnings_text(per_module: &[ModuleTypeErrors]) {
    for te in per_module {
        for warn in &te.errors {
            let label = warn
                .lint_name
                .as_deref()
                .unwrap_or_else(|| super::diag_json::type_warning_code(&warn.kind));
            eprintln!(
                "warning[{label}]: {}:{}:{}: {}",
                super::project_v2::shown(&te.file),
                warn.span.line,
                warn.span.column,
                warn.message,
            );
            if let Some(help) = &warn.help {
                eprintln!("  help: {help}");
            }
        }
    }
}

pub(super) fn print_type_errors_text(per_module: &[ModuleTypeErrors]) {
    for te in per_module {
        for err in &te.errors {
            // A promoted lint names its lint rather than the generic type
            // code — see the single-file renderer's note (B-2026-08-18-25).
            // `-D deprecated` rendered `error[E0200]` here, the TypeMismatch
            // code, which points at neither the rule nor the flag.
            let code = err
                .lint_name
                .clone()
                .unwrap_or_else(|| type_error_code(&err.kind).to_string());
            eprintln!(
                "error[{code}]: {}:{}:{}: {}",
                super::project_v2::shown(&te.file),
                err.span.line,
                err.span.column,
                err.message,
            );
            if let Some(help) = &err.help {
                eprintln!("  help: {help}");
            }
        }
    }
}

/// The warning-severity twin of [`type_errors_json`]. Carries `lint_name` so a
/// consumer can offer the `-A <name>` the human renderer names, matching what
/// `karac check --output=json` emits for the same finding.
pub(super) fn type_warnings_json(per_module: &[ModuleTypeErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for te in per_module {
        let file = super::project_v2::shown(&te.file);
        for warn in &te.errors {
            // The code a lone file's record carries: a symbolic code the
            // message opens with, else the lint's own `W` number, else the
            // kind's, with the one it displaced as `parent_code`
            // (B-2026-11-21-75).
            let kind_code = super::diag_json::type_warning_code(&warn.kind);
            let (code, symbol_parent, message) = super::specific_code(kind_code, &warn.message);
            let own = super::diag_json::lint_code(warn.lint_name.as_deref(), "warning")
                .filter(|_| symbol_parent.is_none());
            let parent = symbol_parent.or(own.map(|_| code));
            let mut record = format!(
                "{{\"severity\":\"warning\",\"phase\":\"typecheck\",\"code\":{},\"concept\":{},\"file\":{},\"line\":{},\"column\":{},\"message\":{}",
                json_string(own.unwrap_or(code)),
                json_string(super::explain::concept_for(
                    "typecheck",
                    kind_code,
                    warn.class.map(|c| c.as_str()),
                    warn.lint_name.as_deref(),
                )),
                json_string(&file),
                warn.span.line,
                warn.span.column,
                json_string(message),
            );
            if let Some(parent) = parent {
                record.push_str(&format!(",\"parent_code\":{}", json_string(parent)));
            }
            if let Some(lint) = warn.lint_name.as_deref() {
                record.push_str(&format!(",\"lint_name\":{}", json_string(lint)));
            }
            record.push_str(&typed_fix_json_tail(warn, &te.fix_diffs));
            record.push('}');
            out.push(record);
        }
    }
    out
}

pub(super) fn type_errors_json(per_module: &[ModuleTypeErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for te in per_module {
        let file = super::project_v2::shown(&te.file);
        for err in &te.errors {
            let kind_code = type_error_code(&err.kind);
            // The spec's symbolic code a message opens with
            // (`error[E_X]: ...`) is the record's code, with the kind's in
            // `parent_code`, as a lone file's feed has it (B-2026-11-21-75).
            let (code, symbol_parent, message) = super::specific_code(kind_code, &err.message);
            // A lint escalated by `-D` is its own `E03nn`, with the code it
            // shared as `parent_code` — the single-file feed's shape
            // (B-2026-11-16-299).
            let own = super::diag_json::lint_code(err.lint_name.as_deref(), "error")
                .filter(|_| symbol_parent.is_none());
            let parent = symbol_parent.or(own.map(|_| code));
            let mut record = format!(
                "{{\"severity\":\"error\",\"phase\":\"typecheck\",\"code\":{},\"concept\":{},\"file\":{},\"line\":{},\"column\":{},\"message\":{},\"class\":{}",
                json_string(own.unwrap_or(code)),
                json_string(super::explain::concept_for(
                    "typecheck",
                    kind_code,
                    err.class.map(|c| c.as_str()),
                    err.lint_name.as_deref(),
                )),
                json_string(&file),
                err.span.line,
                err.span.column,
                json_string(message),
                json_string(err.class.map(|c| c.as_str()).unwrap_or("OTHER")),
            );
            if let Some(parent) = parent {
                record.push_str(&format!(",\"parent_code\":{}", json_string(parent)));
            }
            if let Some(lint) = err.lint_name.as_deref() {
                record.push_str(&format!(",\"lint_name\":{}", json_string(lint)));
            }
            if let Some(expected) = &err.expected {
                record.push_str(&format!(",\"expected\":{}", json_string(expected)));
            }
            if let Some(got) = &err.got {
                record.push_str(&format!(",\"got\":{}", json_string(got)));
            }
            record.push_str(&typed_fix_json_tail(err, &te.fix_diffs));
            record.push('}');
            out.push(record);
        }
    }
    out
}

pub(super) fn type_errors_jsonl(per_module: &[ModuleTypeErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for te in per_module {
        let file = super::project_v2::shown(&te.file);
        for err in &te.errors {
            let code = type_error_code(&err.kind);
            let mut record = format!(
                "{{\"type\":\"type_error\",\"code\":{},\"file\":{},\"line\":{},\"column\":{},\"message\":{},\"class\":{}",
                json_string(code),
                json_string(&file),
                err.span.line,
                err.span.column,
                json_string(&err.message),
                json_string(err.class.map(|c| c.as_str()).unwrap_or("OTHER")),
            );
            if let Some(expected) = &err.expected {
                record.push_str(&format!(",\"expected\":{}", json_string(expected)));
            }
            if let Some(got) = &err.got {
                record.push_str(&format!(",\"got\":{}", json_string(got)));
            }
            record.push('}');
            out.push(record);
        }
    }
    out
}

/// The module-aware diagnostics for a WHOLE package: the same walk → tree →
/// cycles → per-module resolve → per-module typecheck sequence `cmd_build_project`
/// runs, without any codegen.
///
/// Extracted so `karac check` and `karac run` can reach the answer `karac build`
/// already had (B-2026-08-20-16). Before this, those two commands saw only the
/// ONE file named on the command line, which on a package member is neither the
/// program the user wrote nor the program that gets built: an `import` of a
/// sibling module resolves to nothing, so a `pub struct` read through it draws
/// an invented "`X` is not a struct", while a `private` item imported across
/// directories draws no E0111 at all. Both directions were wrong, and
/// `karac check --output=json` is the Mend loop's diagnostics feed.
pub(super) struct PackageCheck {
    pub(super) tree: ProgramTree,
    pub(super) parse_errors: Vec<crate::module::ModuleParseErrors>,
    pub(super) cycles: Vec<Cycle>,
    pub(super) resolve_errors: Vec<ModuleResolveErrors>,
    pub(super) type_errors: Vec<ModuleTypeErrors>,
    pub(super) type_warnings: Vec<ModuleTypeErrors>,
    /// The flattened LATE-PHASE verdict (effect / ownership / concurrency),
    /// `None` when those passes were reached and clean — or were skipped
    /// because an earlier per-module pass already failed (B-2026-08-27-27).
    ///
    /// This is what `karac check` was missing. The per-module passes above
    /// cannot see a cross-module effect at all: the check that catches it runs
    /// on the flattened super-program, which only `karac build` was building.
    pub(super) late_failure: Option<MultiFileLateFailure>,
}

impl PackageCheck {
    /// `src/main.kara`, when it is the package's only module: `karac build`
    /// then builds it as a lone file, against the v2 library it appends, so
    /// `karac check` checks it the same way (B-2026-10-24-39). Not when the
    /// package check refuses one of its imports (an unknown module), which
    /// the lone file's check cannot see (B-2026-10-25-18).
    pub(super) fn lone_entry(&self, root: &std::path::Path) -> Option<std::path::PathBuf> {
        if !self.refuses_only_by_type() {
            return None;
        }
        let mut modules = self.tree.modules.iter().filter(|m| !m.is_synthetic);
        let only = modules.next()?;
        if modules.next().is_some() {
            return None;
        }
        let entry = std::fs::canonicalize(crate::walker::bin_entry_path(
            root,
            crate::walker::Platform::host(),
        ))
        .ok()?;
        (std::fs::canonicalize(&only.file).ok()? == entry).then(|| only.file.clone())
    }

    /// Any error-severity diagnostic, in any module. Warnings are excluded —
    /// they do not gate an exit status.
    /// Whether every refusal is a type-check or late-phase one, which the v2
    /// check of the package's combined source makes as well: no parse error,
    /// cycle or resolve error, the refusals only the module boundaries carry.
    pub(super) fn refuses_only_by_type(&self) -> bool {
        self.parse_errors.is_empty()
            && self.cycles.is_empty()
            && self.resolve_errors.iter().all(|m| m.errors.is_empty())
    }

    /// B-2026-10-25-46: what is left of a type or late-phase refusal once
    /// the v2 check of the package's combined source has passed. That check
    /// makes every refusal the module boundaries do not carry, so a
    /// per-module typecheck, or the flattened late phases, differing from it
    /// on any other rule (a `ref` to an imported `shared` enum, `f64.parse`
    /// typed `Option`) would refuse a package a lone file of the same text
    /// passes. Only the boundary rules' refusals stay.
    pub(super) fn past_the_v2_check(&mut self) {
        if !self.refuses_only_by_type() {
            return;
        }
        for m in &mut self.type_errors {
            m.errors
                .retain(|e| type_error_is_a_module_boundary_rule(&e.kind));
        }
        self.type_errors.retain(|m| !m.errors.is_empty());
        self.late_failure = None;
    }

    /// The machine-applicable resolve fixes of the module at `file`, as
    /// offsets into that file's text: its single edits, then its multi-edit
    /// envelopes.
    pub(super) fn resolve_fixes_of(
        &self,
        file: &std::path::Path,
    ) -> (
        Vec<crate::resolver::TextEdit>,
        Vec<Vec<crate::resolver::TextEdit>>,
    ) {
        let same =
            |p: &std::path::Path| match (std::fs::canonicalize(p), std::fs::canonicalize(file)) {
                (Ok(a), Ok(b)) => a == b,
                _ => p == file,
            };
        let mut singles = Vec::new();
        let mut groups = Vec::new();
        for m in self.resolve_errors.iter().filter(|m| same(&m.file)) {
            singles.extend(
                m.errors
                    .iter()
                    .filter_map(|e| e.replacement.as_deref().cloned()),
            );
            groups.extend(m.fix_diffs.values().cloned());
        }
        (singles, groups)
    }

    /// The machine-applicable fixes of the module-boundary type refusals of
    /// the module at `file` (a wildcard arm or a `..` for a
    /// `#[non_exhaustive]` type of another package; B-2026-11-07-23), as
    /// offsets into that file's text. The combined check cannot see these.
    pub(super) fn boundary_type_fixes_of(
        &self,
        file: &std::path::Path,
    ) -> Vec<crate::resolver::TextEdit> {
        let same =
            |p: &std::path::Path| match (std::fs::canonicalize(p), std::fs::canonicalize(file)) {
                (Ok(a), Ok(b)) => a == b,
                _ => p == file,
            };
        self.type_errors
            .iter()
            .filter(|m| same(&m.file))
            .flat_map(|m| m.errors.iter())
            .filter(|e| type_error_is_a_module_boundary_rule(&e.kind))
            .filter_map(|e| e.fix_it.as_ref())
            .map(|f| crate::resolver::TextEdit {
                offset: f.span.offset,
                length: f.span.length,
                replacement: f.replacement.clone(),
            })
            .collect()
    }

    pub(super) fn has_errors(&self) -> bool {
        !self.parse_errors.is_empty()
            || !self.cycles.is_empty()
            || self.resolve_errors.iter().any(|m| !m.errors.is_empty())
            || self.type_errors.iter().any(|m| !m.errors.is_empty())
            || self.late_failure.is_some()
    }

    /// How many error-severity diagnostics this holds, across every module it
    /// still carries. Used for the caller's summary line, so it counts what was
    /// RENDERED — call it after [`Self::restrict_to_file`], not before.
    pub(super) fn error_count(&self) -> usize {
        self.late_failure
            .as_ref()
            .map(|f| f.diags.len().max(1))
            .unwrap_or(0)
            + self
                .parse_errors
                .iter()
                .map(|m| m.errors.len())
                .sum::<usize>()
            + self.cycles.len()
            + self
                .resolve_errors
                .iter()
                .map(|m| m.errors.len())
                .sum::<usize>()
            + self
                .type_errors
                .iter()
                .map(|m| m.errors.len())
                .sum::<usize>()
    }

    /// Drop every per-module diagnostic that does not belong to `file`, and
    /// return how many ERRORS were dropped.
    ///
    /// `karac check <file>` asks about one file and should answer about that
    /// file — reporting the whole package would flood a caller that named a
    /// single module, and `--output=json` consumers reasonably expect the
    /// diagnostics to concern what they asked about. The dropped count is not
    /// discarded: the caller prints a one-line pointer at `karac check` so a
    /// sibling module's real error is never silently swallowed.
    ///
    /// Cycles are NOT filtered — a cycle is a property of the graph, not of one
    /// file, and it invalidates this file's resolution too.
    pub(super) fn restrict_to_file(&mut self, file: &std::path::Path) -> usize {
        let keep = |p: &std::path::Path| -> bool {
            // Compare canonically where both sides canonicalize; fall back to a
            // literal match so a path that no longer exists still filters
            // predictably rather than dropping everything.
            match (std::fs::canonicalize(p), std::fs::canonicalize(file)) {
                (Ok(a), Ok(b)) => a == b,
                _ => p == file,
            }
        };
        let mut dropped = 0usize;
        dropped += self
            .parse_errors
            .iter()
            .filter(|m| !keep(&m.file))
            .map(|m| m.errors.len())
            .sum::<usize>();
        self.parse_errors.retain(|m| keep(&m.file));
        dropped += self
            .resolve_errors
            .iter()
            .filter(|m| !keep(&m.file))
            .map(|m| m.errors.len())
            .sum::<usize>();
        self.resolve_errors.retain(|m| keep(&m.file));
        dropped += self
            .type_errors
            .iter()
            .filter(|m| !keep(&m.file))
            .map(|m| m.errors.len())
            .sum::<usize>();
        self.type_errors.retain(|m| keep(&m.file));
        self.type_warnings.retain(|m| keep(&m.file));
        dropped
    }

    /// The per-module diagnostics left after [`Self::restrict_to_file`], for
    /// an editor: spans are byte offsets into that one file
    /// (B-2026-11-05-15). Resolve fixes ride along as `karac fix` applies
    /// them; the late flattened passes have no per-file span and are left to
    /// the command line.
    pub(super) fn editor_diagnostics(
        &self,
        file: &std::path::Path,
    ) -> Vec<crate::PlaygroundDiagnostic> {
        let diag = |phase: &'static str,
                    severity: &'static str,
                    message: &str,
                    span: &crate::token::Span,
                    fixes: Vec<crate::SourceFix>| crate::PlaygroundDiagnostic {
            phase,
            severity,
            message: message.to_string(),
            code: None,
            help: Vec::new(),
            line: span.line,
            column: span.column,
            offset: span.offset,
            length: span.length,
            fixes,
        };
        let mut out = Vec::new();
        for m in &self.parse_errors {
            for e in &m.errors {
                out.push(diag("parse", "error", &e.message, &e.span, Vec::new()));
            }
        }
        let this_file =
            |p: &std::path::Path| match (std::fs::canonicalize(p), std::fs::canonicalize(file)) {
                (Ok(a), Ok(b)) => a == b,
                _ => p == file,
            };
        for c in &self.cycles {
            if let Some((f, span)) = c.closing_import(&self.tree) {
                if this_file(f) {
                    let message = format!("circular module dependency: {}", c.format(&self.tree));
                    out.push(diag("resolve", "error", &message, &span, Vec::new()));
                }
            }
        }
        for m in &self.resolve_errors {
            for e in &m.errors {
                let mut fixes: Vec<crate::SourceFix> = e
                    .replacement
                    .iter()
                    .map(|t| crate::SourceFix {
                        offset: t.offset,
                        length: t.length,
                        replacement: t.replacement.clone(),
                    })
                    .collect();
                if let Some(diff) = m
                    .fix_diffs
                    .get(&crate::resolver::SpanKey::from_span(&e.span))
                {
                    fixes = diff
                        .iter()
                        .map(|t| crate::SourceFix {
                            offset: t.offset,
                            length: t.length,
                            replacement: t.replacement.clone(),
                        })
                        .collect();
                }
                out.push(diag("resolve", "error", &e.message, &e.span, fixes));
            }
        }
        // A type error's catalogue code, as the single-file check gives it
        // (B-2026-11-07-46).
        // A warning takes its `W` code, as the single-file feed gives it.
        let coded = |mut d: crate::PlaygroundDiagnostic, code: &str| {
            d.code = Some(code.to_string());
            d
        };
        for m in &self.type_warnings {
            for e in &m.errors {
                let d = diag("typecheck", "warning", &e.message, &e.span, Vec::new());
                let code = super::diag_json::lint_code(e.lint_name.as_deref(), "warning")
                    .unwrap_or_else(|| super::diag_json::type_warning_code(&e.kind));
                out.push(coded(d, code));
            }
        }
        for m in &self.type_errors {
            for e in &m.errors {
                let d = diag("typecheck", "error", &e.message, &e.span, Vec::new());
                let code = super::diag_json::lint_code(e.lint_name.as_deref(), "error")
                    .unwrap_or_else(|| type_error_code(&e.kind));
                out.push(coded(d, code));
            }
        }
        out
    }
}

/// Run [`PackageCheck`] over the package rooted at `root`.
///
/// `Err` carries an already-formatted message for the walk/tree failures that
/// have no per-file span (a broken manifest, a mixed `main.kara`/`lib.kara`
/// entry pair). Callers on the check/run paths treat those as fatal, matching
/// `karac build`.
/// The OS platform whose `_linux` / `_macos` / `_windows` / `_wasm` files a
/// walk should keep, given a v1 compilation target. Only the wasm targets move
/// it; every other target uses the host `karac` was built for. design.md
/// § Platform-specific modules > Target selection states the same rule, and
/// says plainly that the native platforms are therefore host-locked in v1
/// (B-2026-08-20-25).
///
/// Extracted so `cmd_check_project` derives it the same way `cmd_build_project`
/// does — a check that walked a different half of a platform split from the
/// build it is supposed to predict would be the B-2026-08-20-16 divergence in
/// a new place.
pub(super) fn walk_platform_for_target(build_target: &str) -> walker::Platform {
    // A triple names its platform's modules (a cross build takes the
    // target's `_windows` / `_macos` / `_linux` files, not the host's).
    let os = build_target.split('-').skip(1).collect::<Vec<_>>();
    if build_target == "wasm_wasi"
        || build_target == "wasm_browser"
        || build_target.starts_with("wasm32-")
    {
        walker::Platform::Wasm
    } else if os.contains(&"windows") {
        walker::Platform::Windows
    } else if os.contains(&"darwin") {
        walker::Platform::Macos
    } else if os.contains(&"linux") {
        walker::Platform::Linux
    } else {
        walker::Platform::host()
    }
}

pub(super) fn package_check(
    root: &std::path::Path,
    lint_overrides: &crate::lints::CliLintOverrides,
    build_target: &str,
) -> Result<PackageCheck, String> {
    package_check_on(root, lint_overrides, walk_platform_for_target(build_target))
}

/// A type refusal only a module-aware check makes: a private field read
/// or method called from another module, a private type in a public signature, and a
/// `#[non_exhaustive]` type matched or built from another package. The
/// combined source has one scope and one package, so its check cannot see
/// them (B-2026-10-25-46).
fn type_error_is_a_module_boundary_rule(kind: &crate::typechecker::TypeErrorKind) -> bool {
    use crate::typechecker::TypeErrorKind as K;
    matches!(
        kind,
        K::PrivateTypeInPublicSignature
            | K::PrivateFieldAccess
            | K::PrivateMethodAccess
            | K::OrphanImpl
            | K::NonExhaustiveCrossPackageLiteral
            | K::NonExhaustiveCrossPackageMatch
            | K::NonExhaustiveCrossPackagePattern
    )
}

/// [`package_check`] with the OS platform named explicitly rather than derived
/// from the compilation target.
///
/// The derivation only ever answers `_wasm` or the host, so it is the ONLY
/// thing standing between an author and the other half of a platform split:
/// a `_macos` module cannot be reached from a Linux box, which is what
/// `--platform=` overrides (B-2026-08-20-29). Everything downstream of the walk
/// is platform-agnostic — the walker's suffix filter is the whole mechanism —
/// so naming the platform here is the entire feature.
pub(super) fn package_check_on(
    root: &std::path::Path,
    lint_overrides: &crate::lints::CliLintOverrides,
    platform: walker::Platform,
) -> Result<PackageCheck, String> {
    package_check_with_buffer(root, lint_overrides, platform, None)
}

/// [`package_check_on`] with one module's text taken from an editor buffer
/// rather than the disk (B-2026-11-05-15).
pub(super) fn package_check_with_buffer(
    root: &std::path::Path,
    lint_overrides: &crate::lints::CliLintOverrides,
    platform: walker::Platform,
    buffer: Option<(&std::path::Path, &str)>,
) -> Result<PackageCheck, String> {
    // Every caller is `karac check` or the gate in front of `karac run`,
    // both v2 commands, so `ref` takes any named place (B-2026-10-22-28),
    // and each module is checked by the v2 core rules over its D5-rewritten
    // items, as the combined source is behind it (B-2026-10-25-4).
    let lint_overrides = &crate::lints::CliLintOverrides {
        core_places: true,
        strict_core: true,
        ..lint_overrides.clone()
    };
    let walk_opts = WalkerOpts {
        target: platform,
        ..WalkerOpts::default()
    };
    let walked = walker::walk_project(root, walk_opts).map_err(|e| format!("{e}"))?;
    package_check_of_walk(root, lint_overrides, &walked, buffer, false)
}

/// The check of a script inside the package at `root` but outside its
/// `src/`, beside the package's dependencies (B-2026-11-13-30): the program
/// `karac run` builds for it, checked as [`package_check`] checks a package.
pub(super) fn script_check(
    root: &std::path::Path,
    script: &std::path::Path,
    lint_overrides: &crate::lints::CliLintOverrides,
) -> Result<PackageCheck, String> {
    let lint_overrides = &crate::lints::CliLintOverrides {
        core_places: true,
        strict_core: true,
        ..lint_overrides.clone()
    };
    package_check_of_walk(
        root,
        lint_overrides,
        &walker::walk_script(script),
        None,
        false,
    )
}

/// [`package_check_on`] with each module's `_test.kara` companion merged
/// into it, as `karac test` builds them: what `karac fix` of a companion
/// reads its import fixes from (B-2026-11-13-42).
pub(super) fn package_check_with_tests(
    root: &std::path::Path,
    lint_overrides: &crate::lints::CliLintOverrides,
    platform: walker::Platform,
) -> Result<PackageCheck, String> {
    package_check_with_tests_and_buffer(root, lint_overrides, platform, None)
}

/// [`package_check_with_tests`] with one file's text, a module's or a test
/// companion's, taken from an editor buffer: how the editor checks a
/// `_test.kara` file.
pub(super) fn package_check_with_tests_and_buffer(
    root: &std::path::Path,
    lint_overrides: &crate::lints::CliLintOverrides,
    platform: walker::Platform,
    buffer: Option<(&std::path::Path, &str)>,
) -> Result<PackageCheck, String> {
    let lint_overrides = &crate::lints::CliLintOverrides {
        core_places: true,
        strict_core: true,
        ..lint_overrides.clone()
    };
    let walk_opts = WalkerOpts {
        target: platform,
        include_tests: true,
    };
    let walked = walker::walk_project(root, walk_opts).map_err(|e| format!("{e}"))?;
    package_check_of_walk(root, lint_overrides, &walked, buffer, true)
}

/// [`package_check_with_buffer`] past the walk: `walked` is the package's
/// `src/`, or a script's walk.
fn package_check_of_walk(
    root: &std::path::Path,
    lint_overrides: &crate::lints::CliLintOverrides,
    walked: &walker::WalkResult,
    buffer: Option<(&std::path::Path, &str)>,
    merge_test_companions: bool,
) -> Result<PackageCheck, String> {
    // Same lenient dep posture as the `karac run` super-program builder: a
    // dep-resolution failure proceeds without dependency modules rather than
    // failing the check of the local package.
    let dep_walks = super::run_check_cmds::quiet_dep_package_walks(root);
    let built = module::build_program_tree_with_overlay(
        walked,
        &dep_walks,
        module::BuildTreeOpts {
            merge_test_companions,
            overlay_first: false,
        },
        buffer,
    )
    .map_err(|e| format!("{e}"))?;
    let BuildTreeOk {
        mut tree,
        parse_errors,
    } = built;
    let cycles = module::detect_cycles(&tree);
    // Ordering mirrors `cmd_build_project` exactly, including the skips: a
    // half-parsed or cyclic tree cascades spurious E0112/E0113s, and a
    // half-resolved one cascades type errors. Answering differently here would
    // reintroduce the very divergence this function exists to remove.
    let lib = PackageLibrary::of(&tree);
    borrow_bare_params_of(&mut tree, lib.as_ref());
    let resolve_errors = if parse_errors.is_empty() && cycles.is_empty() {
        resolve_modules(&tree, lib.as_ref())
    } else {
        Vec::new()
    };
    let diags = if parse_errors.is_empty() && cycles.is_empty() && resolve_errors.is_empty() {
        typecheck_modules(&tree, lint_overrides, lib.as_ref())
    } else {
        ModuleTypeDiagnostics {
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    };
    // B-2026-08-27-27 — the flattened late phases, the half `karac check` used
    // to skip. Gated on the per-module passes being clean for the reason the
    // comment above gives: a half-resolved tree cascades, and `karac build`
    // reaches these passes under exactly the same condition. Running them here
    // is what makes this function's own claim true — that answering
    // differently from `cmd_build_project` "would reintroduce the very
    // divergence this function exists to remove".
    let late_failure = if parse_errors.is_empty()
        && cycles.is_empty()
        && resolve_errors.is_empty()
        && diags.errors.is_empty()
    {
        let mf = crate::manifest::load_from_cwd(root)
            .ok()
            .map(|(_, m)| manifest::merge_target_overlay(&m, Some(&default_resolution_target(&m))));
        let package_name = mf
            .as_ref()
            .map_or_else(|| "package".to_string(), |m| m.name.clone());
        run_multi_file_late_phases(
            &tree,
            &package_name,
            lint_overrides.core_places,
            lib.as_ref(),
            mf.as_ref(),
        )
        .err()
    } else {
        None
    };
    Ok(PackageCheck {
        tree,
        parse_errors,
        cycles,
        resolve_errors,
        type_errors: diags.errors,
        type_warnings: diags.warnings,
        late_failure,
    })
}

pub(super) fn print_cycles_text(cycles: &[Cycle], tree: &ProgramTree) {
    for c in cycles {
        match c.closing_import(tree) {
            Some((file, span)) => eprintln!(
                "error[E0223]: {}:{}:{}: circular module dependency",
                super::project_v2::shown(file),
                span.line,
                span.column
            ),
            None => eprintln!("error[E0223]: circular module dependency"),
        }
        eprintln!("  cycle: {}", c.format(tree));
        // Name the FILE behind each module on the cycle (B-2026-08-20-19). A
        // cycle is the one module diagnostic with no single span to point at —
        // the offending edge is spread across several files — so the closest
        // thing to "where do I go and look" is the list of files involved. The
        // JSON emitter has carried `cycle_files` all along; the text render
        // left the reader to map dotted module paths back to paths on disk.
        for &id in &c.nodes {
            let m = &tree.modules[id];
            let label = if m.path.is_empty() {
                "(package root)".to_string()
            } else {
                m.path.join(".")
            };
            eprintln!("    {label}  {}", super::project_v2::shown(&m.file));
        }
        eprintln!("  suggestion: {CYCLE_SUGGESTION}");
    }
}

pub(super) fn parse_errors_json(parse_errors: &[ModuleParseErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for pe in parse_errors {
        let file = super::project_v2::shown(&pe.file);
        // The fixes a lone file's check carries: the parse's own, and the
        // ones its help names (`source_fixes`), as offsets into the
        // module's file (B-2026-11-21-74).
        let mut fix_diffs = pe.fix_diffs.clone();
        if let Ok(text) = std::fs::read_to_string(&pe.file) {
            super::source_fixes::complete_parse_errors(
                &text,
                &pe.errors,
                &pe.fix_edits,
                &mut fix_diffs,
            );
        }
        for err in &pe.errors {
            // The symbolic code a message opens with is the record's, with
            // the kind's as `parent_code`, as in a lone file's feed.
            let (code, parent, message) = super::specific_code(err.kind.code(), &err.message);
            let mut record = format!(
                "{{\"severity\":\"error\",\"phase\":\"parse\",\"concept\":\"syntax/lexical-structure\",\"code\":{},\"file\":{},\"line\":{},\"column\":{},\"message\":{}",
                json_string(code),
                json_string(&file),
                err.span.line,
                err.span.column,
                json_string(message),
            );
            if let Some(parent) = parent {
                record.push_str(&format!(",\"parent_code\":{}", json_string(parent)));
            }
            let key = crate::resolver::SpanKey::from_span(&err.span);
            // A `!`'s edit depends on its operand's type (`not`, or `~` for
            // an integer), which `karac fix` reads from the checked file:
            // it is left out here rather than guessed.
            let bang = message.starts_with("the `!` operator is not used");
            if let Some(e) = pe.fix_edits.get(&key).filter(|_| !bang) {
                record.push_str(&format!(
                    ",\"replacement\":{{\"offset\":{},\"length\":{},\"text\":{}}}",
                    e.offset,
                    e.length,
                    json_string(&e.replacement),
                ));
            } else if let Some(edits) = fix_diffs.get(&key).filter(|v| !v.is_empty()) {
                let items: Vec<String> = edits
                    .iter()
                    .map(|e| {
                        format!(
                            "{{\"offset\":{},\"length\":{},\"text\":{}}}",
                            e.offset,
                            e.length,
                            json_string(&e.replacement),
                        )
                    })
                    .collect();
                record.push_str(&format!(",\"fix_diff\":[{}]", items.join(",")));
            }
            record.push('}');
            out.push(record);
        }
    }
    out
}

/// What an import cycle is fixed by, in the text render and the JSON alike.
const CYCLE_SUGGESTION: &str =
    "extract the shared items into a lower-layer module that both ends of the cycle can depend on.";

pub(super) fn cycles_json(cycles: &[Cycle], tree: &ProgramTree) -> Vec<String> {
    cycles
        .iter()
        .map(|c| {
            let paths: Vec<String> = c
                .nodes
                .iter()
                .map(|id| {
                    let p = &tree.modules[*id].path;
                    if p.is_empty() {
                        String::new()
                    } else {
                        p.join(".")
                    }
                })
                .collect();
            let paths_json: Vec<String> = paths.iter().map(|s| json_string(s)).collect();
            let files: Vec<String> = c
                .nodes
                .iter()
                .map(|id| json_string(&super::project_v2::shown(&tree.modules[*id].file)))
                .collect();
            // Located at the import that closes the cycle (B-2026-10-30-32).
            let at = c
                .closing_import(tree)
                .map(|(file, span)| {
                    format!(
                        ",\"file\":{},\"line\":{},\"column\":{}",
                        json_string(&super::project_v2::shown(file)),
                        span.line,
                        span.column
                    )
                })
                .unwrap_or_default();
            // The suggestion the text render prints, as a lone file's
            // record carries one (B-2026-11-21-79).
            format!(
                "{{\"severity\":\"error\",\"phase\":\"module_graph\",\"concept\":\"modules/imports\",\"code\":\"E0223\"{at},\"message\":{},\"cycle_paths\":[{}],\"cycle_files\":[{}],\"hints\":[{{\"description\":{}}}]}}",
                json_string(&c.format(tree)),
                paths_json.join(","),
                files.join(","),
                json_string(CYCLE_SUGGESTION),
            )
        })
        .collect()
}

pub(super) fn parse_errors_jsonl(parse_errors: &[ModuleParseErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for pe in parse_errors {
        let file = super::project_v2::shown(&pe.file);
        for err in &pe.errors {
            out.push(format!(
                "{{\"type\":\"parse_error\",\"file\":{},\"line\":{},\"column\":{},\"message\":{}}}",
                json_string(&file),
                err.span.line,
                err.span.column,
                json_string(&err.message),
            ));
        }
    }
    out
}

pub(super) fn cycles_jsonl(cycles: &[Cycle], tree: &ProgramTree) -> Vec<String> {
    cycles
        .iter()
        .map(|c| {
            let paths: Vec<String> = c
                .nodes
                .iter()
                .map(|id| {
                    let p = &tree.modules[*id].path;
                    if p.is_empty() {
                        String::new()
                    } else {
                        p.join(".")
                    }
                })
                .collect();
            let paths_json: Vec<String> = paths.iter().map(|s| json_string(s)).collect();
            format!(
                "{{\"type\":\"module_cycle\",\"code\":\"E0223\",\"message\":{},\"cycle_paths\":[{}],\"hints\":[{{\"description\":{}}}]}}",
                json_string(&c.format(tree)),
                paths_json.join(","),
                json_string(CYCLE_SUGGESTION),
            )
        })
        .collect()
}

pub(super) fn emit_build_tree_error(e: &BuildTreeError, output: OutputMode) {
    let code = e.code().unwrap_or("module");
    match output {
        OutputMode::Text => {
            eprintln!("error[{code}]: {e}");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"module_graph\",\"concept\":\"modules/imports\",\"code\":{},\"message\":{}}}]}}",
                json_string(code),
                json_string(&e.to_string()),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "build_tree_error",
                &format!(
                    "\"code\":{},\"message\":{}",
                    json_string(code),
                    json_string(&e.to_string()),
                ),
            );
        }
    }
}

pub(super) fn emit_manifest_error(e: &manifest::ManifestError, output: OutputMode) {
    let code = e.code().unwrap_or("manifest");
    match output {
        OutputMode::Text => {
            eprintln!("error[{code}]: {e}");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"manifest\",\"concept\":\"packages/manifests\",\"code\":{},\"message\":{}}}]}}",
                json_string(code),
                json_string(&e.to_string()),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "manifest_error",
                &format!(
                    "\"code\":{},\"message\":{}",
                    json_string(code),
                    json_string(&e.to_string()),
                ),
            );
        }
    }
}

/// The active target triple for resolver invocations that carry no `--target`
/// flag — `test` / `resolve` / `update`. Precedence mirrors the `None`-flag
/// case of `cmd_build_project`'s target resolution: the manifest's
/// `[build].default-target` if set, else the host triple. Threading this
/// through `merge_target_overlay` (below) makes these commands consume the
/// same per-target `[dependencies]` view a plain `karac build` would — the
/// resolver-follow-up-(e) gap where only `cmd_build_project` merged the
/// overlay, so `[target.<triple>.dependencies]` silently dropped out of
/// `test` / `resolve` / `update`.
pub(super) fn default_resolution_target(mf: &manifest::Manifest) -> String {
    mf.build_default_target
        .clone()
        .unwrap_or_else(crate::build_cache::host_target_triple)
}

/// Surface dependency-resolution diagnostics for the static/lenient commands
/// that consult a project but don't fetch — `karac check` and `karac run`
/// (resolver follow-up (m)). **Path-dep-only** — both are fast passes that must
/// not touch the network, so no registry/git provider is threaded in. Returns
/// `false` (halt the command) when a fatal graph error is emitted; `true` to
/// continue.
///
/// The dependency resolution `karac check` and `karac run` do before
/// checking a package or a file of one: the same as `karac build`'s
/// ([`run_dep_resolution`]), fetching a git dependency and, through a
/// configured proxy, a registry one, so every command refuses what the build
/// refuses with the same code (B-2026-11-13-41). It writes no `kara.lock`.
/// The dependencies it resolves are the ones the package check then walks
/// ([`super::run_check_cmds::install_resolved_dep_walks`]). `false` means a
/// diagnostic was emitted and the command must halt.
pub(super) fn surface_dep_graph_diagnostics(
    root: &std::path::Path,
    mf: crate::manifest::Manifest,
    output: OutputMode,
    dev_deps: bool,
) -> bool {
    // `karac check` (`dev_deps`) also resolves `[dev-dependencies]`, as
    // `karac test` will, so one that cannot resolve is reported in check's
    // own form whether or not a `_test.kara` exists (PK1). Only the build
    // graph's packages are walked: a dev-dependency is not importable from
    // the package itself.
    if dev_deps
        && !mf.dev_dependencies.is_empty()
        && run_dep_resolution(root, mf.clone(), output, None, true, false, false).is_err()
    {
        return false;
    }
    let Ok(resolution) = run_dep_resolution(root, mf, output, None, false, false, false) else {
        return false;
    };
    let platform = walk_platform_for_target(crate::target::active_target());
    match dep_package_walks(root, resolution.as_ref(), platform, output) {
        Ok(walks) => {
            super::run_check_cmds::install_resolved_dep_walks(walks);
            true
        }
        Err(()) => false,
    }
}

/// Build the dep graph and resolve it against the active toolchain. Returns
/// `true` to continue with the build, `false` to halt. Every finding is
/// fatal, an unsupported registry/git source included (B-2026-11-13-41). Slice 7
/// of the PubGrub-resolver entry (`docs/implementation_checklist/phase-5-
/// diagnostics.md` line 813). Wiring point: `cmd_build_project` right
/// after the manifest loads.
///
/// `include_dev_deps` activates the test-mode walk (line 884) — the root
/// manifest's `[dev-dependencies]` participate in resolution. Off in
/// build mode; on in test mode. Dev-deps do not propagate through
/// transitive children regardless of the flag.
/// Resolve the project's dependency graph. `Err(())` means a fatal
/// diagnostic was emitted and the build must halt. `Ok(Some(resolution))`
/// carries the concrete package set for cross-package module loading
/// (phase-5 line 898). `Ok(None)` is no longer returned; an unsupported
/// registry/git source is an `Err(())` like any other finding.
pub(super) fn run_dep_resolution(
    root: &std::path::Path,
    mf: crate::manifest::Manifest,
    output: OutputMode,
    offline_root: Option<&std::path::Path>,
    include_dev_deps: bool,
    no_proxy: bool,
    persist_lock: bool,
) -> Result<Option<crate::dep_resolver::Resolution>, ()> {
    let loader = crate::dep_graph::FsLoader;
    let registries = (mf.build_registry.clone(), mf.build_registry_proxy.clone());
    with_dep_providers(
        (registries.0.as_deref(), registries.1.as_deref()),
        offline_root,
        no_proxy,
        |registry_provider, git_provider| {
            let solve = |dir: &std::path::Path,
                         mf: crate::manifest::Manifest,
                         pins: &std::collections::BTreeMap<String, semver::Version>|
             -> Result<
                (crate::dep_resolver::Resolution, crate::dep_graph::DepGraph),
                (),
            > {
                let options = crate::dep_graph::DepGraphOptions {
                    offline_root,
                    include_dev_deps,
                    registry_provider,
                    git_provider,
                    pins: Some(pins),
                };
                let graph =
                    match crate::dep_graph::build_dep_graph_with_options(dir, mf, &loader, options)
                    {
                        Ok(g) => g,
                        Err(e) => {
                            let diag = crate::dep_diagnostic::render_dep_graph_error(&e);
                            emit_dep_diagnostic(&diag, output, "error");
                            return Err(());
                        }
                    };
                let active = crate::dep_resolver::active_toolchain_version();
                match crate::dep_resolver::resolve_with_pins(&graph, &active, offline_root, pins) {
                    Ok(resolution) => Ok((resolution, graph)),
                    Err(boxed) => {
                        let diag = crate::dep_diagnostic::render_resolver_error(&boxed);
                        // A registry dependency with no configured proxy, or a git one
                        // with nothing to fetch it with, is refused: dropped, it would
                        // only resurface as an "unknown module" at its first import
                        // (design.md § 4, Dependencies in 0.8; B-2026-11-13-41).
                        emit_dep_diagnostic(&diag, output, "error");
                        Err(())
                    }
                }
            };

            // Lockfile-pin-over-catalog (follow-up (d)/(h)): read an existing
            // `kara.lock` and prefer its recorded registry-package versions, so a
            // rebuild reproduces the locked graph rather than drifting to the newest
            // compatible version. The pins feed BOTH the graph walk (slice 4 — a pinned
            // registry dep is fetched at exactly that version via `fetch_exact`, even if
            // since-yanked, and added to the candidate set) and version selection (slice
            // 2). Best-effort — an absent / unreadable / malformed lockfile yields no
            // pins (fresh selection), never a build error. Pins bite only where the
            // registry candidate set is widened; path/git deps ignore them.
            //
            // In a workspace (design.md § Workspaces, B-2026-11-13-60) the lock is the
            // root's, shared by every member: it is solved once over all the members'
            // requirements, and this package is then resolved pinned to that solution,
            // so two members always agree on a dependency's version.
            let workspace = crate::dep_graph::workspace_root_of(root, &loader);
            let pins = match &workspace {
                // Offline, the joint solve cannot run: its manifest names the
                // members as path dependencies, which offline mode would look
                // up in `vendor/`, and it would rewrite the shared lock with
                // vendored git and registry dependencies as path ones. The
                // lock's recorded pins stand for it, and the lock is left as
                // the online resolution wrote it (B-2026-11-18-29).
                Some(ws_root) if offline_root.is_some() => read_lockfile_pins(ws_root, output),
                Some(ws_root) => {
                    let wmf = match crate::dep_graph::workspace_lock_manifest(
                        ws_root,
                        &loader,
                        include_dev_deps,
                    ) {
                        Ok(m) => m,
                        Err(e) => {
                            emit_manifest_error(&e, output);
                            return Err(());
                        }
                    };
                    let (joint, _) = solve(ws_root, wmf, &read_lockfile_pins(ws_root, output))?;
                    // `karac resolve` is read-only — it inspects the graph without
                    // rewriting `kara.lock`. Only build / test persist the pin.
                    if persist_lock {
                        persist_lockfile(ws_root, &joint, output);
                    }
                    resolution_pins(&joint)
                }
                None => read_lockfile_pins(root, output),
            };
            let (resolution, graph) = solve(root, mf, &pins)?;
            // Warn on any resolved version the catalog marks yanked (follow-up
            // (h), slice 4). Fresh selection excludes yanked versions, so this
            // fires only when a `kara.lock` pin lands on a version yanked since
            // it was recorded — reproducibility kept it, and the user should
            // hear the pin is now withdrawn.
            emit_yanked_pin_warnings(&resolution, &graph, output);
            // Offline, a git or registry dependency resolved from its `vendor/`
            // copy as a path one; `kara.lock` keeps the source and pin the
            // online resolution recorded.
            let read_vendored_fetch = offline_root.is_some()
                && graph.manifests.values().any(|m| {
                    m.dependencies.values().any(|d| {
                        matches!(
                            d,
                            crate::manifest::DependencySpec::Registry { .. }
                                | crate::manifest::DependencySpec::Git { .. }
                        )
                    })
                });
            if persist_lock && workspace.is_none() && !read_vendored_fetch {
                persist_lockfile(root, &resolution, output);
            }
            Ok(Some(resolution))
        },
    )
}

/// Builds the registry and git providers a resolution fetches through and
/// hands them to `f`: what `karac build` / `check` / `run` / `test` resolve
/// with, and `karac vendor` and `karac update` too (they used to resolve
/// with neither, so a git or registry dependency was never fetched).
/// `registries` is the manifest's `[build].registry` and
/// `[build].registry-proxy`.
pub(super) fn with_dep_providers<R>(
    registries: (Option<&str>, Option<&str>),
    offline_root: Option<&std::path::Path>,
    no_proxy: bool,
    f: impl FnOnce(
        Option<&dyn crate::dep_graph::RegistryProvider>,
        Option<&dyn crate::git_fetch::GitProvider>,
    ) -> R,
) -> R {
    // Activate the registry fetch path. A `ProxyRegistryProvider` — the
    // cache → retry → live-HTTP decorator stack — is threaded into the graph
    // walk so a `[dependencies]` registry entry is fetched, extracted, and
    // recursed into exactly like a path-dep. The *only* difference between
    // the proxy path (slice 4) and the direct-from-source path (follow-ups
    // (j)/(k)) is which base URL the HTTP client points at; the whole stack
    // below (retry + tarball cache + extraction) is base-URL-agnostic.
    //
    // Decide the effective registry base URL, if any:
    //   * `--offline` / no usable cache root: never touch the network — the
    //     `vendor/` walk owns resolution — so no base URL, provider stays off.
    //   * `--no-proxy` (direct-from-source, follow-ups (j)/(k)): fetch straight
    //     from the configured upstream registry (`KARAC_REGISTRY_URL` /
    //     `[build].registry`), bypassing the proxy. Unconfigured → `None`, so a
    //     registry dep keeps the warn-and-continue contract
    //     (`E_REGISTRY_DEP_UNSUPPORTED`) rather than fetching against nothing.
    //   * otherwise (proxy path): fetch through the configured proxy. The
    //     built-in `DEFAULT_PROXY_URL` is a not-yet-live placeholder, so this
    //     only activates once an operator points `KARAC_REGISTRY_PROXY` (or
    //     `[build].registry-proxy`) at a real proxy (`explicit_proxy_configured`).
    //
    // The client stack and provider live in locals whose borrows outlive the
    // `build_dep_graph_with_options` call below (it returns an owned graph
    // with every registry resolution already materialized to disk).
    let cache_root = crate::registry_proxy::default_registry_cache_root();
    let registry_base: Option<String> = if offline_root.is_some() || cache_root.is_none() {
        None
    } else if no_proxy {
        crate::registry_proxy::resolve_direct_registry_url(registries.0)
    } else if crate::registry_proxy::explicit_proxy_configured(registries.1) {
        Some(
            crate::registry_proxy::ProxyConfig::resolve(
                crate::registry_proxy::ProxyMode::Default,
                registries.1,
            )
            .url,
        )
    } else {
        None
    };
    let client_stack: Option<Box<dyn crate::registry_proxy::ProxyClient>> =
        registry_base.map(|url| {
            // A per-user `KARAC_REGISTRY_TOKEN` authenticates a private proxy
            // or private direct registry alike.
            let token = crate::registry_proxy::registry_token_from_env();
            let http = crate::registry_proxy::HttpProxyClient::with_token(url, token);
            let retrying = crate::registry_proxy::RetryingProxyClient::new(
                Box::new(http),
                crate::registry_proxy::RetryPolicy::default(),
            );
            // Tarball cache under <root>/<name>/<version>/package.tar.gz; the
            // provider extracts to the sibling <root>/<name>/<version>/src, so
            // the two share one root without colliding.
            let caching = crate::registry_proxy::CachingProxyClient::new(
                Box::new(retrying),
                cache_root.clone().unwrap_or_default(),
            );
            Box::new(caching) as Box<dyn crate::registry_proxy::ProxyClient>
        });
    let provider = client_stack.as_ref().map(|c| {
        crate::registry_extract::ProxyRegistryProvider::new(
            c.as_ref(),
            cache_root.clone().unwrap_or_default(),
        )
    });

    // Git deps are direct-from-source (no proxy in the loop), so git fetch is
    // gated only on `--offline` — not on `--no-proxy` or an explicitly
    // configured proxy. A git URL is real (unlike the placeholder default
    // proxy), so cloning whenever a git dep is declared is always correct.
    let git_provider = if offline_root.is_none() {
        crate::git_fetch::default_git_cache_root().map(crate::git_fetch::GitCliProvider::new)
    } else {
        None
    };

    let registry_provider = provider
        .as_ref()
        .map(|p| p as &dyn crate::dep_graph::RegistryProvider);
    let git_provider = git_provider
        .as_ref()
        .map(|p| p as &dyn crate::git_fetch::GitProvider);
    f(registry_provider, git_provider)
}

/// The directory whose `dist/` holds the package at `root`'s output: the
/// workspace root for a workspace member (design.md § Workspaces: members
/// share one output directory, B-2026-11-13-60), else `root` itself.
pub(super) fn output_root(root: &std::path::Path) -> std::path::PathBuf {
    let own = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    match crate::dep_graph::workspace_root_of(root, &crate::dep_graph::FsLoader) {
        Some(ws) if ws != own => ws,
        _ => root.to_path_buf(),
    }
}

/// The registry versions a resolution selected, as lock pins: what a
/// workspace member's own resolution is pinned to (B-2026-11-13-60).
fn resolution_pins(
    resolution: &crate::dep_resolver::Resolution,
) -> std::collections::BTreeMap<String, semver::Version> {
    resolution
        .packages
        .values()
        .filter(|p| {
            matches!(
                p.source,
                crate::dep_resolver::ResolvedSource::Registry { .. }
            )
        })
        .map(|p| (p.name.clone(), p.version.clone()))
        .collect()
}

/// Walk each resolved path-dependency's source tree for cross-package
/// module loading (phase-5 line 898). Returns one [`module::DepPackageWalk`]
/// per path-sourced package, in `Resolution`'s deterministic (BTreeMap,
/// name-sorted) order. `Err(())` means a diagnostic was already emitted.
///
/// Dependencies must be library packages: a dep whose entry is
/// `src/main.kara` (or which has no entry at all) is a hard error, since
/// its items have nowhere to hoist and a binary cannot be imported.
/// Dependency test companions are excluded (`include_tests: false`) — a
/// consumer never compiles its deps' tests.
pub(super) fn dep_package_walks(
    root: &std::path::Path,
    resolution: Option<&crate::dep_resolver::Resolution>,
    target: walker::Platform,
    output: OutputMode,
) -> Result<Vec<module::DepPackageWalk>, ()> {
    let Some(resolution) = resolution else {
        return Ok(Vec::new());
    };
    let names = match dep_import_names(root, resolution) {
        Ok(names) => names,
        Err((pkg, message)) => {
            emit_dep_walk_error(&pkg, &message, output);
            return Err(());
        }
    };
    let mut out = Vec::new();
    for pkg in resolution.packages.values() {
        // Path-deps, fetched registry deps, and cloned git deps all carry an
        // on-disk source root the module loader compiles (each threaded its
        // materialized directory into its `ResolvedSource` variant). `Root`
        // is the project itself.
        let dep_root: &std::path::Path = match &pkg.source {
            crate::dep_resolver::ResolvedSource::Path(dir) => dir,
            crate::dep_resolver::ResolvedSource::Registry { dir, .. } => dir,
            crate::dep_resolver::ResolvedSource::Git { dir, .. } => dir,
            _ => continue,
        };
        let walk_opts = WalkerOpts {
            target,
            include_tests: false,
        };
        let walked = match walker::walk_project(dep_root, walk_opts) {
            Ok(w) => w,
            Err(e) => {
                emit_dep_walk_error(&pkg.name, &e.to_string(), output);
                return Err(());
            }
        };
        if walked.entry != walker::EntryKind::Lib {
            let why = match walked.entry {
                walker::EntryKind::Bin => {
                    "it has `src/main.kara` — a binary package cannot be imported"
                }
                _ => "it has no `src/lib.kara` entry file",
            };
            emit_dep_walk_error(
                &pkg.name,
                &format!(
                    "dependency `{}` is not a library package: {}",
                    pkg.name, why
                ),
                output,
            );
            return Err(());
        }
        out.push(dep_package_walk(&pkg.name, names.get(&pkg.name), walked));
    }
    Ok(out)
}

/// A resolved package's on-disk source root; `None` for the root package.
pub(super) fn resolved_dep_dir(
    pkg: &crate::dep_resolver::ResolvedPackage,
) -> Option<&std::path::Path> {
    match &pkg.source {
        crate::dep_resolver::ResolvedSource::Path(dir) => Some(dir),
        crate::dep_resolver::ResolvedSource::Registry { dir, .. } => Some(dir),
        crate::dep_resolver::ResolvedSource::Git { dir, .. } => Some(dir),
        _ => None,
    }
}

/// Each resolved dependency's registered import name, and the renames its
/// own imports need (design.md § Packages and manifests). A package is
/// registered under the name the root package writes for it: the root's
/// `alias`, else the package's `[package].import`, else its name with `-`
/// mapped to `_`. A dependency that writes another name for one of its own
/// dependencies (its own `alias`, or a package the root aliased) gets a
/// rename from that name to the registered one. Two packages registered
/// under one name are an error, `(package, message)`.
pub(super) type DepImportNames = std::collections::BTreeMap<String, DepImportName>;

/// One resolved package's import names ([`dep_import_names`]).
#[derive(Clone)]
pub(super) struct DepImportName {
    /// The name its modules are registered under.
    pub(super) import_name: String,
    /// Its own imports' renames ([`module::DepPackageWalk::renames`]).
    pub(super) renames: Vec<(String, String)>,
    /// The registered names of the packages it declares itself.
    pub(super) declares: Vec<String>,
    /// Whether the root package declares it.
    pub(super) direct: bool,
}

pub(super) fn dep_import_names(
    root: &std::path::Path,
    resolution: &crate::dep_resolver::Resolution,
) -> Result<DepImportNames, (String, String)> {
    let root_mf = manifest::load_from_root(root).ok();
    let mut manifests = std::collections::BTreeMap::new();
    for pkg in resolution.packages.values() {
        if let Some(mf) = resolved_dep_dir(pkg).and_then(|d| manifest::load_from_root(d).ok()) {
            manifests.insert(pkg.name.clone(), mf);
        }
    }
    let declared = |name: &str| manifests.get(name).and_then(|m| m.import.as_deref());
    let registered = |name: &str| -> String {
        match &root_mf {
            Some(r) => r.import_name_for_dependency(name, declared(name)),
            None => declared(name)
                .map(str::to_string)
                .unwrap_or_else(|| manifest::default_import_name(name)),
        }
    };
    let mut out = DepImportNames::new();
    let mut claimed: std::collections::BTreeMap<String, String> = Default::default();
    for (name, mf) in &manifests {
        let import_name = registered(name);
        if let Some(other) = claimed.get(&import_name) {
            return Err((
                name.clone(),
                format!(
                    "dependencies `{other}` and `{name}` are both imported as `{import_name}`; give one of them an `alias` in kara.toml"
                ),
            ));
        }
        claimed.insert(import_name.clone(), name.clone());
        let renames = mf
            .dependencies
            .keys()
            .filter_map(|dep| {
                let written = mf.import_name_for_dependency(dep, declared(dep));
                let target = registered(dep);
                (written != target).then_some((written, target))
            })
            .collect();
        let declares = mf
            .every_declared_dependency()
            .into_iter()
            .map(registered)
            .collect();
        let direct = root_mf
            .as_ref()
            .is_none_or(|r| r.every_declared_dependency().contains(name.as_str()));
        out.insert(
            name.clone(),
            DepImportName {
                import_name,
                renames,
                declares,
                direct,
            },
        );
    }
    Ok(out)
}

/// The [`module::DepPackageWalk`] of the resolved package `name`. A package
/// [`dep_import_names`] read no manifest for imports under its default name
/// and has no dependency scope.
pub(super) fn dep_package_walk(
    name: &str,
    names: Option<&DepImportName>,
    walked: walker::WalkResult,
) -> module::DepPackageWalk {
    match names {
        Some(n) => module::DepPackageWalk {
            name: name.to_string(),
            import_name: n.import_name.clone(),
            renames: n.renames.clone(),
            declares: Some(n.declares.clone()),
            direct: n.direct,
            walked,
        },
        None => module::DepPackageWalk {
            name: name.to_string(),
            import_name: manifest::default_import_name(name),
            renames: Vec::new(),
            declares: None,
            direct: true,
            walked,
        },
    }
}

/// Render a dependency-walk failure (walker error or non-library dep) in
/// the active output mode. Mirrors `emit_walker_error`'s shape with the
/// owning package named in the message.
pub(super) fn emit_dep_walk_error(pkg: &str, message: &str, output: OutputMode) {
    match output {
        OutputMode::Text => {
            eprintln!("error[walker]: in dependency `{pkg}`: {message}");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"walker\",\"concept\":\"packages/manifests\",\"code\":\"walker\",\"package\":{},\"message\":{}}}]}}",
                json_string(pkg),
                json_string(message),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "walker_error",
                &format!(
                    "\"code\":\"walker\",\"package\":{},\"message\":{}",
                    json_string(pkg),
                    json_string(message),
                ),
            );
        }
    }
}

/// `karac-toolchain.toml` enforcement (tracker line 892). Returns
/// `true` to continue with the build, `false` to halt. When the file
/// is absent the function is a no-op. When present, the declared
/// `version` constraint is intersected against the active compiler
/// version; mismatch surfaces `E_TOOLCHAIN_VERSION_MISMATCH` with a
/// `karaup` hint. Parse errors halt with the file-specific symbolic
/// code so the operator hears about a malformed pin rather than
/// silently building against an unintended toolchain.
pub(super) fn enforce_toolchain_pin(root: &std::path::Path, output: OutputMode) -> bool {
    let load = crate::karac_toolchain::load_from_start(root);
    let (path, spec) = match load {
        Ok(Some(pair)) => pair,
        Ok(None) => return true,
        Err(e) => {
            emit_toolchain_load_error(&e, output);
            return false;
        }
    };
    let active = crate::dep_resolver::active_toolchain_version();
    match crate::karac_toolchain::enforce(&spec, &path, &active) {
        Ok(()) => true,
        Err(mismatch) => {
            emit_toolchain_mismatch(&mismatch, output);
            false
        }
    }
}

/// Render a `karac_toolchain::ToolchainError` (parse / IO failure) into
/// the active output mode. Symbolic code surfaces so downstream tooling
/// can recognize the kind of failure without parsing the message.
pub(super) fn emit_toolchain_load_error(
    err: &crate::karac_toolchain::ToolchainError,
    output: OutputMode,
) {
    let code = err.code();
    let primary = err.to_string();
    // A `version` written as a bare number has one edit, the quotes, which
    // `karac fix` applies to the pin file: the record names the file and
    // the edit, with offsets into that file (B-2026-11-23-2).
    let fix = crate::karac_toolchain::version_quote_fix(err);
    let help = fix
        .as_ref()
        .map(|(_, _, _, quoted)| format!("write it as a string: `version = {quoted}`"));
    let tail = match (&fix, &help) {
        (Some((path, offset, length, quoted)), Some(help)) => format!(
            ",\"file\":{},\"help\":{},\"fix_diff\":[{{\"offset\":{offset},\"length\":{length},\"text\":{}}}]",
            json_string(&path.display().to_string()),
            json_string(help),
            json_string(quoted),
        ),
        _ => String::new(),
    };
    match output {
        OutputMode::Text => {
            eprintln!("error[{code}]: {primary}");
            if let Some(help) = &help {
                eprintln!("   = help: {help}");
            }
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"toolchain_pin\",\"concept\":\"packages/manifests\",\"code\":{},\"message\":{}{tail}}}]}}",
                json_string(code),
                json_string(&primary),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "toolchain_pin_error",
                &format!(
                    "\"code\":{},\"message\":{}{tail}",
                    json_string(code),
                    json_string(&primary),
                ),
            );
        }
    }
}

/// The `,"notes":[..],"help":".."` tail a package-level diagnostic's JSON
/// record carries, the lines its text render prints as `= note:` and
/// `= help:` (B-2026-11-21-79).
fn notes_help_tail(notes: &[&str], help: &str) -> String {
    let notes: Vec<String> = notes.iter().map(|n| json_string(n)).collect();
    format!(
        ",\"notes\":[{}],\"help\":{}",
        notes.join(","),
        json_string(help)
    )
}

/// Render a `karac_toolchain::ToolchainMismatch` diagnostic into the
/// active output mode. The note documents the v1 limitation: karac
/// today reads the pin but does not auto-switch — operators install
/// the required toolchain via `karaup` (deferred) or manually.
pub(super) fn emit_toolchain_mismatch(
    mismatch: &crate::karac_toolchain::ToolchainMismatch,
    output: OutputMode,
) {
    let code = mismatch.code();
    let primary = mismatch.message();
    let note = format!(
        "install a matching toolchain via `karaup install {}` (karaup is not in 0.8)",
        mismatch.required
    );
    let help =
        "or relax the `version` constraint in `karac-toolchain.toml` to admit the active toolchain";
    match output {
        OutputMode::Text => {
            eprintln!("error[{code}]: {primary}");
            eprintln!("   = note: {note}");
            eprintln!("   = help: {help}");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"toolchain_pin\",\"concept\":\"packages/manifests\",\"code\":{},\"message\":{},\"required\":{},\"active\":{}{}}}]}}",
                json_string(code),
                json_string(&primary),
                json_string(&mismatch.required.to_string()),
                json_string(&mismatch.active.to_string()),
                notes_help_tail(&[&note], help),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "toolchain_pin_error",
                &format!(
                    "\"code\":{},\"message\":{},\"required\":{},\"active\":{}{}",
                    json_string(code),
                    json_string(&primary),
                    json_string(&mismatch.required.to_string()),
                    json_string(&mismatch.active.to_string()),
                    notes_help_tail(&[&note], help),
                ),
            );
        }
    }
}

/// Pre-check diagnostic for `karac build --offline` when the project root
/// has no `./vendor/` directory. The resolver would otherwise error per-dep
/// with `E_OFFLINE_VENDOR_ENTRY_MISSING`; surfacing the missing root once,
/// up front, is a clearer operator hint.
pub(super) fn emit_offline_no_vendor_dir(vendor_dir: &std::path::Path, output: OutputMode) {
    let code = "E_OFFLINE_NO_VENDOR_DIR";
    let primary = format!(
        "offline build requires a vendor directory at `{}` but none was found",
        vendor_dir.display()
    );
    let note = "--offline resolves every transitive path-dep against `./vendor/<name>/`";
    let help = "run `karac vendor` to populate the vendor directory, then re-run with `--offline`";
    match output {
        OutputMode::Text => {
            eprintln!("error[{code}]: {primary}");
            eprintln!("   = note: {note}");
            eprintln!("   = help: {help}");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"dep_resolution\",\"concept\":\"packages/manifests\",\"code\":{},\"message\":{}{}}}]}}",
                json_string(code),
                json_string(&primary),
                notes_help_tail(&[note], help),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "dep_resolution_error",
                &format!(
                    "\"code\":{},\"message\":{}{}",
                    json_string(code),
                    json_string(&primary),
                    notes_help_tail(&[note], help),
                ),
            );
        }
    }
}

/// Read the project's `kara.lock` (if present) and extract the registry-package
/// version pins for lockfile-pin-over-catalog resolution (follow-up (d)/(h),
/// slice 3). Best-effort: a missing / unreadable / malformed lockfile yields an
/// empty pin map, so a fresh project or a corrupt lockfile falls back to fresh
/// version selection rather than failing the build. A lockfile that exists but
/// does not parse is said so, as a warning: its pins are dropped and the file
/// is rewritten, which the user should hear (B-2026-11-16-303, whose
/// `E_LOCKFILE_*` codes nothing reported before).
pub(super) fn read_lockfile_pins(
    root: &std::path::Path,
    output: OutputMode,
) -> std::collections::BTreeMap<String, semver::Version> {
    let path = root.join("kara.lock");
    let Ok(source) = std::fs::read_to_string(&path) else {
        return std::collections::BTreeMap::new();
    };
    match crate::lockfile::Lockfile::parse(&path, &source) {
        Ok(lock) => lock.version_pins(),
        Err(e) => {
            emit_lockfile_diag(
                &e,
                output,
                "the lockfile's version pins are ignored and versions are chosen afresh; \
                 the next `karac build` or `karac test` rewrites `kara.lock`",
                "if the file was edited by hand, restore it from version control",
                false,
            );
            std::collections::BTreeMap::new()
        }
    }
}

/// Emit a `W_DEPENDENCY_YANKED` warning for each resolved package whose selected
/// version the catalog marks yanked (resolver follow-up (h), slice 4). Fresh
/// selection never picks a yanked version, so this only fires when a `kara.lock`
/// pin lands on a version yanked *since* it was recorded — reproducibility kept
/// the pin, and the user should hear it is now withdrawn. Purely advisory: it
/// never fails the build.
pub(super) fn emit_yanked_pin_warnings(
    resolution: &crate::dep_resolver::Resolution,
    graph: &crate::dep_graph::DepGraph,
    output: OutputMode,
) {
    for pkg in resolution.packages.values() {
        let Some(yanked) = graph.yanked_versions.get(&pkg.name) else {
            continue;
        };
        if !yanked.contains(&pkg.version) {
            continue;
        }
        let diag = crate::dep_diagnostic::Diagnostic {
            code: "W_DEPENDENCY_YANKED",
            primary: format!(
                "dependency `{}` is pinned to version {}, which the registry has yanked",
                pkg.name, pkg.version
            ),
            notes: vec![
                "the version is recorded in `kara.lock`, but the registry has since withdrawn it — a yanked release is kept resolvable for reproducibility, yet should not be relied on for new work".to_string(),
            ],
            help: Some(format!(
                "run `karac update {}` to move to a non-yanked version, or pin a different version in `kara.toml`",
                pkg.name
            )),
        };
        emit_dep_diagnostic(&diag, output, "warning");
    }
}

/// Slice 4 of the lockfile entry (phase-5 line 831). Materializes a fresh
/// `kara.lock` from the resolver's output and writes it at the project root.
/// On read-then-rewrite paths, suppresses the write when the bytes are
/// identical so file mtimes are stable across no-op rebuilds. Any lockfile
/// IO failure is emitted as a warning (build-blocking would be too strict
/// in v1.1 — the resolver already succeeded; lockfile drift is recoverable
/// on the next build). Errors mid-build don't fail the build.
pub(super) fn persist_lockfile(
    root: &std::path::Path,
    resolution: &crate::dep_resolver::Resolution,
    output: OutputMode,
) {
    let lockfile = match crate::lockfile::Lockfile::from_resolution(
        resolution,
        root,
        crate::lockfile::compute_path_dep_hash,
    ) {
        Ok(lf) => lf,
        Err(e) => {
            emit_lockfile_warning(&e, output);
            return;
        }
    };

    let lockfile_path = root.join("kara.lock");
    let fresh_toml = lockfile.to_toml();

    // No-op-when-unchanged: avoid touching file mtime on a quiet rebuild.
    if let Ok(existing) = std::fs::read_to_string(&lockfile_path) {
        if existing == fresh_toml {
            return;
        }
    }

    if let Err(io) = std::fs::write(&lockfile_path, &fresh_toml) {
        let err = crate::lockfile::LockfileError::Io {
            path: lockfile_path,
            error: io.to_string(),
            op: "write",
        };
        emit_lockfile_warning(&err, output);
    }
}

pub(super) fn emit_lockfile_warning(err: &crate::lockfile::LockfileError, output: OutputMode) {
    emit_lockfile_diag(
        err,
        output,
        "the resolver succeeded; the lockfile write is a follow-up step",
        "check filesystem permissions for the project root",
        true,
    );
}

fn emit_lockfile_diag(
    err: &crate::lockfile::LockfileError,
    output: OutputMode,
    note: &str,
    help: &str,
    json_on_stdout: bool,
) {
    let primary = err.to_string();
    let code = err.code();
    match output {
        OutputMode::Text => {
            eprintln!("warning[{code}]: {primary}");
            eprintln!("   = note: {note}");
            eprintln!("   = help: {help}");
        }
        OutputMode::Json => {
            let doc = format!(
                "{{\"status\":\"ok\",\"diagnostics\":[{{\"severity\":\"warning\",\"phase\":\"lockfile\",\"concept\":\"packages/manifests\",\"code\":{},\"message\":{}{}}}]}}",
                json_string(code),
                json_string(&primary),
                notes_help_tail(&[note], help),
            );
            // A read warning comes before the command's own document, which
            // `check --output=json` keeps as the only one on stdout.
            if json_on_stdout {
                println!("{doc}");
            } else {
                eprintln!("{doc}");
            }
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "lockfile_warning",
                &format!(
                    "\"code\":{},\"message\":{}{}",
                    json_string(code),
                    json_string(&primary),
                    notes_help_tail(&[note], help),
                ),
            );
        }
    }
}

/// `karac build`, `check` or `test` at a virtual workspace root
/// (B-2026-11-13-50): the same command, with the same arguments, in each
/// member's directory in `[workspace].members` order. Text output heads each
/// member's report with its name. Exits 1 when any member fails or is
/// missing, else 0.
pub(super) fn run_in_members(
    root: &std::path::Path,
    mf: &crate::manifest::Manifest,
    output: OutputMode,
) -> ! {
    if mf.workspace_members.is_empty() {
        eprintln!(
            "error[manifest]: `{}` is a workspace root with no `[package]` and no \
             `[workspace].members`, so there is nothing to run the command in",
            root.join(crate::manifest::MANIFEST_FILENAME).display()
        );
        process::exit(1);
    }
    let exe = std::env::current_exe().unwrap_or_else(|e| {
        eprintln!("error: cannot find the running karac: {e}");
        process::exit(1);
    });
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let mut failed = false;
    for member in &mf.workspace_members {
        let dir = root.join(member);
        if !dir.join(crate::manifest::MANIFEST_FILENAME).is_file() {
            eprintln!(
                "error[manifest]: workspace member `{member}` has no `{}` at `{}`",
                crate::manifest::MANIFEST_FILENAME,
                dir.display()
            );
            failed = true;
            continue;
        }
        if output == OutputMode::Text {
            eprintln!("==> {member}");
        }
        let ok = process::Command::new(&exe)
            .args(&args)
            .current_dir(&dir)
            .status()
            .is_ok_and(|s| s.success());
        failed |= !ok;
    }
    process::exit(i32::from(failed));
}

/// The manifest's soft notices (an unknown `[package]` key), as every
/// command that reads the manifest reports them, once per process
/// (B-2026-11-13-41). Text output only: the machine reports carry
/// diagnostics, and these are not.
pub(super) fn emit_manifest_warnings(mf: &crate::manifest::Manifest, output: OutputMode) {
    static SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if output != OutputMode::Text
        || mf.warnings.is_empty()
        || SHOWN.swap(true, std::sync::atomic::Ordering::Relaxed)
    {
        return;
    }
    for w in &mf.warnings {
        eprintln!("warning[manifest]: {}", w.message);
    }
}

pub(super) fn emit_dep_diagnostic(
    diag: &crate::dep_diagnostic::Diagnostic,
    output: OutputMode,
    severity: &str,
) {
    match output {
        OutputMode::Text => {
            eprintln!(
                "{}[{}]: {}",
                if severity == "warning" {
                    "warning"
                } else {
                    "error"
                },
                diag.code,
                diag.primary,
            );
            for note in &diag.notes {
                eprintln!("   = note: {note}");
            }
            if let Some(help) = &diag.help {
                eprintln!("   = help: {help}");
            }
        }
        OutputMode::Json => {
            let notes_json = diag
                .notes
                .iter()
                .map(|n| json_string(n))
                .collect::<Vec<_>>()
                .join(",");
            println!(
                "{{\"status\":\"{}\",\"diagnostics\":[{{\"severity\":\"{}\",\"phase\":\"dep_resolution\",\"concept\":\"packages/manifests\",\"code\":{},\"message\":{},\"notes\":[{}]{}}}]}}",
                if severity == "warning" { "ok" } else { "error" },
                severity,
                json_string(diag.code),
                json_string(&diag.primary),
                notes_json,
                diag.help
                    .as_ref()
                    .map(|h| format!(",\"help\":{}", json_string(h)))
                    .unwrap_or_default(),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                &format!("dep_resolution_{severity}"),
                &format!(
                    "\"code\":{},\"message\":{}",
                    json_string(diag.code),
                    json_string(&diag.primary),
                ),
            );
        }
    }
}

/// Emit a one-line confirmation `note:` when `--no-proxy` is set. The note
/// reports both the active proxy URL (so the operator sees what `karac`
/// would have consulted) and a pointer at the v1.1.x registry-proxy
/// follow-up. Silent when `--no-proxy` is absent — the proxy is the
/// default and the existing registry-dep-unsupported warning carries the
/// status. Emitted on the first cmd_* entry point so it is consistent
/// across `build`, `update`, and `vendor`.
pub(super) fn emit_no_proxy_note(no_proxy: bool) {
    if !no_proxy {
        return;
    }
    // Best-effort: if we're in a project, honor its `[build]` pins so the
    // reported URLs match what a fetch would consult. Outside a project (or on
    // a malformed manifest) fall through to env/default.
    let manifest = std::env::current_dir()
        .ok()
        .and_then(|cwd| manifest::load_from_cwd(&cwd).ok())
        .map(|(_, mf)| mf);
    let manifest_proxy = manifest
        .as_ref()
        .and_then(|mf| mf.build_registry_proxy.clone());
    let manifest_registry = manifest.as_ref().and_then(|mf| mf.build_registry.clone());
    let config = crate::registry_proxy::ProxyConfig::resolve(
        crate::registry_proxy::ProxyMode::Disabled,
        manifest_proxy.as_deref(),
    );
    // When a direct upstream registry is configured (env or `[build].registry`),
    // `--no-proxy` fetches direct-from-source (follow-ups (j)/(k)) rather than
    // warn-and-continue; name the registry so the operator sees where deps come
    // from. Otherwise keep the pre-fetch note.
    match crate::registry_proxy::resolve_direct_registry_url(manifest_registry.as_deref()) {
        Some(registry_url) => eprintln!(
            "note: --no-proxy active; registry deps fetch direct-from-source at {registry_url} (proxy at {} bypassed)",
            config.url
        ),
        None => eprintln!(
            "note: --no-proxy active; registry deps will not consult the proxy at {} (set KARAC_REGISTRY_URL or [build].registry to fetch direct-from-source)",
            config.url
        ),
    }
}

pub(super) fn emit_walker_error(e: &walker::WalkerError, output: OutputMode) {
    let code = e.code().unwrap_or("walker");
    match output {
        OutputMode::Text => {
            eprintln!("error[{code}]: {e}");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"walker\",\"concept\":\"packages/manifests\",\"code\":{},\"message\":{}}}]}}",
                json_string(code),
                json_string(&e.to_string()),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "walker_error",
                &format!(
                    "\"code\":{},\"message\":{}",
                    json_string(code),
                    json_string(&e.to_string()),
                ),
            );
        }
    }
}
