//! B-2026-10-01-10 -- `let _ = x;` runs `x`'s `Drop` body at the statement
//! when the frame that owns `x` is an enclosing one, or when the block did
//! not declare `x` (an `if let` binding).

use super::*;

/// B-2026-10-01-10 — `match r { Ok(w) => { let _ = w; println("arm") } .. }`
/// over an inline `Result[W1, i64]` printed `arm dW1_8` compiled against
/// `dW1_8 arm` under `--interp`. NLL fires a discarded binding at the
/// statement only when the block declares it and the innermost frame owns
/// it; every other spelling waited for a later statement's end. Covered: a
/// local discarded inside an `if` (both paths), a bare block, a loop body, a
/// return path, a holder struct, a user enum, a `Vec`, an arm binding, an
/// arm binding discarded one block deeper, and an `if let` binding.
#[test]
fn e2e_discard_in_nested_block_runs_body_at_statement() {
    let src = r#"struct W1 { v: i64, s: String }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct H { w: W1, n: i64 }
enum E { A(W1), B(i64) }
fn mk(n: i64) -> W1 { return W1 { v: n, s: f"ssssssssssssssssssssssssssssss{n}" } }
fn in_if(c: bool) { let a = mk(1); if c { let _ = a; println("x") }; println("y") }
fn in_block() { let a = mk(2); { let _ = a; println("bx") }; println("by") }
fn in_loop() { for i in 0..2 { let a = mk(10 + i); if i == 1 { let _ = a; println("li") }; println("le") } }
fn holder(c: bool) { let h = H { w: mk(3), n: 1 }; if c { let _ = h; println("hs") }; println("hs2") }
fn enum_local(c: bool) { let e = E.A(mk(4)); if c { let _ = e; println("en") }; println("en2") }
fn vec_local(c: bool) { let v = [mk(5), mk(6)]; if c { let _ = v; println("ve") }; println("ve2") }
fn early(c: bool) -> i64 { let a = mk(7); if c { let _ = a; return 1 }; println("r2"); return 0 }
fn main() {
    in_if(true);
    in_if(false);
    in_block();
    in_loop();
    holder(true);
    holder(false);
    enum_local(true);
    enum_local(false);
    vec_local(true);
    vec_local(false);
    println(f"{early(true)}");
    println(f"{early(false)}");
    let o: Result[W1, i64] = Ok(mk(8));
    match o { Ok(w) => { let _ = w; println("arm") }, Err(_) => {} };
    let o2: Result[W1, i64] = Ok(mk(9));
    match o2 { Ok(w) => { if true { let _ = w; println("in") }; println("arm2") }, Err(_) => {} };
    let o3: Result[W1, i64] = Ok(mk(12));
    if let Ok(w) = o3 { let _ = w; println("il") };
    println("end")
}
"#;
    let want = "dW1_1\nx\ny\ndW1_1\ny\ndW1_2\nbx\nby\ndW1_10\nle\ndW1_11\nli\nle\ndW1_3\nhs\nhs2\ndW1_3\nhs2\ndW1_4\nen\nen2\ndW1_4\nen2\ndW1_5\ndW1_6\nve\nve2\ndW1_5\ndW1_6\nve2\ndW1_7\n1\ndW1_7\nr2\n0\ndW1_8\narm\ndW1_9\nin\narm2\ndW1_12\nil\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
