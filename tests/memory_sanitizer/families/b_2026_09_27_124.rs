//! B-2026-09-27-124 -- an `if let` / `while let` that destructures a tuple
//! payload out of a named `Option`/`Result` local frees every heap leaf once.

use super::*;

/// B-2026-09-27-124 — the block-form interior retraction ran for a
/// borrow-mode bind, so a read-only `if let Some((a, b)) = o { println(a) }`
/// over `Option[(String, String)]` left both leaves with no owner (4 B in 2
/// blocks at -O0; `match` was clean). Covers the read-only arms (named local,
/// param, fresh temp, `while let`, `Result`, a loop) beside the consuming ones
/// that must still hand the leaf over: a by-value call, a rebind and a `push`.
#[test]
fn asan_iflet_tuple_payload_destructure_frees_leaves_once() {
    assert_clean_asan_run(
        r#"fn eats(s: String) { println(f"es{s}"); }
fn mko() -> Option[(String, String)] { return Some((f"x{1}", f"y{2}")); }
fn prm(o: Option[(String, String)]) { if let Some((a, b)) = o { println(f"p{a}") } }
fn main() {
    let o1: Option[(String, String)] = Some((f"a{1}", f"b{2}"));
    if let Some((a, b)) = o1 { eats(a); }
    println("--");
    let o2: Option[(String, String)] = Some((f"c{1}", f"d{2}"));
    if let Some((a, b)) = o2 { let k = b; println(f"k{k}"); }
    println("--");
    let o3: Option[(String, String)] = Some((f"e{1}", f"f{2}"));
    let mut v: Vec[String] = [];
    if let Some((a, b)) = o3 { v.push(a); v.push(b); }
    println(f"v{v.len()}");
    println("--");
    if let Some((a, b)) = mko() { println(f"t{a}{b}"); }
    println("--");
    prm(Some((f"g{1}", f"h{2}")));
    let o4: Option[(String, String)] = Some((f"i{1}", f"j{2}"));
    prm(o4);
    println("--");
    let mut o5: Option[(String, String)] = Some((f"k{1}", f"l{2}"));
    while let Some((a, b)) = o5 { println(f"w{a}"); o5 = None; }
    println("--");
    let mut o6: Option[(String, String)] = Some((f"m{1}", f"n{2}"));
    while let Some((a, b)) = o6 { eats(b); o6 = None; }
    println("--");
    let o8: Result[(String, i64), i64] = Ok((f"q{1}", 5));
    if let Ok((a, n)) = o8 { println(f"r{a}{n}"); }
    let o9: Result[(String, i64), i64] = Ok((f"s{1}", 6));
    if let Ok((a, n)) = o9 { eats(a); }
    for i in 0..3 {
        let oo: Option[(String, String)] = Some((f"z{i}", f"zz{i}"));
        if let Some((a, b)) = oo { println(f"l{a}"); }
    }
    println("end");
}
"#,
        &[
            "esa1", "--", "kd2", "--", "v2", "--", "tx1y2", "--", "pg1", "pi1", "--", "wk1", "--",
            "esn2", "--", "rq15", "ess1", "lz0", "lz1", "lz2", "end",
        ],
        "iflet_tuple_payload_destructure_frees_leaves_once",
    );
}
