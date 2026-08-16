//! `karac fmt` / `debug` / `fix` / `migrate` — the source-rewriting tools.
//!
//! Extracted verbatim from `cli.rs` (structural-debt extraction, slice 1).
//! Free functions are `pub(super)` — private plumbing of the CLI module.

use super::*;

pub(super) fn cmd_fmt(filename: &str) {
    let source = read_source(filename);
    let parsed = crate::parse(&source);
    if !parsed.errors.is_empty() {
        for err in &parsed.errors {
            eprintln!(
                "error[parse]: {}:{}:{}: {}",
                filename, err.span.line, err.span.column, err.message
            );
        }
        process::exit(1);
    }
    let formatted = crate::formatter::format_program(&parsed.program);
    print!("{formatted}");
}

/// `karac debug <crash.json>` — render a `std.panic` crash report
/// (`docs/design.md § 4. Crash Report Format`). `input` is a file path or `-`
/// for stdin. Default output is the human-readable form (TTY-aware colour);
/// `--output=json` re-emits the parsed structured JSON, pretty-printed — a
/// faithful, additive-safe passthrough that also validates the file is
/// well-formed JSON. Track 6 § CLI surface.
pub(super) fn cmd_debug(input: &str, output: OutputMode) {
    use std::io::Read as _;

    let raw = if input == "-" {
        let mut buf = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut buf) {
            eprintln!("error: reading crash report from stdin: {e}");
            process::exit(1);
        }
        buf
    } else {
        match std::fs::read_to_string(input) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: reading crash report '{input}': {e}");
                process::exit(1);
            }
        }
    };

    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: '{input}' is not valid JSON: {e}");
            process::exit(1);
        }
    };

    match output {
        // Structured re-emit: pretty-print the parsed JSON verbatim. Preserves
        // any additive fields this `karac` doesn't model, and normalises
        // formatting for downstream tooling / diffing.
        OutputMode::Json | OutputMode::Jsonl => match serde_json::to_string_pretty(&value) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("error: re-serializing crash report: {e}");
                process::exit(1);
            }
        },
        OutputMode::Text => match crate::crash_report::CrashReport::from_value(&value) {
            Ok(report) => {
                print!(
                    "{}",
                    crate::crash_report::render(&report, crate::effect_render::ColorChoice::Auto)
                );
            }
            Err(e) => {
                eprintln!("error: '{input}' is not a crash report: {e}");
                process::exit(1);
            }
        },
    }
}

