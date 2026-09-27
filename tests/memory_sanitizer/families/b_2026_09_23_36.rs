//! B-2026-09-23-36 -- a SHADOWED local `Array` whose generations are not all
//! handed back together (an older generation returned on one exit, a later one
//! on only some, a nested-block shadow over a returned outer local) has one
//! owner per generation on every path.

use super::*;

/// B-2026-09-23-36 — the hand-back class is decided per GENERATION of a
/// shadowed name (the `let` it comes from), and a later generation's per-path
/// drop flag is its own bit. Before: `free(): double free detected in tcache 2`
/// on the JIT, `-O0` and `-O2` for every function here, the older generation
/// staying armed in the callee while the caller freed it too. `inner` is the
/// nested-block shadow, `two_some` has two generations each returned on one
/// exit, `wrapped` hands them back inside `Some`.
#[test]
fn asan_shadowed_array_generations_handed_back_on_different_exits_free_once() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn older(c: bool, b: i64) -> Array[R, 2] { let x: Array[R, 2] = [mkr(b+1), mkr(b+2)]; if c { return x }; let x: Array[R, 2] = [mkr(b+8), mkr(b+9)]; println("dies"); return x }
fn some_exits(c: bool, b: i64) -> Array[R, 2] { let x: Array[R, 2] = [mkr(b+1), mkr(b+2)]; let x: Array[R, 2] = [mkr(b+8), mkr(b+9)]; if c { let w: Array[R, 2] = [mkr(b+5), mkr(b+6)]; return w }; println("dies"); x }
fn inner(b: i64) -> Array[R, 2] { let x: Array[R, 2] = [mkr(b+1), mkr(b+2)]; { let x: Array[R, 2] = [mkr(b+8), mkr(b+9)]; println(f"i{x[0].id}") }; println("dies"); return x }
fn two_some(c: i64, b: i64) -> Array[R, 2] { let x: Array[R, 2] = [mkr(b+1), mkr(b+2)]; if c == 1 { return x }; let x: Array[R, 2] = [mkr(b+3), mkr(b+4)]; if c == 2 { return x }; println("dies"); let w: Array[R, 2] = [mkr(b+5), mkr(b+6)]; return w }
fn wrapped(c: bool, b: i64) -> Option[Array[R, 2]] { let x: Array[R, 2] = [mkr(b+1), mkr(b+2)]; if c { return Some(x) }; let x: Array[R, 2] = [mkr(b+8), mkr(b+9)]; println("dies"); Some(x) }
fn main() {
    let a = older(true, 10); println(f"a{a[0].id}");
    let b = older(false, 20); println(f"b{b[0].id}");
    let c = some_exits(true, 30); println(f"c{c[0].id}");
    let d = some_exits(false, 40); println(f"q{d[0].id}");
    let e = inner(50); println(f"e{e[0].id}");
    for k in 1..4 { let t = two_some(k, 100 * k); println(f"t{t[0].id}") };
    match wrapped(true, 60) { Some(v) => println(f"w{v[0].id}"), None => println("n") };
    match wrapped(false, 70) { Some(v) => println(f"w{v[0].id}"), None => println("n") };
    println("end")
}
"#,
        &[
            "a11", "d11", "d12", "dies", "d21", "d22", "b28", "d28", "d29", "d38", "d39", "d31",
            "d32", "c35", "d35", "d36", "dies", "d41", "d42", "q48", "d48", "d49", "i58", "d58",
            "d59", "dies", "e51", "d51", "d52", "t101", "d101", "d102", "d201", "d202", "t203",
            "d203", "d204", "d301", "d302", "d303", "d304", "dies", "t305", "d305", "d306", "w61",
            "d61", "d62", "dies", "d71", "d72", "w78", "d78", "d79", "end",
        ],
        "asan_shadowed_array_generations_handed_back_on_different_exits_free_once",
    );
}

/// B-2026-09-23-36 — the `String`-element spelling, where the old per-name
/// answer TRANSFERRED statically instead of declining: the generation kept on
/// the other exit leaked (58 B in 2 blocks at `-O0` on the row's cell), and
/// the nested-block shadow double-freed.
#[test]
fn asan_shadowed_string_array_generations_handed_back_on_different_exits_free_once() {
    assert_clean_asan_run(
        r#"fn mks(i: i64) -> String { return f"heap-string-longer-than-sso-{i}" }
fn older(c: bool, b: i64) -> Array[String, 2] { let x: Array[String, 2] = [mks(b+1), mks(b+2)]; if c { return x }; let x: Array[String, 2] = [mks(b+8), mks(b+9)]; println("dies"); return x }
fn two_some(c: i64, b: i64) -> Array[String, 2] { let x: Array[String, 2] = [mks(b+1), mks(b+2)]; if c == 1 { return x }; let x: Array[String, 2] = [mks(b+3), mks(b+4)]; if c == 2 { return x }; println("dies"); let w: Array[String, 2] = [mks(b+5), mks(b+6)]; return w }
fn nested(c: bool, b: i64) -> Array[String, 2] { let x: Array[String, 2] = [mks(b+1), mks(b+2)]; { let x: Array[String, 2] = [mks(b+8), mks(b+9)]; if c { return x }; println("in") }; println("dies"); return x }
fn main() {
    let a = older(true, 10); println(a[1]);
    let b = older(false, 20); println(b[1]);
    for k in 1..4 { let t = two_some(k, 100 * k); println(t[0]) };
    let n = nested(true, 30); println(n[0]);
    let m = nested(false, 40); println(m[0]);
    println("end")
}
"#,
        &[
            "heap-string-longer-than-sso-12",
            "dies",
            "heap-string-longer-than-sso-29",
            "heap-string-longer-than-sso-101",
            "heap-string-longer-than-sso-203",
            "dies",
            "heap-string-longer-than-sso-305",
            "heap-string-longer-than-sso-38",
            "in",
            "dies",
            "heap-string-longer-than-sso-41",
            "end",
        ],
        "asan_shadowed_string_array_generations_handed_back_on_different_exits_free_once",
    );
}
