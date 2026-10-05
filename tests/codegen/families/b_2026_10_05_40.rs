//! B-2026-10-05-40 -- `self = v` in a `mut ref self` method did nothing
//! compiled: the method returned with the receiver unchanged and the new value
//! leaked, where `--interp` and the free-function `p: mut ref T` spelling both
//! wrote it back. An enum iterator that advances by reassigning `self` looped
//! forever.

use super::*;

/// Whole-receiver assignment writes back for an enum, a struct with a heap
/// field (unconditional, conditional, built from `self`'s own fields, through
/// a nested method call, on a `Vec` element), and for enum and generic-enum
/// iterators that step by replacing `self`.
#[test]
fn e2e_self_assign_in_mut_ref_self_method() {
    let src = r#"enum Light { Red, Green(i64) }
impl Light {
    fn flip(mut ref self) { self = Light.Green(5); }
    fn show(ref self) -> i64 { match self { Light.Red => 0, Light.Green(n) => n } }
}
struct P { x: i64, s: String }
impl P {
    fn reset(mut ref self) { self = P { x: 9, s: "new".to_string() }; }
    fn inc(mut ref self) { self = P { x: self.x + 1, s: self.s.clone() }; }
    fn twice(mut ref self) { self.inc(); self.inc(); }
    fn maybe(mut ref self, b: bool) { if b { self = P { x: 50, s: "c".to_string() }; } }
}
struct Q { v: String, k: i64 }
impl Q { fn re(mut ref self, x: String) { self = Q { v: x, k: self.k + 1 }; } }
enum Steps { Up(i64), Done }
impl Iterator for Steps {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] {
        let cur = match self { Steps.Up(n) => n, Steps.Done => -1 };
        if cur < 0 { return None }
        if cur >= 3 { self = Steps.Done; } else { self = Steps.Up(cur + 1); }
        Some(cur)
    }
}
enum Cnt[T] { More(T, i64), Done }
impl[T: Clone] Iterator for Cnt[T] {
    type Item = T;
    fn next(mut ref self) -> Option[T] {
        let (v, n) = match self { Cnt.More(v, n) => (v.clone(), n), Cnt.Done => return None };
        if n <= 1 { self = Cnt.Done; } else { self = Cnt.More(v.clone(), n - 1); }
        Some(v)
    }
}
fn main() {
    let mut l = Light.Red;
    l.flip();
    println(f"a:{l.show()}");
    let mut p = P { x: 1, s: "old".to_string() };
    p.reset();
    println(f"b:{p.x} {p.s}");
    p.twice();
    p.maybe(false);
    println(f"c:{p.x} {p.s}");
    p.maybe(true);
    println(f"d:{p.x} {p.s}");
    let mut v = vec![P { x: 0, s: "e".to_string() }];
    v[0].inc();
    println(f"e:{v[0].x} {v[0].s}");
    let mut q = Q { v: "x".to_string(), k: 0 };
    q.re("y".to_string());
    q.re("z".to_string());
    println(f"f:{q.v} {q.k}");
    let mut st = Steps.Up(1);
    let mut guard = 0;
    while let Some(n) = st.next() { println(f"g:{n}"); guard += 1; if guard > 9 { break } }
    let mut ct = Cnt.More(7, 2);
    while let Some(n) = ct.next() { println(f"h:{n}"); guard += 1; if guard > 19 { break } }
}
"#;
    let want = "a:5\nb:9 new\nc:11 new\nd:50 c\ne:1 e\nf:z 2\ng:1\ng:2\ng:3\nh:7\nh:7\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