/// Apply machine-applicable suggestions back into the source file.
///
/// Runs the full single-file pipeline (resolve → typecheck → lower →
/// effectcheck → ownership → ...), then collects every diagnostic that
/// carries a `replacement: Some(_)` payload across all phases that have
/// gained machine-applicable metadata. Edits are sorted in reverse
/// byte-offset order (so earlier edits don't invalidate later offsets)
/// and the source file is overwritten. With `dry_run = true`, prints the
/// would-be rewrites to stdout without touching disk.
///
/// Phases that contribute fixes today:
/// - Resolver: E0112 (UnknownModule, round 12.29), E0113
///   (UnknownItemInModule, round 12.28), E0100 (UndefinedName) and E0104
///   (UndefinedType) — both pre-12-era. All four are `did you mean`
///   corrections; the suggestion is a concrete identifier and the error
///   span is the misspelled token.
/// - Ownership: N0507 (UnusedMutCaptureNote, round 12.31) — closure
///   prefix `mut ref` → `ref`. Note (not error), so it does not block
///   compilation; `karac fix` applies it opportunistically.
///
/// Other diagnostic kinds carry descriptive (multi-step) suggestions
/// that are not mechanically applicable; they remain visible through
/// `karac check` and must be acted on by hand.
pub(super) fn cmd_fix(filename: &str, dry_run: bool) {
    let source = read_source(filename);
    let mut pipeline = Pipeline::new(filename, &source);
    let mut edits: Vec<crate::resolver::TextEdit> = Vec::new();
    if pipeline.has_parse_errors() {
        // The file doesn't fully parse, but parsing may still have
        // synthesized machine-applicable recovery edits (e.g. deleting a
        // stray comma in a comma-separated `with` clause). Apply those —
        // each pass unblocks the next re-check. Post-parse phases can't run
        // on an unparseable file, so only parse edits are available here; if
        // there are none, report the parse errors and exit as before.
        edits.extend(pipeline.parsed.fix_edits.values().cloned());
        // Multi-edit parse envelopes (B-2026-08-13-13) — the brace-wrap for a
        // bare assignment `match` arm body. Both halves or neither, so they
        // live only here, never in the single-edit map.
        edits.extend(pipeline.parsed.fix_diffs.values().flatten().cloned());
        if edits.is_empty() {
            for err in &pipeline.parsed.errors {
                eprintln!(
                    "error[parse]: {}:{}:{}: {}",
                    filename, err.span.line, err.span.column, err.message
                );
            }
            process::exit(1);
        }
    } else {
        pipeline.run_all_checks();
        if let Some(ref r) = pipeline.resolved {
            edits.extend(
                r.errors
                    .iter()
                    .filter_map(|e| e.replacement.as_deref().cloned()),
            );
            // Multi-edit envelopes (B-2026-07-31-33). A rename that has to
            // touch use sites as well as the declaration cannot fit the
            // single-edit `replacement` slot, so it lands here instead —
            // and only here, so applying just `replacement` can never leave
            // a half-renamed program behind.
            edits.extend(r.error_fix_diffs.values().flatten().cloned());
        }
        if let Some(ref ef) = pipeline.effects {
            edits.extend(
                ef.errors
                    .iter()
                    .filter_map(|e| e.replacement.as_deref().cloned()),
            );
        }
        if let Some(ref o) = pipeline.ownership {
            edits.extend(
                o.errors
                    .iter()
                    .filter_map(|e| e.replacement.as_deref().cloned()),
            );
            edits.extend(
                o.notes
                    .iter()
                    .filter_map(|e| e.replacement.as_deref().cloned()),
            );
            // Multi-edit `fix_diff` envelopes (B-2026-07-06-4). The
            // `ConcurrentSharedStruct` / `ConcurrentPlainStruct`
            // diagnostics carry a full machine-applicable migration
            // (`par struct` keyword insert + per-mut-field `Mutex[T]`
            // wraps) in `error_fix_diffs`, keyed by the diagnostic's
            // primary span. `collect_diagnostics` already emits these as
            // a top-level `"fix_diff":[...]` array to JSON, but until now
            // `cmd_fix` collected only each error's single-edit
            // `.replacement` — so `karac fix` applied nothing for these
            // two even though the JSON advertised a fix. Flatten every
            // envelope's edits in here; the descending-offset sort +
            // overlap dedup below applies them safely alongside the
            // single-edit replacements.
            edits.extend(o.error_fix_diffs.values().flatten().cloned());
        }
        if let Some(ref t) = pipeline.typed {
            // Typecheck fix-its (e.g. E0205 missing-match-arm insertion, the
            // `#[non_exhaustive]` cross-package wildcard) use FixIt{span,
            // replacement}; convert to the TextEdit offset/length form.
            edits.extend(t.errors.iter().filter_map(|e| {
                e.fix_it.as_ref().map(|f| crate::resolver::TextEdit {
                    offset: f.span.offset,
                    length: f.span.length,
                    replacement: f.replacement.clone(),
                })
            }));
            // WARNINGS carry fix-its too, and until B-2026-08-03-9's
            // `map_value_clone_reinsert` there was no producer, so this channel
            // went unread — a warning could advertise a machine-applicable fix
            // in `--output=json` that `karac fix` then declined to apply. A
            // warning's fix is by definition optional-but-safe (the code
            // already compiles and is correct), which is exactly what `fix` is
            // for.
            edits.extend(t.warnings.iter().filter_map(|e| {
                e.fix_it.as_ref().map(|f| crate::resolver::TextEdit {
                    offset: f.span.offset,
                    length: f.span.length,
                    replacement: f.replacement.clone(),
                })
            }));
        }
    }

    if edits.is_empty() {
        println!("(no fixable diagnostics in {filename})");
        return;
    }

    // Drop overlapping edits (e.g. the same token reported by multiple
    // sources). Sort by offset descending so that applying them in order
    // does not invalidate the offsets of later edits.
    edits.sort_by_key(|e| std::cmp::Reverse(e.offset));
    let mut deduped: Vec<crate::resolver::TextEdit> = Vec::with_capacity(edits.len());
    let mut last_start = usize::MAX;
    for edit in edits {
        let end = edit.offset.saturating_add(edit.length);
        if end > last_start {
            // Overlaps a later (higher-offset) edit already in the buffer
            // — skip silently. This is a defense-in-depth measure; the
            // resolver shouldn't normally emit overlapping replacements.
            continue;
        }
        last_start = edit.offset;
        deduped.push(edit);
    }

    if dry_run {
        println!("would apply {} fix(es) to {filename}:", deduped.len());
        for edit in deduped.iter().rev() {
            // Render in source order for human readability.
            let original = source
                .get(edit.offset..edit.offset.saturating_add(edit.length))
                .unwrap_or("<?>");
            let (line, col) = crate::byte_offset_to_line_col(&source, edit.offset);
            println!(
                "  {filename}:{line}:{col}: `{}` → `{}`",
                original, edit.replacement
            );
        }
        return;
    }

    let mut rewritten = source.clone();
    for edit in &deduped {
        let end = edit.offset.saturating_add(edit.length);
        if end > rewritten.len() {
            // Source shrank between read and apply — bail rather than
            // produce an out-of-bounds slice.
            eprintln!(
                "error: fix would write past end of file ({} > {}) — aborting without modifying {filename}",
                end,
                rewritten.len()
            );
            process::exit(1);
        }
        rewritten.replace_range(edit.offset..end, &edit.replacement);
    }
    // Refuse to write a rewrite that PARSES WORSE than the input.
    //
    // Every machine-applicable edit is an (offset, length, replacement)
    // triple, and nothing downstream checks that the offset still means what
    // the diagnostic meant. B-2026-08-11-35 is what that costs: an edit whose
    // span had not been rebased out of an f-string hole landed ~77 bytes early
    // and deleted everything in between, truncating mid-token — and `fix`
    // reported `applied 1 fix(es)` and exited 0, over a file with no backup.
    // The bounds check above did not catch it, because the bogus range was
    // comfortably inside the file.
    //
    // A corrupting edit essentially always breaks the parse, so re-parsing the
    // result is a cheap, general net across every fix producer rather than a
    // guard bolted onto the one that misfired — including producers that do not
    // exist yet. The test is "no WORSE", not "clean": the `has_parse_errors`
    // branch above deliberately applies recovery edits to a file that does not
    // parse, and each pass is meant to reduce the count, not necessarily reach
    // zero in one go.
    let before = pipeline.parsed.errors.len();
    let after = crate::parse(&rewritten).errors.len();
    if after > before {
        eprintln!(
            "error: applying {} fix(es) would leave {filename} with MORE parse errors than it \
             started with ({before} -> {after}) — refusing to write.\n       \
             This means a fix's edit range did not mean what its diagnostic meant; the file is \
             unchanged. Re-run with `--dry-run` to see the edits, and please report it.",
            deduped.len()
        );
        process::exit(1);
    }
    if let Err(e) = std::fs::write(filename, &rewritten) {
        eprintln!("error: failed to write {filename}: {e}");
        process::exit(1);
    }
    println!("applied {} fix(es) to {filename}", deduped.len());
}

