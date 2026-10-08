//! Iterators on the MIR pipeline (redesign A2): `for` over a type that
//! implements `Iterator`, and trait default methods built on `next`.

use karac::mir::interp::Outcome;

fn run(src: &str) -> Result<(String, i32), String> {
    let r = karac::mir::lower::run_source(src)?;
    match (&r.outcome, r.exit_code()) {
        (_, Some(code)) => Ok((r.output, code)),
        (Outcome::Error(e), None) => Err(format!("run: {e}\n--- output so far\n{}", r.output)),
        _ => Err("no exit code".into()),
    }
}

fn assert_runs(src: &str, want: &str) {
    match run(src) {
        Ok((out, 0)) if out == want => {}
        other => panic!("want exit 0 and\n{want}--- got {other:?}"),
    }
}

/// `for x in it` over a program's own `Iterator` calls its `next` until
/// `None`, directly and through a bound `I: Iterator[Item = i64]`.
#[test]
fn a_for_loop_calls_a_programs_own_next() {
    let src = r#"
struct Counter { n: i64, stop: i64 }
impl Iterator for Counter {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] {
        if self.n >= self.stop { return None; }
        self.n = self.n + 1;
        Some(self.n)
    }
}
fn total[I: Iterator[Item = i64]](it: I) -> i64 {
    let mut t = 0;
    for x in it {
        t = t + x;
    }
    t
}
fn main() {
    let c = Counter { n: 0, stop: 4 };
    for x in c {
        println(f"{x}");
    }
    println(f"{total(Counter { n: 0, stop: 3 })}");
}
"#;
    assert_runs(src, "1\n2\n3\n4\n6\n");
}

/// Under `I: Iterator[Item = i64]`, `inner.next()` is an `Option[i64]`,
/// so its payload can be doubled.
#[test]
fn an_item_binding_on_a_bound_fixes_next_s_type() {
    let src = r#"
struct Counter { n: i64, stop: i64 }
impl Iterator for Counter {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] {
        if self.n >= self.stop { return None; }
        self.n = self.n + 1;
        Some(self.n)
    }
}
struct Doubled[I] { inner: I }
impl[I: Iterator[Item = i64]] Iterator for Doubled[I] {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] {
        match self.inner.next() {
            Some(x) => Some(x * 2),
            None => None,
        }
    }
}
fn main() {
    let mut c = Counter { n: 0, stop: 3 };
    let mut d = Doubled { inner: c };
    let mut t = 0;
    loop {
        match d.next() {
            Some(x) => { t = t + x; }
            None => break,
        }
    }
    println(f"{t}");
}
"#;
    assert_runs(src, "12\n");
}

/// A trait default method matching on `self.next()`. Checking it used to
/// loop forever: the scrutinee's `Option[Self.Item]` was freshened over and
/// over through its projection.
#[test]
fn a_trait_default_method_calls_the_implementors_next() {
    let src = r#"
trait It2 {
    type Item;
    fn nxt(mut ref self) -> Option[Self.Item];
    fn cnt(mut ref self) -> i64 {
        let mut n = 0;
        loop {
            match self.nxt() {
                Some(_) => { n = n + 1; }
                None => break,
            }
        }
        n
    }
}
struct Counter { n: i64, stop: i64 }
impl It2 for Counter {
    type Item = i64;
    fn nxt(mut ref self) -> Option[i64] {
        if self.n >= self.stop { return None; }
        self.n = self.n + 1;
        Some(self.n)
    }
}
fn main() {
    let mut c = Counter { n: 0, stop: 4 };
    println(f"{c.cnt()}");
}
"#;
    assert_runs(src, "4\n");
}

/// A trait default method with a parameter of its own, called on the
/// implementing type.
#[test]
fn a_trait_default_method_takes_a_plain_parameter() {
    let src = r#"
trait It2 {
    type Item;
    fn nxt(mut ref self) -> Option[Self.Item];
    fn skipn(mut ref self, k: i64) -> i64 {
        let mut n = 0;
        while n < k {
            match self.nxt() {
                Some(_) => { n = n + 1; }
                None => break,
            }
        }
        n
    }
}
struct Counter { n: i64, stop: i64 }
impl It2 for Counter {
    type Item = i64;
    fn nxt(mut ref self) -> Option[i64] {
        if self.n >= self.stop { return None; }
        self.n = self.n + 1;
        Some(self.n)
    }
}
fn main() {
    let mut c = Counter { n: 0, stop: 4 };
    println(f"{c.skipn(2)}");
}
"#;
    assert_runs(src, "2\n");
}

