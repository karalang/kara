//! B-2026-10-03-22 — a closure that captures nothing, returned out of the
//! fn that made it, read its one-byte placeholder env from that fn's dead
//! stack frame on every call (ASAN: stack-use-after-return, seen only on the
//! instrumented `-O0` leg). The body no longer loads an env it never reads.

use super::*;

/// B-2026-10-03-22 — capture-less closures returned from a fn, over a scalar
/// and a `String` param and from both arms of an `if`, beside a capturing
/// (heap-env) control.
#[test]
fn asan_escaping_captureless_closure_reads_no_dead_env() {
    assert_clean_asan_run_min_allocs(
        r#"fn mk() -> Fn(i64) -> i64 { |x: i64| x + 1 }
fn mks() -> Fn(String) -> i64 { |s: String| s.len() }
fn mkc(k: i64) -> Fn(i64) -> i64 { |x: i64| x + k }
fn pick(b: bool) -> Fn(i64) -> i64 { if b { |x: i64| x * 2 } else { |x: i64| x * 3 } }
fn main() {
    let g = mk();
    println(f"a {g(4)} {g(5)}");
    let h = mks();
    println(f"b {h("ab".to_string() + "-heap-string-long-enough")}");
    let c = mkc(10);
    println(f"c {c(1)}");
    let p = pick(true);
    let q = pick(false);
    println(f"d {p(5)} {q(5)}");
    println("end")
}"#,
        &["a 5 6", "b 26", "c 11", "d 10 15", "end"],
        "asan_escaping_captureless_closure_reads_no_dead_env",
        2,
    );
}