// `byte_offset_to_line_col` was promoted to `crate::byte_offset_to_line_col`
// in `src/lib.rs` so codegen's debugger-contract metadata emission can reuse
// it. The cli still calls it from `apply_fixes` below; the rename is a single
// crate-path tweak with no behavior change.

/// Implementation of `karac migrate shared-to-par <Type>` — phase-7
/// L215a foundation slice. Locates the `shared struct <Type>` definition
/// in the parsed source, runs the L201a type-definition rewrite via
/// [`crate::ownership::build_fix_diff_edits`], and prints (dry-run) or
/// writes (`--apply`) the resulting edits.
///
/// **Scope (v1, L215a–L215b4).** Type-definition rewrite (keyword rename
/// `shared` → `par`, `mut ` strip per mut field, `Mutex[T]` wrap per mut
/// field) plus consumer-site `lock self.field { ... }` wraps across every
/// read/write of bindings of `<Type>` — annotated bindings (L215b1),
/// `lock self.field` wrap shape + read-site rewrite (L215b2), typecheck-
/// resolved inferred bindings + mutating-method-call wraps (L215b3), and
/// cross-file workspace walk (L215b4). When the file argument is omitted,
/// the tool discovers the project root via `kara.toml`, walks every
/// `.kara` module under `src/`, and runs the per-file rewrite pipeline
/// against each.
///
/// **Workspace dirty-check** (`--apply` only). When `--apply` is set
/// without `--force`, the tool refuses to run if `git status --porcelain`
/// reports any modifications. The check shells out to `git`; absence
/// of `git` (or running outside a repo) is treated as "no dirt to
/// guard against" rather than an error — the guard is opportunistic,
/// not load-bearing. `--force` bypasses the check unconditionally.
/// In project-mode the check runs from the project root; in single-
/// file mode it runs from the file's parent directory.
pub(super) fn cmd_migrate(
    type_name: &str,
    apply: bool,
    force: bool,
    file: Option<&str>,
    atomic: bool,
) {
    match file {
        Some(f) => cmd_migrate_single_file(type_name, apply, force, f),
        None => cmd_migrate_project(type_name, apply, force, atomic),
    }
}

