//! B-2026-09-26-18 -- reassigning a `String` or `Vec` binding from an `if`,
//! `match` or block expression frees the displaced value exactly once.

use super::*;

/// B-2026-09-26-18 — before, each of these reassignments leaked the old
/// buffer (valgrind at `-O0`: 1 block per assignment, 96 B for the `Vec`
/// cells). The `{ let t = s; t }` cell hands back the displaced buffer
/// itself and must NOT free it, so it guards the other direction.
#[test]
fn asan_string_reassigned_from_branch_frees_old_value() {
    assert_clean_asan_run(
        r#"fn mk(n: i64) -> String { f"m{n}" }
fn main() {
    let mut s = f"a{1}";
    let c = s.len() > 0;
    s = if c { f"q{2}" } else { f"z{3}" };
    println(s)
    s = { f"b{4}" };
    println(s)
    s = match s.len() { 0 => f"x{5}", _ => f"y{6}" };
    println(s)
    s = if c { "lit" } else { mk(7) };
    println(s)
    let u = f"u{8}";
    s = if not c { mk(9) } else { u };
    println(s)
    let mut i = 0;
    while i < 3 { let w = f"w{i}"; s = match i { 1 => w, _ => f"k{i}" }; i = i + 1; }
    println(s)
    s = { let t = s; t };
    println(s)
    s = if c { let w = f"v{10}"; w } else { f"e{11}" };
    println(s)
    let mut v: Vec[String] = Vec.new();
    v.push(f"p{12}");
    v = { let mut z: Vec[String] = Vec.new(); z.push(f"zz{13}"); z };
    println(v[0])
    let mut v2: Vec[String] = Vec.new();
    v2.push(f"r{14}");
    v = if not c { Vec.new() } else { v2 };
    println(f"{v[0]} {v.len()}")
}
"#,
        &[
            "q2", "b4", "y6", "lit", "u8", "k2", "k2", "v10", "zz13", "r14 1",
        ],
        "B-2026-09-26-18 branch/block reassignment frees the old buffer",
    );
}
