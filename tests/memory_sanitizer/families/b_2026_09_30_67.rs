//! B-2026-09-30-67: params handed back inside an Array, Vec or tuple run one body each.

use super::*;

/// B-2026-09-30-67 — by-value params handed to a callee that returns them
/// inside an `Array`, a `Vec` or a tuple run each `Drop` body once, in the
/// caller. The array and `Vec` let sites never asked whether a call result was
/// a view of the caller's params, so the binding's walk ran every element body
/// and the caller ran them again; the discarded spellings (`arrc(a, b);`,
/// `let _ = ..`) doubled on every surface; and a MIXED result (`tupc(a,
/// mk(51))`) was marked a view of `a` wholesale, so the fresh element's body
/// ran nowhere. Covers bound, generic, associated, read, rebound, discarded,
/// wildcard and mixed spellings, and a projection out of a handed-back tuple.
#[test]
fn asan_params_handed_back_inside_a_container_run_one_body_each() {
    assert_clean_asan_run(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
struct H { k: i64 }
impl H { fn two(a: R, b: R) -> Array[R, 2] { return [a, b]; } }
fn arrc(a: R, b: R) -> Array[R, 2] { return [a, b]; }
fn arrg[T](a: T, b: T) -> Array[T, 2] { return [a, b]; }
fn arr1(a: R) -> Array[R, 1] { return [a]; }
fn vc(a: R, b: R) -> Vec[R] { return vec![a, b]; }
fn tupc(a: R, b: R) -> (R, R) { return (a, b); }
fn tupk(a: R) -> (R, i64) { return (a, 7); }
fn oc(a: R, b: R) -> i64 { let e = arrc(a, b); return 1; }
fn ov(a: R, b: R) -> i64 { let e = vc(a, b); return 1; }
fn o1(a: R) -> i64 { let e = arr1(a); return 1; }
fn og(a: R, b: R) -> i64 { let e = arrg(a, b); return 1; }
fn oh(a: R, b: R) -> i64 { let e = H.two(a, b); return 1; }
fn orr(a: R, b: R) -> i64 { let e = arrc(a, b); println(f"i{e[0].id}"); return 1; }
fn ob(a: R, b: R) -> i64 { let e = arrc(a, b); let f = e; return 1; }
fn dc(a: R, b: R) -> i64 { arrc(a, b); return 1; }
fn dv(a: R, b: R) -> i64 { vc(a, b); return 1; }
fn dt(a: R, b: R) -> i64 { tupc(a, b); return 1; }
fn dg(a: R, b: R) -> i64 { arrg(a, b); return 1; }
fn wc(a: R, b: R) -> i64 { let _ = arrc(a, b); return 1; }
fn wv(a: R, b: R) -> i64 { let _ = vc(a, b); return 1; }
fn wt(a: R, b: R) -> i64 { let _ = tupc(a, b); return 1; }
fn mt(a: R) -> i64 { let e = tupc(a, mk(51)); return 1; }
fn md(a: R) -> i64 { tupc(a, mk(52)); return 1; }
fn mw(a: R) -> i64 { let _ = tupc(a, mk(53)); return 1; }
fn pk(a: R) -> i64 { let t = tupk(a); return t.1; }
fn main() {
  println("oc"); oc(mk(1), mk(2));
  println("ov"); ov(mk(3), mk(4));
  println("o1"); o1(mk(5));
  println("og"); og(mk(6), mk(7));
  println("oh"); oh(mk(8), mk(9));
  println("or"); orr(mk(10), mk(11));
  println("ob"); ob(mk(12), mk(13));
  println("dc"); dc(mk(16), mk(17));
  println("dv"); dv(mk(18), mk(19));
  println("dt"); dt(mk(20), mk(21));
  println("dg"); dg(mk(22), mk(23));
  println("wc"); wc(mk(24), mk(25));
  println("wv"); wv(mk(26), mk(27));
  println("wt"); wt(mk(28), mk(29));
  println("mt"); mt(mk(30));
  println("md"); md(mk(31));
  println("mw"); mw(mk(32));
  println("pk"); let k = pk(mk(33)); println(f"k{k}");
  println("end")
}
"#,
        &[
            "oc", "dR2", "dR1", "ov", "dR4", "dR3", "o1", "dR5", "og", "dR7", "dR6", "oh", "dR9",
            "dR8", "or", "i10", "dR11", "dR10", "ob", "dR13", "dR12", "dc", "dR17", "dR16", "dv",
            "dR19", "dR18", "dt", "dR21", "dR20", "dg", "dR23", "dR22", "wc", "dR25", "dR24", "wv",
            "dR27", "dR26", "wt", "dR29", "dR28", "mt", "dR51", "dR30", "md", "dR52", "dR31", "mw",
            "dR53", "dR32", "pk", "dR33", "k7", "end",
        ],
        "asan_params_handed_back_inside_a_container_run_one_body_each",
    );
}