/// Single-file migration (L215a–b3 surface). When the user passes
/// `<file.kara>` explicitly, only that file is parsed + rewritten — the
/// struct definition must live in the named file or the tool errors.
pub(super) fn cmd_migrate_single_file(type_name: &str, apply: bool, force: bool, filename: &str) {
    let source = read_source(filename);
    let outcome = compute_migration_edits_for_file(filename, &source, type_name);
    match outcome {
        FileMigrationOutcome::ParseFailed(msgs) => {
            for m in &msgs {
                eprintln!("{m}");
            }
            process::exit(1);
        }
        FileMigrationOutcome::WrongKind => {
            eprintln!(
                "error: `{type_name}` is not a `shared struct` — `karac migrate shared-to-par` only applies to `shared struct` definitions (run `karac fix` on a `par {{ ... }}` diagnostic instead)"
            );
            process::exit(1);
        }
        FileMigrationOutcome::NoStructDef => {
            eprintln!(
                "error: no struct named `{type_name}` found in `{filename}` — `karac migrate shared-to-par` rewrites the type definition in place, so the type must be defined in the migration file"
            );
            process::exit(1);
        }
        FileMigrationOutcome::Ok(plan) => {
            if plan.edits.is_empty() {
                println!("(no migration edits needed for `{type_name}` in {filename})");
                return;
            }
            if apply && !force && workspace_has_uncommitted_changes(filename) {
                eprintln!(
                    "error: workspace has uncommitted changes — refusing to run `karac migrate --apply` without `--force`"
                );
                eprintln!(
                    "       commit or stash pending work first, or re-run with `--force` to bypass the guard."
                );
                process::exit(1);
            }
            emit_migration_for_file(&plan, apply);
            if !apply {
                println!(
                    "(dry-run — re-run with `--apply` to write changes; consumer-site lock-block wraps cover assign / compound-assign writes, reads, and mutating method calls against bindings of `{type_name}` in this file — including type-inferred bindings when the file typechecks. Cross-file walks now run by default when `<file>` is omitted; see project-mode below)"
                );
            }
        }
    }
}

