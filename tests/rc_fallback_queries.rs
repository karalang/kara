//! Tests for the P1.1 RC-fallback query analyzer
//! (`src/rc_fallback_queries.rs`). Each test runs
//! parse → resolve → typecheck → ownershipcheck (the path `ownership_ok`
//! uses), then asserts the emitted `RcFallbackDecision` queries.
//!
//! The driver snippet is the canonical closure-capture-with-outer-use
//! RC trigger from `tests/ownership.rs` — `o` is captured by-value into
//! a closure and used again afterwards, which the ownership pass routes
//! to an RC fallback (not a UseAfterMove error).

use karac::queries::{CompilerQuery, Confidence, Phase, QueryKind};
use karac::rc_fallback_queries::analyze;
use karac::{ownershipcheck, parse, resolve, typecheck};

/// Note: deliberately does NOT assert `ownership.errors.is_empty()` — the
/// `#[no_rc]` suppression cases legitimately raise an ownership error at
/// the use site, and we only care about the query output.
fn analyze_src(src: &str) -> Vec<CompilerQuery> {
    let pr = parse(src);
    assert!(pr.errors.is_empty(), "parse errors: {:?}", pr.errors);
    let resolved = resolve(&pr.program);
    assert!(
        resolved.errors.is_empty(),
        "resolve errors: {:?}",
        resolved.errors
    );
    let typed = typecheck(&pr.program, &resolved);
    assert!(typed.errors.is_empty(), "type errors: {:?}", typed.errors);
    let ownership = ownershipcheck(&pr.program, &typed);
    analyze(&pr.program, &ownership)
}

const RC_TRIGGER: &str = "struct Owned { x: i64 }\n\
     fn take(o: Owned) { }\n\
     fn main() {\n\
         let o = Owned { x: 1 };\n\
         let _f = || take(o);\n\
         let _u = o;\n\
     }";

#[test]
fn rc_fallback_query_fires_for_an_rc_promoted_binding() {
    let queries = analyze_src(RC_TRIGGER);
    let rc: Vec<_> = queries
        .iter()
        .filter(|q| q.kind == QueryKind::RcFallbackDecision)
        .collect();
    assert_eq!(rc.len(), 1, "expected one RC-fallback query; got {:?}", rc);
    let q = rc[0];
    assert_eq!(q.id.def_path.render(), "main");
    assert_eq!(q.cross_phase_origin, Some(Phase::Ownership));
    assert_eq!(q.default_confidence, Confidence::Medium);
    // keep_rc is the standing pick (RC already inserted).
    assert_eq!(q.default, 0);
    let labels: Vec<&str> = q.options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(labels, vec!["keep_rc", "prefer_rc", "no_rc"]);
    // Resolution surface advertises both attributes (the conflict pair).
    assert_eq!(
        q.resolution_surface.attributes,
        vec!["no_rc".to_string(), "prefer_rc".to_string()],
    );
    // The keep_rc note names the binding and the trigger rationale.
    let keep_note = q.options[0].note.as_deref().unwrap_or_default();
    assert!(
        keep_note.contains("`o`") && keep_note.contains("closure capture"),
        "keep_rc note must name the binding + trigger; got {keep_note:?}",
    );
}

#[test]
fn rc_fallback_query_suppressed_by_prefer_rc_on_function() {
    let src = RC_TRIGGER.replace("fn main() {", "#[prefer_rc]\nfn main() {");
    let queries = analyze_src(&src);
    assert!(
        !queries
            .iter()
            .any(|q| q.kind == QueryKind::RcFallbackDecision),
        "an accepted (`#[prefer_rc]`) function emits no query; got {queries:?}",
    );
}

#[test]
fn rc_fallback_query_suppressed_by_no_rc_on_function() {
    let src = RC_TRIGGER.replace("fn main() {", "#[no_rc]\nfn main() {");
    let queries = analyze_src(&src);
    assert!(
        !queries
            .iter()
            .any(|q| q.kind == QueryKind::RcFallbackDecision),
        "a `#[no_rc]` function's RC site is an error, not a query; got {queries:?}",
    );
}

#[test]
fn rc_fallback_query_suppressed_by_no_rc_struct_type_on_parameter() {
    // Type-level `#[no_rc]` suppression keys on `RcEntry.type_name`,
    // which the ownership pass populates for parameter/`self` bindings.
    // Here the RC-promoted value is the *parameter* `o` of a `#[no_rc]`
    // type, so the query is suppressed (see module doc on the
    // local-binding limitation).
    let src = "#[no_rc]\n\
               struct Owned { x: i64 }\n\
               fn take(o: Owned) { }\n\
               fn run(o: Owned) {\n\
                   let _f = || take(o);\n\
                   let _u = o;\n\
               }";
    let queries = analyze_src(src);
    assert!(
        !queries
            .iter()
            .any(|q| q.kind == QueryKind::RcFallbackDecision),
        "a `#[no_rc]` value type resolves the decision at the type; got {queries:?}",
    );
}

#[test]
fn rc_fallback_query_suppressed_by_allow_rc_fallback() {
    let src = RC_TRIGGER.replace("fn main() {", "#[allow(rc_fallback)]\nfn main() {");
    let queries = analyze_src(&src);
    assert!(
        !queries
            .iter()
            .any(|q| q.kind == QueryKind::RcFallbackDecision),
        "a silenced (`#[allow(rc_fallback)]`) function emits no query; got {queries:?}",
    );
}

#[test]
fn no_query_for_program_without_rc_fallback() {
    // No move-after-consume anywhere — no RC fallback, no query.
    let src = "fn main() {\n\
                   let x: i64 = 1;\n\
                   let y = x + 1;\n\
                   let _ = y;\n\
               }";
    let queries = analyze_src(src);
    assert!(
        queries.is_empty(),
        "clean program emits no query; got {queries:?}"
    );
}
