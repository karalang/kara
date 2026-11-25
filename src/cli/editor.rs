//! The checks `karac check <file>` runs, for an editor buffer.
//!
//! The language server used to check a buffer with its own sequence of phases
//! (`run_static_checks` in `lib.rs`), which ran neither the v2 core rules nor
//! the Kāra library, so it reported errors `karac check` does not: three on the
//! `karac new --cli` scaffold, where `split_whitespace` and `lines` are library
//! iterators (B-2026-10-31-19). Both entry points here build the same
//! [`Pipeline`] `cmd_check` builds, so the editor and the command line cannot
//! disagree about a single file.

use super::diag_json::collect_diagnostics;
use super::Pipeline;
use crate::manifest;
use crate::{PlaygroundDiagnostic, SourceFix};

/// The name the buffer is checked under. Nothing is read from it: it labels
/// the JSON records, which are read back below for their text.
const BUFFER: &str = "main.kara";

pub(super) fn checked(source: &str) -> Pipeline {
    let mut pipeline = Pipeline::new(BUFFER, source).strict_core().with_library();
    pipeline.run_all_checks();
    pipeline
}

/// [`checked`] as far as a position query reads: through the effect check
/// (B-2026-11-21-90).
pub(super) fn checked_for_nav(source: &str) -> Pipeline {
    let mut pipeline = Pipeline::new(BUFFER, source).strict_core().with_library();
    pipeline.run_checks_through_effects();
    pipeline
}

/// [`checked`], against the whole library rather than the part the buffer
/// reaches: completion offers what the buffer has not named yet.
pub(super) fn checked_whole_library(source: &str) -> Pipeline {
    let mut pipeline = Pipeline::new(BUFFER, source)
        .strict_core()
        .whole_library()
        .with_library();
    pipeline.run_all_checks();
    pipeline
}

/// `karac check`'s diagnostics for `source`, errors, warnings and notes alike,
/// each with its byte span and its machine-applicable fix.
pub fn check_source(source: &str) -> Vec<PlaygroundDiagnostic> {
    diagnostics_of(&checked(source))
}

/// One full check of a buffer, as the incremental checker keeps it.
pub(crate) struct Checked {
    pub(crate) diags: Vec<PlaygroundDiagnostic>,
    /// Each function's inferred effects, rendered, keyed as the effect
    /// checker keys them (`f`, `Type.method`).
    pub(crate) effects: std::collections::HashMap<String, String>,
    pub(crate) hints: Vec<super::editor_hints::InlayHint>,
}

/// [`check_source`], with the effects and inlay hints of the same check, for
/// the incremental check ([`super::editor_incremental`]).
pub(crate) fn check_detailed(source: &str) -> Checked {
    let pipeline = checked(source);
    let effects = pipeline
        .effects
        .as_ref()
        .map(|e| {
            e.inferred_effects
                .iter()
                .map(|(k, v)| (k.clone(), crate::render_effect_set(v)))
                .collect()
        })
        .unwrap_or_default();
    let checked = Checked {
        diags: diagnostics_of(&pipeline),
        effects,
        hints: super::editor_hints::hints_of(&pipeline, source),
    };
    // A position query on this text reads the same check (B-2026-11-21-90).
    super::editor_nav::remember(source, pipeline);
    checked
}

fn diagnostics_of(pipeline: &Pipeline) -> Vec<PlaygroundDiagnostic> {
    let diags = collect_diagnostics(pipeline);
    diags
        .entries
        .iter()
        .zip(&diags.spans)
        .filter_map(|(entry, span)| {
            let v: serde_json::Value = serde_json::from_str(entry).ok()?;
            Some(PlaygroundDiagnostic {
                phase: phase_of(v["phase"].as_str().unwrap_or("")),
                severity: severity_of(v["severity"].as_str().unwrap_or("")),
                message: v["message"].as_str().unwrap_or("").to_string(),
                code: v["code"].as_str().map(str::to_string),
                help: v["hints"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|h| h["description"].as_str())
                    .map(str::to_string)
                    .collect(),
                line: span.line,
                column: span.column,
                offset: span.offset,
                length: span.length,
                fixes: fixes_of(&v),
            })
        })
        .collect()
}

/// The resolution, type and effect tables of `source` as `karac check` builds
/// them, for hover; `None` when the buffer does not parse.
pub(crate) fn analyze(
    source: &str,
) -> Option<(
    crate::resolver::ResolveResult,
    crate::typechecker::TypeCheckResult,
    crate::effectchecker::EffectCheckResult,
)> {
    let mut pipeline = checked_for_nav(source);
    Some((
        pipeline.resolved.take()?,
        pipeline.typed.take()?,
        pipeline.effects.take()?,
    ))
}

fn phase_of(phase: &str) -> &'static str {
    match phase {
        "parse" => "parse",
        "resolve" => "resolve",
        "typecheck" => "typecheck",
        "effect" => "effect",
        "ownership" => "ownership",
        "lint" => "lint",
        "comptime" => "comptime",
        "provider_escape" => "provider_escape",
        "simd_check" => "simd_check",
        _ => "check",
    }
}

fn severity_of(severity: &str) -> &'static str {
    match severity {
        "warning" => "warning",
        "note" => "note",
        _ => "error",
    }
}

