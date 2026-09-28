//! B-2026-09-28-40, B-2026-09-28-23: a struct with no `Drop` of its own whose fields run bodies, handed back on some paths.

use super::*;

/// B-2026-09-28-40 — a struct with no `Drop` of its own whose fields run
/// user bodies, handed back WRAPPED on some paths (`if c { return Some(x); }
/// None`): a projection (`maybew(x.w, ..)`, the row's title), a named local
/// and a fresh temporary, each on both paths. The callee now adopts the
/// fields' bodies under the per-path flag and the caller stands its walk down
/// on every path; before, the handed path ran them twice and the other lost
/// them, differently per surface.
#[test]
fn interp_field_bodies_struct_handed_back_wrapped_on_some_paths() {
    let out = run(r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}{self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"n{n}" }; }
struct W { r: D, s: D, b: i64 }
fn mkw(n: i64) -> W { W { r: mkd(n), s: mkd(n + 100), b: n } }
struct X { w: W, t: D }
fn maybew(x: W, c: bool) -> Option[W] { if c { return Some(x); } None }
fn eatw(x: W) { println("ew") }
fn backw(x: W) -> W { return x }
struct Q { z: i64 }
impl Q {
    fn mw(ref self, x: W, c: bool) -> Option[W] { if c { return Some(x); } None }
    fn bw(ref self, x: W, c: bool) -> W { if c { return x; } mkw(0) }
}
struct F { d: D, n: i64 }
fn mkf(i: i64) -> F { F { d: mkd(i), n: i } }
fn pf(f: F, k: bool) -> F { if k { return f } println("n"); return mkf(0) }
fn mixw(x: W, c: bool) -> Option[W] { if c { return Some(x); } eatw(x); None }
fn main() {
    let x = X { w: mkw(1), t: mkd(300) }; let o = maybew(x.w, true); println(f"{o.is_some()}");
    let y = X { w: mkw(2), t: mkd(302) }; let p = maybew(y.w, false); println(f"{p.is_some()}");
    let a = mkw(3); let o2 = maybew(a, true); println("mid"); println(f"{o2.is_some()}");
    let b = mkw(4); let p2 = maybew(b, false); println("mid"); println(f"{p2.is_some()}");
    let o3 = maybew(mkw(5), true); println("mid"); println(f"{o3.is_some()}");
    let p3 = maybew(mkw(6), false); println("mid"); println(f"{p3.is_some()}");
    println("end")
}
"#);
    assert_eq!(out, "dD300n300\ntrue\ndD101n101\ndD1n1\ndD102n102\ndD2n2\ndD302n302\nfalse\nmid\ntrue\ndD103n103\ndD3n3\ndD104n104\ndD4n4\nmid\nfalse\nmid\ntrue\ndD105n105\ndD5n5\ndD106n106\ndD6n6\nmid\nfalse\nend\n");
}

/// B-2026-09-28-23 — the BARE hand-back (`if c { return x; } mkw(0)`) of the
/// same field-bodies struct, and the row's own `pf(f: F, k: bool) -> F` over a
/// one-`Drop`-field `F`: a named local, a fresh temporary and a projection.
#[test]
fn interp_field_bodies_struct_handed_back_bare_on_some_paths() {
    let out = run(r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}{self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"n{n}" }; }
struct W { r: D, s: D, b: i64 }
fn mkw(n: i64) -> W { W { r: mkd(n), s: mkd(n + 100), b: n } }
struct X { w: W, t: D }
fn maybew(x: W, c: bool) -> Option[W] { if c { return Some(x); } None }
fn eatw(x: W) { println("ew") }
fn backw(x: W) -> W { return x }
struct Q { z: i64 }
impl Q {
    fn mw(ref self, x: W, c: bool) -> Option[W] { if c { return Some(x); } None }
    fn bw(ref self, x: W, c: bool) -> W { if c { return x; } mkw(0) }
}
struct F { d: D, n: i64 }
fn mkf(i: i64) -> F { F { d: mkd(i), n: i } }
fn pf(f: F, k: bool) -> F { if k { return f } println("n"); return mkf(0) }
fn mixw(x: W, c: bool) -> Option[W] { if c { return Some(x); } eatw(x); None }
fn barew(x: W, c: bool) -> W { if c { return x; } mkw(0) }
fn main() {
    let a = mkw(1); let o = barew(a, true); println("mid"); let b = mkw(2); let p = barew(b, false); println("mid");
    let q = barew(mkw(3), false); println("mid"); let y = X { w: mkw(4), t: mkd(5) }; let r = barew(y.w, true); println("mid");
    let f1 = pf(mkf(6), false); println("mid"); let f2 = mkf(7); let f3 = pf(f2, false); println("mid"); let f4 = pf(mkf(8), true); println("mid");
    println("end")
}
"#);
    assert_eq!(out, "dD101n101\ndD1n1\nmid\ndD102n102\ndD2n2\ndD100n100\ndD0n0\nmid\ndD103n103\ndD3n3\ndD100n100\ndD0n0\nmid\ndD104n104\ndD4n4\ndD5n5\nmid\nn\ndD6n6\ndD0n0\nmid\nn\ndD7n7\ndD0n0\nmid\ndD8n8\nmid\nend\n");
}

/// B-2026-09-28-40 — the METHOD spelling (`q.mw(a, c)`, `q.bw(x.w, c)`), and
/// a free callee that forwards the param to a consumer on the path that does
/// not hand it back (`eatw(x); None`).
#[test]
fn interp_field_bodies_struct_handed_back_on_some_paths_by_a_method() {
    let out = run(r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}{self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"n{n}" }; }
struct W { r: D, s: D, b: i64 }
fn mkw(n: i64) -> W { W { r: mkd(n), s: mkd(n + 100), b: n } }
struct X { w: W, t: D }
fn maybew(x: W, c: bool) -> Option[W] { if c { return Some(x); } None }
fn eatw(x: W) { println("ew") }
fn backw(x: W) -> W { return x }
struct Q { z: i64 }
impl Q {
    fn mw(ref self, x: W, c: bool) -> Option[W] { if c { return Some(x); } None }
    fn bw(ref self, x: W, c: bool) -> W { if c { return x; } mkw(0) }
}
struct F { d: D, n: i64 }
fn mkf(i: i64) -> F { F { d: mkd(i), n: i } }
fn pf(f: F, k: bool) -> F { if k { return f } println("n"); return mkf(0) }
fn mixw(x: W, c: bool) -> Option[W] { if c { return Some(x); } eatw(x); None }
fn main() {
    let q = Q { z: 0 }; let a = mkw(1); let o = q.mw(a, true); println("mid"); println(f"{o.is_some()}");
    let b = mkw(2); let p = q.mw(b, false); println("mid"); let r = q.mw(mkw(3), false); println("mid");
    let x = X { w: mkw(4), t: mkd(5) }; let s = q.bw(x.w, false); println("mid");
    let o2 = mixw(mkw(6), false); println("mid"); let c = mkw(7); let p2 = mixw(c, true); println("mid"); println(f"{p2.is_some()}");
    println("end")
}
"#);
    assert_eq!(out, "mid\ntrue\ndD101n101\ndD1n1\ndD102n102\ndD2n2\nmid\ndD103n103\ndD3n3\nmid\ndD104n104\ndD4n4\ndD100n100\ndD0n0\ndD5n5\nmid\new\ndD106n106\ndD6n6\nmid\nmid\ntrue\ndD107n107\ndD7n7\nend\n");
}
