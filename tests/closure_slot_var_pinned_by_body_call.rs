//! A closure argument whose body fixes one of the slot's variables through a
//! call of its own (`T` of `Fn() -> Result[T, E]` by `|| Ok(one())`) is
//! judged against the slot as that call left it, and the parameter it leaves
//! open (`E`) stays the call's to fix later (B-2026-11-19-53). The shapes
//! that already checked (`|| Ok(1)`, `|| Ok(mk())` with `T` fixed by another
//! argument, a block body) are pinned beside it.

use karac::{check_source, run_playground};

const HEAD: &str = "struct W[T, E] { t: T, f: Fn() -> Result[T, E] }\nimpl[T, E] W[T, E] {\n    fn new(f: escaping Fn() -> Result[T, E], t: own T) -> W[T, E] { W { t: t, f: f } }\n    fn with(own self, n: i64) -> W[T, E] { self }\n    fn call(self) -> Result[T, E] { (self.f)() }\n}\nenum Er { A }\nstruct C { id: i64 }\nfn one() -> i64 { 1 }\nfn mk() -> C { C { id: 7 } }\nfn name() -> Str { \"s\" }\n";

fn errors(main: &str) -> Vec<String> {
    check_source(&format!("{HEAD}{main}"))
        .into_iter()
        .filter(|d| d.severity == "error")
        .map(|d| d.message)
        .collect()
}

fn runs(main: &str, want: &str) {
    let src = format!("{HEAD}{main}");
    let errs = errors(main);
    assert!(errs.is_empty(), "{errs:#?}");
    let r = run_playground(&src);
    assert!(r.ok, "{:#?}", r.diagnostics);
    assert_eq!(r.stdout.concat(), want);
}

#[test]
fn closure_body_call_fixes_the_slot_variable_then_let_fixes_the_rest() {
    runs(
        "fn main() { let a = W.new(|| Ok(one()), 5); let a2: W[i64, Er] = a; match a2.call() { Ok(v) => println(f\"{v} {a2.t}\"), Err(_) => println(\"e\") } }\n",
        "1 5\n",
    );
}

#[test]
fn closure_body_call_through_a_chained_method_into_an_annotated_let() {
    runs(
        "fn main() { let a: W[i64, Er] = W.new(|| Ok(one()), 5).with(3); match a.call() { Ok(v) => println(f\"{v}\"), Err(_) => println(\"e\") } }\n",
        "1\n",
    );
}

#[test]
fn closure_body_call_leaves_the_error_type_to_a_later_annotation() {
    runs(
        "fn main() { let a = W.new(|| Ok(one()), 5); let b: W[i64, bool] = a; match b.call() { Ok(v) => println(f\"{v}\"), Err(_) => println(\"e\") } }\n",
        "1\n",
    );
}

#[test]
fn closure_body_call_when_another_argument_fixed_the_variable() {
    runs(
        "fn main() { let a: W[C, Er] = W.new(|| Ok(mk()), C { id: 2 }); match a.call() { Ok(v) => println(f\"{v.id} {a.t.id}\"), Err(_) => println(\"e\") } }\n",
        "7 2\n",
    );
}

#[test]
fn closure_block_body_and_err_body_into_the_slot() {
    runs(
        "fn main() { let a: W[i64, Er] = W.new(|| { let x = one(); Ok(x + 1) }, 5); match a.call() { Ok(v) => println(f\"{v}\"), Err(_) => println(\"e\") } }\n",
        "2\n",
    );
    runs(
        "fn main() { let a: W[i64, Er] = W.new(|| Err(Er.A), 5); match a.call() { Ok(v) => println(f\"{v}\"), Err(_) => println(\"err\") } }\n",
        "err\n",
    );
}

#[test]
fn closure_body_call_of_the_wrong_type_is_still_refused() {
    let errs =
        errors("fn main() { let a: W[i64, Er] = W.new(|| Ok(name()), 5); println(\"no\") }\n");
    assert!(
        errs.iter()
            .any(|m| m.contains("expected 'i64', found 'Str'")),
        "{errs:#?}"
    );
    let errs =
        errors("fn main() { let a: W[Str, Er] = W.new(|| Ok(one()), \"x\"); println(\"no\") }\n");
    assert!(
        errs.iter()
            .any(|m| m.contains("expected 'Str', found 'i64'")),
        "{errs:#?}"
    );
}
