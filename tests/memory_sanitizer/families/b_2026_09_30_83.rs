//! B-2026-09-30-83 — a field store through an `Array[T, N]` element place
//! (`a[1].s = ..`) lands in the array, so the displaced heap field is freed
//! once and the stored one is freed at scope exit, never twice.
//! B-2026-09-30-84 — an `Array` element or element field handed to a `ref` /
//! `mut ref` parameter is borrowed in place rather than copied and freed.

use super::*;

/// B-2026-09-30-83 — before the fix the store emitted nothing: the new
/// `String` was built and never reached the array. Stores into an annotated
/// array local, an array field, a `mut ref` array parameter and (with
/// B-2026-09-20-25) an array held in a tuple element, each read back through
/// `rd` so the allocations survive -O2.
#[test]
fn asan_store_through_array_element_field_frees_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct P { s: String, n: i64 }
struct B { a: Array[P, 2], k: i64 }
fn mk(p: String, k: i64) -> String {
    f"{p}-heap-string-longer-than-sso-{k}"
}
fn rd(s: ref String) -> i64 { if s.contains("-heap-") { s.len() } else { 0 } }
fn fill(a: mut ref Array[P, 2]) { a[0].s = mk("p", 4); }
fn main() {
    let mut a: Array[P, 2] = [P { s: mk("a", 1), n: 1 }, P { s: mk("b", 1), n: 2 }];
    a[1].s = mk("c", 1);
    println(f"{rd(a[0].s)} {rd(a[1].s)} {a[1].s}");
    let mut b = B { a: [P { s: mk("d", 2), n: 1 }, P { s: mk("e", 2), n: 2 }], k: 3 };
    b.a[0].s = mk("f", 2);
    println(f"{rd(b.a[0].s)} {b.a[0].s} {b.k}");
    let mut t: (Array[P, 2], i64) = ([P { s: mk("g", 3), n: 1 }, P { s: mk("h", 3), n: 2 }], 5);
    t.0[1].s = mk("i", 3);
    println(f"{rd(t.0[1].s)} {t.0[1].s} {t.1}");
    let mut c: Array[P, 2] = [P { s: mk("j", 4), n: 1 }, P { s: mk("k", 4), n: 2 }];
    fill(mut c);
    println(f"{c[0].s} {rd(c[1].s)}");
}
"#,
        &[
            "31 31 c-heap-string-longer-than-sso-1",
            "31 f-heap-string-longer-than-sso-2 3",
            "31 i-heap-string-longer-than-sso-3 5",
            "p-heap-string-longer-than-sso-4 31",
        ],
        "asan_store_through_array_element_field_frees_once",
        12,
    );
}

/// B-2026-09-30-84 — `rd(a[0])` over `let a: Array[String, 1]` shallow-copied
/// the element into a call temp and freed it after the call, and the array's
/// own drop freed the same buffer again: `free(): double free` at -O0 on an
/// annotated local, an element's field, a struct's array field, an array in a
/// tuple element and a `mut ref` argument (which also lost the write); and the
/// same for a `Vec` or `Array` held in a struct field (through `self` too), a
/// `Vec` in a tuple element, and a `ref` / `mut ref` array parameter.
#[test]
fn asan_array_element_ref_argument_is_borrowed() {
    assert_clean_asan_run_min_allocs(
        r#"struct P { s: String, n: i64 }
struct B { a: Array[P, 1] }
fn mk(p: String, k: i64) -> String {
    f"{p}-heap-string-longer-than-sso-{k}"
}
fn rd(s: ref String) -> i64 { if s.contains("-heap-") { s.len() } else { 0 } }
fn rdp(p: ref P) -> i64 { rd(p.s) + p.n }
fn bump(s: mut ref String) { s.push_str("yy"); }
struct O { v: Vec[String], a: Array[String, 2] }
impl O {
    fn first(ref self) -> i64 { rd(self.a[1]) }
    fn grow(mut ref self) { bump(mut self.a[0]); }
}
fn through(a: mut ref Array[String, 2]) { bump(a[1]); }
fn peek(a: ref Array[String, 2]) -> i64 { rd(a[1]) }
fn main() {
    let a: Array[String, 2] = [mk("a", 1), mk("b", 1)];
    let k = rd(a[1]);
    println(f"{rd(a[0])} {k}");
    let e: Array[P, 1] = [P { s: mk("c", 2), n: 4 }];
    println(f"{rd(e[0].s)} {rdp(e[0])}");
    let b = B { a: [P { s: mk("d", 3), n: 5 }] };
    println(f"{rd(b.a[0].s)} {rdp(b.a[0])}");
    let t: (Array[String, 1], i64) = ([mk("e", 4)], 6);
    println(f"{rd(t.0[0])} {t.1}");
    let mut m: Array[String, 1] = [mk("f", 5)];
    bump(mut m[0]);
    println(f"{rd(m[0])} {m[0]}");
    let mut v: Vec[String] = Vec.new();
    v.push(mk("g", 6));
    let mut o = O { v: v, a: [mk("h", 7), mk("i", 7)] };
    bump(mut o.v[0]);
    o.grow();
    println(f"{rd(o.v[0])} {o.first()} {o.a[0]}");
    let mut w: Vec[String] = Vec.new();
    w.push(mk("j", 8));
    let u: (Vec[String], i64) = (w, 9);
    println(f"{rd(u.0[0])} {u.1}");
    let mut r: Array[String, 2] = [mk("k", 9), mk("l", 9)];
    through(mut r);
    println(f"{peek(r)} {r[1]}");
}
"#,
        &[
            "31 31",
            "31 35",
            "31 36",
            "31 6",
            "33 f-heap-string-longer-than-sso-5yy",
            "33 31 h-heap-string-longer-than-sso-7yy",
            "31 9",
            "33 l-heap-string-longer-than-sso-9yy",
        ],
        "asan_array_element_ref_argument_is_borrowed",
        6,
    );
}
