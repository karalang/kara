//! B-2026-09-28-62 -- a `shared enum` struct-variant constructor passed
//! straight to a parameter is released once.

use super::*;

/// B-2026-09-28-62 — a `shared enum` STRUCT-variant constructor handed straight
/// to a parameter: by value (read, or payload moved on), `ref`, a plain enum,
/// a generic one at `Vec[String]`, and one whose enum and payload both have a
/// `Drop` body.
///
/// Before: the whole RC object was never released compiled (80 B per two
/// calls plain, 112 B generic) and neither body ran, while the tuple-variant
/// spelling and `--interp` were right.
#[test]
fn test_shared_enum_struct_variant_arg_is_released() {
    let out = run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
shared enum H { Y { v: Vec[String] }, N }
shared enum Hg[T] { Y { v: T }, N }
shared enum Hd { Y { r: R }, N }
impl Drop for Hd { fn drop(mut ref self) { println("dHd") } }
fn mkv(n: i64) -> Vec[String] { [f"aaaaaaaa{n}", f"bbbbbbbb{n}"] }
fn eatv(v: Vec[String]) -> i64 { v.len() }
fn rd(h: H) -> i64 { match h { H.Y { v } => { v.len() } H.N => { 0 } } }
fn ho(h: H) -> i64 { match h { H.Y { v } => { eatv(v) } H.N => { 0 } } }
fn rdg(h: Hg[Vec[String]]) -> i64 { match h { Hg.Y { v } => { v.len() } Hg.N => { 0 } } }
fn hog(h: Hg[Vec[String]]) -> i64 { match h { Hg.Y { v } => { eatv(v) } Hg.N => { 0 } } }
fn tk(h: Hd) -> i64 { 1 }
fn rf(h: ref Hd) -> i64 { match h { Hd.Y { r } => { r.id } Hd.N => { 0 } } }
fn main() {
    let mut i = 0; let mut t = 0;
    while i < 2 { t = t + rd(H.Y { v: mkv(i) }) + ho(H.Y { v: mkv(i) }); i = i + 1; }
    println(f"plain {t}");
    i = 0; t = 0;
    while i < 2 { t = t + rdg(Hg.Y { v: mkv(i) }) + hog(Hg.Y { v: mkv(i) }); i = i + 1; }
    println(f"generic {t}");
    println("byval"); let a = tk(Hd.Y { r: R { id: 1 } }); println(f"  {a}");
    println("ref"); let b = rf(Hd.Y { r: R { id: 2 } }); println(f"  {b}");
    println("end")
}"#);
    assert_eq!(
        out, "plain 8\ngeneric 8\nbyval\ndHd\ndR1\n  1\nref\ndHd\ndR2\n  2\nend\n",
        "got:\n{out}"
    );
}
