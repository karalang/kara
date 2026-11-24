//! core-semantics §9.6 function kinds where `karac check` read the instance
//! instead of the declaration:
//! - B-2026-11-19-76: a generic `F`'s bound decides its calls and what it
//!   takes. A bare `f: F` under `F: MutFn(..)` or `F: OnceFn(..)` is a
//!   borrow and cannot be called (fixed with `own`), nor an `OnceFn` through
//!   `mut ref F`; `F: Fn(..)` refuses a closure that writes a capture.
//! - B-2026-11-19-74: a closure that writes a capture is not stored in an
//!   `Fn` slot: a struct field, a tuple element, a `Vec` element, a generic
//!   field instantiated at `Fn`.
//! - B-2026-11-19-75: a `MutFn` in a tuple element or a generic field is
//!   called only through unique access, as one in a struct field is.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, args: &[&str], src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-fn-kind-bound-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

fn check(tag: &str, src: &str) -> (bool, String) {
    let out = karac(tag, &["check"], src);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

fn refused(tag: &str, src: &str, needle: &str) {
    let (ok, diag) = check(tag, src);
    assert!(
        !ok && diag.contains(needle),
        "{tag}: want {needle:?}: {diag}"
    );
}

fn accepted(tag: &str, src: &str) {
    let (ok, diag) = check(tag, src);
    assert!(ok, "{tag}: {diag}");
}

const COUNTER: &str = "fn main() { let mut n = 0; println(call(|| { n += 1; n })); println(n); }\n";

#[test]
fn generic_bare_mut_fn_bound_param_is_not_called_and_is_fixed_with_own() {
    let src = format!("fn call[F: MutFn() -> i64](f: F) -> i64 {{ f() + f() }}\n{COUNTER}");
    refused(
        "mutfn-bare",
        &src,
        "`f` is a borrowed `F`, whose bound is `MutFn`, so it cannot be called",
    );
    let fix = karac("mutfn-bare-fix", &["fix", "--dry-run"], &src);
    let text = String::from_utf8_lossy(&fix.stdout);
    assert!(text.contains("→ `own `"), "{text}");
    // The `own` parameter is rebound before it is called (design.md §7;
    // review ruling 2026-11-24).
    let own = format!(
        "fn call[F: MutFn() -> i64](f: own F) -> i64 {{ let mut f = f; f() + f() }}\n{COUNTER}"
    );
    accepted("mutfn-own", &own);
    let run = karac("mutfn-own-run", &["__mir-run"], &own);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "3\n2\n");
}

#[test]
fn generic_mut_fn_bound_in_a_where_clause_is_read_too() {
    let src = format!("fn call[F](f: F) -> i64 where F: MutFn() -> i64 {{ f() + f() }}\n{COUNTER}");
    refused("mutfn-where", &src, "whose bound is `MutFn`");
    let by_ref = "fn call[F: MutFn() -> i64](f: mut ref F) -> i64 { f() + f() }\n\
         fn main() { let mut n = 0; let mut g = || { n += 1; n }; println(call(mut g)); }\n";
    accepted("mutfn-mutref", by_ref);
}

#[test]
fn generic_once_fn_bound_param_is_called_only_when_owned() {
    let body = "fn main() { let r = \"a\".to_string(); println(call(|| r.len() as i64)); }\n";
    refused(
        "oncefn-bare",
        &format!("fn call[F: OnceFn() -> i64](f: F) -> i64 {{ f() }}\n{body}"),
        "whose bound is `OnceFn`, so it cannot be called",
    );
    refused(
        "oncefn-mutref",
        "fn call[F: OnceFn() -> i64](f: mut ref F) -> i64 { f() }\n\
         fn main() { let mut g = || 5; println(call(mut g)); }\n",
        "whose bound is `OnceFn`, so it cannot be called",
    );
    accepted(
        "oncefn-own",
        &format!("fn call[F: OnceFn() -> i64](f: own F) -> i64 {{ f() }}\n{body}"),
    );
}

