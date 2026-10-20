//! B-2026-10-19-44: a closure that escapes (stored in a struct field or a
//! collection) holds no borrow, so a view it captures used to lose its
//! loan there and the compiled closure read a freed local. The borrow
//! checker now refuses the store (E0509, core semantics §5.7, §9.3); a
//! closure capturing an owned copy, a borrowing closure called locally
//! and one passed down to `retain` still run.

const PIN: &str = r#"
struct Holder { f: Fn() -> i64 }
fn mk() -> Holder {
    let s = "abc".to_string();
    let r = ref s;
    Holder { f: || r.len() }
}
fn main() {
    let h = mk();
    println(f"{(h.f)()}");
}
"#;

const PUSHED: &str = r#"
fn mk() -> Vec[Fn() -> i64] {
    let s = "abc".to_string();
    let r = ref s;
    let mut v: Vec[Fn() -> i64] = Vec.new();
    v.push(|| r.len() as i64);
    v
}
fn main() { let v = mk(); println(f"{(v[0])()}"); }
"#;

const OK: &str = r#"
struct Holder { f: Fn() -> i64 }
fn mk() -> Holder {
    let s = "abc".to_string();
    let t = s.clone();
    Holder { f: || t.len() as i64 }
}
fn local() -> i64 {
    let s = "abcd".to_string();
    let r = ref s;
    let f = || r.len() as i64;
    f()
}
fn keep(v: mut ref Vec[i64]) {
    let s = "abc".to_string();
    let r = ref s;
    v.retain(|x| x < r.len() as i64);
}
fn main() {
    let h = mk();
    let mut v = vec![1, 5];
    keep(mut v);
    println(f"{(h.f)()} {local()} {v.len()}");
}
"#;

fn refused(src: &str) -> String {
    karac::mir::lower::build_source(src)
        .err()
        .expect("the borrow checker refuses a view stored in an escaping closure")
}

#[test]
fn a_view_captured_by_a_closure_stored_in_a_field_is_refused() {
    let err = refused(PIN);
    assert!(err.contains("[E0509]"), "{err}");
    assert!(err.contains("escaping closure"), "{err}");
    assert!(err.starts_with("borrow check mk: 6:"), "{err}");
}

#[test]
fn a_view_captured_by_a_closure_pushed_into_a_vec_is_refused() {
    let err = refused(PUSHED);
    assert!(err.contains("[E0509]"), "{err}");
    assert!(err.starts_with("borrow check mk: 6:"), "{err}");
}

#[test]
fn owned_captures_and_closures_that_do_not_escape_still_run() {
    let out = karac::mir::lower::run_source(OK).expect("MIR interpreter");
    assert_eq!(out.output, "3 4 1\n");
}
