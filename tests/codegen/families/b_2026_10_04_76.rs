//! B-2026-10-04-76 — an index read of a non-`Copy` element as a block tail
//! or a `shared enum` arm tail handed the element out as an alias. A block's
//! own `let` (or the arm's pattern binding) owns the container, so the frame
//! drain freed the element the consumer then read and freed again. The tail
//! is now deep-copied before that drain, as the function and `if` tails
//! already were.

use super::*;

/// B-2026-10-04-76 — block tails over a block-local, outer and nested
/// container, an `if` arm block, a by-value call argument, `shared` /
/// nested-`shared` / value / `Option` arm tails (bare and block), `if let`,
/// `Vec[Vec[String]]` and `Vec[struct]` elements, a mixed arm, read-only
/// `println` / `.len()` consumers, a discarded block, and a loop.
#[test]
fn e2e_index_element_block_and_arm_tail_is_copied() {
    let src = r#"
fn mkv(s: String) -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push(s.clone() + "-a");
    v.push(s + "-b");
    v
}
shared enum M { My(Vec[String]), N }
enum E { A(Vec[String]), B }
shared enum H { Z(M), W }
struct R { n: String }
fn take(s: String) -> i64 { s.len() }
fn c1() -> String { let m = mkv("c1"); { let x = m; x[1] } }
fn c2() -> String { let m = mkv("c2"); let s = { m[1] }; s }
fn c3() -> String { let m = mkv("c3"); if true { let x = m; x[1] } else { "n" } }
fn c4() -> i64 { let m = mkv("c4"); take({ let x = m; x[1] }) }
fn c5() -> String { let m = mkv("c5"); let y = 1; { let k = 0; { let x = m; x[y + k] } } }
fn c6() -> String { let t = M.My(mkv("c6")); match t { M.My(x) => x[1], _ => "n" } }
fn c7() -> String { let t = M.My(mkv("c7")); match t { M.My(x) => { let y = x; y[1] }, _ => "n" } }
fn c8() -> String { let t = H.Z(M.My(mkv("c8"))); match t { H.Z(M.My(x)) => x[1], _ => "n" } }
fn c9() -> String { let t = M.My(mkv("c9")); if let M.My(x) = t { x[1] } else { "n" } }
fn c10() -> String { let t = E.A(mkv("c10")); match t { E.A(x) => x[1], _ => "n" } }
fn c11() -> String { let t = Some(mkv("c11")); match t { Some(x) => x[1], None => "n" } }
fn c12() -> String { let m: Vec[Vec[String]] = [mkv("c12a"), mkv("c12b")]; { let x = m; x[1][0] } }
fn c13() -> String { let m: Vec[R] = [R { n: "c13".to_string() + "x" }, R { n: "c13".to_string() + "y" }]; let r = { let x = m; x[1] }; r.n }
fn c14() -> String { let t = M.My(mkv("c14")); match t { M.My(x) => x[1], M.N => "c14".to_string() + "n" } }
fn main() {
    let s1 = { let mm = mkv("s1"); { let x = mm; x[1] } };
    println(s1);
    println(c1()); println(c2()); println(c3()); println(c4()); println(c5());
    println(c6()); println(c7()); println(c8()); println(c9()); println(c10());
    println(c11()); println(c12()); println(c13()); println(c14());
    let t1 = M.My(mkv("r1"));
    println(match t1 { M.My(x) => x[1], _ => "n" });
    let t2 = M.My(mkv("r2"));
    println(match t2 { M.My(x) => { let y = x; y[1] }, _ => "n" });
    let t3 = M.My(mkv("r3"));
    println(if let M.My(x) = t3 { x[1] } else { "n" });
    let m4 = mkv("r4");
    println({ let x = m4; x[1] });
    let m5 = mkv("r5");
    println({ let x = m5; x[1] }.len());
    let m6 = mkv("r6");
    { let x = m6; x[1] };
    let mut k = 0;
    while k < 2 {
        let t = M.My(mkv(f"w{k}"));
        let z = match t { M.My(x) => x[1], _ => "n" };
        let m = mkv(f"v{k}");
        let z2 = { let x = m; x[0] };
        println(f"{z} {z2}");
        k = k + 1;
    }
}
"#;
    let want = "s1-b\nc1-b\nc2-b\nc3-b\n4\nc5-b\nc6-b\nc7-b\nc8-b\nc9-b\nc10-b\nc11-b\nc12b-a\nc13y\nc14-b\nr1-b\nr2-b\nr3-b\nr4-b\n4\nw0-b v0-a\nw1-b v1-a\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