#[test]
fn generic_fn_bound_refuses_a_closure_that_writes_a_capture() {
    refused(
        "fn-bound-mcap",
        &format!("fn call[F: Fn() -> i64](f: F) -> i64 {{ f() + f() }}\n{COUNTER}"),
        "the closure mutates n, so it is a MutFn, but call expects Fn",
    );
    accepted(
        "fn-bound-read",
        "fn call[F: Fn() -> i64](f: F) -> i64 { f() + f() }\n\
         fn main() { let n = 2; println(call(|| n + 1)); }\n",
    );
}

const STORE: &str =
    "the closure mutates n, so it is a MutFn, but it is stored where `Fn` is expected";

#[test]
fn fn_struct_field_refuses_a_closure_that_writes_a_capture() {
    refused(
        "slot-field",
        "struct H { f: Fn() -> i64 }\n\
         fn main() { let mut n = 0; let mut h = H { f: || { n += 1; n } }; println((h.f)()); }\n",
        STORE,
    );
    accepted(
        "slot-field-mutfn",
        "struct H { f: MutFn() -> i64 }\n\
         fn main() { let mut n = 0; let mut h = H { f: || { n += 1; n } }; println((h.f)()); }\n",
    );
}

#[test]
fn fn_vec_and_tuple_slots_refuse_a_closure_that_writes_a_capture() {
    refused(
        "slot-vec-let",
        "fn main() { let mut n = 0; let h: Vec[Fn() -> i64] = [|| { n += 1; n }]; println(h[0]()); }\n",
        STORE,
    );
    refused(
        "slot-vec-arg",
        "fn use_it(h: Vec[Fn() -> i64]) -> i64 { h[0]() }\n\
         fn main() { let mut n = 0; println(use_it([|| { n += 1; n }])); }\n",
        STORE,
    );
    refused(
        "slot-tuple",
        "fn main() { let mut n = 0; let h: (Fn() -> i64, i64) = (|| { n += 1; n }, 0); println((h.0)()); }\n",
        STORE,
    );
    accepted(
        "slot-tuple-read",
        "fn main() { let n = 1; let h: (Fn() -> i64, i64) = (|| n + 1, 0); println((h.0)()); }\n",
    );
}

#[test]
fn generic_field_at_fn_refuses_a_closure_that_writes_a_capture() {
    refused(
        "slot-gfield",
        "struct G[F] { f: F }\n\
         fn main() { let mut n = 0; let mut h: G[Fn() -> i64] = G { f: || { n += 1; n } }; println((h.f)()); }\n",
        STORE,
    );
}

#[test]
fn mut_fn_in_a_tuple_or_generic_field_needs_unique_access_to_call() {
    refused(
        "call-tuple",
        "fn use_it(h: (MutFn() -> i64, i64)) -> i64 { (h.0)() + (h.0)() }\n\
         fn main() { let mut n = 0; let mut h: (MutFn() -> i64, i64) = (|| { n += 1; n }, 0); println(use_it(h)); }\n",
        "a call of the `MutFn` in `h.0`",
    );
    refused(
        "call-gfield",
        "struct G[F] { f: F }\n\
         fn use_it(h: G[MutFn() -> i64]) -> i64 { (h.f)() + (h.f)() }\n\
         fn main() { let mut h: G[MutFn() -> i64] = G { f: || 5 }; println(use_it(h)); }\n",
        "a call of the `MutFn` in `h.f`",
    );
    accepted(
        "call-gfield-mutref",
        "struct G[F] { f: F }\n\
         fn use_it(h: mut ref G[MutFn() -> i64]) -> i64 { (h.f)() + (h.f)() }\n\
         fn main() { let mut n = 0; let mut h: G[MutFn() -> i64] = G { f: || { n += 1; n } }; println(use_it(mut h)); }\n",
    );
}
