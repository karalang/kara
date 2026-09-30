//! B-2026-09-20-11 -- a codegen failure raised after the expression walk
//! (the module verifier) carries no source position rather than a stale one.

use super::*;

/// B-2026-09-20-11 — a module-verification failure reported
/// `mv.kara:188:25` for a two-line file. The verifier runs after every
/// expression is compiled, the stdlib last, so the walk cursor that
/// `compile_program_spanned` stamps onto every error held a stdlib position
/// and the CLI rendered it against the user's file. The cursor is now cleared
/// once the walk ends, so a whole-module failure has no span and renders as
/// the bare sentence.
///
/// The program is B-2026-09-27-62's: a user fn named like a runtime C extern
/// reaches the verifier today. If that row is fixed and this compiles, the
/// test says so rather than passing with nothing checked; swap in another
/// program that fails verification.
#[test]
fn verifier_failure_carries_no_fabricated_span() {
    let src =
        "fn malloc(x: i64) -> i64 { return x + 1; }\nfn main() { println(f\"{malloc(41)}\") }\n";
    let mut parsed = karac::parse(src);
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let ownership = karac::ownershipcheck(&parsed.program, &typed);
    let err = match compile_to_ir(&parsed.program, Some(&ownership), None) {
        Ok(_) => panic!(
            "this program no longer fails module verification (B-2026-09-27-62 fixed?); \
             pick another program that reaches the verifier"
        ),
        Err(e) => e,
    };
    assert!(
        err.message.starts_with("Module verification failed"),
        "expected a verifier failure, got: {}",
        err.message
    );
    assert_eq!(
        err.span, None,
        "a whole-module failure must not carry the walk cursor's last position"
    );
}
