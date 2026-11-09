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

/// [`check_source`], with each function's inferred effects, rendered, keyed
/// as the effect checker keys them (`f`, `Type.method`), for the
/// incremental check ([`super::editor_incremental`]).
pub(crate) fn check_with_effects(
    source: &str,
) -> (
    Vec<PlaygroundDiagnostic>,
    std::collections::HashMap<String, String>,
) {
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
    (diagnostics_of(&pipeline), effects)
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
    let mut pipeline = checked(source);
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
    if let Ok(mf) = manifest::load_from_root(&root) {
        lints.apply_manifest_lints(&mf.lints);
    }
    let platform = super::build_cmds::walk_platform_for_target(crate::target::active_target());
    let Ok(mut pc) =
        super::build_cmds::package_check_with_buffer(&root, &lints, platform, Some((path, source)))
    else {
        return check_source(source);
    };
    // A lone `src/main.kara` is checked as a file (B-2026-10-24-39).
    if pc.lone_entry(&root).is_some() {
        return check_source(source);
    }
    pc.restrict_to_file(path);
    pc.editor_diagnostics(path)
}
