//! B-2026-10-06-77 -- a `let` bound to an `if`/`match` choosing one of two
//! `ref` params is a re-borrow: it must take no free of its own, or the
//! caller's buffer is freed twice.

use super::*;

#[test]
fn asan_let_bound_to_branch_choosing_a_ref_param() {
    assert_clean_asan_run(
        r#"fn pick(a: ref String, b: ref String) -> i64 {
    let longer = if a.len() > b.len() { a } else { b };
    let m = match a.len() { 0 => b, _ => a };
    longer.len() * 100 + m.len()
}
fn joined(a: ref Vec[String], b: ref Vec[String]) -> String {
    let v = match b.len() > a.len() { true => b, false => a };
    let mut out = "";
    for s in v { out = out + s; }
    out + f"{v.len()}"
}
fn main() {
    let x = f"hel{1}lo";
    let y = f"h{2}";
    println(f"{pick(x, y)} {pick(y, x)}");
    let p = vec![f"p{0}", f"q{1}"];
    let r = vec![f"r{2}", f"s{3}", f"t{4}"];
    println(joined(p, r));
    println(f"{x} {y} {p.len()} {r[2]}");
}
"#,
        &["606 602", "r2s3t43", "hel1lo h2 2 t4"],
        "asan_let_bound_to_branch_choosing_a_ref_param",
    );
}
