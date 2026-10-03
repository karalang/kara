//! B-2026-09-28-60 — two `shared enum`s declaring the same variant name
//! (`G[T] { Y(T) }`, `M { Y(Vec[String]) }`) crashed `karac build` in about
//! half of all builds: the payload binding took its word offsets from the
//! pattern's own enum but its heap type from whichever enum declaring that
//! variant the unordered map yielded first. Both now come from the same enum.

use super::*;

/// B-2026-09-28-60 — three shared enums sharing `Y`, bound by `if let`,
/// `match`, a two-payload variant, a generic instance and a `for` loop.
/// Compiled three times, since the crash was decided by map order.
#[test]
fn e2e_shared_enums_sharing_a_variant_name_bind_their_own_payload() {
    let src = r#"
shared enum G[T] { Y(T), N }
shared enum M { Y(Vec[String]), N }
shared enum K { Y(String, i64), N }
fn mkv(s: String) -> Vec[String] { let mut v = Vec.new(); v.push(s); v.push("second-heap-string-long-enough".to_string()); v }
fn hs(k: i64) -> String { f"heap-string-long-enough-{k}" }
fn main() {
    let g: M = M.Y(mkv(hs(3)));
    if let M.Y(x) = g { println(f"a{x.len()} {x[0]}") }
    let g2: M = M.Y(mkv(hs(4)));
    let n2 = match g2 { M.Y(x) => x.len(), M.N => 0 };
    println(f"b{n2}");
    let k: K = K.Y(hs(5), 7);
    if let K.Y(s, n) = k { println(f"c{s.len()} {n}") }
    let k2: K = K.Y(hs(6), 8);
    let n3 = match k2 { K.Y(s, m) => s.len() + m, K.N => 0 };
    println(f"d{n3}");
    let h: G[String] = G.Y(hs(7));
    if let G.Y(s) = h { println(f"e{s.len()}") }
    let mut gs: Vec[M] = Vec.new();
    gs.push(M.Y(mkv(hs(8))));
    gs.push(M.N);
    for g3 in gs { match g3 { M.Y(x) => println(f"f{x.len()}"), M.N => println("fn") } }
    println("end")
}"#;
    let want = "a2 heap-string-long-enough-3\nb2\nc25 7\nd33\ne25\nf2\nfn\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    for round in 0..3 {
        assert_eq!(
            run_program(src).as_deref(),
            Some(want),
            "AOT, build {round}"
        );
    }
}

/// B-2026-09-28-60 — a `G[M]` whose payload is the other enum, matched by a
/// nested pattern naming both: the shape that crashed on every build. Output
/// only; its 40-byte leak is B-2026-10-03-35, which does not need the
/// collision.
#[test]
fn e2e_nested_shared_enums_sharing_a_variant_name_bind_their_own_payload() {
    let src = r#"
shared enum G[T] { Y(T), N }
shared enum M { Y(Vec[String]), N }
fn mkv(s: String) -> Vec[String] { let mut v = Vec.new(); v.push(s); v.push("second-heap-string-long-enough".to_string()); v }
fn main() {
    let h: G[M] = G.Y(M.Y(mkv("heap-string-long-enough-5".to_string())));
    let n = match h { G.Y(M.Y(x)) => x.len(), _ => 0 };
    println(f"g{n}");
    println("end")
}"#;
    let want = "g2\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    for round in 0..3 {
        assert_eq!(
            run_program(src).as_deref(),
            Some(want),
            "AOT, build {round}"
        );
    }
}