/// Project-mode migration (L215b4). Discovers the project root via
/// `kara.toml`, walks every module under `src/`, runs the per-file
/// rewrite pipeline against each, and aggregates the results. Exactly
/// one walked file must contain `shared struct <Type>`; zero or more
/// than one is a hard error. Files with no edits are silently skipped.
///
/// The pass is two-stage so that consumer-only modules participate:
/// the def-file's mut-field set is collected first, then every file's
/// consumer rewrite runs with that set (using
/// [`build_consumer_rewrite_edits_with_mut_fields`]).
pub(super) fn cmd_migrate_project(type_name: &str, apply: bool, force: bool, atomic: bool) {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            process::exit(1);
        }
    };
    let Some(root) = manifest::discover_project_root(&cwd) else {
        eprintln!(
            "error: `karac migrate shared-to-par` could not find a `kara.toml` in the current directory or any ancestor — run from inside a project, or pass an explicit `<file.kara>` argument for single-file mode"
        );
        process::exit(1);
    };
    let walked = match walker::walk_project(&root, WalkerOpts::default()) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("error: cannot walk project at `{}`: {}", root.display(), e);
            process::exit(1);
        }
    };

    // Stage 1: parse every file (resolve + typecheck for type_ctx), find
    // the def-file, and collect its mut-field set. Parse errors abort —
    // a file that doesn't parse can't be safely rewritten. Typecheck
    // errors degrade gracefully (L215b3 "manual at the review step").
    struct PreparedFile {
        filename: String,
        source: String,
        pipeline: Pipeline,
        has_shared_def: bool,
        has_wrong_kind: bool,
    }
    let mut prepared: Vec<PreparedFile> = Vec::new();
    for module in &walked.modules {
        let filename = module.file.to_string_lossy().into_owned();
        let source = match std::fs::read_to_string(&module.file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: cannot read `{}`: {e}", module.file.display());
                process::exit(1);
            }
        };
        let mut pipeline = Pipeline::new(&filename, &source);
        if pipeline.has_parse_errors() {
            for err in &pipeline.parsed.errors {
                eprintln!(
                    "error[parse]: {}:{}:{}: {}",
                    filename, err.span.line, err.span.column, err.message
                );
            }
            process::exit(1);
        }
        pipeline.resolve();
        pipeline.typecheck();
        let struct_def = pipeline
            .parsed
            .program
            .items
            .iter()
            .find_map(|it| match it {
                Item::StructDef(s) if s.name == type_name => Some(s),
                _ => None,
            });
        let (has_shared_def, has_wrong_kind) = match struct_def {
            Some(s) if s.is_shared => (true, false),
            Some(_) => (false, true),
            None => (false, false),
        };
        prepared.push(PreparedFile {
            filename,
            source,
            pipeline,
            has_shared_def,
            has_wrong_kind,
        });
    }

    let def_files: Vec<&PreparedFile> = prepared.iter().filter(|p| p.has_shared_def).collect();
    let wrong_kind_files: Vec<&PreparedFile> =
        prepared.iter().filter(|p| p.has_wrong_kind).collect();
    if def_files.is_empty() && !wrong_kind_files.is_empty() {
        eprintln!(
            "error: `{type_name}` is not a `shared struct` (found a non-shared definition in `{}`) — `karac migrate shared-to-par` only applies to `shared struct` definitions",
            wrong_kind_files[0].filename
        );
        process::exit(1);
    }
    if def_files.is_empty() {
        eprintln!(
            "error: no `shared struct {type_name}` found in any module under `{}/src/` — `karac migrate shared-to-par` rewrites the type definition in place, so the type must be defined somewhere in the project",
            root.display()
        );
        process::exit(1);
    }
    if def_files.len() > 1 {
        let names: Vec<String> = def_files.iter().map(|p| p.filename.clone()).collect();
        eprintln!(
            "error: multiple `shared struct {type_name}` definitions found across the project ({} files); each migration target must be unique. Files: {}",
            def_files.len(),
            names.join(", ")
        );
        process::exit(1);
    }

    // Stage 1b: compute per-field Atomic/Mutex classification (L215c).
    // On by default; `--no-atomic` clears `atomic` and restores the
    // L215a–b4 behavior (every mut field is Mutex[T] and the consumer-
    // rewrite wraps every site). Project-mode only — single-file mode
    // lacks workspace visibility for the "every write is a bare `=`
    // assign" judgment, so its `atomic` is always false.
    let mut_fields = crate::ownership::collect_struct_mut_field_names(
        type_name,
        &def_files[0].pipeline.parsed.program.items,
    );
    let field_kinds: std::collections::HashMap<String, crate::ownership::FieldWrapKind> = if atomic
    {
        let project_files: Vec<crate::ownership::ProjectMigrationFile<'_>> = prepared
            .iter()
            .map(|f| crate::ownership::ProjectMigrationFile {
                program_items: &f.pipeline.parsed.program.items,
                type_ctx: f.pipeline.typed.as_ref().map(|t| {
                    crate::ownership::ConsumerRewriteTypeCtx {
                        pattern_binding_types: &t.pattern_binding_types,
                        method_callee_types: &t.method_callee_types,
                    }
                }),
            })
            .collect();
        crate::ownership::classify_field_wrap_kinds(
            type_name,
            &mut_fields,
            &def_files[0].pipeline.parsed.program.items,
            &project_files,
        )
    } else {
        std::collections::HashMap::new()
    };
    // L215c-cons — Atomic-classified fields' consumer sites are now
    // auto-rewritten by `build_consumer_rewrite_edits_with_mut_fields`:
    // bare `c.f = v` writes become `c.f.store(v, MemoryOrdering.Release)`
    // and bare `c.f` reads become `c.f.load(MemoryOrdering.Acquire)`.
    // The Mutex-classified fields continue to receive the lock-wrap
    // shape from the same walker. Pass the full mut-fields set as the
    // rewrite target and the Atomic subset as the dispatch discriminator.
    let atomic_fields: std::collections::HashSet<String> = field_kinds
        .iter()
        .filter_map(|(name, k)| match k {
            crate::ownership::FieldWrapKind::Atomic => Some(name.clone()),
            crate::ownership::FieldWrapKind::Mutex => None,
        })
        .collect();
    let atomic_field_count = atomic_fields.len();

    // Stage 2: run the type-def + consumer rewrite per file with the
    // classifier-aware emitter for the type def, and the Mutex-only
    // subset for the consumer wrap.
    let mut plans: Vec<FileMigrationPlan> = Vec::with_capacity(prepared.len());
    for file in &prepared {
        let typedef_edits = if file.has_shared_def {
            crate::ownership::build_fix_diff_edits_with_field_kinds(
                type_name,
                crate::ownership::BindingKind::Shared,
                &file.pipeline.parsed.program.items,
                &field_kinds,
            )
        } else {
            Vec::new()
        };
        let type_ctx =
            file.pipeline
                .typed
                .as_ref()
                .map(|t| crate::ownership::ConsumerRewriteTypeCtx {
                    pattern_binding_types: &t.pattern_binding_types,
                    method_callee_types: &t.method_callee_types,
                });
        let consumer_edits = crate::ownership::build_consumer_rewrite_edits_with_mut_fields(
            type_name,
            &file.pipeline.parsed.program.items,
            type_ctx,
            &mut_fields,
            &atomic_fields,
        );
        let mut edits: Vec<crate::resolver::TextEdit> = typedef_edits;
        edits.extend(consumer_edits);
        edits.sort_by_key(|e| std::cmp::Reverse(e.offset));
        edits.dedup_by(|a, b| {
            a.offset == b.offset && a.length == b.length && a.replacement == b.replacement
        });
        if edits.is_empty() {
            continue;
        }
        plans.push(FileMigrationPlan {
            filename: file.filename.clone(),
            source: file.source.clone(),
            edits,
        });
    }

    if plans.is_empty() {
        println!(
            "(no migration edits needed for `{type_name}` across {} module(s) under {})",
            walked.modules.len(),
            root.display()
        );
        return;
    }

    if apply && !force && workspace_has_uncommitted_changes(&root.to_string_lossy()) {
        eprintln!(
            "error: workspace has uncommitted changes — refusing to run `karac migrate --apply` without `--force`"
        );
        eprintln!(
            "       commit or stash pending work first, or re-run with `--force` to bypass the guard."
        );
        process::exit(1);
    }

    let total_edits: usize = plans.iter().map(|p| p.edits.len()).sum();
    if !apply {
        println!(
            "would apply {total_edits} migration edit(s) across {} file(s) for `{type_name}`:",
            plans.len()
        );
    }
    for plan in &plans {
        emit_migration_for_file(plan, apply);
    }
    if !apply {
        println!(
            "(dry-run — re-run with `--apply` to write changes; consumer-site lock-block wraps cover assign / compound-assign writes, reads, and mutating method calls against bindings of `{type_name}` across the project — including type-inferred bindings in each file that typechecks)"
        );
        if atomic_field_count > 0 {
            println!(
                "(note: {atomic_field_count} field(s) on `{type_name}` were classified as `Atomic[T]` — their consumer assigns rewritten to `.store(v, MemoryOrdering.Release)` and reads rewritten to `.load(MemoryOrdering.Acquire)`)"
            );
        }
    } else if atomic_field_count > 0 {
        println!(
            "(note: {atomic_field_count} field(s) on `{type_name}` were rewritten as `Atomic[T]` — their consumer assigns auto-rewritten to `.store(v, MemoryOrdering.Release)` and reads to `.load(MemoryOrdering.Acquire)`)"
        );
    }
}

