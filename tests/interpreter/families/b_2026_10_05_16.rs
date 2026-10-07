//! B-2026-10-05-16: `for` over a value of a user type that implements
//! `Iterator` pulls it through its own `next`

use super::*;

/// B-2026-10-05-16: design.md's own `CountUp` example in a `for` loop, a
/// generic iterator, an iterator held in a struct built by a constructor
/// call, `break` and `continue`, a labelled break out of two nested user
/// iterators, and the iterator's state written back between pulls (a
/// `next` that counts its calls). Every loop used to run once, with the
/// iterator itself bound to the loop variable.
const LOOPS: &str = r#"struct CountUp { current: i64, limit: i64 }
impl Iterator for CountUp {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] {
        if self.current >= self.limit { return None }
        self.current += 1;
        Some(self.current - 1)
    }
}
struct Rep[T] { v: T, left: i64 }
impl[T: Clone] Iterator for Rep[T] {
    type Item = T;
    fn next(mut ref self) -> Option[T] {
        if self.left == 0 { return None }
        self.left -= 1;
        Some(self.v.clone())
    }
}
struct Stack { items: Vec[i64], pulls: i64 }
impl Stack {
    fn of(n: i64) -> Stack {
        let mut items: Vec[i64] = Vec.new();
        for i in 0..n { items.push(i * 10); }
        return Stack { items: items, pulls: 0 };
    }
}
impl Iterator for Stack {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] {
        self.pulls += 1;
        return self.items.pop();
    }
}
fn main() {
    let mut a = 0;
    for v in CountUp { current: 0, limit: 5 } { a = a * 10 + v; }
    println(f"a {a}");
    let mut b = String.new();
    for s in Rep { v: "ab", left: 3 } { b.push_str(s); }
    println(f"b {b}");
    let mut c: Vec[i64] = Vec.new();
    for v in Stack.of(4) { c.push(v); }
    println(f"c {c.len()} {c[0]} {c[3]}");
    let mut d = 0;
    for v in CountUp { current: 0, limit: 100 } {
        if v % 2 == 0 { continue; }
        if v > 7 { break; }
        d += v;
    }
    println(f"d {d}");
    let mut e = 0;
    'outer: for x in CountUp { current: 1, limit: 10 } {
        for y in CountUp { current: 1, limit: 10 } {
            if x * y == 12 { e = x * 100 + y; break 'outer; }
        }
    }
    println(f"e {e}");
    let it = CountUp { current: 3, limit: 6 };
    let mut f = 0;
    for v in it { f += v; }
    println(f"f {f}");
}
"#;

#[test]
fn interp_for_over_a_user_iterator_pulls_its_next() {
    let parsed = karac::parse(LOOPS);
    let resolved = karac::resolve(&parsed.program);
    let errors = karac::typecheck(&parsed.program, &resolved).errors;
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(
        run_no_errors(LOOPS),
        "a 1234\nb ababab\nc 4 30 0\nd 16\ne 206\nf 12\n"
    );
}
