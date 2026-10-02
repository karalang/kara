//! B-2026-10-02-57 -- `--interp` scoped names dynamically: a callee's free
//! function or constant resolved to a CALLER'S local of the same name, and
//! every global lookup walked the whole call stack.

use super::*;

/// B-2026-10-02-57 — a callee reads ITS item scope, not its caller's locals.
///
/// Before: `a99 99`, `b5 5`, `c102` and `e7 5`, because `helper`,
/// `LIMIT` and `g` were found in `main`'s scopes first. A closure still
/// sees the local it captured (`d`), and a callee's own local shadows only
/// inside that callee (`twice`). The JIT and AOT print the same lines.
/// `d(3000)` is the depth that used to cost a whole-stack walk per call.
#[test]
fn test_callee_names_resolve_lexically_not_through_the_caller() {
    let out = run(r#"fn helper() -> i64 {
    return 1;
}
fn f() -> i64 {
    return helper();
}
const LIMIT: i64 = 10;
fn g() -> i64 {
    return LIMIT;
}
struct P {
    n: i64,
}
impl P {
    fn get(self) -> i64 {
        return self.n + helper();
    }
}
fn twice(h: Fn() -> i64) -> i64 {
    let helper = 50;
    return h() + h() + helper;
}
fn d(n: i64) -> i64 {
    if n == 0 {
        return 0;
    }
    return 1 + d(n - 1);
}
fn main() {
    let helper = || 99;
    println(f"a{f()} {helper()}");
    let LIMIT = 5;
    println(f"b{g()} {LIMIT}");
    let p = P { n: 3 };
    println(f"c{p.get()}");
    let k = || helper() + 1;
    println(f"d{k()} {twice(k)}");
    let f = 7;
    println(f"e{f} {g()}");
    println(f"f{d(3000)}");
}
"#);
    assert_eq!(
        out, "a1 99\nb10 5\nc4\nd100 250\ne7 10\nf3000\n",
        "got:\n{out}"
    );
}
