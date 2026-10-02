//! B-2026-10-02-35 — `Vec.swap` is inlined: two loads and two stores of the
//! element type, instead of a call into the runtime that LLVM could not inline.

use super::*;

/// B-2026-10-02-35 — the inline swap still relocates every element type exactly:
/// `i64` through a `mut ref` param, `i == j`, `String`, a heap tuple, a `Drop`
/// struct (bodies run once, at the binding's death, never at the swap) and a
/// nested `Vec`. Output matches `--interp`.
#[test]
fn e2e_vec_swap_inline_relocates_every_elem_type() {
    let src = r#"struct Item { id: i64, tag: String }
impl Drop for Item { fn drop(mut ref self) { println(f"D{self.id}") } }
fn mk(i: i64) -> Item { return Item { id: i, tag: f"tag_{i}_long_enough_to_live_on_the_heap" } }
fn swap_in(a: mut ref Vec[i64], i: i64, j: i64) { a.swap(i, j); }
fn main() {
    let mut v: Vec[i64] = [1, 2, 3, 4, 5];
    swap_in(mut v, 0, 4);
    v.swap(1, 1);
    v.swap(3, 2);
    println(f"{v[0]}{v[1]}{v[2]}{v[3]}{v[4]}");
    let mut s: Vec[String] = ["alpha_heap_string_xxxxxxxxxxxxxxx", "b"];
    s.swap(0, 1);
    println(f"{s[0]} {s[1]}");
    let mut t: Vec[(i64, String)] = [(1, "one_heap_string_xxxxxxxxxxxxxxxxxx"), (2, "two")];
    t.swap(1, 0);
    println(f"{t[0].0}{t[0].1} {t[1].0}{t[1].1}");
    let mut w: Vec[Item] = Vec.new();
    w.push(mk(1));
    w.push(mk(2));
    w.push(mk(3));
    w.swap(0, 2);
    w.swap(1, 1);
    println(f"{w[0].id}{w[1].id}{w[2].id} {w[0].tag}");
    let mut n: Vec[Vec[i64]] = [[1], [2, 3]];
    n.swap(0, 1);
    println(f"{n[0].len()} {n[1][0]}");
    println("end");
}
"#;
    let want = "52431\nb alpha_heap_string_xxxxxxxxxxxxxxx\n2two 1one_heap_string_xxxxxxxxxxxxxxxxxx\n321 tag_3_long_enough_to_live_on_the_heap\nD3\nD2\nD1\n2 1\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// B-2026-10-02-35 — the reason for the row: no swap may lower to a call. Kata
/// 324's select arm ran 1.57x slower than its C and Rust mirrors because each
/// `swap` in its partition loop called `karac_vec_swap` across the runtime
/// archive, where LLVM could not inline it; written out by hand the same
/// program tied C. A `mut ref` param receiver and a local are both checked,
/// since they reach the swap through different pointer loads.
#[test]
fn ir_vec_swap_emits_no_runtime_call() {
    let src = r#"
fn swap_in(a: mut ref Vec[i64], i: i64, j: i64) { a.swap(i, j); }
fn main() {
    let mut v: Vec[i64] = [1, 2, 3];
    swap_in(mut v, 0, 2);
    let mut s: Vec[String] = ["a", "b"];
    s.swap(0, 1);
    println(f"{v[0]} {s[0]}");
}
"#;
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let ir = karac::codegen::compile_to_ir(&parsed.program, None, None).expect("codegen failed");
    assert!(
        ir.contains("swap.iv"),
        "the inline swap's loads are missing, so this program never reached it"
    );
    let calls: Vec<&str> = ir
        .lines()
        .filter(|l| l.contains("call") && l.contains("@karac_vec_swap"))
        .collect();
    assert!(
        calls.is_empty(),
        "Vec.swap lowered to a runtime call: {calls:?}"
    );
}
