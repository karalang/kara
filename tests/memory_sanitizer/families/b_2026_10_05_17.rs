//! B-2026-10-05-17 -- a `for` over a user `Iterator` holds the iterator in a
//! local, pulls `next()` until `None`, and releases the iterator once.

use super::*;

/// Every iterator, every yielded `String` and the `Drop`-bodied neighbour are
/// released exactly once, including on `break` and an early `return`.
#[test]
fn asan_for_over_user_struct_iterator() {
    assert_clean_asan_run(
        r#"struct CountUp { current: i64, limit: i64 }
impl Iterator for CountUp {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] {
        if self.current >= self.limit { return None }
        self.current += 1;
        Some(self.current - 1)
    }
}
struct Words { items: Vec[String], i: i64 }
impl Iterator for Words {
    type Item = String;
    fn next(mut ref self) -> Option[String] {
        if self.i >= self.items.len() { return None }
        self.i += 1;
        Some(self.items[self.i - 1].clone())
    }
}
struct Pairs { n: i64, i: i64 }
impl Iterator for Pairs {
    type Item = (i64, String);
    fn next(mut ref self) -> Option[(i64, String)] {
        if self.i >= self.n { return None }
        self.i += 1;
        Some((self.i, f"p{self.i}"))
    }
}
struct Rep[T] { v: T, n: i64 }
impl[T: Clone] Iterator for Rep[T] {
    type Item = T;
    fn next(mut ref self) -> Option[T] {
        if self.n <= 0 { return None }
        self.n -= 1;
        Some(self.v.clone())
    }
}
struct Nested { items: Vec[Vec[i64]], o: i64, k: i64 }
impl Nested { fn new(items: Vec[Vec[i64]]) -> Nested { Nested { items: items, o: 0, k: 0 } } }
impl Iterator for Nested {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] {
        while self.o < self.items.len() {
            if self.k < self.items[self.o].len() {
                self.k += 1;
                return Some(self.items[self.o][self.k - 1]);
            }
            self.o += 1;
            self.k = 0;
        }
        None
    }
}
struct Tok { s: String }
impl Drop for Tok { fn drop(mut ref self) { println(f"drop {self.s}"); } }
struct Holder { c: CountUp }
fn mk() -> CountUp { CountUp { current: 0, limit: 3 } }
fn words(n: i64) -> Words { let mut v: Vec[String] = Vec.new(); for i in 0..n { v.push(f"w{i}"); } Words { items: v, i: 0 } }
fn sum_it(c: CountUp) -> i64 { let mut s = 0; for v in c { s += v; } s }
fn first_big(c: CountUp) -> i64 { for v in c { if v > 2 { return v } } -1 }
fn stop_at_w1() { for s in words(4) { if s == "w1" { return } println(f"r:{s}"); } }
fn main() {
    for v in CountUp { current: 0, limit: 3 } { println(f"a:{v}"); }
    let c = CountUp { current: 5, limit: 7 };
    for v in c { println(f"b:{v}"); }
    for v in mk() { println(f"c:{v}"); }
    for s in Words { items: vec!["x".to_string(), "yy".to_string()], i: 0 } { println(f"d:{s} {s.len()}"); }
    for s in Words { items: vec!["x".to_string(), "stop".to_string(), "z".to_string()], i: 0 } {
        if s == "stop" { break }
        println(f"e:{s}");
    }
    'outer: for a in CountUp { current: 0, limit: 4 } {
        for b in CountUp { current: 0, limit: 4 } {
            if b == 2 { continue 'outer }
            if a == 2 { break 'outer }
            println(f"f:{a}{b}");
        }
    }
    for a in CountUp { current: 0, limit: 5 } { if a % 2 == 0 { continue } println(f"g:{a}"); }
    for (k, s) in Pairs { n: 2, i: 0 } { println(f"h:{k}={s}"); }
    for x in Rep { v: 7, n: 2 } { println(f"i:{x}"); }
    let r = Rep { v: "hi".to_string(), n: 2 };
    for s in r { println(f"j:{s}"); }
    let mut total = 0;
    for v in Nested.new(vec![vec![1, 2], vec![], vec![3]]) { total += v; }
    println(f"k:{total}");
    println(f"l:{sum_it(CountUp { current: 0, limit: 5 })} {first_big(CountUp { current: 0, limit: 9 })} {first_big(mk())}");
    let mut acc: Vec[String] = Vec.new();
    for s in words(3) { acc.push(s); }
    println(f"m:{acc.len()} {acc[2]}");
    stop_at_w1();
    let t = Tok { s: "t".to_string() };
    let h = Holder { c: CountUp { current: 2, limit: 4 } };
    for v in h.c { println(f"n:{v}"); }
    let cs = vec![CountUp { current: 0, limit: 2 }];
    for c2 in cs { for v in c2 { println(f"o:{v}"); } }
    let x = if total > 0 { CountUp { current: 5, limit: 6 } } else { mk() };
    for v in x { println(f"p:{v}"); }
    for v in { CountUp { current: 9, limit: 10 } } { println(f"q:{v}"); }
    println(f"end {t.s}");
}
"#,
        &[
            "a:0",
            "a:1",
            "a:2",
            "b:5",
            "b:6",
            "c:0",
            "c:1",
            "c:2",
            "d:x 1",
            "d:yy 2",
            "e:x",
            "f:00",
            "f:01",
            "f:10",
            "f:11",
            "g:1",
            "g:3",
            "h:1=p1",
            "h:2=p2",
            "i:7",
            "i:7",
            "j:hi",
            "j:hi",
            "k:6",
            "l:10 3 -1",
            "m:3 w2",
            "r:w0",
            "n:2",
            "n:3",
            "o:0",
            "o:1",
            "p:5",
            "q:9",
            "end t",
            "drop t",
        ],
        "asan_for_over_user_struct_iterator",
    );
}

/// The enum iterators release each replaced `self` once.
#[test]
fn asan_for_over_user_enum_iterator() {
    assert_clean_asan_run(
        r#"enum Steps { Up(i64), Done }
impl Iterator for Steps {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] {
        let cur = match self { Steps.Up(n) => n, Steps.Done => -1 };
        if cur < 0 { return None }
        if cur >= 3 { self = Steps.Done; } else { self = Steps.Up(cur + 1); }
        Some(cur)
    }
}
enum Cnt[T] { More(T, i64), Done }
impl[T: Clone] Iterator for Cnt[T] {
    type Item = T;
    fn next(mut ref self) -> Option[T] {
        let (v, n) = match self { Cnt.More(v, n) => (v.clone(), n), Cnt.Done => return None };
        if n <= 1 { self = Cnt.Done; } else { self = Cnt.More(v.clone(), n - 1); }
        Some(v)
    }
}
fn main() {
    let mut guard = 0;
    for v in Steps.Up(1) { println(f"a:{v}"); guard += 1; if guard > 9 { break } }
    let s = Steps.Up(2);
    for v in s { println(f"b:{v}"); guard += 1; if guard > 19 { break } }
    for v in Cnt.More(8, 2) { println(f"c:{v}"); guard += 1; if guard > 29 { break } }
    for v in Cnt.More(4, 1) { println(f"d:{v}"); guard += 1; if guard > 39 { break } }
}
"#,
        &["a:1", "a:2", "a:3", "b:2", "b:3", "c:8", "c:8", "d:4"],
        "asan_for_over_user_enum_iterator",
    );
}
