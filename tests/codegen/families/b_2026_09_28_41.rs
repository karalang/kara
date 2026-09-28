//! B-2026-09-28-41 — a `Drop`-carrying value stored on only some paths in
//! the spellings B-2026-09-28-20's fix declined.

use super::*;

/// B-2026-09-28-41 cell (c) — a by-value struct with no `Drop` of its own
/// that owns a `shared` field and a field running a user body
/// (`struct Z { d: D, h: N }`), stored on only some paths
/// (`if c { xs.push(z); }`), lost `drop D` on the path that did not store it
/// on all four surfaces, and a projected argument (`zst(mut zs, q.z, ..)`)
/// double-freed compiled on both paths and ran the body twice interpreted on
/// the storing one. The callee's per-path drop now runs the fields' bodies
/// before the combined value drop, and the caller stands a projection's
/// memory and bodies down on every path.
#[test]
fn e2e_shared_field_struct_stored_on_one_path_runs_its_field_body_once() {
    let src = r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}{self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"n{n}" }; }
struct W { r: D, s: D, b: i64 }
fn mkw(n: i64) -> W { W { r: mkd(n), s: mkd(n + 100), b: n } }
shared struct N { v: i64 }
struct Z { d: D, h: N }
struct Q { z: Z, t: D }
fn zst(xs: mut ref Vec[Z], z: Z, c: bool) { if c { xs.push(z); } }
fn zown(z: Z, c: bool) -> i64 { let mut xs: Vec[Z] = Vec.new(); if c { xs.push(z); } xs.len() }
struct H { k: i64 }
impl H { fn m(ref self, xs: mut ref Vec[Z], z: Z, c: bool) { if c { xs.push(z); } } }
fn main() {
    let mut zs: Vec[Z] = Vec.new();
    zst(mut zs, Z { d: mkd(1), h: N { v: 1 } }, false); println("a");
    zst(mut zs, Z { d: mkd(2), h: N { v: 2 } }, true); println("b");
    let z3 = Z { d: mkd(3), h: N { v: 3 } }; zst(mut zs, z3, false); println("c");
    let z4 = Z { d: mkd(4), h: N { v: 4 } }; zst(mut zs, z4, true); println("d");
    let q5 = Q { z: Z { d: mkd(5), h: N { v: 5 } }, t: mkd(305) }; zst(mut zs, q5.z, false); println("e");
    let q6 = Q { z: Z { d: mkd(6), h: N { v: 6 } }, t: mkd(306) }; zst(mut zs, q6.z, true); println("f");
    let h = H { k: 0 };
    h.m(mut zs, Z { d: mkd(7), h: N { v: 7 } }, false); println("g");
    let z8 = Z { d: mkd(8), h: N { v: 8 } }; h.m(mut zs, z8, true); println("h");
    println(f"i{zown(Z { d: mkd(9), h: N { v: 9 } }, false)}");
    let z10 = Z { d: mkd(10), h: N { v: 10 } }; println(f"i{zown(z10, true)}");
    println(f"{zs.len()}");
    println("end")
}"#;
    let want = "dD1n1\na\nb\ndD3n3\nc\nd\ndD5n5\ndD305n305\ne\ndD306n306\nf\ndD7n7\ng\nh\ndD9n9\ni0\ndD10n10\ni1\n4\ndD2n2\ndD4n4\ndD6n6\ndD8n8\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
