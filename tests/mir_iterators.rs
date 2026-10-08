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

/// The library's `Iterator` (`runtime/stdlib/source/iter.kara`) gives a
/// program's own iterator its provided methods: the adaptors chain, a `for`
/// destructures what `enumerate` yields, and `main` comes before the impls
/// whose `type Item` it loops through.
#[test]
fn the_library_iterator_methods_chain_on_a_programs_iterator() {
    let src = r#"
fn main() {
    println(Counter { n: 0 }.take(3).count());
    for (i, x) in Counter { n: 0 }.skip(2).step_by(3).take(3).enumerate() {
        println(f"{i}: {x}");
    }
    println(Counter { n: 0 }.take(5).last().unwrap());
    println(Counter { n: 0 }.nth(4).unwrap());
    for w in Words { n: 0 }.skip(1).take(2) {
        println(w);
    }
    println(Words { n: 0 }.take(3).last().unwrap());
}
struct Counter { n: i64 }
impl Iterator for Counter {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] {
        self.n = self.n + 1;
        Some(self.n * 10)
    }
}
struct Words { n: i64 }
impl Iterator for Words {
    type Item = String;
    fn next(mut ref self) -> Option[String] {
        self.n = self.n + 1;
        Some(f"w{self.n}")
    }
}
"#;
    assert_runs(src, "3\n0: 30\n1: 60\n2: 90\n50\n50\nw2\nw3\nw3\n");
}

/// The library methods that combine iterators or call a function on each
/// item: `chain`, `zip` (whose item is a tuple of two inner items, so a
/// `for` resolves both), `fold`, `any` (which stops early and leaves the
/// rest), `all`, `find`, `position` and `for_each`.
#[test]
fn the_library_iterator_combinators_and_folds() {
    let src = r#"
struct Counter { n: i64 }
impl Iterator for Counter {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] { self.n = self.n + 1; Some(self.n) }
}
fn main() {
    for x in Counter { n: 0 }.take(2).chain(Counter { n: 10 }.take(2)) { println(x); }
    for (a, b) in Counter { n: 0 }.zip(Counter { n: 100 }.skip(1)).take(2) {
        println(f"{a} {b}");
        if a > 1 { break; }
    }
    println(Counter { n: 0 }.take(4).fold(0, |acc, x| acc + x));
    let mut c = Counter { n: 0 };
    println(c.any(|x| x == 3));
    println(c.n);
    println(Counter { n: 0 }.take(5).all(|x| x < 9));
    println(Counter { n: 0 }.find(|x| *x > 6).unwrap());
    println(Counter { n: 0 }.position(|x| x == 4).unwrap());
    let mut total = 0;
    Counter { n: 0 }.take(3).for_each(|x| { total = total + x; });
    println(total);
}
"#;
    assert_runs(
        src,
        "1\n2\n11\n12\n1 102\n2 103\n10\ntrue\n3\ntrue\n7\n3\n6\n",
    );
}

/// A program's own adaptor whose impl fixes one of the struct's arguments
/// (`impl[I: Iterator, B] Iterator for Mapped[I, I.Item, B]`): the impl's
/// arguments are read off the struct's by position (`[0, 2]`), for a `for`
/// over it and a method call on it. Its stored closure is `escaping`, so it
/// writes its own moved-in copy of `seen`, which stays 0 outside.
#[test]
fn an_impl_that_fixes_a_struct_argument_and_a_stored_closure_that_writes() {
    let src = r#"
struct Counter { n: i64 }
impl Iterator for Counter {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] { self.n = self.n + 1; Some(self.n) }
}
struct Mapped[I, T, B] { it: I, f: MutFn(own T) -> B }
impl[I: Iterator, B] Iterator for Mapped[I, I.Item, B] {
    type Item = B;
    fn next(mut ref self) -> Option[B] {
        match self.it.next() {
            Some(x) => Some((self.f)(x)),
            None => None,
        }
    }
}
fn mapped[T, I: Iterator[Item = T], B](it: own I, f: escaping MutFn(own T) -> B) -> Mapped[I, T, B] {
    Mapped { it: it, f: f }
}
fn main() {
    for x in mapped(Counter { n: 0 }, |x: i64| x * 10).take(3) { println(x); }
    let mut seen = 0;
    for s in mapped(Counter { n: 0 }.take(2), |x: i64| { seen = seen + 1; f"<{x}>" }) {
        println(s);
    }
    println(seen);
    println(mapped(Counter { n: 5 }, |x: i64| x + 1).nth(1).unwrap());
}
"#;
    assert_runs(src, "10\n20\n30\n<1>\n<2>\n0\n8\n");
}

/// An adaptor generic over its function, `F: MutFn(ref I.Item)`: the call
/// solves `I` from the iterator, the bound's projection gives the closure
/// its parameter type, and the stored function is called with the bound's
/// signature.
#[test]
fn an_adaptor_generic_over_a_function_bound() {
    assert_runs(
        r#"
struct Counter { n: i64 }
impl Iterator for Counter {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] { self.n = self.n + 1; Some(self.n) }
}
struct Insp[I, F] { it: I, f: F }
impl[I: Iterator, F: MutFn(ref I.Item)] Iterator for Insp[I, F] {
    type Item = I.Item;
    fn next(mut ref self) -> Option[I.Item] {
        match self.it.next() {
            Some(x) => { (self.f)(x); Some(x) }
            None => None,
        }
    }
}
fn insp[I: Iterator, F: MutFn(ref I.Item)](it: own I, f: own F) -> Insp[I, F] { Insp { it: it, f: f } }
fn show(x: ref i64) { println(f"<{x}>"); }
fn main() {
    let mut t = 0;
    for x in insp(Counter { n: 0 }, show) { t = t + x; if x == 2 { break; } }
    for x in insp(Counter { n: 0 }, |x| println(x * 10)) { t = t + x; if x == 2 { break; } }
    println(t);
}
"#,
        "<1>\n<2>\n10\n20\n6\n",
    );
}

/// The library's adaptors that store a function: `map`, `filter`,
/// `filter_map`, `take_while`, `skip_while` and `inspect`, each generic over
/// its function with a function-trait bound. `map`'s result type is fixed by
/// the closure alone.
#[test]
fn the_library_adaptors_that_store_a_function() {
    assert_runs(
        r#"
struct Counter { n: i64 }
impl Iterator for Counter {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] { self.n = self.n + 1; Some(self.n) }
}
fn main() {
    let k = 10;
    for s in Counter { n: 0 }.map(|x| f"<{x * k}>").take(3) { println(s); }
    println(Counter { n: 0 }.filter(|x| x % 3 == 0).take(4).fold(0, |a, x| a + x));
    for y in Counter { n: 0 }.filter_map(|x| if x % 2 == 0 { Some(x * x) } else { None }).take(3) { println(y); }
    for x in Counter { n: 0 }.skip_while(|x| x < 5).take_while(|x| x < 8) { println(x); }
    let t = Counter { n: 0 }.take(3).inspect(|x| println(f"saw {x}")).fold(0, |a, x| a + x);
    println(t);
}
"#,
        "<10>\n<20>\n<30>\n30\n4\n16\n36\n5\n6\n7\nsaw 1\nsaw 2\nsaw 3\n6\n",
    );
}
