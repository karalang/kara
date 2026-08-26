//! Fail-closed gate on the baked stdlib modules that `codegen::lower_stdlib_source`
//! compiles (B-2026-08-25-13).
//!
//! `lower_stdlib_source` type-checks each baked module and uses only the side
//! tables, discarding `.errors`. That let `runtime/stdlib/cli.kara` ship four
//! stores of a BORROWED `String` into an OWNED `String` field — three lines that
//! are a hard type error in any user program. Codegen moved the borrowed buffer
//! into the new owner and the compiled arg parser double-freed the moment a
//! `--name value` pair was parsed. The interpreter does not share the move, so it
//! presented as an A/B divergence whose compiled half was heap corruption.
//!
//! The gate could not simply be `assert!(tc.errors.is_empty())`, because the
//! typecheck `lower_stdlib_source` runs registers the WHOLE prelude — including a
//! second copy of the very module being compiled. Every type is then declared
//! twice, and the module's own calls go ambiguous against candidates identical to
//! themselves. On the tree that filed this row that manufactured ten errors in
//! three modules whose sources declare each item exactly once:
//!
//! - `protobuf` — 6 `AmbiguousMethod` (`read_varint` x4, `read_len_delim` x2)
//! - `cli` — 2 `AmbiguousMethod` (`help_text`, `version_line`)
//! - `pool` — 2 `E_DROP_DUPLICATE_IMPL` (`Pool`, `PooledConnection`)
//!
//! [`karac::typecheck_stdlib_module_excluding_self`] drops the self-copy, which
//! clears all ten by construction rather than by filtering error kinds — the
//! resulting environment is the one a user program compiling the same source
//! gets. `errors_are_meaningful_only_with_the_self_copy_excluded` pins that
//! difference, and the two `gate_rejects_*` tests keep the gate honest: a check
//! that only ever asserts an empty list is worth nothing if it cannot fail.

/// The modules `codegen::lower_stdlib_source` lowers, as
/// [`karac::prelude::STDLIB_SOURCES`] keys.
///
/// Kept honest by `lowered_module_list_matches_codegen`, which re-derives the
/// set from `src/codegen.rs` itself — a 10th lowered module added without a line
/// here fails that test rather than silently escaping the gate.
const LOWERED_MODULES: &[&str] = &[
    "tracing.kara",
    "ordering.kara",
    "protobuf.kara",
    "mem.kara",
    "regex.kara",
    "pool.kara",
    "process.kara",
    "cli.kara",
    "priority_queue.kara",
];

fn source_of(module: &str) -> &'static str {
    karac::prelude::STDLIB_SOURCES
        .iter()
        .find(|(name, _)| *name == module)
        .unwrap_or_else(|| panic!("`{module}` is not a STDLIB_SOURCES key"))
        .1
}

/// Type-check `src` as its own program the way the gate does, returning
/// `(parse errors, typecheck errors)` rendered for assertion messages.
fn check_standalone(module: &str, src: &str) -> (Vec<String>, Vec<String>) {
    let mut parsed = karac::parse(src);
    let parse_errs = parsed.errors.iter().map(|e| e.to_string()).collect();
    karac::desugar_program(&mut parsed.program);
    let resolve = karac::resolve(&parsed.program);
    let tc = karac::typecheck_stdlib_module_excluding_self(&parsed.program, &resolve, module);
    let tc_errs = tc
        .errors
        .iter()
        .map(|e| format!("[{:?}] {}", e.kind, e.message))
        .collect();
    (parse_errs, tc_errs)
}

/// THE GATE. Every module whose body codegen lowers must type-check clean.
#[test]
fn every_lowered_stdlib_module_typechecks_clean() {
    for module in LOWERED_MODULES {
        let (parse_errs, tc_errs) = check_standalone(module, source_of(module));
        assert!(
            parse_errs.is_empty(),
            "runtime/stdlib/{module} does not parse:\n  {}",
            parse_errs.join("\n  ")
        );
        assert!(
            tc_errs.is_empty(),
            "runtime/stdlib/{module} does not type-check, but codegen lowers its \
             bodies anyway — `lower_stdlib_source` keeps only the side tables and \
             discards these errors, so this ships as a miscompile rather than a \
             compile failure (B-2026-08-25-13). Fix the module; do NOT relax this \
             test.\n  {}",
            tc_errs.join("\n  ")
        );
    }
}

