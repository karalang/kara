//! B-2026-09-27-65 — a `for` loop view of a boxed generic enum handed to a METHOD by value.

use super::*;

/// B-2026-09-27-65 — B-2026-09-27-60 gave a `for` loop's view of a boxed
/// generic enum element its own callee variant so the element's `Drop` body
/// stays with the `Vec`, but only at a FREE call. The method spellings missed
/// it: an instance method (`k.take(h)`, `k.takr(h)`) ran the body in the
/// callee as well as at the `Vec`'s death, and the static spelling
/// (`K.st(h)`) crashed, because that path also never moved a boxed-enum
/// binding into a by-value callee, never disarmed a handed-back box
/// (`K.id(h)`), and made its use-after-move copy after the argument was loaded
/// (`K.st(h); K.st(h)`). A named local handed to a static call (`K.st(h)`,
/// `K.id(g)`) crashed the same way with no loop involved.
#[test]
fn asan_for_loop_boxed_generic_enum_view_handed_to_method_by_value() {
    assert_clean_asan_run(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
enum Ho[T] { Full(T), Empty }
fn mks(i: i64) -> S { return S { id: i, s: "ab".to_string() + "cd" } }
fn shows(h: Ho[S]) { match h { Ho.Full(r) => println(f"s{r.id}{r.s}"), Ho.Empty => println("e") } }
struct K { k: i64 }
impl K {
    fn take(self, h: Ho[S]) { shows(h) }
    fn takr(ref self, h: Ho[S]) { shows(h) }
    fn st(h: Ho[S]) { shows(h) }
    fn id(h: Ho[S]) -> Ho[S] { return h }
}
fn mkv(a: i64, b: i64) -> Vec[Ho[S]] {
    let mut v: Vec[Ho[S]] = Vec.new();
    v.push(Ho.Full(mks(a)));
    v.push(Ho.Empty);
    v.push(Ho.Full(mks(b)));
    return v
}
fn main() {
    println("method");
    {
        let v = mkv(1, 2);
        for h in v { let k = K { k: 1 }; k.take(h) }
        println(f"n{v.len()}");
    }
    println("refself");
    {
        let v = mkv(3, 4);
        let k = K { k: 2 };
        for h in v { k.takr(h) }
        println(f"n{v.len()}");
    }
    println("static");
    {
        let v = mkv(5, 6);
        for h in v { K.st(h) }
        println(f"n{v.len()}");
    }
    println("twice");
    {
        let v = mkv(7, 8);
        let k = K { k: 3 };
        for h in v { K.st(h); K.st(h); k.takr(h) }
        println(f"n{v.len()}");
    }
    println("handback");
    {
        let v = mkv(9, 10);
        for h in v { let j = K.id(h); shows(j) }
        println(f"n{v.len()}");
    }
    println("local");
    {
        let h: Ho[S] = Ho.Full(mks(11));
        K.st(h);
        let g: Ho[S] = Ho.Full(mks(12));
        let j = K.id(g);
        shows(j);
    }
    println("end")
}
"#,
        &[
            "method", "s1abcd", "e", "s2abcd", "n3", "dS1", "dS2", "refself", "s3abcd", "e",
            "s4abcd", "n3", "dS3", "dS4", "static", "s5abcd", "e", "s6abcd", "n3", "dS5", "dS6",
            "twice", "s7abcd", "s7abcd", "s7abcd", "e", "e", "e", "s8abcd", "s8abcd", "s8abcd",
            "n3", "dS7", "dS8", "handback", "s9abcd", "dS9", "e", "s10abcd", "dS10", "n3", "dS9",
            "dS10", "local", "s11abcd", "dS11", "s12abcd", "dS12", "end",
        ],
        "asan_for_loop_boxed_generic_enum_view_handed_to_method_by_value",
    );
}
