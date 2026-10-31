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
use crate::{PlaygroundDiagnostic, SourceFix};

/// The name the buffer is checked under. Nothing is read from it: it labels
/// the JSON records, which are read back below for their text.
const BUFFER: &str = "main.kara";

fn checked(source: &str) -> Pipeline {
    let mut pipeline = Pipeline::new(BUFFER, source).strict_core().with_library();
    pipeline.run_all_checks();
    pipeline
}

/// `karac check`'s diagnostics for `source`, errors, warnings and notes alike,
/// each with its byte span and its machine-applicable fix.
pub fn check_source(source: &str) -> Vec<PlaygroundDiagnostic> {
    let pipeline = checked(source);
    let diags = collect_diagnostics(&pipeline);
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

/// The record's edits: a single `replacement`, a `fix_diff` envelope, or a
/// typechecker `fix_it`, in the shapes `karac fix` applies. A typecheck record
/// carries its edit twice, as `fix_it` and as `replacement`, so `fix_it` is
/// read only when neither of the others is there.
fn fixes_of(v: &serde_json::Value) -> Vec<SourceFix> {
    let edit = |e: &serde_json::Value, text: &str| -> Option<SourceFix> {
        Some(SourceFix {
            offset: e["offset"].as_u64()? as usize,
            length: e["length"].as_u64()? as usize,
            replacement: e[text].as_str()?.to_string(),
        })
    };
    let mut fixes: Vec<SourceFix> = edit(&v["replacement"], "text").into_iter().collect();
    if let Some(diff) = v["fix_diff"].as_array() {
        fixes.extend(diff.iter().filter_map(|e| edit(e, "text")));
    }
    if !fixes.is_empty() {
        return fixes;
    }
    if let Some(span) = v["fix_it"].get("span") {
        fixes.extend(v["fix_it"]["replacement"].as_str().and_then(|r| {
            Some(SourceFix {
                offset: span["offset"].as_u64()? as usize,
                length: span["length"].as_u64()? as usize,
                replacement: r.to_string(),
            })
        }));
    }
    fixes
}