/// A trait default method may name `Self.Item` in a parameter: inside each
/// impl it is that impl's own `Item`.
#[test]
fn a_default_method_parameter_names_self_item() {
    let src = r#"
trait It2 {
    type Item;
    fn nxt(mut ref self) -> Option[Self.Item];
    fn fold2[B](mut ref self, init: B, f: MutFn(B, Self.Item) -> B) -> B {
        let mut acc = init;
        loop {
            match self.nxt() {
                Some(x) => { acc = f(acc, x); }
                None => break,
            }
        }
        acc
    }
}
struct Counter { n: i64, stop: i64 }
impl It2 for Counter {
    type Item = i64;
    fn nxt(mut ref self) -> Option[i64] {
        if self.n >= self.stop { return None; }
        self.n = self.n + 1;
        Some(self.n)
    }
}
fn main() {
    let mut d = Counter { n: 0, stop: 4 };
    println(f"{d.fold2(100, |a, x| a + x)}");
}
"#;
    assert_runs(src, "110\n");
}

/// An adaptor written in Kāra: a default `map2` wraps `self` in a generic
/// struct whose `It` impl carries `where I: It[Item = T]`. It type checks,
/// including `let it = self` in a default body and the impl's own methods
/// called on `self` inside it; the builder stops at the closure stored in
/// the struct's `MutFn` field, which erased function values will lower.
#[test]
fn a_map_adaptor_in_kara_type_checks_up_to_its_stored_closure() {
    let src = r#"
trait It {
    type Item;
    fn nxt(mut ref self) -> Option[Self.Item];
    fn map2[B](own self, f: own MutFn(Self.Item) -> B) -> MapIt[Self, Self.Item, B] {
        MapIt { it: self, f: f }
    }
    fn count2(own self) -> i64 {
        let mut it = self;
        let mut n = 0;
        while let Some(_) = it.nxt() { n = n + 1; }
        n
    }
}
struct MapIt[I, T, B] { it: I, f: MutFn(T) -> B }
impl[I, T, B] It for MapIt[I, T, B] where I: It[Item = T] {
    type Item = B;
    fn nxt(mut ref self) -> Option[B] {
        match self.it.nxt() {
            Some(x) => Some((self.f)(x)),
            None => None,
        }
    }
}
struct Counter { n: i64, stop: i64 }
impl It for Counter {
    type Item = i64;
    fn nxt(mut ref self) -> Option[i64] {
        if self.n >= self.stop { return None; }
        self.n = self.n + 1;
        Some(self.n)
    }
}
fn main() {
    println(Counter { n: 0, stop: 3 }.map2(|x| x + 1).count2());
}
"#;
    match run(src) {
        Err(e) if e.contains("a function value") => {}
        other => panic!("want the stored-closure refusal, got {other:?}"),
    }
}

/// Adaptors that store no closure, written as trait default methods over
/// generic structs: `Tk[I]` binds `type Item = I.Item`, so the builder
/// normalizes `Tk[Counter].Item` to `i64` at the instance. The default
/// method `en` returns `En[Self]`, which is `En[Tk[Counter]]` in one impl's
/// copy and `En[Counter]` in another's, and `lst` annotates a local with
/// `Option[Self.Item]`.
#[test]
fn closure_free_adaptors_are_generic_structs_over_self() {
    let src = r#"
trait It {
    type Item;
    fn nxt(mut ref self) -> Option[Self.Item];
    fn cnt(own self) -> i64 {
        let mut it = self;
        let mut n = 0;
        while let Some(_) = it.nxt() { n = n + 1; }
        n
    }
    fn tk(own self, n: i64) -> Tk[Self] {
        Tk { it: self, left: n }
    }
    fn en(own self) -> En[Self] {
        En { it: self, i: 0 }
    }
    fn lst(own self) -> Option[Self.Item] {
        let mut it = self;
        let mut last: Option[Self.Item] = None;
        while let Some(x) = it.nxt() { last = Some(x); }
        last
    }
}
struct Tk[I] { it: I, left: i64 }
impl[I: It] It for Tk[I] {
    type Item = I.Item;
    fn nxt(mut ref self) -> Option[I.Item] {
        if self.left <= 0 { return None; }
        self.left = self.left - 1;
        self.it.nxt()
    }
}
struct En[I] { it: I, i: i64 }
impl[I: It] It for En[I] {
    type Item = (i64, I.Item);
    fn nxt(mut ref self) -> Option[(i64, I.Item)] {
        match self.it.nxt() {
            Some(x) => { let k = self.i; self.i = self.i + 1; Some((k, x)) }
            None => None,
        }
    }
}
struct Counter { n: i64 }
impl It for Counter {
    type Item = i64;
    fn nxt(mut ref self) -> Option[i64] {
        self.n = self.n + 1;
        Some(self.n * 10)
    }
}
fn main() {
    println(Counter { n: 0 }.tk(4).cnt());
    let mut e = Counter { n: 0 }.tk(3).en();
    while let Some((i, v)) = e.nxt() { println(f"{i} {v}"); }
    println(Counter { n: 0 }.tk(5).lst().unwrap());
    let mut d = Counter { n: 4 }.en();
    let (i, v) = d.nxt().unwrap();
    println(f"{i} {v}");
}
"#;
    assert_runs(src, "4\n0 10\n1 20\n2 30\n50\n0 50\n");
}
