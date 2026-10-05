//! B-2026-10-05-40 -- `self = v` in a `mut ref self` method writes back, and
//! the displaced receiver is released exactly once.

use super::*;

/// The new value is stored through the receiver instead of being leaked, and
/// nothing is freed twice.
#[test]
fn asan_self_assign_in_mut_ref_self_method() {
    assert_clean_asan_run(
        r#"enum Light { Red, Green(i64) }
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
"#,
        &[
            "a:5", "b:9 new", "c:11 new", "d:50 c", "e:1 e", "f:z 2", "g:1", "g:2", "g:3", "h:7",
            "h:7",
        ],
        "asan_self_assign_in_mut_ref_self_method",
    );
}

/// The displaced receiver's field runs its `Drop` body at the assignment, once,
/// as the free-function `h: mut ref H` spelling does.
#[test]
fn asan_self_assign_runs_displaced_drop_body_once() {
    assert_clean_asan_run(
        r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.id}"); } }
struct H { r: R, n: i64 }
impl H {
    fn swap_in(mut ref self, id: i64) { self = H { r: R { id: id, tag: f"t{id}" }, n: self.n + 1 }; }
    fn maybe(mut ref self, b: bool) { if b { self = H { r: R { id: 50, tag: "c".to_string() }, n: 0 }; } }
}
fn main() {
    let mut h = H { r: R { id: 1, tag: "a".to_string() }, n: 0 };
    h.swap_in(2);
    println(f"{h.r.id} {h.r.tag} {h.n}");
    h.maybe(false);
    h.maybe(true);
    println(f"{h.r.id} {h.n}");
}
"#,
        &["drop 1", "2 t2 1", "drop 2", "50 0", "drop 50"],
        "asan_self_assign_runs_displaced_drop_body_once",
    );
}
