//! B-2026-10-02-69 -- `replace(d, v)` forwarded through a wrapper whose
//! by-value param it moves into `*d` freed that param's buffer twice.

use super::*;

/// B-2026-10-02-69 — a caller-retained by-value param stored into `*dest`
/// by `std.mem::replace` kept the caller's buffer, which the caller then
/// freed after the call while the place freed it again. Covers the String,
/// generic and `Vec` params, a conditional replace, and a statement-level one.
#[test]
fn asan_replace_through_wrapper_copies_the_moved_param() {
    assert_clean_asan_run(
        r#"fn rep(d: mut ref String, v: String) -> String { return replace(d, v) }
fn repg[T](d: mut ref T, v: T) -> T { return replace(d, v) }
fn repv(d: mut ref Vec[String], v: Vec[String]) -> Vec[String] { return replace(d, v) }
fn repc(d: mut ref String, v: String, c: bool) -> String {
    if c { return replace(d, v) }
    return "heap-string-longer-than-sso-none".to_string()
}
fn put(d: mut ref String, v: String) { let o = replace(d, v); println(o); }
fn main() {
    let mut s = "heap-string-longer-than-sso-0".to_string();
    let i = 5;
    let a = rep(mut s, f"heap-string-longer-than-sso-{i}");
    println(a);
    let b = repg(mut s, "heap-string-longer-than-sso-6".to_string());
    println(b);
    let c = repc(mut s, f"heap-string-longer-than-sso-{i}{i}", true);
    let e = repc(mut s, f"heap-string-longer-than-sso-{i}{i}{i}", false);
    println(c);
    println(e);
    put(mut s, f"heap-string-longer-than-sso-8{i}");
    println(s);
    let mut v: Vec[String] = Vec.new();
    v.push("heap-string-longer-than-sso-v0".to_string());
    let mut n: Vec[String] = Vec.new();
    n.push("heap-string-longer-than-sso-v1".to_string());
    let ov = repv(mut v, n);
    println(ov[0]);
    println(v[0]);
}
"#,
        &[
            "heap-string-longer-than-sso-0",
            "heap-string-longer-than-sso-5",
            "heap-string-longer-than-sso-6",
            "heap-string-longer-than-sso-none",
            "heap-string-longer-than-sso-55",
            "heap-string-longer-than-sso-85",
            "heap-string-longer-than-sso-v0",
            "heap-string-longer-than-sso-v1",
        ],
        "replace_through_wrapper",
    );
}
