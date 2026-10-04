//! B-2026-10-04-36 -- a method on an element of an `Array` held in a tuple
//! element (`t.0[i].len()`) failed the build with "indexed-receiver method
//! ... requires the indexed container to be a named variable"; the `Vec`
//! spelling was fixed by B-2026-07-20-4.

use super::*;

/// Read and mutating methods on `Vec`, `String` and user-struct elements of
/// a tuple-held `Array`, at a literal and a loop index. Output matches the
/// interpreter.
#[test]
fn e2e_method_on_element_of_array_held_in_tuple() {
    let src = r#"struct P { n: i64, s: String }
impl P { fn get(ref self) -> i64 { self.n } fn bump(mut ref self) { self.n += 1; } }
fn main() {
    let mut f: (Array[Vec[i64], 2], i64) = ([[1], [2, 3]], 0);
    f.0[0].push(4);
    println(f"a:{f.0[0].len()} {f.0[1].len()} {f.0[1].contains(3)}");
    let g: (i64, Array[String, 2]) = (0, [f"ab", f"cde"]);
    let c = g.1[0].clone();
    println(f"b:{g.1[1].len()} {c} {g.1[1].to_uppercase()}");
    let mut p: (Array[P, 2], i64) = ([P { n: 1, s: f"x" }, P { n: 2, s: f"y" }], 0);
    p.0[1].bump();
    println(f"c:{p.0[0].get()} {p.0[1].get()}");
    let mut i = 0;
    let mut tot = 0;
    while i < 2 { tot += f.0[i].len(); i += 1; }
    println(f"d:{tot}");
}
"#;
    let want = "a:2 2 true\nb:3 ab CDE\nc:1 3\nd:4\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
