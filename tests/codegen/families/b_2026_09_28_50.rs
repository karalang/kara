//! B-2026-09-28-50 — a destructured or projected part of a by-value param
//! moved into a container on only some paths runs its `Drop` body once.

use super::*;

/// B-2026-09-28-50 — `let W { r, s, b } = w; if c { xs.push(r); }` (and
/// `let r = w.r;`) into a `mut ref` container reported `w.r` as moved on
/// EVERY path, so the caller skipped it and the path that kept it ran no body
/// on any surface; into a LOCAL container the part was adopted per path but
/// the push never cleared its flag, so the pushed path ran the body twice
/// (`dD9 ` on an emptied value compiled). A destructured name is now an alias
/// of its part on both backends, and a push under an outliving root inside a
/// branch joins the conditional query. Neighbours: the other part pushed on
/// the other arm, a `Map.insert`, a conditional `return Some(r)`, a call
/// that takes `r`, a tail push, a `..` rest pattern, a renamed field.
#[test]
fn e2e_param_part_pushed_on_one_path_runs_its_body_once() {
    let src = r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}{self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"n{n}" }; }
struct W { r: D, s: D, b: i64 }
fn mkw(n: i64) -> W { W { r: mkd(n), s: mkd(n + 100), b: n } }
fn d1(xs: mut ref Vec[D], w: W, c: bool) { let W { r, s, b } = w; if c { xs.push(r); } }
fn d2(xs: mut ref Vec[D], c: bool) { let w = mkw(2); let W { r, s, b } = w; if c { xs.push(r); } }
fn d3(xs: mut ref Vec[D], w: W, c: bool) { let r = w.r; if c { xs.push(r); } }
fn d4(xs: mut ref Vec[D], w: W, c: bool) { let W { r, s, b } = w; if c { xs.push(r); } else { println("no"); } }
fn d5(w: W, c: bool) -> i64 { let mut xs: Vec[D] = Vec.new(); let W { r, s, b } = w; if c { xs.push(r); } xs.len() }
fn keep(d: D) { println(f"kept{d.id}"); }
fn e3(xs: mut ref Vec[D], w: W, c: bool) { let W { r, s, b } = w; if c { xs.push(r); } else { xs.push(s); } }
fn e4(m: mut ref Map[i64, D], w: W, c: bool) { let W { r, s, b } = w; if c { m.insert(b, r); } }
fn e5(w: W, c: bool) -> Option[D] { let W { r, s, b } = w; if c { return Option.Some(r); } Option.None }
fn e6(w: W, c: bool) { let W { r, s, b } = w; if c { keep(r); } println("e6"); }
fn e7(xs: mut ref Vec[D], w: W, c: bool) { let W { r, s, b } = w; if c { xs.push(r) } }
fn e8(xs: mut ref Vec[D], w: W, c: bool) { let W { r, .. } = w; if c { xs.push(r); } }
fn e9(xs: mut ref Vec[D], w: W, c: bool) { let W { r: q, s, b } = w; if c { xs.push(q); } }
fn main() {
    let mut ds: Vec[D] = Vec.new();
    d1(mut ds, mkw(1), false); println("a");
    d2(mut ds, false); println("b");
    d3(mut ds, mkw(3), false); println("c");
    d4(mut ds, mkw(4), false); println("d");
    println(f"{d5(mkw(5), false)}"); println("e");
    let w6 = mkw(6); d1(mut ds, w6, false); println("f");
    d1(mut ds, mkw(7), true); println("g");
    d3(mut ds, mkw(8), true); println("h");
    println(f"{d5(mkw(9), true)}"); println("i");
    e3(mut ds, mkw(25), false); println("c1"); e3(mut ds, mkw(26), true); println("c2");
    let mut m: Map[i64, D] = Map.new();
    e4(mut m, mkw(27), false); println("d1"); e4(mut m, mkw(28), true); println("d2");
    let o9 = e5(mkw(29), false); println("f1"); let o10 = e5(mkw(30), true); println("f2");
    e6(mkw(31), false); e6(mkw(32), true);
    e7(mut ds, mkw(33), false); println("h1"); e7(mut ds, mkw(34), true); println("h2");
    e8(mut ds, mkw(35), false); println("i1"); e8(mut ds, mkw(36), true); println("i2");
    e9(mut ds, mkw(37), false); println("j1"); e9(mut ds, mkw(38), true); println("j2");
    let w39 = mkw(39); e3(mut ds, w39, true); println("k");
    println(f"{ds.len()} {m.len()}");
    match o10 { Option.Some(x) => println(f"x{x.id}"), Option.None => println("none") }
    println("end");
}"#;
    let want = "dD1n1\ndD101n101\na\ndD102n102\ndD2n2\nb\ndD3n3\ndD103n103\nc\nno\ndD4n4\ndD104n104\nd\ndD5n5\ndD105n105\n0\ne\ndD6n6\ndD106n106\nf\ndD107n107\ng\ndD108n108\nh\ndD9n9\ndD109n109\n1\ni\ndD25n25\nc1\ndD126n126\nc2\ndD27n27\ndD127n127\nd1\ndD128n128\nd2\ndD129n129\nf1\ndD130n130\nf2\ne6\ndD131n131\ndD31n31\nkept32\ne6\ndD132n132\ndD32n32\ndD33n33\ndD133n133\nh1\ndD134n134\nh2\ndD35n35\ndD135n135\ni1\ndD136n136\ni2\ndD37n37\ndD137n137\nj1\ndD138n138\nj2\ndD139n139\nk\n8 1\ndD28n28\ndD7n7\ndD8n8\ndD125n125\ndD26n26\ndD34n34\ndD36n36\ndD38n38\ndD39n39\nx30\ndD30n30\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
