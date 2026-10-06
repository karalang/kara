//! B-2026-10-03-19 -- `std.mem` `replace` / `swap` on a field reached
//! through a `mut ref` param (`replace(h.name, v)` with `h: mut ref H`) failed
//! to build ("unsupported `mut ref` place expression"), and a nested field
//! forwarded through a `mut ref` root (`bump(w.h.n)`) wrote to a copy, so the
//! caller never saw the write.

use super::*;

/// Field, nested-field, Vec-field and i64-field `replace` / `swap` through a
/// `mut ref` param, plus nested fields forwarded to a `mut ref i64` param.
#[test]
fn e2e_mem_replace_swap_through_mut_ref_fields() {
    let src = r#"struct P { x: i64 }
struct H { name: String, n: i64, tags: Vec[String], p: P }
struct W { h: H }
fn rep(h: mut ref H, v: String) -> String { return replace(h.name, v) }
fn repn(h: mut ref H, v: i64) -> i64 { return replace(h.n, v) }
fn sw(h: mut ref H, o: String) -> String { let mut q = o; swap(h.name, mut q); return q; }
fn repv(h: mut ref H, v: Vec[String]) -> Vec[String] { return replace(h.tags, v) }
fn deep(w: mut ref W, v: String) -> String { return replace(w.h.name, v) }
fn px(h: mut ref H) -> i64 { return replace(h.p.x, 77) }
fn bump(v: mut ref i64) { v = v + 1; }
fn go(w: mut ref W) { bump(w.h.n); bump(w.h.p.x); }
fn main() {
    let mut h = H { name: "heap-string-longer-than-sso-0".to_string(), n: 1, tags: vec!["t1".to_string()], p: P { x: 3 } };
    let i = 7;
    let old = rep(mut h, f"heap-string-longer-than-sso-{i}");
    println(old);
    println(h.name);
    println(f"n:{repn(mut h, 5)} {h.n}");
    let o2 = sw(mut h, f"swapped-{i}-long-enough-for-heap");
    println(f"s:{o2} {h.name}");
    let t = repv(mut h, vec!["a".to_string(), "b".to_string()]);
    println(f"v:{t.len()} {t[0]} {h.tags.len()} {h.tags[1]}");
    let mut w = W { h: h };
    println(f"w:{deep(mut w, f"deep-{i}")} {w.h.name}");
    println(f"p:{px(mut w.h)} {w.h.p.x}");
    go(mut w);
    println(f"g:{w.h.n} {w.h.p.x}");
}
"#;
    let want = "heap-string-longer-than-sso-0\nheap-string-longer-than-sso-7\nn:1 5\ns:heap-string-longer-than-sso-7 swapped-7-long-enough-for-heap\nv:1 t1 2 b\nw:swapped-7-long-enough-for-heap deep-7\np:3 77\ng:6 78\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