/// Non-vacuity, in the exact shape that shipped: `v` is a `ref String`, the
/// field is an owned `String`. This is what `cli.kara` did four times, and what
/// codegen turned into a double free.
#[test]
fn gate_rejects_a_borrowed_string_stored_into_an_owned_field() {
    let src = format!(
        "{}\n{}",
        source_of("ordering.kara"),
        r#"
struct GateProbeHolder { value: String }

fn gate_probe_store(v: ref String) -> GateProbeHolder {
    return GateProbeHolder { value: v };
}
"#
    );
    let (parse_errs, tc_errs) = check_standalone("ordering.kara", &src);
    assert!(parse_errs.is_empty(), "probe must parse: {parse_errs:?}");
    assert!(
        tc_errs
            .iter()
            .any(|e| e.contains("expected 'String', found 'ref String'")),
        "the gate must reject a borrowed String stored into an owned field — \
         the defect that motivated this row. Got: {tc_errs:?}"
    );
}

/// Non-vacuity, second shape: a plain return-type mismatch, to show the gate is
/// not keyed to one diagnostic.
#[test]
fn gate_rejects_a_return_type_mismatch() {
    let src = format!(
        "{}\n{}",
        source_of("ordering.kara"),
        "\nfn gate_probe_ret() -> i64 {\n    return \"not an integer\";\n}\n"
    );
    let (parse_errs, tc_errs) = check_standalone("ordering.kara", &src);
    assert!(parse_errs.is_empty(), "probe must parse: {parse_errs:?}");
    assert!(
        tc_errs
            .iter()
            .any(|e| e.contains("expected 'i64', found 'String'")),
        "the gate must reject a return-type mismatch. Got: {tc_errs:?}"
    );
}

/// Why the gate needs its own entry point rather than reading the errors
/// `lower_stdlib_source` already computes.
///
/// If the double registration is ever fixed at its root, this test starts
/// failing — that is the correct signal to DELETE it (and to consider pointing
/// `lower_stdlib_source` at the excluding entry point), not to weaken the gate.
#[test]
fn errors_are_meaningful_only_with_the_self_copy_excluded() {
    let src = source_of("cli.kara");
    let mut parsed = karac::parse(src);
    karac::desugar_program(&mut parsed.program);
    let resolve = karac::resolve(&parsed.program);

    // The env `lower_stdlib_source` uses: whole prelude, self-copy included.
    let with_self_copy = karac::typecheck_stdlib_module(&parsed.program, &resolve);
    assert!(
        !with_self_copy.errors.is_empty(),
        "cli.kara is expected to report the injected-self-copy artifact under \
         the plain entry point; if it no longer does, see this test's doc comment"
    );
    assert!(
        with_self_copy
            .errors
            .iter()
            .any(|e| e.message.contains("ambiguous method")),
        "the artifact is an ambiguous-method report against identical candidates"
    );

    // The verification env: identical but for the self-copy.
    let excluding =
        karac::typecheck_stdlib_module_excluding_self(&parsed.program, &resolve, "cli.kara");
    assert!(
        excluding.errors.is_empty(),
        "excluding the self-copy must clear the artifact. If these are genuine \
         cli.kara type errors rather than the duplicate-registration artifact, \
         `every_lowered_stdlib_module_typechecks_clean` is failing too and is \
         the one to read: {:?}",
        excluding
            .errors
            .iter()
            .map(|e| &e.message)
            .collect::<Vec<_>>()
    );
}

/// Keep [`LOWERED_MODULES`] tied to what `codegen.rs` actually lowers, so the
/// gate cannot be outgrown by a new `lower_stdlib_source` call.
#[test]
fn lowered_module_list_matches_codegen() {
    let codegen_src = include_str!("../src/codegen.rs");
    // Match the call name, then skip whatever whitespace separates it from its
    // first argument: `lower_stdlib_source("mem", ..)` and the rustfmt-wrapped
    // `lower_stdlib_source(\n    "priority_queue",\n    ..)` are the same call.
    // Anchoring on `lower_stdlib_source("` instead misses every wrapped call —
    // which is exactly how `priority_queue` escaped this gate from the day the
    // gate landed (B-2026-08-25-13) until B-2026-08-25-32, with this very test
    // reporting green throughout because the omission was SYMMETRIC: the module
    // was missing from the scan AND from the list, so the two agreed. A guard
    // that derives both sides of a comparison through the same blind spot
    // cannot see it.
    let mut found: Vec<String> = codegen_src
        .match_indices("lower_stdlib_source(")
        .filter_map(|(i, pat)| {
            let rest = codegen_src[i + pat.len()..].trim_start();
            let rest = rest.strip_prefix('"')?;
            rest.find('"').map(|end| format!("{}.kara", &rest[..end]))
        })
        .collect();
    found.sort();
    found.dedup();

    let mut expected: Vec<String> = LOWERED_MODULES.iter().map(|m| m.to_string()).collect();
    expected.sort();

    assert_eq!(
        found, expected,
        "the set of modules `lower_stdlib_source` compiles has changed. Add the \
         new module to LOWERED_MODULES so it is gated too — every module lowered \
         here has its typecheck errors discarded (B-2026-08-25-13)."
    );
}
