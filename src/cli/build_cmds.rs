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

    let (root, _mf) = match manifest::load_from_cwd(&cwd) {
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
        // parsed cleanly. The user can iterate.
        print_parse_errors_text(&parse_errors);
    }

    // Run effectcheck once on a merged Program containing every
    // non-synthetic module's items so cross-module callee resolution
    // works at the bare-name level the effectchecker indexes by. See
    // `build_doc_effects_table` for the trade-offs.
    let effects = build_doc_effects_table(&tree);

    let output_dir = root.join("dist").join("doc");
    match crate::doc::build_docs(&tree, &output_dir, Some(&effects)) {
        Ok(result) => {
            println!(
                "rendered {} doc page(s) under {}",
                result.written.len().saturating_sub(1), // minus the index
                output_dir.display()
            );
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
        for item in &module.items {
            let Item::Function(f) = item else { continue };
            if !f.is_pub {
                continue;
            }

            // Prefer the declared annotation (the user's contract);
            // fall back to the inferred set if no explicit annotation.
            let display = match effects.declared_effects.get(&f.name) {
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
                    .get(&f.name)
                    .map(|set: &EffectSet| effect_set_to_display(set, false))
                    .unwrap_or_default(),
            };

            if !display.effects.is_empty() || display.polymorphic {
                out.insert((module.path.clone(), f.name.clone()), display);
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
        profile: crate::manifest::CompileProfile::Default,
        profile_config: crate::manifest::ProfileConfig::default(),
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
        mir_program: None,
        library_pruned: None,
    };
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
                out.push(LateDiag {
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
                out.push(LateDiag {
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
                out.push(LateDiag {
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
                out.push(LateDiag {
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
            eprintln!(
                "error[parse]: {}:{}:{}: {}",
                pe.file.display(),
                err.span.line,
                err.span.column,
                err.message,
            );
        }
    }
}

/// Resolver errors collected for one specific module, with the source file
/// retained so diagnostics can be printed with their original location.
pub(super) struct ModuleResolveErrors {
    file: PathBuf,
    errors: Vec<ResolveError>,
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
}

impl PackageLibrary {
    pub(super) fn of(tree: &ProgramTree) -> Option<Self> {
        let mut src = String::new();
        for m in tree.modules.iter().filter(|m| !m.is_synthetic) {
            src.push_str(&std::fs::read_to_string(&m.file).ok()?);
            src.push('\n');
        }
        let user_items = crate::parse(&src).program.items.len();
        let (text, start) = crate::mir::lower::library_source(&src);
        let mut parsed = crate::parse(&text);
        if !parsed.errors.is_empty() || parsed.program.items.len() < user_items {
            return None;
        }
        let program = &mut parsed.program;
        if crate::mir::library_prune::enabled() {
            crate::mir::library_prune::prune(program, &text, start);
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
        Some(PackageLibrary { items, start })
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
        let program = PackageLibrary::program(lib, &m.items);
        let mut result = Resolver::new(&program)
            .with_tree(tree, id as ModuleId)
            .with_test_file(m.is_test_file)
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
        if !result.errors.is_empty() {
            out.push(ModuleResolveErrors {
                file: m.file.clone(),
                errors: result.errors,
                fix_diffs: result.error_fix_diffs,
            });
        }
    }
    out
}

pub(super) fn resolve_error_code(kind: &ResolveErrorKind) -> &'static str {
    match kind {
        // NOTE severity — the spec calls this a warning, so it gets a `W`
        // code, not an `E` one (B-2026-08-21-2 follow-up).
        ResolveErrorKind::LayoutUnassignedFields => "W0150",
        // The private half is a lint (W); the `pub` half is the hard
        // FFI-contract error the spec mandates (E) — B-2026-08-21-2.
        ResolveErrorKind::LayoutReprCIgnored => "W0151",
        ResolveErrorKind::LayoutReprCOnPubStruct => "E0151",
        ResolveErrorKind::FloatInSerializedType => "W0152",
        ResolveErrorKind::UnknownModule => "E0112",
        ResolveErrorKind::UnknownItemInModule => "E0113",
        ResolveErrorKind::PrivateItemAccess => "E0111",
        ResolveErrorKind::AmbiguousWildcardImport => "E0124",
        ResolveErrorKind::UndefinedName => "E0100",
        ResolveErrorKind::DuplicateDefinition => "E0101",
        ResolveErrorKind::ReservedIdentifier => "E0102",
        ResolveErrorKind::PrivateAccess => "E0103",
        ResolveErrorKind::UndefinedType => "E0104",
        ResolveErrorKind::UndefinedVariant => "E0105",
        ResolveErrorKind::UndefinedField => "E0106",
        ResolveErrorKind::UndefinedLabel => "E0107",
        ResolveErrorKind::OperatorTraitImplRestricted => "E0108",
        ResolveErrorKind::IntoTraitImplNotAllowed => "E0109",
        ResolveErrorKind::ImplLevelEffectVarNotAllowed => "E0110",
        ResolveErrorKind::ReservedEffectResource => "E0114",
        ResolveErrorKind::CompilerBuiltinReserved => "E0115",
        ResolveErrorKind::ContinueOnBlockLabel => "E0116",
        ResolveErrorKind::NonExhaustiveInvalidTarget => "E0117",
        ResolveErrorKind::TrackCallerInvalidTarget => "E0118",
        ResolveErrorKind::GpuInvalidTarget => "E0800",
        ResolveErrorKind::CodegenHintInvalidTarget => "E_CODEGEN_HINT_INVALID_POSITION",
        ResolveErrorKind::CodegenHintOnExternDecl => "E_CODEGEN_HINT_ON_EXTERN_DECL",
        ResolveErrorKind::DeprecatedOnImpl => "E0119",
        ResolveErrorKind::DeprecatedOnField => "E0120",
        ResolveErrorKind::UnknownAttribute => "E0121",
        ResolveErrorKind::ProfileInvalidTarget => "E0122",
        ResolveErrorKind::UnknownProfile => "E0123",
        ResolveErrorKind::QueryResolutionConflict => "E_QUERY_RESOLUTION_CONFLICT",
        ResolveErrorKind::UnionNonExhaustiveForbidden => "E_UNION_NON_EXHAUSTIVE_FORBIDDEN",
        ResolveErrorKind::DefaultAttributeInvalidPosition => "E_DEFAULT_ATTRIBUTE_INVALID_POSITION",
        ResolveErrorKind::DefaultAttributeWithoutDerive => "E_DEFAULT_ATTRIBUTE_WITHOUT_DERIVE",
        ResolveErrorKind::PanicHandlerInvalid => "E_PANIC_HANDLER",
        ResolveErrorKind::MalformedAttributeArgs => "E_MALFORMED_ATTRIBUTE_ARGS",
    }
}

pub(super) fn print_resolve_errors_text(per_module: &[ModuleResolveErrors]) {
    for re in per_module {
        let file = re.file.display().to_string();
        for err in &re.errors {
            let code = resolve_error_code(&err.kind);
            eprintln!(
                "error[{code}]: {}:{}:{}: {}",
                re.file.display(),
                err.span.line,
                err.span.column,
                err.message,
            );
            if let Some(ref s) = err.suggestion {
                eprintln!("  help: did you mean `{s}`?");
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

/// B-2026-08-27-27 — the flattened late-phase failure as `check` diagnostics.
/// `phase` is carried through verbatim ("effect" / "ownership" / …) so a
/// consumer can tell them apart, and each diagnostic keeps the file and
/// line:column the span table resolved.
pub(super) fn late_failure_json(f: &MultiFileLateFailure) -> Vec<String> {
    f.diags
        .iter()
        .map(|d| {
            format!(
                "{{\"severity\":\"error\",\"phase\":{},\"file\":{},\"line\":{},\"column\":{},\"message\":{}}}",
                json_string(&f.phase),
                json_string(d.file.as_deref().unwrap_or("")),
                d.line,
                d.column,
                json_string(&d.message),
            )
        })
        .collect()
}

pub(super) fn late_failure_jsonl(f: &MultiFileLateFailure) -> Vec<String> {
    f.diags
        .iter()
        .map(|d| {
            format!(
                "{{\"type\":{},\"file\":{},\"line\":{},\"column\":{},\"message\":{}}}",
                json_string(&format!("{}_error", f.phase)),
                json_string(d.file.as_deref().unwrap_or("")),
                d.line,
                d.column,
                json_string(&d.message),
            )
        })
        .collect()
}

pub(super) fn print_late_failure_text(f: &MultiFileLateFailure) {
    eprintln!("error[{}]: {}", f.phase, f.message);
}

pub(super) fn resolve_errors_json(per_module: &[ModuleResolveErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for re in per_module {
        let file = re.file.display().to_string();
        for err in &re.errors {
            let code = resolve_error_code(&err.kind);
            let suggestion = match err.suggestion.as_deref() {
                Some(s) => format!(",\"suggestion\":{}", json_string(s)),
                None => String::new(),
            };
            let replacement = replacement_json_tail(err);
            let fix_diff = fix_diff_json_tail(err, &re.fix_diffs);
            let hints = stub_hints_tail(&file, err);
            out.push(format!(
                "{{\"severity\":\"error\",\"phase\":\"resolve\",\"code\":{},\"file\":{},\"line\":{},\"column\":{},\"message\":{}{}{}{}{}}}",
                json_string(code),
                json_string(&file),
                err.span.line,
                err.span.column,
                json_string(&err.message),
                suggestion,
                replacement,
                fix_diff,
                hints,
            ));
        }
    }
    out
}

pub(super) fn resolve_errors_jsonl(per_module: &[ModuleResolveErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for re in per_module {
        let file = re.file.display().to_string();
        for err in &re.errors {
            let code = resolve_error_code(&err.kind);
            let suggestion = match err.suggestion.as_deref() {
                Some(s) => format!(",\"suggestion\":{}", json_string(s)),
                None => String::new(),
            };
            let replacement = replacement_json_tail(err);
            let fix_diff = fix_diff_json_tail(err, &re.fix_diffs);
            let hints = stub_hints_tail(&file, err);
            out.push(format!(
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
            ));
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
        let program = PackageLibrary::program(lib, &m.items);
        let resolved = Resolver::new(&program)
            .with_tree(tree, id as ModuleId)
            .resolve();
        let mut result = crate::typechecker::TypeChecker::new(&program, &resolved)
            .with_tree(tree, id as ModuleId)
            .with_cli_lint_overrides(lint_overrides.clone())
            .check();
        let in_lib = |e: &crate::typechecker::TypeError| PackageLibrary::holds(lib, e.span.offset);
        result.errors.retain(|e| !in_lib(e));
        result.warnings.retain(|e| !in_lib(e));
        // B-2026-08-18-19 — the warnings used to be DROPPED here: `result`
        // carries them and only `.errors` was read, so a project build was
        // structurally incapable of reporting `deprecated` no matter what the
        // render path did.
        if !result.warnings.is_empty() {
            warn_out.push(ModuleTypeErrors {
                file: m.file.clone(),
                errors: result.warnings,
            });
        }
        if !result.errors.is_empty() {
            out.push(ModuleTypeErrors {
                file: m.file.clone(),
                errors: result.errors,
            });
        }
    }
    ModuleTypeDiagnostics {
        errors: out,
        warnings: warn_out,
    }
}

pub(super) fn type_error_code(kind: &crate::typechecker::TypeErrorKind) -> &'static str {
    use crate::typechecker::TypeErrorKind as K;
    match kind {
        K::PrivateTypeInPublicSignature => "E0221",
        K::TypeMismatch => "E0200",
        K::UndefinedField => "E0201",
        K::WrongNumberOfArgs => "E0202",
        K::MissingField => "E0203",
        K::ExtraField => "E0204",
        K::NonExhaustiveMatch => "E0205",
        K::NotCallable => "E0206",
        K::NotAStruct => "E0207",
        K::InvalidBinaryOp => "E0208",
        K::InvalidUnaryOp => "E0209",
        K::InvalidCast => "E0210",
        K::ConditionNotBool => "E0211",
        K::BranchTypeMismatch => "E0212",
        K::ReturnTypeMismatch => "E0213",
        K::GpuNotSafe => "E0801",
        K::StringNotIndexable => "E0268",
        K::IteratorNotIndexable => "E0274",
        K::TypeNotIndexable => "E0275",
        K::NilCoalesceNotWrapped => "E0276",
        K::OptionalChainNotOption => "E0277",
        K::SharedFieldNotMut => "E0269",
        K::AtomicMissingOrdering => "E0270",
        K::AtomicInvalidInnerType => "E0272",
        K::PatternScrutineeMismatch => "E0273",
        _ => "E0200",
    }
}

/// The warning-severity twin of [`print_type_errors_text`]. Same layout, with
/// the severity word and the lint label a `-A <name>` invocation would take —
/// matching what `karac check` prints for the same finding.
pub(super) fn print_type_warnings_text(per_module: &[ModuleTypeErrors]) {
    for te in per_module {
        for warn in &te.errors {
            let label = warn.lint_name.as_deref().unwrap_or("typecheck");
            eprintln!(
                "warning[{label}]: {}:{}:{}: {}",
                te.file.display(),
                warn.span.line,
                warn.span.column,
                warn.message,
            );
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
                te.file.display(),
                err.span.line,
                err.span.column,
                err.message,
            );
        }
    }
}

/// The warning-severity twin of [`type_errors_json`]. Carries `lint_name` so a
/// consumer can offer the `-A <name>` the human renderer names, matching what
/// `karac check --output=json` emits for the same finding.
pub(super) fn type_warnings_json(per_module: &[ModuleTypeErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for te in per_module {
        let file = te.file.display().to_string();
        for warn in &te.errors {
            let mut record = format!(
                "{{\"severity\":\"warning\",\"phase\":\"typecheck\",\"file\":{},\"line\":{},\"column\":{},\"message\":{}",
                json_string(&file),
                warn.span.line,
                warn.span.column,
                json_string(&warn.message),
            );
            if let Some(lint) = warn.lint_name.as_deref() {
                record.push_str(&format!(",\"lint_name\":{}", json_string(lint)));
            }
            record.push('}');
            out.push(record);
        }
    }
    out
}

pub(super) fn type_errors_json(per_module: &[ModuleTypeErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for te in per_module {
        let file = te.file.display().to_string();
        for err in &te.errors {
            let code = type_error_code(&err.kind);
            let mut record = format!(
                "{{\"severity\":\"error\",\"phase\":\"typecheck\",\"code\":{},\"file\":{},\"line\":{},\"column\":{},\"message\":{},\"class\":{}",
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

pub(super) fn type_errors_jsonl(per_module: &[ModuleTypeErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for te in per_module {
        let file = te.file.display().to_string();
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
        let entry = std::fs::canonicalize(root.join("src").join("main.kara")).ok()?;
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
/// from another module, a private type in a public signature, and a
/// `#[non_exhaustive]` type matched or built from another package. The
/// combined source has one scope and one package, so its check cannot see
/// them (B-2026-10-25-46).
fn type_error_is_a_module_boundary_rule(kind: &crate::typechecker::TypeErrorKind) -> bool {
    use crate::typechecker::TypeErrorKind as K;
    matches!(
        kind,
        K::PrivateTypeInPublicSignature
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
    // Same lenient dep posture as the `karac run` super-program builder: a
    // dep-resolution failure proceeds without dependency modules rather than
    // failing the check of the local package.
    let dep_walks = super::run_check_cmds::quiet_dep_package_walks(root);
    let built =
        module::build_program_tree_with_deps(&walked, &dep_walks, module::BuildTreeOpts::default())
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
        let package_name = crate::manifest::load_from_cwd(root)
            .map(|(_, m)| m.name)
            .unwrap_or_else(|_| "package".to_string());
        run_multi_file_late_phases(
            &tree,
            &package_name,
            lint_overrides.core_places,
            lib.as_ref(),
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
        eprintln!("error[E0223]: circular module dependency");
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
                "<crate root>".to_string()
            } else {
                m.path.join(".")
            };
            eprintln!("    {label}  {}", m.file.display());
        }
        eprintln!(
            "  suggestion: extract the shared items into a lower-layer module that both ends of the cycle can depend on."
        );
    }
}

pub(super) fn parse_errors_json(parse_errors: &[ModuleParseErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for pe in parse_errors {
        let file = pe.file.display().to_string();
        for err in &pe.errors {
            out.push(format!(
                "{{\"severity\":\"error\",\"phase\":\"parse\",\"code\":\"E0001\",\"file\":{},\"line\":{},\"column\":{},\"message\":{}}}",
                json_string(&file),
                err.span.line,
                err.span.column,
                json_string(&err.message),
            ));
        }
    }
    out
}

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
                .map(|id| json_string(&tree.modules[*id].file.display().to_string()))
                .collect();
            format!(
                "{{\"severity\":\"error\",\"phase\":\"module_graph\",\"code\":\"E0223\",\"message\":{},\"cycle_paths\":[{}],\"cycle_files\":[{}]}}",
                json_string(&c.format(tree)),
                paths_json.join(","),
                files.join(","),
            )
        })
        .collect()
}

pub(super) fn parse_errors_jsonl(parse_errors: &[ModuleParseErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for pe in parse_errors {
        let file = pe.file.display().to_string();
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
                "{{\"type\":\"module_cycle\",\"code\":\"E0223\",\"message\":{},\"cycle_paths\":[{}]}}",
                json_string(&c.format(tree)),
                paths_json.join(","),
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
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"module_graph\",\"code\":{},\"message\":{}}}]}}",
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
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"manifest\",\"code\":{},\"message\":{}}}]}}",
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
/// A structural graph error — a dependency cycle, a missing path-dep, or a
/// workspace-deref failure — fails `build_dep_graph_with_options` and halts.
/// A version conflict or an MSRV (`kara-version`) violation surfaces from the
/// resolve step and halts. Registry/git deps, by contrast, cannot be satisfied
/// without a fetch and neither command fetches or loads dependency modules from
/// a registry/git source, so an `E_*_DEP_UNSUPPORTED` finding is a build-time
/// concern — it is skipped here (the same dep surfaces on `karac build`).
pub(super) fn surface_dep_graph_diagnostics(
    root: &std::path::Path,
    mf: crate::manifest::Manifest,
    output: OutputMode,
) -> bool {
    let loader = crate::dep_graph::FsLoader;
    let options = crate::dep_graph::DepGraphOptions {
        offline_root: None,
        include_dev_deps: false,
        registry_provider: None,
        git_provider: None,
        // Path-dep-only lenient walk — no lockfile pinning here.
        pins: None,
    };
    let graph = match crate::dep_graph::build_dep_graph_with_options(root, mf, &loader, options) {
        Ok(g) => g,
        Err(e) => {
            let diag = crate::dep_diagnostic::render_dep_graph_error(&e);
            emit_dep_diagnostic(&diag, output, "error");
            return false;
        }
    };
    let active = crate::dep_resolver::active_toolchain_version();
    match crate::dep_resolver::resolve(&graph, &active) {
        Ok(_) => true,
        Err(boxed) => {
            if matches!(
                boxed.code(),
                "E_REGISTRY_DEP_UNSUPPORTED" | "E_GIT_DEP_UNSUPPORTED"
            ) {
                return true;
            }
            let diag = crate::dep_diagnostic::render_resolver_error(&boxed);
            emit_dep_diagnostic(&diag, output, "error");
            false
        }
    }
}

/// Build the dep graph and resolve it against the active toolchain. Returns
/// `true` to continue with the build, `false` to halt. Registry/git
/// unsupported errors downgrade to warnings — the rest are fatal. Slice 7
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
/// (phase-5 line 898); `Ok(None)` is the legacy warning-and-continue path
/// (unsupported registry/git sources outside offline mode), where the
/// build proceeds without dependency modules.
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
        crate::registry_proxy::resolve_direct_registry_url(mf.build_registry.as_deref())
    } else if crate::registry_proxy::explicit_proxy_configured(mf.build_registry_proxy.as_deref()) {
        Some(
            crate::registry_proxy::ProxyConfig::resolve(
                crate::registry_proxy::ProxyMode::Default,
                mf.build_registry_proxy.as_deref(),
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

    // Lockfile-pin-over-catalog (follow-up (d)/(h)): read an existing
    // `kara.lock` and prefer its recorded registry-package versions, so a
    // rebuild reproduces the locked graph rather than drifting to the newest
    // compatible version. The pins feed BOTH the graph walk (slice 4 — a pinned
    // registry dep is fetched at exactly that version via `fetch_exact`, even if
    // since-yanked, and added to the candidate set) and version selection (slice
    // 2). Best-effort — an absent / unreadable / malformed lockfile yields no
    // pins (fresh selection), never a build error. Pins bite only where the
    // registry candidate set is widened; path/git deps ignore them.
    let pins = read_lockfile_pins(root);
    let options = crate::dep_graph::DepGraphOptions {
        offline_root,
        include_dev_deps,
        registry_provider: provider
            .as_ref()
            .map(|p| p as &dyn crate::dep_graph::RegistryProvider),
        git_provider: git_provider
            .as_ref()
            .map(|p| p as &dyn crate::git_fetch::GitProvider),
        pins: Some(&pins),
    };
    let graph = match crate::dep_graph::build_dep_graph_with_options(root, mf, &loader, options) {
        Ok(g) => g,
        Err(e) => {
            let diag = crate::dep_diagnostic::render_dep_graph_error(&e);
            emit_dep_diagnostic(&diag, output, "error");
            return Err(());
        }
    };
    let active = crate::dep_resolver::active_toolchain_version();
    match crate::dep_resolver::resolve_with_pins(&graph, &active, offline_root, &pins) {
        Ok(resolution) => {
            // Warn on any resolved version the catalog marks yanked (follow-up
            // (h), slice 4). Fresh selection excludes yanked versions, so this
            // fires only when a `kara.lock` pin lands on a version yanked since
            // it was recorded — reproducibility kept it, and the user should
            // hear the pin is now withdrawn.
            emit_yanked_pin_warnings(&resolution, &graph, output);
            // `karac resolve` is read-only — it inspects the graph without
            // rewriting `kara.lock`. Only build / test persist the pin.
            if persist_lock {
                persist_lockfile(root, &resolution, output);
            }
            Ok(Some(resolution))
        }
        Err(boxed) => {
            let diag = crate::dep_diagnostic::render_resolver_error(&boxed);
            let code = boxed.code();
            // In offline mode, registry/git deps can't be satisfied from
            // vendor/ today (registry/git vendoring lands alongside line
            // 845); the unsupported-source diagnostic must halt the build
            // so the operator doesn't get a silent partial resolution.
            // Outside offline, the existing warning-and-continue behavior
            // preserves the pre-fetch v1.1 contract.
            let severity = if offline_root.is_some() {
                "error"
            } else {
                match code {
                    "E_REGISTRY_DEP_UNSUPPORTED" | "E_GIT_DEP_UNSUPPORTED" => "warning",
                    _ => "error",
                }
            };
            emit_dep_diagnostic(&diag, output, severity);
            if severity == "warning" {
                Ok(None)
            } else {
                Err(())
            }
        }
    }
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
    resolution: Option<&crate::dep_resolver::Resolution>,
    target: walker::Platform,
    output: OutputMode,
) -> Result<Vec<module::DepPackageWalk>, ()> {
    let Some(resolution) = resolution else {
        return Ok(Vec::new());
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
        out.push(module::DepPackageWalk {
            name: pkg.name.clone(),
            walked,
        });
    }
    Ok(out)
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
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"walker\",\"code\":\"walker\",\"package\":{},\"message\":{}}}]}}",
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
    match output {
        OutputMode::Text => {
            eprintln!("error[{code}]: {primary}");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"toolchain_pin\",\"code\":{},\"message\":{}}}]}}",
                json_string(code),
                json_string(&primary),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "toolchain_pin_error",
                &format!(
                    "\"code\":{},\"message\":{}",
                    json_string(code),
                    json_string(&primary),
                ),
            );
        }
    }
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
    match output {
        OutputMode::Text => {
            eprintln!("error[{code}]: {primary}");
            eprintln!("   = note: install a matching toolchain via `karaup install {}` (karaup ships post-v1)", mismatch.required);
            eprintln!("   = help: or relax the `version` constraint in `karac-toolchain.toml` to admit the active toolchain");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"toolchain_pin\",\"code\":{},\"message\":{},\"required\":{},\"active\":{}}}]}}",
                json_string(code),
                json_string(&primary),
                json_string(&mismatch.required.to_string()),
                json_string(&mismatch.active.to_string()),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "toolchain_pin_error",
                &format!(
                    "\"code\":{},\"message\":{},\"required\":{},\"active\":{}",
                    json_string(code),
                    json_string(&primary),
                    json_string(&mismatch.required.to_string()),
                    json_string(&mismatch.active.to_string()),
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
    match output {
        OutputMode::Text => {
            eprintln!("error[{code}]: {primary}");
            eprintln!("   = note: --offline resolves every transitive path-dep against `./vendor/<name>/`");
            eprintln!("   = help: run `karac vendor` to populate the vendor directory, then re-run with `--offline`");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"dep_resolution\",\"code\":{},\"message\":{}}}]}}",
                json_string(code),
                json_string(&primary),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "dep_resolution_error",
                &format!(
                    "\"code\":{},\"message\":{}",
                    json_string(code),
                    json_string(&primary),
                ),
            );
        }
    }
}

/// Read the project's `kara.lock` (if present) and extract the registry-package
/// version pins for lockfile-pin-over-catalog resolution (follow-up (d)/(h),
/// slice 3). Best-effort: a missing / unreadable / malformed lockfile yields an
/// empty pin map, so a fresh project or a corrupt lockfile falls back to fresh
/// version selection rather than failing the build.
pub(super) fn read_lockfile_pins(
    root: &std::path::Path,
) -> std::collections::BTreeMap<String, semver::Version> {
    let path = root.join("kara.lock");
    let Ok(source) = std::fs::read_to_string(&path) else {
        return std::collections::BTreeMap::new();
    };
    match crate::lockfile::Lockfile::parse(&path, &source) {
        Ok(lock) => lock.version_pins(),
        Err(_) => std::collections::BTreeMap::new(),
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
        };
        emit_lockfile_warning(&err, output);
    }
}

pub(super) fn emit_lockfile_warning(err: &crate::lockfile::LockfileError, output: OutputMode) {
    let primary = err.to_string();
    let code = err.code();
    match output {
        OutputMode::Text => {
            eprintln!("warning[{code}]: {primary}");
            eprintln!("   = note: the resolver succeeded; the lockfile write is a follow-up step");
            eprintln!("   = help: check filesystem permissions for the project root");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"ok\",\"diagnostics\":[{{\"severity\":\"warning\",\"phase\":\"lockfile\",\"code\":{},\"message\":{}}}]}}",
                json_string(code),
                json_string(&primary),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "lockfile_warning",
                &format!(
                    "\"code\":{},\"message\":{}",
                    json_string(code),
                    json_string(&primary),
                ),
            );
        }
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
                "{{\"status\":\"{}\",\"diagnostics\":[{{\"severity\":\"{}\",\"phase\":\"dep_resolution\",\"code\":{},\"message\":{},\"notes\":[{}]{}}}]}}",
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
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"walker\",\"code\":{},\"message\":{}}}]}}",
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
