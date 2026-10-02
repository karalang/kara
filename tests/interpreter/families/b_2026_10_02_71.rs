//! B-2026-10-02-71 -- a closure captures the LOCAL scopes only; it reaches
//! functions and constants through the item scope at call time.

use super::*;

/// B-2026-10-02-71 -- what a closure sees after its capture stopped copying
/// the item scope: a captured local is a snapshot taken at creation, a local
/// that shadows a function wins inside the closure but not inside a callee,
/// an escaping closure still reads a global constant, a `mut ref` closure
/// still writes through, and `sort_by` / `map` closures still see items.
#[test]
fn test_closure_capture_skips_item_scope_semantics() {
    let out = run(r#"fn helper() -> i64 {
    return 1;
}
const LIMIT: i64 = 10;
fn apply(f: Fn() -> i64) -> i64 {
    let helper = 50;
    return f() + helper;
}
fn make(k: i64) -> Fn(i64) -> i64 {
    return |x| x + k + LIMIT;
}
fn main() {
    let mut x = 1;
    let c = || x + helper();
    x = 100;
    println(f"{c()} {x}");
    let helper = || 7;
    let d = || helper() + LIMIT;
    println(f"{d()} {apply(d)}");
    let add = make(5);
    println(f"{add(1)}");
    let mut total = 0;
    let mut bump = mut ref || {
        total += 1;
    };
    bump();
    bump();
    println(f"{total}");
    let v = [3, 1, 2];
    let mut w = v.clone();
    w.sort_by(|a, b| b.cmp(a));
    println(f"{w} {v.iter().map(|e| e * LIMIT).collect()}");
}
"#);
    assert_eq!(
        out, "2 100\n17 67\n16\n2\n[3, 2, 1] [30, 10, 20]\n",
        "got:\n{out}"
    );
}
