//! B-2026-09-17-14 -- a destructured TUPLE payload (`Some((a, b))`) runs its
//! leaves' `Drop` bodies last to first, as arm locals do, on every backend.

use super::*;

/// B-2026-09-17-14 — `match o { Some((a, b)) => .. }` over `Option[(R, R)]`
/// printed `dR3 dR4` on the JIT, `-O0` and `-O2` against `dR4 dR3` under
/// `--interp`, which is the reference: the destructured leaves are arm
/// bindings and drop in reverse, as they already did on every compiled
/// surface for a heap-free payload. A BOXED payload (one holding a `String`)
/// ran the scrutinee's forward element walk instead; a destructuring arm now
/// swaps in a reversed walk. Covers three elements, `Result`, `if let`,
/// `while let` with a reassignment, a nested tuple, a struct leaf (whose own
/// fields keep their reverse declaration order), a reassignment inside the arm
/// (the displaced value reversed, the stored one forward), and two controls
/// that stay forward: the whole-binding arm and a by-value parameter.
#[test]
fn e2e_destructured_tuple_payload_drops_last_to_first() {
    let Some(out) = run_program(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct P { r: R, s: R }
fn mk(i: i64) -> R { return R { id: i, name: f"n{i}" }; }
fn two() {
    let o: Option[(R, R)] = Some((mk(3), mk(4)));
    match o { Some((a, b)) => { println(f"m{a.id}"); println("mid"); } None => { println("n") } }
}
fn three() {
    let o: Option[(R, R, R)] = Some((mk(5), mk(6), mk(7)));
    match o { Some((a, b, c)) => { println(f"t{b.id}") } None => { println("n") } }
}
fn res() {
    let o: Result[(R, R), i64] = Ok((mk(8), mk(9)));
    match o { Ok((a, b)) => { println(f"r{b.id}{a.id}") } Err(e) => { println("n") } }
}
fn iflet() {
    let o: Option[(R, R)] = Some((mk(10), mk(11)));
    if let Some((a, b)) = o { println(f"i{a.id}") }
}
fn wlet() {
    let mut o: Option[(R, R)] = Some((mk(12), mk(13)));
    while let Some((a, b)) = o { println(f"w{a.id}"); o = None; }
}
fn nest() {
    let o: Option[((R, R), R)] = Some(((mk(14), mk(15)), mk(16)));
    match o { Some(((a, b), c)) => { println(f"x{c.id}") } None => { println("n") } }
}
fn whole() {
    let o: Option[(R, R)] = Some((mk(17), mk(18)));
    match o { Some(p) => { println(f"p{p.0.id}") } None => { println("n") } }
}
fn st() {
    let o: Option[(R, P)] = Some((mk(19), P { r: mk(20), s: mk(21) }));
    match o { Some((a, P { r, s })) => { println(f"s{r.id}") } None => { println("n") } }
}
fn reb() {
    let mut o: Option[(R, R)] = Some((mk(22), mk(23)));
    if let Some((a, b)) = o { println(f"b{a.id}"); o = Some((mk(24), mk(25))); }
    println("kept");
}
fn prm(o: Option[(R, R)]) {
    match o { Some((a, b)) => { println(f"q{a.id}") } None => { println("n") } }
}
fn main() {
    two(); println("--");
    three(); println("--");
    res(); println("--");
    iflet(); println("--");
    wlet(); println("--");
    nest(); println("--");
    whole(); println("--");
    st(); println("--");
    reb(); println("--");
    prm(Some((mk(26), mk(27)))); println("--");
    println("end");
}
"#,
    ) else {
        return;
    };
    let got: Vec<&str> = out.lines().collect();
    assert_eq!(
        got,
        [
            "m3", "mid", "dR4", "dR3", "--", "t6", "dR7", "dR6", "dR5", "--", "r98", "dR9", "dR8",
            "--", "i10", "dR11", "dR10", "--", "w12", "dR13", "dR12", "--", "x16", "dR16", "dR15",
            "dR14", "--", "p17", "dR17", "dR18", "--", "s20", "dR21", "dR20", "dR19", "--", "b22",
            "dR23", "dR22", "dR24", "dR25", "kept", "--", "q26", "dR26", "dR27", "--", "end"
        ],
        "got:\n{out}"
    );
}