/// Outcome of running the migration pipeline against a single file.
pub(super) enum FileMigrationOutcome {
    /// Parse failed; the inner messages are pre-formatted error lines.
    ParseFailed(Vec<String>),
    /// A struct named `<Type>` exists in this file but is not a
    /// `shared struct` (`shared-to-par` is the only migration kind today,
    /// so a plain struct of the same name is "you ran the wrong tool").
    WrongKind,
    /// No struct named `<Type>` in this file. Single-file mode treats
    /// this as a hard error (the def must live in the migration file);
    /// project-mode bypasses this enum entirely and computes consumer
    /// edits via [`build_consumer_rewrite_edits_with_mut_fields`].
    NoStructDef,
    /// File defines `shared struct <Type>` and edits were computed.
    Ok(FileMigrationPlan),
}

/// Per-file rewrite payload — `filename` + `source` are carried through
/// so the emitter can compute line/column previews and the apply path
/// can write the rewritten bytes back without re-reading.
pub(super) struct FileMigrationPlan {
    filename: String,
    source: String,
    edits: Vec<crate::resolver::TextEdit>,
}

/// Run the parse → resolve → typecheck → rewrite pipeline against a
/// single file's source. Shared between single-file and project-mode
/// entry points. The struct-definition lookup happens here so the
/// caller can distinguish the three "no struct def in this file" /
/// "struct def is a plain struct" / "struct def is shared" cases.
pub(super) fn compute_migration_edits_for_file(
    filename: &str,
    source: &str,
    type_name: &str,
) -> FileMigrationOutcome {
    let mut pipeline = Pipeline::new(filename, source);
    if pipeline.has_parse_errors() {
        let msgs: Vec<String> = pipeline
            .parsed
            .errors
            .iter()
            .map(|err| {
                format!(
                    "error[parse]: {}:{}:{}: {}",
                    filename, err.span.line, err.span.column, err.message
                )
            })
            .collect();
        return FileMigrationOutcome::ParseFailed(msgs);
    }
    pipeline.resolve();
    pipeline.typecheck();

    let struct_def = pipeline
        .parsed
        .program
        .items
        .iter()
        .find_map(|it| match it {
            Item::StructDef(s) if s.name == type_name => Some(s),
            _ => None,
        });
    let has_shared_def = match struct_def {
        Some(s) if s.is_shared => true,
        Some(_) => return FileMigrationOutcome::WrongKind,
        None => false,
    };

    let typedef_edits = if has_shared_def {
        crate::ownership::build_fix_diff_edits(
            type_name,
            crate::ownership::BindingKind::Shared,
            &pipeline.parsed.program.items,
        )
    } else {
        Vec::new()
    };
    let type_ctx = pipeline
        .typed
        .as_ref()
        .map(|t| crate::ownership::ConsumerRewriteTypeCtx {
            pattern_binding_types: &t.pattern_binding_types,
            method_callee_types: &t.method_callee_types,
        });
    let consumer_edits = crate::ownership::build_consumer_rewrite_edits_in_program(
        type_name,
        &pipeline.parsed.program.items,
        type_ctx,
    );

    let mut edits: Vec<crate::resolver::TextEdit> = typedef_edits;
    edits.extend(consumer_edits);
    edits.sort_by_key(|e| std::cmp::Reverse(e.offset));
    edits.dedup_by(|a, b| {
        a.offset == b.offset && a.length == b.length && a.replacement == b.replacement
    });

    if has_shared_def {
        FileMigrationOutcome::Ok(FileMigrationPlan {
            filename: filename.to_string(),
            source: source.to_string(),
            edits,
        })
    } else {
        FileMigrationOutcome::NoStructDef
    }
}

