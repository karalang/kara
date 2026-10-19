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
fn total[I: Iterator[Item = i64]](it: own I) -> i64 {
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
    fn fold2[B](mut ref self, init: own B, f: MutFn(own B, own Self.Item) -> B) -> B {
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

/// A closure handed to a library adaptor writes its capture through a loan
/// (core semantics §9.1), so the count `inspect` keeps is visible after the
/// chain runs.
#[test]
fn an_adaptor_closure_writes_its_capture() {
    assert_runs(
        r#"
struct Counter { n: i64 }
impl Iterator for Counter {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] {
        self.n = self.n + 1;
        Some(self.n)
    }
}
fn main() {
    let mut seen = 0;
    let total = Counter { n: 0 }.take(4).inspect(|x| { seen = seen + 1; }).fold(0, |a, x| a + x);
    println(f"{total} {seen}");
}
"#,
        "10 4\n",
    );
}

/// `v.iter()` is the library's `VecIter`, a Kāra struct that borrows the
/// `Vec`, and the adaptors run over it with items `ref T`.
#[test]
fn a_vec_s_iter_is_a_kara_struct_under_the_adaptors() {
    assert_runs(
        r#"
struct Person { name: String, n: i64 }
fn main() {
    let v = vec![1, 2, 3, 4];
    let d = v.iter().map(|x| x * 2).fold(0, |a, b| a + b);
    println(f"{d} {v.iter().count()}");
    for (i, x) in v.iter().enumerate() {
        print(f"{i}:{x} ");
    }
    println("");
    for x in v.iter().rev() {
        print(f"{x} ");
    }
    println("");
    let ps = vec![Person { name: "a".to_string(), n: 1 }, Person { name: "bb".to_string(), n: 2 }];
    for p in ps.iter().filter(|p| p.n > 1) {
        println(f"{p.name}");
    }
    let total = ps.iter().map(|p| p.name.len()).fold(0, |a, b| a + b);
    println(f"{total}");
    println(f"{v.iter().any(|x| *x == 3)} {v.iter().position(|x| *x == 3) ?? -1}");
}
"#,
        "20 4\n0:1 1:2 2:3 3:4 \n4 3 2 1 \nbb\n3\ntrue 2\n",
    );
}

/// `sum`, `reduce` and `collect` produce values, not borrows, when the
/// items are `ref T` of a `Copy` `T`; `cloned` copies each item.
#[test]
fn sum_reduce_cloned_and_collect_read_through_ref_items() {
    assert_runs(
        r#"
fn main() {
    let v = vec![1, 2, 3, 4];
    let s: i64 = v.iter().sum();
    let fs = vec![1.5, 2.25];
    let fsum: f64 = fs.iter().sum();
    let m = v.iter().reduce(|a, b| if a > b { a } else { b }) ?? 0;
    let w: Vec[i64] = v.iter().cloned().filter(|x| *x % 2 == 0).collect();
    let e: Vec[i64] = v.iter().map(|x| x * 10).collect();
    println(f"{s} {fsum} {m} {w.len()} {w[1]} {e[3]}");
}
"#,
        "10 3.75 4 2 4 40\n",
    );
}

/// `collect` goes through `FromIterator`, so the annotation picks the
/// collection: `Set`, `Map` and `String` as well as `Vec`.
#[test]
fn collect_builds_the_annotated_collection() {
    assert_runs(
        r#"
fn main() {
    let v = vec![1, 2, 2];
    let s: Set[i64] = v.iter().collect();
    println(f"{s.len()}");
    let ks = vec![1, 2];
    let m: Map[i64, i64] = ks.iter().map(|k| (*k, *k * 10)).collect();
    println(f"{m.len()} {m[2]}");
    let cs = vec!['a', 'b'];
    let st: String = cs.iter().cloned().collect();
    println(st);
}
"#,
        "2\n2 20\nab\n",
    );
}

/// An item borrows the collection, not the iterator (core semantics §5.4):
/// two `next` results are live together, and `nth`, `last` and `find`
/// return items from their own iterator.
#[test]
fn an_item_borrows_the_collection_not_the_iterator() {
    assert_runs(
        r#"
fn main() {
    let v = vec![1, 2, 3];
    let mut it = v.iter();
    let a = it.next();
    let b = it.next();
    println(f"{a ?? 0} {b ?? 0}");
    println(f"{v.iter().nth(1) ?? 0} {v.iter().last() ?? 0}");
    let f = v.iter().find(|x| *x > 1);
    println(f"{f ?? 0}");
}
"#,
        "1 2\n2 3\n2\n",
    );
}

/// An iterator whose `next` returns a borrow of what it owns is refused:
/// its items would point into the iterator.
#[test]
fn a_lending_next_is_refused() {
    let src = r#"
struct Own { buf: Vec[i64], i: i64 }
impl Iterator for Own {
    type Item = ref i64;
    fn next(mut ref self) -> Option[Self.Item] {
        let i = self.i;
        self.i = i + 1;
        self.buf.get(i)
    }
}
fn main() {
    let mut o = Own { buf: vec![1, 2], i: 0 };
    println(f"{o.next() ?? 0}");
}
"#;
    match run(src) {
        Err(e) if e.contains("Own.next") && e.contains("[E0509]") => {}
        other => panic!("want E0509 in Own.next, got {other:?}"),
    }
}

/// A method's own type parameter can be filled from another argument, so
/// `pick`'s `U` may borrow the receiver's place: the result keeps the
/// receiver borrowed and the write after it is refused.
#[test]
fn a_method_s_own_type_parameter_keeps_the_receiver_borrowed() {
    let src = r#"
struct S { name: String }
impl S {
    fn name_ref(ref self) -> ref String { self.name }
    fn pick[U](ref self, f: own Fn(ref S) -> U) -> U { f(self) }
}
fn main() {
    let mut s = S { name: "a".to_string() };
    let r = s.pick(|x| x.name_ref());
    s.name = "b".to_string();
    println(r);
}
"#;
    match run(src) {
        Err(e) if e.contains("write of _1.0 while it is borrowed") => {}
        other => panic!("want the write refused, got {other:?}"),
    }
}

/// A `ref` item is read where a value is expected (core semantics §5.10,
/// §6.1): in a tuple literal pushed into a `Vec`, and a shared handle as a
/// counted copy.
#[test]
fn a_ref_item_reads_as_a_value_in_a_value_slot() {
    assert_runs(
        r#"
shared struct Node { val: i64 }
fn mk() -> Vec[(i64, i64)] {
    let mut pairs: Vec[(i64, i64)] = Vec.new();
    let v = vec![6, 5];
    for j in v.iter() {
        pairs.push((1, j));
    }
    pairs.sort();
    return pairs;
}
fn main() {
    for p in mk().iter() {
        let (i, j) = p;
        print(f"{i}:{j} ");
    }
    let xs = vec![Node { val: 3 }];
    let mut q: Vec[Node] = Vec.new();
    for x in xs.iter() {
        q.push(x);
    }
    println(f"{q[0].val} {xs[0].val}");
}
"#,
        "1:5 1:6 3 3\n",
    );
}

/// `flat_map`, `flatten` and `scan` are adapter structs in Kāra; the inner
/// iterator moves through `Option.take` and back.
#[test]
fn flat_map_flatten_and_scan_run_as_kara_structs() {
    assert_runs(
        r#"
fn main() {
    let v = vec![1, 2, 3];
    let w: Vec[Vec[i64]] = vec![vec![1, 2], Vec.new(), vec![3]];
    let a: Vec[i64] = w.iter().flat_map(|p| p.iter()).collect();
    println(f"{a}");
    let mut n = 0;
    for x in w.iter().map(|p| p.iter()).flatten() { n = n + x; }
    println(f"{n}");
    let c: Vec[i64] = v.iter().scan(0, |acc, x| Some((acc + x, acc + x))).collect();
    println(f"{c}");
    let d: Vec[i64] = v.iter().scan(0, |acc, x| if x < 3 { Some((acc, x)) } else { None }).collect();
    println(f"{d}");
}
"#,
        "[1, 2, 3]\n6\n[1, 3, 6]\n[1, 2]\n",
    );
}

/// `peek` lends the next item without taking it; `by_ref` lets a loop stop
/// part-way and leaves the rest in the iterator.
#[test]
fn peekable_and_by_ref_keep_the_rest() {
    assert_runs(
        r#"
fn main() {
    let v = vec![1, 2, 3];
    let mut p = v.iter().peekable();
    match p.peek() { Some(x) => println(f"peek {x}"), None => println("none") }
    match p.peek() { Some(x) => println(f"again {x}"), None => println("none") }
    match p.next() { Some(x) => println(f"next {x}"), None => println("none") }
    println(f"count {p.count()}");
    let mut e = v.iter().take(0).peekable();
    if e.peek().is_none() { println("empty"); }
    let mut it = v.iter();
    for x in it.by_ref() { if x == 2 { break; } }
    let rest: Vec[i64] = it.collect();
    println(f"{rest}");
}
"#,
        "peek 1\nagain 1\nnext 1\ncount 2\nempty\n[3]\n",
    );
}

/// `collect` reads a `ref` item as a value only when the item is `Copy`;
/// `ref String` items gathered as `String` need `.cloned()`, and the
/// builder says so instead of sharing the collection's buffers.
#[test]
fn collecting_ref_strings_as_strings_needs_cloned() {
    let src = r#"
fn main() {
    let v: Vec[String] = ["alpha", "beta"];
    let r: Vec[String] = v.iter().rev().collect();
    println(f"{r}");
}
"#;
    let err = run(src).unwrap_err();
    assert!(err.contains("write `.cloned()` before `collect`"), "{err}");
    assert_runs(
        &src.replace(".rev().collect()", ".rev().cloned().collect()"),
        "[beta, alpha]\n",
    );
}

/// §5.10: `(i64, ref i64)` items collected as `(i64, i64)` copy the viewed
/// part, so the result outlives a temporary source.
#[test]
fn a_tuple_with_a_ref_to_a_copy_part_collects_by_value() {
    assert_runs(
        r#"
fn nums() -> Vec[i64] {
    return Vec[10, 20, 30];
}
fn main() {
    let pf: Vec[(i64, i64)] = nums().iter().enumerate().collect();
    println(f"{pf.len()} {pf[1].0} {pf[2].1}");
}
"#,
        "3 1 30\n",
    );
}

/// A set's, a map's and a sorted collection's `iter()` are library sources
/// (`SetIter`, `MapIter`, ...) that borrow the collection, so the adaptors
/// run over them. A sorted one also runs backwards.
#[test]
fn set_and_map_iterators_are_library_sources() {
    let src = r#"
fn main() {
    let mut s: Set[i64] = Set.new();
    s.insert(3);
    s.insert(5);
    let t: i64 = s.iter().map(|x| x * 2).sum();
    println(f"{t}");
    let mut ss: SortedSet[String] = SortedSet.new();
    ss.insert("b".to_string());
    ss.insert("a".to_string());
    let back: Vec[String] = ss.iter().rev().cloned().collect();
    println(f"{back[0]}{back[1]}");
    let mut m: SortedMap[String, i64] = SortedMap.new();
    m.insert("a".to_string(), 1);
    m.insert("b".to_string(), 2);
    let ks: Vec[String] = m.iter().map(|(k, v)| f"{k}{v}").collect();
    println(f"{ks[0]}{ks[1]}");
    let mut h: Map[String, i64] = Map.new();
    h.insert("x".to_string(), 4);
    h.insert("y".to_string(), 6);
    let big = h.iter().filter(|(_, v)| *v > 5).count();
    println(f"{big}");
    for (k, v) in m.iter() {
        println(f"{k}={v}");
    }
}
"#;
    assert_runs(src, "16\nba\na1b2\n1\na=1\nb=2\n");
}

/// `String.chars()` is the library's `Chars`: it steps a byte position by
/// each char's width, so multi-byte chars come out whole.
#[test]
fn a_string_s_chars_are_a_library_source() {
    let src = r#"
fn main() {
    let s = "héllo".to_string();
    let n = s.chars().filter(|c| c != 'l').count();
    println(f"{n}");
    let kept: String = s.chars().filter(|c| c != 'é').collect();
    println(kept);
    let mut w = 0;
    for c in s.chars() {
        w = w + c.len_utf8();
    }
    println(f"{w}");
}
"#;
    assert_runs(src, "3\nhllo\n6\n");
}

/// A program's own `Skip`, `Chars` or `Take` (a struct, or a variant) does
/// not clash with the library's adaptor structs, which are named apart
/// (`IterSkip`, `StrChars`, ...).
#[test]
fn a_program_s_own_skip_or_chars_does_not_clash_with_the_library() {
    let src = r#"
struct Skip { n: i64 }
struct Chars { k: i64 }
enum Token { Letter(u8), Take(i64) }
fn main() {
    let s = Skip { n: 3 };
    let c = Chars { k: 4 };
    let t = Token.Take(5);
    let v = vec![1, 2, 3, 4];
    let total: i64 = v.iter().skip(1).take(2).sum();
    match t {
        Token.Take(x) => println(f"{s.n} {c.k} {x} {total}"),
        Token.Letter(_) => println("letter"),
    }
}
"#;
    assert_runs(src, "3 4 5 5\n");
}

/// `chain`'s bound `Other: Iterator[Item = Self.Item]` is checked against
/// the receiver: a side yielding `ref i64` does not chain with one yielding
/// `i64`, and the error says `.cloned()` is the fix; the fixed spelling runs.
#[test]
fn chain_needs_both_sides_to_yield_the_same_item() {
    let src = r#"
fn main() {
    let a = vec![1, 2];
    let b = vec![10, 20];
    let r: Vec[i64] = (0..3).chain(b.iter()).collect();
    println(f"{r.len()}");
}
"#;
    let err = match run(src) {
        Err(e) => e,
        other => panic!("want a refusal, got {other:?}"),
    };
    assert!(
        err.contains("`J.Item = i64` is not satisfied") && err.contains("`.cloned()`"),
        "{err}"
    );
    let src = r#"
fn main() {
    let a = vec![1, 2];
    let b = vec![10, 20];
    let r: Vec[i64] = (0..3).chain(b.iter().cloned()).collect();
    let s: Vec[i64] = a.iter().chain(b.iter()).cloned().collect();
    println(f"{r.len()} {r[3]} {s.len()} {s[2]}");
}
"#;
    assert_runs(src, "5 10 4 10\n");
}

/// A string literal passed for a `Str` parameter is a view of static text:
/// no `String` is built for it (the 10-10 static-literal ruling), while a
/// `String` passed there is viewed through `as_str`.
#[test]
fn a_literal_for_a_str_parameter_allocates_nothing() {
    let src = r#"
fn shout(s: Str) -> i64 { s.len() }
fn main() {
    let owned = "xy".to_string();
    println(shout("abc") + shout(owned));
}
"#;
    assert_runs(src, "5\n");
    let l = karac::mir::lower::build_source(src).expect("MIR");
    let main = karac::mir::pretty::pretty_body(&l.program.bodies["main"], &l.tys);
    assert!(main.contains("shout(const \"abc\")"), "{main}");
    assert!(!main.contains("String.from(const \"abc\")"), "{main}");
}

/// `collect()` passed straight to `push` takes the `Vec`'s element type as
/// its target, as a `let` annotation would give it (B-2026-10-12-34).
#[test]
fn collect_into_a_push_takes_the_element_type() {
    let src = r#"
fn main() {
    let cs: Vec[char] = "hey".chars().collect();
    let mut v: Vec[String] = Vec.new();
    v.push(cs.iter().collect());
    let mut w: Vec[Vec[char]] = Vec.new();
    w.push(cs.iter().cloned().collect());
    println(f"{v[0]} {w[0].len()}");
}
"#;
    assert_runs(src, "hey 3\n");
}

/// A slice iterates through the library's `SliceIter`, so its `collect()`
/// fills the annotated collection, a `String` of chars included
/// (B-2026-10-12-32).
#[test]
fn a_slice_iterator_collects_into_the_annotated_type() {
    let src = r#"
fn total(s: Slice[i64]) -> i64 {
    let mut n = 0;
    for x in s.iter() {
        n += x;
    }
    n
}

fn main() {
    let cs: Vec[char] = "hello".chars().collect();
    let b: String = cs[1..3].iter().collect();
    let c: Vec[char] = cs[2..].iter().cloned().collect();
    let v: Vec[i64] = [4, 5, 6, 7].iter().cloned().collect();
    let firsts: Vec[i64] = v[1..3].iter().map(|x| x * 10).collect();
    println(f"{b} {c.len()} {total(v[..2])} {firsts[1]} {v[1..].iter().count()}");
}
"#;
    assert_runs(src, "el 3 9 60 3\n");
}

/// `chars()` and a slice's `iter()` run from either end, so `.rev()`
/// reverses them; `chars()` steps back over a multi-byte char whole
/// (B-2026-10-12-35).
#[test]
fn chars_and_a_slice_iterator_run_backwards() {
    let src = r#"
fn main() {
    let s = "héllo→x";
    let r: String = s.chars().rev().collect();
    println(r);
    let mut it = s.chars();
    let a = it.next_back();
    let b = it.next();
    println(f"{a} {b} {it.count()}");
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v.push(2);
    v.push(3);
    let w: Vec[i64] = v[1..].iter().rev().cloned().collect();
    println(f"{w[0]} {w.len()}");
}
"#;
    assert_runs(src, "x→olléh\nSome(x) Some(h) 5\n3 2\n");
}

/// A deque's `iter()` is an iterator value like a Vec's, so it can be
/// stepped, summed, reversed and collected, not only looped over.
#[test]
fn a_deque_iterator_is_a_value() {
    let src = r#"
fn main() {
    let mut d: VecDeque[i64] = VecDeque.new();
    d.push_back(9);
    d.push_front(1);
    d.push_back(4);
    let Some(q) = d.iter().next() else { return; };
    let t: i64 = d.iter().sum();
    let r: Vec[i64] = d.iter().rev().cloned().collect();
    println(f"{q} {t} {r[0]} {r.len()}");
}
"#;
    assert_runs(src, "1 14 4 3\n");
}

/// `v.into_iter()` is an iterator value that moves the elements out, so it
/// collects into any target, runs from either end, and drops what it did
/// not hand out front to back; a `for` loop over a borrowed `v` still
/// lends (B-2026-10-13-4).
#[test]
fn into_iter_is_a_value_that_moves_the_elements_out() {
    let src = r#"
struct D { n: i64 }
impl Drop for D { fn drop(mut ref self) { println(f"drop {self.n}"); } }
fn weigh(v: Vec[i64]) -> i64 {
    let mut t = 0;
    for (i, x) in v.into_iter().enumerate() { t += i * x; }
    for x in v.into_iter() { t += x; }
    t
}
fn take(ds: own Vec[D]) {
    let mut it = ds.into_iter();
    {
        let a = it.next();
        let b = it.next_back();
        println("took two");
    }
    println("the rest");
}
fn main() {
    let cs: Vec[char] = "hello".chars().collect();
    let s: String = cs.into_iter().collect();
    let v: Vec[i64] = [1, 2, 3].iter().cloned().collect();
    let w: Vec[i64] = v.into_iter().rev().map(|x| x * 2).collect();
    println(f"{s} {w[0]} {weigh(w)}");
    let mut ds: Vec[D] = Vec.new();
    ds.push(D { n: 1 });
    ds.push(D { n: 2 });
    ds.push(D { n: 3 });
    ds.push(D { n: 4 });
    take(ds);
}
"#;
    assert_runs(
        src,
        "hello 6 20\ntook two\ndrop 4\ndrop 1\nthe rest\ndrop 2\ndrop 3\n",
    );
}

/// The spec's terminal methods are provided methods of `Iterator`
/// (docs/library/iterators.md; B-2026-10-13-7): `min` / `min_by` /
/// `min_by_key` keep the first of several equal items and the `max`es the
/// last, `product` of nothing is 1, `find_map` stops at its first `Some`, and
/// `partition` keeps the items' order on both sides.
#[test]
fn the_terminal_methods_min_max_product_find_map_partition() {
    let src = r#"
struct P { name: String, age: i64 }

fn main() {
    let ps: Vec[P] = vec![
        P { name: "ann".to_string(), age: 30 },
        P { name: "bob".to_string(), age: 25 },
        P { name: "cat".to_string(), age: 30 },
        P { name: "dan".to_string(), age: 25 },
    ];
    let young = ps.iter().min_by_key(|p| p.age);
    let old = ps.iter().max_by_key(|p| p.age);
    match (young, old) {
        (Some(y), Some(o)) => println(f"{y.name} {o.name}"),
        _ => println("none"),
    }
    let by = ps.iter().max_by(|a, b| a.age.cmp(b.age));
    let lo = ps.iter().min_by(|a, b| a.age.cmp(b.age));
    match (lo, by) {
        (Some(l), Some(b)) => println(f"{l.name} {b.name}"),
        _ => println("none"),
    }
    let words: Vec[String] = vec!["pear".to_string(), "apple".to_string(), "fig".to_string()];
    match (words.iter().min(), words.iter().max()) {
        (Some(a), Some(b)) => println(f"{a} {b}"),
        _ => println("none"),
    }
    let (short, long): (Vec[String], Vec[String]) = words.iter().cloned().partition(|w| w.len() < 4);
    println(f"{short.len()} {long.len()} {short[0]} {long[1]}");
    let fs: Vec[f64] = vec![1.5, 2.0, 4.0];
    let p: f64 = fs.iter().product();
    let empty: Vec[i64] = Vec.new();
    let e: i64 = empty.iter().product();
    let m = empty.iter().max();
    println(f"{p} {e} {m.is_none()}");
    let mut seen = 0;
    let hit = (1..100).find_map(|i| {
        seen += 1;
        if i * i > 50 { Some(i * 10) } else { None }
    });
    println(f"{hit ?? 0} {seen}");
}
"#;
    assert_runs(
        src,
        "bob cat\nbob cat\napple pear\n1 2 fig apple\n12.0 1 true\n80 8\n",
    );
}

/// `max` over `ref` items is a view of the largest (`*` reads it), and a
/// range has the terminals too.
#[test]
fn max_of_a_vec_iterator_is_a_view_and_a_range_has_min() {
    let src = r#"
fn main() {
    let m = vec![3, 1, 2, 2];
    let top = *m.iter().max().unwrap();
    let low: i64 = (0..4).min() ?? -1;
    let wide = m.iter().map(|x| *x * 10).max() ?? 0;
    println(f"{top} {low} {wide}");
}
"#;
    assert_runs(src, "3 0 30\n");
}

/// A bare `s.chars().collect()` builds any `FromIterator[char]` target, a
/// `String` included, not only a `Vec[char]` (B-2026-10-13-10).
#[test]
fn chars_collect_into_a_string() {
    let src = r#"
fn main() {
    let text = String.from("héllo");
    let s: String = text.chars().collect();
    let v: Vec[char] = text.chars().collect();
    let t: String = "abc".chars().collect();
    println(f"{s} {v.len()} {t}");
}
"#;
    assert_runs(src, "héllo 5 abc\n");
}
