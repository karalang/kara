//! B-2026-10-04-76 — an index read of a non-`Copy` element as a block tail
//! or a `shared enum` arm tail was freed twice. The ASAN twin of the codegen
//! fixture.

use super::*;

/// B-2026-10-04-76 — the codegen fixture's table under ASAN.
#[test]
fn asan_index_element_block_and_arm_tail_is_copied() {
    assert_clean_asan_run_min_allocs(
        r#"fn mkv(s: String) -> Vec[String] {
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
}"#,
        &[
            "s1-b",
            "c1-b",
            "c2-b",
            "c3-b",
            "4",
            "c5-b",
            "c6-b",
            "c7-b",
            "c8-b",
            "c9-b",
            "c10-b",
            "c11-b",
            "c12b-a",
            "c13y",
            "c14-b",
            "r1-b",
            "r2-b",
            "r3-b",
            "r4-b",
            "4",
            "w0-b v0-a",
            "w1-b v1-a",
        ],
        "asan_index_element_block_and_arm_tail_is_copied",
        20,
    );
}
