//! B-2026-09-30-73 — an irrefutable `if let x = g` / `let x = g else` over a
//! named struct local bound `x` as a second owner beside `g`: compiled, the
//! struct was freed twice (`free(): double free`); `--interp` ran `Drop` bodies
//! twice for a binding moved on inside the construct. Both backends now treat
//! the construct as `{ let x = g; .. }` / `let x = g`, as `match g { x => .. }`
//! has since B-2026-09-30-46. Borrowed (`ref`) params keep the view path.

use super::*;

/// B-2026-09-30-73 — `if let x = g` and `let x = g else` over a named struct local or by-value param free it once.
#[test]
fn asan_if_let_whole_named_struct_binding_moves_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct Rs { id: i64, s: String }
impl Drop for Rs { fn drop(mut ref self) { println(f"ds{self.id} {self.s.len()}") } }
struct H { v: Rs, n: i64 }
struct P { s: String, n: i64 }
fn mk(k: i64) -> String { f"heap-string-long-enough-for-heap-{k}" }
fn hh(id: i64, n: i64) -> H { H { v: Rs { id: id, s: mk(id) }, n: n } }
fn byval(h: H) { if let x = h { println(x.n) } }
fn byref(h: ref H) { if let x = h { println(x.n) } }
fn tailv(g: H) -> i64 { if let x = g { x.n } else { 0 } }
fn lebyval(h: H) { let x = h else { return }; println(x.n) }
fn c1() { let g = hh(4, 1); if let x = g { println(x.n) } }
fn c2() { let g = P { s: mk(5), n: 2 }; if let x = g { println(x.n) } }
fn c3() { let g = P { s: mk(6), n: 3 }; if let x = g { println(x.s.len()) }; println(7) }
fn c4() { let g = hh(8, 1); if let x = g { let y = x; println(y.n) } }
fn c5() { let g = P { s: mk(9), n: 3 }; if let x = g { println(x.n) } else { println(0) } }
fn c6() { let mut v: Vec[P] = Vec.new(); let g = P { s: mk(10), n: 4 }; if let x = g { v.push(x) }; println(v.len()) }
fn c7() { let g = hh(11, 1); let x = g else { return }; println(x.n) }
fn c8() { let g = hh(14, 1); byval(g) }
fn c9() { let g = hh(15, 1); byref(g); println(g.n) }
fn c10() { let g = hh(16, 6); println(tailv(g)) }
fn c11() { let g = hh(18, 8); lebyval(g) }
fn c12() { let x = 99; let g = hh(19, 9); if let x = g { println(x.n) }; println(x) }
fn c13() { let g = hh(20, 1); let n = if let x = g { x.n + 1 } else { 0 }; println(n) }
fn c14() { let mut g = hh(21, 1); g.n = 2; if let x = g { println(x.n) } }
fn c15() { for i in 0..2 { let g = hh(22, i); if let x = g { println(x.n) } } }
fn c16() { let g = hh(23, 1); let x = g else { return }; let y = x; println(y.n) }
fn main() {
    c1();
    c2();
    c3();
    c4();
    c5();
    c6();
    c7();
    c8();
    c9();
    c10();
    c11();
    c12();
    c13();
    c14();
    c15();
    c16();
    println("end")
}"#,
        &[
            "1", "ds4 34", "2", "34", "7", "1", "ds8 34", "3", "1", "1", "ds11 35", "1", "ds14 35",
            "1", "1", "ds15 35", "6", "ds16 35", "8", "ds18 35", "9", "ds19 35", "99", "ds20 35",
            "2", "2", "ds21 35", "0", "ds22 35", "1", "ds22 35", "1", "ds23 35", "end",
        ],
        "asan_if_let_whole_named_struct_binding_moves_once",
        20,
    );
}