/// The record's edits: a single `replacement` or a `fix_diff` envelope, the
/// one edit shape every phase emits (B-2026-11-05-30), and what `karac fix`
/// applies.
fn fixes_of(v: &serde_json::Value) -> Vec<SourceFix> {
    let edit = |e: &serde_json::Value| -> Option<SourceFix> {
        Some(SourceFix {
            offset: e["offset"].as_u64()? as usize,
            length: e["length"].as_u64()? as usize,
            replacement: e["text"].as_str()?.to_string(),
        })
    };
    let mut fixes: Vec<SourceFix> = edit(&v["replacement"]).into_iter().collect();
    if let Some(diff) = v["fix_diff"].as_array() {
        fixes.extend(diff.iter().filter_map(edit));
    }
    fixes
}

/// `karac check`'s diagnostics for the editor buffer `source` of the file at
/// `path`. A member of a `kara.toml` package is checked as `karac check
/// <file>` checks it, with the whole package and the buffer in place of the
/// file on disk, so `import util;` finds the sibling module
/// (B-2026-11-05-15). Any other file, or a package that cannot be assembled,
/// is checked on its own as [`check_source`] does.
pub fn check_source_at(source: &str, path: &std::path::Path) -> Vec<PlaygroundDiagnostic> {
    let Some(root) = super::run_check_cmds::package_root_of_member(&path.to_string_lossy()) else {
        return check_source(source);
    };
    let mut lints = crate::lints::CliLintOverrides::default();
    let mf = manifest::load_from_root(&root).ok();
    if let Some(mf) = &mf {
        lints.apply_manifest_lints(&mf.lints);
    }
    let platform = super::build_cmds::walk_platform_for_target(crate::target::active_target());
    let Ok(mut pc) =
        super::build_cmds::package_check_with_buffer(&root, &lints, platform, Some((path, source)))
    else {
        return check_source(source);
    };
    // A lone `src/main.kara` is checked as a file (B-2026-10-24-39), under
    // its package's `public_effects`: a package that infers its public
    // effects owes no declaration in the editor either.
    if pc.lone_entry(&root).is_some() {
        let mut pipeline = Pipeline::new(BUFFER, source).strict_core().with_library();
        if let Some(mf) = &mf {
            pipeline.profile_config.public_effects = mf.profile_config.public_effects;
        }
        pipeline.run_all_checks();
        return diagnostics_of(&pipeline);
    }
    // Judged as `karac check` judges the package: by the v2 check of its
    // combined source, the buffer in place of the file, once the module
    // boundaries hold. Its diagnostics in this file are reported in the
    // file's own offsets (B-2026-11-25-53).
    let mut v2 = Vec::new();
    if pc.refuses_only_by_type() {
        if let Some(found) = v2_diagnostics_in(&pc, path, source, mf.as_ref(), &lints) {
            v2 = found;
            pc.past_the_v2_check();
            pc.type_warnings.clear();
        }
    }
    pc.restrict_to_file(path);
    let mut out = pc.editor_diagnostics(path);
    out.extend(v2);
    out
}

/// The v2 check of the package's combined source with `source` as the file
/// at `path`, and its diagnostics placed in that file. `None` when the
/// package does not combine.
fn v2_diagnostics_in(
    pc: &super::build_cmds::PackageCheck,
    path: &std::path::Path,
    source: &str,
    mf: Option<&manifest::Manifest>,
    lints: &crate::lints::CliLintOverrides,
) -> Option<Vec<PlaygroundDiagnostic>> {
    let files =
        super::project_v2::source_files_with_buffer(&pc.tree, false, Some((path, source))).ok()?;
    let a = super::project_v2::amalgamate(&pc.tree, &files, |_, _| Vec::new(), "").ok()?;
    let entry_name = super::project_v2::shown(&pc.tree.modules[pc.tree.root].file);
    let pipeline =
        super::run_v2::check_v2_unreported(&entry_name, &a.text, Some(&a), mf, lints.clone());
    let here = |f: &str| match (std::fs::canonicalize(f), std::fs::canonicalize(path)) {
        (Ok(x), Ok(y)) => x == y,
        _ => std::path::Path::new(f) == path,
    };
    let diags = collect_diagnostics(&pipeline);
    let mut out = Vec::new();
    for (entry, span) in diags.entries.iter().zip(&diags.spans) {
        // Placed as `karac check --output=json` places it: the file, and
        // the line and column in it.
        let Ok(v) =
            serde_json::from_str::<serde_json::Value>(&a.remap_json_text(&entry_name, entry))
        else {
            continue;
        };
        if !v["file"].as_str().is_some_and(here) {
            continue;
        }
        let (Some(line), Some(column)) = (v["line"].as_u64(), v["column"].as_u64()) else {
            continue;
        };
        let (line, column) = (line as usize, column as usize);
        let line_start: usize = source
            .split_inclusive('\n')
            .take(line.saturating_sub(1))
            .map(str::len)
            .sum();
        let offset = source[line_start.min(source.len())..]
            .char_indices()
            .nth(column.saturating_sub(1))
            .map_or(source.len(), |(i, _)| line_start + i);
        out.push(PlaygroundDiagnostic {
            phase: phase_of(v["phase"].as_str().unwrap_or("")),
            severity: severity_of(v["severity"].as_str().unwrap_or("")),
            message: v["message"].as_str().unwrap_or("").to_string(),
            code: v["code"].as_str().map(str::to_string),
            help: v["hints"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|h| h["description"].as_str())
                .map(str::to_string)
                .collect(),
            line,
            column,
            offset,
            length: span.length,
            fixes: fixes_of(&v),
        });
    }
    Some(out)
}
