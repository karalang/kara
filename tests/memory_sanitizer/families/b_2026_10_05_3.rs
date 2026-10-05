//! B-2026-10-05-3 — a `let` that shadows a shared binding released the new
//! box twice and leaked the old one. The ASAN twin of the codegen fixture.

use super::*;

/// B-2026-10-05-3 — the codegen fixture's table under ASAN.
#[test]
fn asan_shadowed_shared_binding_releases_each_box_once() {
    assert_clean_asan_run_min_allocs(
        r#"shared enum S1 { A(String), N }
shared struct P { s: String }
fn hs(k: i64) -> String { f"heap-string-long-enough-{k}" }
fn take(p: P) -> i64 { p.s.len() }
fn sh(d: P) -> i64 { let d = P { s: hs(30) }; d.s.len() }
fn early(c: bool) -> i64 { let p = P { s: hs(1) }; let p = P { s: hs(2) }; if c { return p.s.len() } let p = P { s: hs(3) }; p.s.len() + 1 }
fn keep() -> P { let a = P { s: hs(4) }; let b = a; let a = P { s: hs(5) }; b }
fn opt() -> i64 { let o: Option[P] = Some(P { s: hs(6) }); let o: Option[P] = Some(P { s: hs(7) }); match o { Some(p) => p.s.len(), None => 0 } }
fn part2() {
    let w = P { s: hs(16) };
    { let w = P { s: hs(17) }; println(f"g {w.s}") }
    println(f"g2 {w.s}");
    println(f"e {early(true)} {early(false)}");
    println(f"k {keep().s}");
    println(f"o {opt()}");
    let mut i = 0;
    while i < 3 { let q = P { s: hs(i) }; let q = P { s: hs(i + 10) }; println(f"l {q.s}"); i = i + 1; }
    let t = P { s: hs(20) };
    let t = P { s: hs(21) };
    let t = P { s: hs(22) };
    println(f"t {t.s}");
    let c = P { s: hs(23) };
    let f = || c.s.len();
    let c = P { s: hs(24) };
    println(f"c {f()} {c.s}");
    let m = S1.A(hs(25));
    let r = match m { S1.A(s) => { let m = S1.A(hs(26)); s.len() }, _ => 0 };
    println(f"m {r}");
    let mut z = P { s: hs(27) };
    let z2 = P { s: hs(28) };
    let z = P { s: hs(29) };
    println(f"z {z.s} {z2.s}");
}
fn main() {
    let d = S1.A(hs(4));
    let d = S1.A(hs(5));
    println("a");
    let p = P { s: hs(1) };
    let p = P { s: hs(2) };
    println(f"b {p.s}");
    let q = S1.A(hs(6));
    let n = match q { S1.A(s) => s.len(), _ => 0 };
    let q = S1.A(hs(7));
    let m = match q { S1.A(s) => s.len(), _ => 0 };
    println(f"c {n} {m}");
    let mut r = P { s: hs(8) };
    r = P { s: hs(9) };
    let r = P { s: hs(10) };
    println(f"d {r.s}");
    let t = P { s: hs(11) };
    let k = take(t);
    let t = P { s: hs(12) };
    println(f"e {k} {t.s}");
    let u = P { s: hs(13) };
    let u2 = u;
    let u = P { s: hs(14) };
    println(f"f {u2.s} {u.s}");
    println(f"h {sh(P { s: hs(17) })}");
    let x = P { s: hs(18) };
    let x = P { s: x.s + "!" };
    println(f"i {x.s}");
    let y = P { s: hs(19) };
    let y = y;
    println(f"j {y.s}");
    let z = P { s: hs(20) };
    let z = 5;
    println(f"k {z}");
    part2();
}"#,
        &[
            "a",
            "b heap-string-long-enough-2",
            "c 25 25",
            "d heap-string-long-enough-10",
            "e 26 heap-string-long-enough-12",
            "f heap-string-long-enough-13 heap-string-long-enough-14",
            "h 26",
            "i heap-string-long-enough-18!",
            "j heap-string-long-enough-19",
            "k 5",
            "g heap-string-long-enough-17",
            "g2 heap-string-long-enough-16",
            "e 25 26",
            "k heap-string-long-enough-4",
            "o 25",
            "l heap-string-long-enough-10",
            "l heap-string-long-enough-11",
            "l heap-string-long-enough-12",
            "t heap-string-long-enough-22",
            "c 26 heap-string-long-enough-24",
            "m 26",
            "z heap-string-long-enough-29 heap-string-long-enough-28",
        ],
        "asan_shadowed_shared_binding_releases_each_box_once",
        20,
    );
}
