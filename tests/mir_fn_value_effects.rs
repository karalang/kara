//! A call through a function value whose written type declares an effect
//! clause (a struct field's `Fn(i64) -> i64 with reads(Clock)`, or an
//! annotated `let`'s) carries that clause to the call in the built MIR
//! (`Body::fn_value_effects`), for the effect analysis to charge in place
//! of every effect (core-semantics §12). A clause-less type carries
//! nothing: a call through it has every effect. B-2026-10-27-2.

use karac::mir::syntax::{Body, Operand, TerminatorKind};

/// The declared clause of each call through a local in `body`, in block
/// order, as `verb(Resource)` strings; `None` for a call that carries none.
fn call_clauses(body: &Body) -> Vec<Option<Vec<String>>> {
    body.blocks
        .iter()
        .filter_map(|bb| match &bb.terminator.kind {
            TerminatorKind::Call {
                func: Operand::Copy(p) | Operand::Move(p),
                ..
            } => Some(
                body.fn_value_effects
                    .iter()
                    .find(|(l, _)| *l == p.local)
                    .map(|(_, d)| {
                        d.iter()
                            .map(|e| format!("{}({})", e.verb, e.resource))
                            .collect()
                    }),
            ),
            _ => None,
        })
        .collect()
}

fn clauses(src: &str, f: &str) -> Vec<Option<Vec<String>>> {
    let l = karac::mir::lower::build_source(src).unwrap_or_else(|e| panic!("{e}"));
    call_clauses(
        l.program
            .bodies
            .get(f)
            .unwrap_or_else(|| panic!("no body {f}")),
    )
}

#[test]
fn mir_a_call_through_a_field_carries_its_declared_clause() {
    let src = "struct Job {\n    run: Fn(i64) -> i64 with reads(Clock) writes(Stdout),\n    bare: Fn(i64) -> i64,\n}\n\
fn sq(x: i64) -> i64 {\n    x * x\n}\n\
fn go(j: Job) -> i64 {\n    (j.run)(3) + (j.bare)(4)\n}\n\
fn main() {\n    let j = Job { run: sq, bare: sq };\n    println(f\"{go(j)}\");\n}\n";
    assert_eq!(
        clauses(src, "go"),
        vec![
            Some(vec![
                "reads(Clock)".to_string(),
                "writes(Stdout)".to_string()
            ]),
            None
        ]
    );
}

#[test]
fn mir_a_call_through_an_annotated_let_carries_its_declared_clause() {
    // A `let` of a function item is called directly, so the value comes
    // from a branch.
    let src = "fn sq(x: i64) -> i64 {\n    x * x\n}\n\
fn cube(x: i64) -> i64 {\n    x * x * x\n}\n\
fn main() {\n    let n = 3;\n    let f: Fn(i64) -> i64 with reads(Clock) = if n > 2 { sq } else { cube };\n    \
let g = if n > 2 { sq } else { cube };\n    let h: Fn(i64) -> i64 = if n > 2 { sq } else { cube };\n    \
println(f\"{f(2) + g(3) + h(4)}\");\n}\n";
    let got = clauses(src, "main");
    assert!(
        got.contains(&Some(vec!["reads(Clock)".to_string()])),
        "{got:?}"
    );
    assert_eq!(got.len(), 3, "{got:?}");
    assert_eq!(got.iter().filter(|c| c.is_some()).count(), 1, "{got:?}");
}
