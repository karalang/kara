//! B-2026-09-26-18 -- reassigning a `String` or `Vec` binding from an `if`,
//! `match` or block expression frees the displaced value.

use super::*;

/// B-2026-09-26-18 — the reassignment's eager free of the old buffer keyed on
/// the RHS shape and had no term for a branch or block whose tails build a
/// fresh f-string or literal, move another binding, or hand back a binding
/// the block declares. Output was always right; the old buffer leaked once
/// per assignment. This pins the output of every spelling the fix admits,
/// including the `{ let t = s; t }` hand-back it must decline.
#[test]
fn e2e_string_reassigned_from_branch_keeps_its_value() {
    let out = run_program(
        r#"fn mk(n: i64) -> String { f"m{n}" }
fn main() {
    let mut s = f"a{1}";
    let c = s.len() > 0;
    s = if c { f"q{2}" } else { f"z{3}" };
    println(s);
    s = { f"b{4}" };
    println(s);
    s = match s.len() { 0 => f"x{5}", _ => f"y{6}" };
    println(s);
    s = if c { "lit" } else { mk(7) };
    println(s);
    let u = f"u{8}";
    s = if not c { mk(9) } else { u };
    println(s);
    let mut i = 0;
    while i < 3 { let w = f"w{i}"; s = match i { 1 => w, _ => f"k{i}" }; i = i + 1; }
    println(s);
    s = { let t = s; t };
    println(s);
    s = if c { let w = f"v{10}"; w } else { f"e{11}" };
    println(s);
    let mut v: Vec[String] = Vec.new();
    v.push(f"p{12}");
    v = { let mut z: Vec[String] = Vec.new(); z.push(f"zz{13}"); z };
    println(v[0]);
    let mut v2: Vec[String] = Vec.new();
    v2.push(f"r{14}");
    v = if not c { Vec.new() } else { v2 };
    println(f"{v[0]} {v.len()}")
}
"#,
    );
    assert_eq!(
        out,
        Some("q2\nb4\ny6\nlit\nu8\nk2\nk2\nv10\nzz13\nr14 1\n".to_string()),
        "a branch or block reassignment must store the new value"
    );
}
