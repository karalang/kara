//! `Never ≤ T` holds at the top of a value and under a covariant slot, never
//! under an invariant one (design.md § Subtyping and variance): a
//! `Vec[Never]` is not a `Vec[i64]`, and an `Option[Never]` is not the
//! `Option[i64]` a `mut ref` parameter writes through (B-2026-11-19-67).

use karac::{check_source, PlaygroundDiagnostic};

fn errors(src: &str) -> Vec<PlaygroundDiagnostic> {
    check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .collect()
}

fn refused(src: &str, expected: &str) {
    let es = errors(src);
    assert_eq!(es.len(), 1, "{es:#?}");
    assert!(es[0].message.contains(expected), "{es:#?}");
    assert!(!es[0].message.contains("unwrap it first"), "{es:#?}");
}

#[test]
fn never_invariance_a_vec_of_never_is_not_a_vec_of_i64() {
    refused(
        "fn main() { let v: Vec[Never] = Vec.new(); let mut w: Vec[i64] = v; w.push(5); println(w.len()); }\n",
        "expected 'Vec[i64]'",
    );
}

#[test]
fn never_invariance_an_option_of_never_into_a_mut_ref_parameter_is_refused() {
    refused(
        "fn set(r: mut ref Option[i64]) { *r = Some(1234567); }\n\
         fn main() { let mut o: Option[Never] = None; set(mut o); match o { Some(n) => { let s: String = n; println(s.len()); } None => println(\"none\") } }\n",
        "expected 'mut ref Option[i64]'",
    );
}

#[test]
fn never_invariance_a_vec_of_never_into_a_mut_ref_parameter_is_refused() {
    refused(
        "fn set(r: mut ref Vec[i64]) { r.push(1); }\n\
         fn main() { let mut o: Vec[Never] = Vec.new(); set(mut o); println(o.len()); }\n",
        "expected 'mut ref Vec[i64]'",
    );
}

#[test]
fn never_invariance_a_mut_ref_binding_is_invariant_in_its_target() {
    refused(
        "fn main() { let mut o: Option[Never] = None; let r: mut ref Option[i64] = mut ref o; *r = Some(3); }\n",
        "expected 'mut ref Option[i64]'",
    );
}

#[test]
fn never_invariance_leaves_a_covariant_widening_alone() {
    for src in [
        "fn main() { let c = false; let v = if c { Some(todo()) } else { None }; let w: Option[String] = v; println(w.is_none()); }\n",
        "fn show(o: Option[String]) -> bool { o.is_none() }\nfn main() { let n: Option[Never] = None; println(show(n)); let r: Result[i64, Never] = Ok(3); let r2: Result[i64, String] = r; println(r2.is_ok()); }\n",
        "fn main() { let v: Vec[i64] = Vec.new(); let x: i64 = if v.is_empty() { 1 } else { panic(\"no\") }; println(x); }\n",
    ] {
        assert!(errors(src).is_empty(), "{src}: {:#?}", errors(src));
    }
}
