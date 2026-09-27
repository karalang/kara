//! B-2026-09-17-11 -- a `shared` handle inside an `Option`/`Result` STRUCT
//! payload: every spelling releases its refcount exactly once.

use super::*;

/// B-2026-09-17-11 — the observable face of the leak: with a `Drop` on the
/// `shared` struct, a handle that is never released never runs its body.
///
/// Before: `dI1`, `dI3` and `dI4` never printed on any compiled surface (the
/// fresh `Option[ShP]` temp, the single-field `Option[ShOut]` temp, and the
/// `Result[ShP, i64]` temp). Each now prints exactly once. Counted rather than
/// sequenced: the INTERPRETER does not run the three temp bodies at all and
/// orders `dI2`/`dI5` the other way, which is its own row, so this fixture
/// pins the compiled backend only.
#[test]
fn e2e_shared_handle_in_optres_struct_payload_runs_its_body_once() {
    let Some(out) = run_program(
        r#"shared struct ShIn { s: String }
impl Drop for ShIn { fn drop(mut ref self) { println(f"dI{self.s}") } }
struct ShP { i: ShIn, n: i64 }
struct ShOut { i: ShIn }
fn mk(n: i64) -> ShP { return ShP { i: ShIn { s: f"{n}" }, n: n }; }
fn f(w: Option[ShP]) -> i64 { return 7; }
fn fo(w: Option[ShOut]) -> i64 { return 8; }
fn fr(w: Result[ShP, i64]) -> i64 { return 9; }
fn main() {
    println(f"a{f(Some(mk(1)))}");
    let o = Some(mk(2)); println(f"b{f(o)}");
    println(f"c{fo(Some(ShOut { i: ShIn { s: f"3" } }))}");
    println(f"d{fr(Ok(mk(4)))}");
    let r: Result[ShP, i64] = Ok(mk(5)); println("e");
    println("end");
}
"#,
    ) else {
        return;
    };
    for n in 1..=5 {
        let body = format!("dI{n}");
        assert_eq!(
            out.lines().filter(|l| *l == body).count(),
            1,
            "{body} in:\n{out}"
        );
    }
    let rest: Vec<&str> = out.lines().filter(|l| !l.starts_with("dI")).collect();
    assert_eq!(rest, ["a7", "b7", "c8", "d9", "e", "end"], "got:\n{out}");
}

/// B-2026-09-17-11 — the memory fixture's program, as a compiled-output pin.
#[test]
fn e2e_shared_handle_in_optres_struct_payload_output() {
    let Some(out) = run_program(
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
    ) else {
        return;
    };
    assert_eq!(
        out, "a7\nb8\nc9\nd10\ne7\nf8\ng5\nh3\ni7\nj1\nktrue\nl7\nl7\nl7\nm7\nntruetruetrue\n",
        "got:\n{out}"
    );
}
