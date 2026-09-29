//! B-2026-09-29-41 — a pattern binding that SHADOWS its own identifier
//! scrutinee (`match s { Some(S { r, s }) => r.id, … }`). Every step codegen
//! takes on the scrutinee after the arm's bindings exist found it BY NAME, and
//! so found the binding: the boxed-field suppress wrote the `Option`'s w0
//! through the arm's `String` slot (a segfault), and the neighbouring
//! spellings double-freed or lost a `Drop` body. Codegen now renames such a
//! binding before compiling (`src/codegen/scrutinee_shadow.rs`).

use super::*;

/// B-2026-09-29-41 — `match`, `if let`, `let .. else`, a guard, a whole-payload
/// rebind, a renamed field binding named after the param, a body that reads the
/// shadowing binding, a nested match, each with a fresh temp and a named
/// argument, plus a local struct scrutinee and a local `if let`. Before the fix
/// the first call segfaulted at `-O0` and `-O2`.
#[test]
fn test_pattern_binding_shadowing_its_scrutinee_keeps_one_owner() {
    let out = run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mks(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn f1(s: Option[S]) -> i64 { match s { Some(S { r, s }) => r.id, None => 0 } }
fn f2(s: Option[S]) -> i64 { if let Some(S { r, s }) = s { r.id } else { 0 } }
fn f3(s: Option[S]) -> i64 { match s { Some(s) => s.r.id, None => 0 } }
fn f4(s: Option[S]) -> i64 { match s { Some(S { r: s, s: t }) => s.id, None => 0 } }
fn f5(s: Option[S]) -> i64 { match s { Some(S { r, s }) => { println(s.len()); r.id } None => 0 } }
fn f6(s: Option[S]) -> i64 { let Some(S { r, s }) = s else { return 0 }; println(s.len()); r.id }
fn f7(s: Option[S]) -> i64 { match s { Some(S { r, s }) if s.len() > 3 => r.id, _ => 0 } }
fn f8(s: Option[S]) -> i64 { match s { Some(S { r, s }) => { let n = match r { R { id } => id }; println(s.len()); n } None => 0 } }
fn main() {
    println(f"a{f1(Some(mks(1)))}"); let x1 = Some(mks(2)); println(f"b{f1(x1)}");
    println(f"a{f2(Some(mks(3)))}"); let x2 = Some(mks(4)); println(f"b{f2(x2)}");
    println(f"a{f3(Some(mks(5)))}"); let x3 = Some(mks(6)); println(f"b{f3(x3)}");
    println(f"a{f4(Some(mks(7)))}"); let x4 = Some(mks(8)); println(f"b{f4(x4)}");
    println(f"a{f5(Some(mks(9)))}"); let x5 = Some(mks(10)); println(f"b{f5(x5)}");
    println(f"a{f6(Some(mks(11)))}"); let x6 = Some(mks(12)); println(f"b{f6(x6)}");
    println(f"a{f7(Some(mks(13)))}"); let x7 = Some(mks(14)); println(f"b{f7(x7)}");
    println(f"a{f8(Some(mks(15)))}"); let x8 = Some(mks(16)); println(f"b{f8(x8)}");
    let s = mks(17);
    let k = match s { S { r, s } => r.id };
    println(f"c{k}");
    let s = Some(mks(18));
    if let Some(s) = s { println(f"c{s.r.id}") }
    println("end")
}"#);
    assert_eq!(out, "d1\na1\nb2\nd2\nd3\na3\nb4\nd4\nd5\na5\nb6\nd6\nd7\na7\nb8\nd8\n29\nd9\na9\n30\nb10\nd10\n30\nd11\na11\n30\nb12\nd12\nd13\na13\nb14\nd14\n30\nd15\na15\n30\nb16\nd16\nd17\nc17\nc18\nd18\nend\n");
}
