//! Call-site `mut` markers on the MIR pipeline: a program `karac check`
//! refuses for a missing marker does not run on the MIR front ends either
//! (B-2026-10-21-77).

use karac::mir::interp::Outcome;

fn run(src: &str) -> Result<(String, i32), String> {
    let r = karac::mir::lower::run_source(src)?;
    match (&r.outcome, r.exit_code()) {
        (_, Some(code)) => Ok((r.output, code)),
        (Outcome::Error(e), None) => Err(format!("run: {e}\n--- output so far\n{}", r.output)),
        _ => Err("no exit code".into()),
    }
}

const FORWARD: &str = "struct W { n: i64 }\n\
fn bump(w: mut ref W) { w.n += 1; }\n\
fn twice(w: mut ref W) { bump(w); bump(w); }\n\
fn main() {\n    let mut w = W { n: 0 };\n    twice(mut w);\n    println(f\"{w.n}\");\n}\n";

#[test]
fn a_forwarded_mut_ref_argument_without_its_marker_does_not_run() {
    match run(FORWARD) {
        Err(e) => assert!(
            e.contains(
                "every argument to a mutating parameter is marked, a `mut ref` binding's included"
            ),
            "unexpected refusal: {e}"
        ),
        other => panic!("the MIR front end must refuse the unmarked forward, got {other:?}"),
    }
    let marked = FORWARD.replace("bump(w); bump(w);", "bump(mut w); bump(mut w);");
    match run(&marked) {
        Ok((out, 0)) => assert_eq!(out, "2\n"),
        other => panic!("the marked forward must run and print 2, got {other:?}"),
    }
}