/// Render the dry-run preview block or apply the plan to disk. Shared
/// between single-file and project-mode emitters so the per-file
/// output shape stays identical across both paths. The single-file
/// dry-run footer and the project-mode top-level header/footer are
/// emitted by the respective callers, not here.
pub(super) fn emit_migration_for_file(plan: &FileMigrationPlan, apply: bool) {
    let filename = &plan.filename;
    let source = &plan.source;
    let sorted = &plan.edits;
    if !apply {
        println!(
            "would apply {} migration edit(s) to {filename}:",
            sorted.len()
        );
        for edit in sorted.iter().rev() {
            let original = source
                .get(edit.offset..edit.offset.saturating_add(edit.length))
                .unwrap_or("<?>");
            let (line, col) = crate::byte_offset_to_line_col(source, edit.offset);
            let preview = if edit.length == 0 {
                format!("(insert) → `{}`", edit.replacement)
            } else {
                format!("`{}` → `{}`", original, edit.replacement)
            };
            println!("  {filename}:{line}:{col}: {preview}");
        }
        return;
    }

    let mut rewritten = source.clone();
    for edit in sorted {
        let end = edit.offset.saturating_add(edit.length);
        if end > rewritten.len() {
            eprintln!(
                "error: migrate would write past end of file ({} > {}) — aborting without modifying {filename}",
                end,
                rewritten.len()
            );
            process::exit(1);
        }
        rewritten.replace_range(edit.offset..end, &edit.replacement);
    }
    if let Err(e) = std::fs::write(filename, &rewritten) {
        eprintln!("error: failed to write {filename}: {e}");
        process::exit(1);
    }
    println!("applied {} migration edit(s) to {filename}", sorted.len());
}

/// Returns `true` when `git status --porcelain` reports any modified
/// or untracked files. The check is opportunistic — when `git` is
/// absent, the path isn't a git repo, or the invocation fails for any
/// other reason, the result is `false` (no guard rather than spurious
/// rejection). The intent is to prevent `karac migrate --apply` from
/// burying user work under a tool-applied diff, not to enforce a
/// universal pre-flight check.
pub(super) fn workspace_has_uncommitted_changes(filename: &str) -> bool {
    let working_dir = std::path::Path::new(filename)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let Ok(output) = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&working_dir)
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    !output.stdout.is_empty()
}
