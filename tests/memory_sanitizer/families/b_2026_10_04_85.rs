//! B-2026-10-04-85 and B-2026-10-04-86 — storing into a `Vec` / `Array`
//! element whose type is a user enum, `Option` or `Result`.

use super::*;

/// `v[0] = n` over a named user-enum local double-freed its payload
/// compiled (-85); over a named `Option` it segfaulted compiled, and on every
/// surface the displaced `Option` / `Result` element's body never ran and its
/// box leaked (-86). An `Array[Option[R], N]` / `Array[Option[String], N]`
/// local freed no element payload at all. Covers `Vec` and `Array`, named and
/// fresh sources, `Result`, a store on one branch (both paths), and an
/// `Array[Option[R], 2]` passed by value and moved.
#[test]
fn asan_store_into_optres_or_enum_element() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
enum E { A(R), B }
fn eat(a: Array[Option[R], 2]) -> i64 { 1 }
fn vec_enum() { let mut v: Vec[E] = Vec.new(); v.push(E.A(mk(1))); let n = E.A(mk(2)); v[0] = n; println("a") }
fn arr_enum() { let mut v: Array[E, 1] = [E.A(mk(3))]; let n = E.A(mk(4)); v[0] = n; println("b") }
fn vec_opt_named() { let mut v: Vec[Option[R]] = Vec.new(); v.push(Some(mk(5))); let n = Some(mk(6)); v[0] = n; println("c") }
fn arr_opt_named() { let mut v: Array[Option[R], 1] = [Some(mk(7))]; let n = Some(mk(8)); v[0] = n; println("d") }
fn vec_opt_fresh() { let mut v: Vec[Option[R]] = Vec.new(); v.push(Some(mk(9))); v.push(None); v[1] = Some(mk(10)); v[0] = None; println("e") }
fn vec_res() { let mut v: Vec[Result[R, String]] = Vec.new(); v.push(Ok(mk(11))); v.push(Err(f"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee{3}")); v[0] = Err(f"ffffffffffffffffffffffffffffffff{4}"); v[1] = Ok(mk(12)); println("f") }
fn nested_store(c: bool) { let mut v: Vec[Option[R]] = Vec.new(); v.push(Some(mk(13))); if c { v[0] = Some(mk(14)); } println("g") }
fn arr_opt_local() { let a: Array[Option[R], 2] = [Some(mk(15)), None]; println(f"k{eat(a)}"); let b: Array[Option[R], 2] = [Some(mk(16)), Some(mk(17))]; let c = b; println("h") }
fn arr_opt_string() { let a: Array[Option[String], 1] = [Some(f"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx{1}")]; println("i") }
fn arr_res_store() { let mut d: Array[Result[R, String], 2] = [Ok(mk(18)), Err(f"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee{1}")]; d[1] = Ok(mk(19)); println("j") }
fn main() {
    vec_enum(); arr_enum(); vec_opt_named(); arr_opt_named(); vec_opt_fresh(); vec_res();
    nested_store(true); nested_store(false); arr_opt_local(); arr_opt_string(); arr_res_store();
    println("end");
}
"#,
        &[
            "dR1", "dR2", "a", "dR3", "dR4", "b", "dR5", "dR6", "c", "dR7", "dR8", "d", "dR9",
            "dR10", "e", "dR11", "dR12", "f", "dR13", "dR14", "g", "dR13", "g", "k1", "dR15",
            "dR16", "dR17", "h", "i", "dR18", "dR19", "j", "end",
        ],
        "store_into_optres_or_enum_element",
    );
}
