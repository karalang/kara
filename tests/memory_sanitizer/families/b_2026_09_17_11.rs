//! B-2026-09-17-11 -- a `shared` handle inside an `Option`/`Result` STRUCT
//! payload: every spelling releases its refcount exactly once.

use super::*;

/// B-2026-09-17-11 — `Option`/`Result` payloads whose only owned content is a
/// `shared` handle, over every spelling the row and its neighbours measured.
///
/// Before: 448 B in 14 blocks (the refcount blocks) plus their `String`s,
/// 62 allocs / 34 frees at -O0. Three causes, one per spelling group:
///
/// * `Option[ShOut]` with a SINGLE `shared` field (`lo`, `b`, `f`) read as a
///   transparent `String` wrapper, so the `{ptr,len,cap}` overlay freed nothing;
/// * a `Result` whose struct half carries only a `shared` handle (`lr`, `lp`,
///   `c`, `d`, `k`) had no struct drop, because `type_expr_has_drop_heap`
///   answers `false` for a handle;
/// * a fresh `Option[ShP]` temp handed to a callee that does not entry-copy
///   it (`a`, `g`, `h`, `l`) had no owner in either frame.
///
/// `i` and `j` are the guards the other way: a callee that rebinds the arm
/// payload (`let q = p`) or stores the param owns it, so the caller must not.
#[test]
fn asan_shared_handle_in_optres_struct_payload_released_once() {
    assert_clean_asan_run(
        r#"shared struct ShIn { s: String }
struct ShOut { i: ShIn }
struct ShP { i: ShIn, n: i64 }
fn g(p: ShP) -> i64 { return p.n; }
fn mk(n: i64) -> ShP { return ShP { i: ShIn { s: f"aaaaaaaaaaaaaaaaaaa{n}" }, n: n }; }
fn mkOut(c: String) -> ShOut { return ShOut { i: ShIn { s: f"bbbbbbbbbbbbbbbbbbb{c}" } }; }
fn ign(w: Option[ShP]) -> i64 { return 7; }
fn ignOut(w: Option[ShOut]) -> i64 { return 8; }
fn ignRes(w: Result[ShP, i64]) -> i64 { return 9; }
fn ignResOut(w: Result[ShOut, i64]) -> i64 { return 10; }
fn toG(w: Option[ShP]) -> i64 { match w { Some(p) => { return g(p); }, None => { return 0; } } }
fn bind(w: Option[ShP]) -> i64 { let x = w; return 3; }
fn rebind(w: Option[ShP]) -> i64 { if let Some(p) = w { let q = p; return q.n; } return 0; }
fn keep(w: Option[ShP], v: mut ref Vec[Option[ShP]]) { v.push(w); }
fn pass(w: Result[ShP, i64]) -> Result[ShP, i64] { return w; }
fn main() {
    let lo = Some(mkOut("a"));
    let lr: Result[ShOut, i64] = Ok(mkOut("b"));
    let lp: Result[ShP, i64] = Ok(mk(1));
    println(f"a{ign(Some(mk(2)))}");
    println(f"b{ignOut(Some(mkOut("c")))}");
    println(f"c{ignRes(Ok(mk(3)))}");
    println(f"d{ignResOut(Ok(mkOut("d")))}");
    let o = Some(mk(4));
    println(f"e{ign(o)}");
    let oo = Some(mkOut("e"));
    println(f"f{ignOut(oo)}");
    println(f"g{toG(Some(mk(5)))}");
    println(f"h{bind(Some(mk(6)))}");
    println(f"i{rebind(Some(mk(7)))}");
    let mut v: Vec[Option[ShP]] = [];
    keep(Some(mk(8)), mut v);
    println(f"j{v.len()}");
    let pr = pass(Ok(mk(9)));
    println(f"k{pr.is_ok()}");
    for k in 0..3 { println(f"l{ign(Some(mk(k)))}"); }
    println(f"m{ign(None)}");
    println(f"n{lo.is_some()}{lr.is_ok()}{lp.is_ok()}");
}
"#,
        &[
            "a7",
            "b8",
            "c9",
            "d10",
            "e7",
            "f8",
            "g5",
            "h3",
            "i7",
            "j1",
            "ktrue",
            "l7",
            "l7",
            "l7",
            "m7",
            "ntruetruetrue",
        ],
        "shared_handle_in_optres_struct_payload_released_once",
    );
}
