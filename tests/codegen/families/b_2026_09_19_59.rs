//! B-2026-09-19-59 -- an `Array` payload laid INLINE in `Option`/`Result`'s
//! payload area (`Array[S, 1]` over a one-`String` `S`) has one owner, and its
//! elements' `Drop` bodies run once, before the memory they read.

use super::*;

/// B-2026-09-19-59 — before the fix the named-source cells aborted with
/// `free(): double free detected in tcache 2` (the source kept its element
/// drop beside the envelope's), and the arm-bound cells that did not abort
/// printed their body over freed memory (`dd` + garbage).
#[test]
fn e2e_inline_array_payload_in_option_result_has_one_owner() {
    let Some(out) = run_program(
        r#"struct S { tag: String }
impl Drop for S { fn drop(mut ref self) { println(f"d{self.tag}") } }
fn mk(t: String) -> S { return S { tag: t } }
fn mko() -> Option[Array[S, 1]] { let a: Array[S, 1] = [mk(f"gggggggg7")]; return Option.Some(a) }
fn mkr() -> Result[Array[S, 1], i64] { let a: Array[S, 1] = [mk(f"hhhhhhhh8")]; return Result.Ok(a) }
fn main() {
    { let a: Array[S, 1] = [mk(f"aaaaaaaa1")]; match Option.Some(a) { Option.Some(v) => { println(f"m{v[0].tag}") }, Option.None => {} }; println("c1") }
    { let a: Array[S, 1] = [mk(f"bbbbbbbb2")]; let o = Option.Some(a); match o { Option.Some(v) => { println("l") }, Option.None => {} }; println("c2") }
    { let a: Array[S, 1] = [mk(f"cccccccc3")]; let o = Option.Some(a); if let Option.Some(v) = o { println("i") }; println("c3") }
    { let a: Array[S, 1] = [mk(f"dddddddd4")]; let mut w: Vec[Option[Array[S, 1]]] = Vec.new(); w.push(Option.Some(a)); println(f"p{w.len()}") }
    { let a: Array[String, 1] = [f"eeeeeeee5"]; match Option.Some(a) { Option.Some(v) => { println(f"s{v[0]}") }, Option.None => {} }; println("c5") }
    { let a: Array[S, 1] = [mk(f"ffffffff6")]; let o: Result[Array[S, 1], i64] = Result.Ok(a); match o { Result.Ok(v) => { println("r") }, Result.Err(e) => {} }; println("c6") }
    { let o = mko(); match o { Option.Some(v) => { println("g") }, Option.None => {} }; println("c7") }
    { match mkr() { Result.Ok(v) => { println("k") }, Result.Err(e) => {} }; println("c8") }
    { let a: Array[S, 1] = [mk(f"iiiiiiii9")]; let o: Result[i64, Array[S, 1]] = Result.Err(a); match o { Result.Ok(n) => {}, Result.Err(v) => { println("e") } }; println("c9") }
    { let a: Array[S, 1] = [mk(f"jjjjjjjj0")]; let o = Option.Some(a); let p = o; match p { Option.Some(v) => { println("q") }, Option.None => {} }; println("c10") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "maaaaaaaa1\ndaaaaaaaa1\nc1\nl\ndbbbbbbbb2\nc2\ni\ndcccccccc3\nc3\np1\nddddddddd4\nseeeeeeee5\nc5\nr\ndffffffff6\nc6\ng\ndgggggggg7\nc7\nk\ndhhhhhhhh8\nc8\ne\ndiiiiiiii9\nc9\nq\ndjjjjjjjj0\nc10\nend\n", "got:\n{out}");
}
