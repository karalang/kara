//! B-2026-09-30-80 -- a read method on a literal, block or branch receiver frees
//! the receiver once, and a let-bound borrow of its element is not freed again.

use super::*;

/// Each fresh receiver is freed once at the end of the frame; an outer binding
/// handed back through a block or branch is freed by its own scope only; and a
/// let-bound `Option[ref String]` from a temp does not free the element.
#[test]
fn asan_read_method_on_literal_block_or_branch_receiver() {
    assert_clean_asan_run(
        r#"struct P { n: i64, s: String }
fn mk(n: i64) -> Vec[i64] { return [n, n + 1] }
fn ms(n: i64) -> Vec[String] { return [f"s{n}", f"t{n}"] }
fn mp(n: i64) -> Vec[P] { return [P { n: n, s: f"p{n}" }] }
fn main() {
    let c = true;
    let mut k = 0;
    while k < 2 {
        let a = [1, 3].contains(3);
        let b = { mk(k) }.contains(k + 1);
        let d = (if c { mk(5) } else { mk(7) }).first();
        let e = (match c { true => ms(k), false => ms(9) }).contains(f"t{k}");
        match d { Some(x) => println(f"a:{a} {b} {x} {e}"), None => println("none") }
        let f = { ms(k) }.get(1);
        match f { Some(x) => println(f"f:{x}"), None => println("none") }
        let g = Vec[f"x{k}", f"y{k}"].last();
        match g { Some(x) => println(f"g:{x}"), None => println("none") }
        match { mp(k) }.first() { Some(p) => println(f"p:{p.n} {p.s}"), None => println("none") }
        let v = [[1, 2], [3, k]].last();
        match v { Some(w) => println(f"v:{w.len()} {w[1]}"), None => println("none") }
        let n = { mk(k) }.get(5);
        match n { Some(x) => println(f"n:{x}"), None => println("n:none") }
        let m = ms(k).last();
        match m { Some(x) => println(f"m:{x}"), None => println("none") }
        k += 1;
    }
    let outer = ms(4);
    let h = { outer }.contains(f"s4");
    let o2 = ms(6);
    let r = (if c { o2 } else { ms(1) }).last();
    match r { Some(x) => println(f"h:{h} {x}"), None => println("none") }
}
"#,
        &[
            "a:true true 5 true",
            "f:t0",
            "g:y0",
            "p:0 p0",
            "v:2 0",
            "n:none",
            "m:t0",
            "a:true true 5 true",
            "f:t1",
            "g:y1",
            "p:1 p1",
            "v:2 1",
            "n:none",
            "m:t1",
            "h:true t6",
        ],
        "read_method_on_literal_block_or_branch_receiver",
    );
}
