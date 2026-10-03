//! B-2026-09-28-5 — a borrow accessor's `Option` that nothing binds
//! (`v.first();` discarded, `v.get(1).is_some()` probed) frees the box a
//! wide payload is spilled into, and nothing else: the interior stays the
//! container's.

use super::*;

/// B-2026-09-28-5 — discarded and `is_*`-probed `get`/`first`/`last` over
/// `Vec`, `Slice`, `Map`, a `ref Vec` param, a `Vec[Option[S]]` and a struct
/// field each leaked the 32-byte payload box (608 B in 19 blocks at `-O0`).
#[test]
fn asan_borrow_accessor_unbound_option_frees_box_shell() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
struct H { items: Vec[S] }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn probe(v: ref Vec[S]) -> bool { v.first().is_some() }
fn sl(s: Slice[S]) -> bool { s.first(); s.get(0).is_some() }
fn main() {
    let v = vec![mk(1), mk(2)];
    v.first();
    v.last();
    v.get(1);
    v.get(9);
    let a = v.first().is_some();
    let b = v.get(1).is_none();
    let c = v.last().is_some();
    println(f"v{a}{b}{c}");
    let mut n = 0;
    for i in 0..4 {
        v.get(i);
        if v.get(i).is_some() { n = n + 1; }
    }
    println(f"n{n}");
    let p = probe(v);
    let q = sl(v.as_slice());
    println(f"p{p}{q}");
    let mut m: Map[i64, S] = Map.new();
    m.insert(3, mk(3));
    m.get(3);
    m.get(4);
    let e = m.get(3).is_some();
    println(f"m{e}");
    let ov = vec![Some(mk(4)), None];
    ov.first();
    let f = ov.get(1).is_some();
    println(f"o{f}");
    let h = H { items: vec![mk(5)] };
    h.items.first();
    let g = h.items.last().is_some();
    println(f"h{g}");
    println("end")
}
"#,
        &[
            "vtruefalsetrue",
            "n2",
            "d1",
            "d2",
            "ptruetrue",
            "d3",
            "mtrue",
            "d4",
            "otrue",
            "d5",
            "htrue",
            "end",
        ],
        "asan_borrow_accessor_unbound_option_frees_box_shell",
        17,
    );
}
