//! `dyn Trait` on the MIR interpreter (design.md § `dyn Trait`): the three
//! coercions (an owned value into `dyn Trait`, a borrow into `ref dyn` /
//! `mut ref dyn`, a view of an owned `dyn`), calls through the method table
//! with each receiver mode, supertrait and default methods in the table, a
//! trait's arguments and associated types, and the drop of an owned `dyn`
//! (the concrete value's drop, then the block). The check refuses what the
//! MIR builder does not build yet. Each program's output is worked out by
//! hand; the interpreter also reports any block left at exit.

use karac::mir::interp::Outcome;

/// The interpreter prints `want` and finishes with nothing leaked.
fn runs(src: &str, want: &str) {
    let run = karac::mir::lower::run_source(src).expect("MIR");
    assert!(
        matches!(run.outcome, Outcome::Returned(_)),
        "{:?}\n{}",
        run.outcome,
        run.output
    );
    assert_eq!(run.output, want);
}

/// The check refuses the program with a message containing `want`.
fn refused(src: &str, want: &str) {
    let err = match karac::mir::lower::build_source(src) {
        Ok(_) => panic!("built"),
        Err(e) => e,
    };
    assert!(err.contains(want), "{err}");
}

const SHAPES: &str = r#"trait Shape {
    fn area(self) -> f64;
    fn name(self) -> String { "shape" }
}
struct Circle { r: f64 }
struct Square { side: f64 }
struct Tag { name: String }
impl Shape for Circle {
    fn area(self) -> f64 { 3.0 * self.r * self.r }
}
impl Shape for Square {
    fn area(self) -> f64 { self.side * self.side }
    fn name(self) -> String { "square" }
}
impl Shape for Tag {
    fn area(self) -> f64 { self.name.len() as f64 }
}
fn show(s: ref dyn Shape) { println(f"{s.name()} {s.area()}"); }
"#;

/// A `let`, an argument, a return, a branch and a collection element each
/// coerce, and each call runs the value's own method, a default one
/// included.
#[test]
fn mir_dyn_calls_each_values_own_method() {
    let src = format!(
        "{SHAPES}{}",
        r#"fn make(big: bool) -> dyn Shape {
    if big { Square { side: 3.0 } } else { Circle { r: 1.0 } }
}
fn wrap(t: own Tag) -> dyn Shape { t }
fn main() {
    let a: dyn Shape = Circle { r: 2.0 };
    println(f"{a.area()}");
    show(a);
    let sq = Square { side: 2.0 };
    show(sq);
    show(make(true));
    show(Tag { name: "abcd" });
    let cc = a_circle();
    let rc: ref Circle = ref cc;
    show(rc);
    let w = wrap(Tag { name: "hello" });
    let shapes: Vec[dyn Shape] = [Circle { r: 1.0 }, Square { side: 2.0 }];
    let mut v: Vec[dyn Shape] = Vec.new();
    v.push(w);
    v.push(make(false));
    let mut total = 0.0;
    for s in shapes.iter() { total += s.area(); }
    for s in v.iter() { total += s.area(); }
    println(f"{total}");
}
fn a_circle() -> Circle { Circle { r: 3.0 } }
"#
    );
    runs(
        &src,
        "12.0\nshape 12.0\nsquare 4.0\nsquare 9.0\nshape 4.0\nshape 27.0\n15.0\n",
    );
}

/// A view chosen by a branch is the view it was handed.
#[test]
fn mir_dyn_view_passes_through_a_branch() {
    let src = format!(
        "{SHAPES}{}",
        r#"fn pick(a: ref dyn Shape, b: ref dyn Shape, first: bool) -> f64 {
    let s: ref dyn Shape = if first { a } else { b };
    s.area()
}
fn main() {
    let c = Circle { r: 1.0 };
    let t = Tag { name: "xy" };
    println(f"{pick(c, t, true)} {pick(c, t, false)}");
}
"#
    );
    runs(&src, "3.0 2.0\n");
}

/// `mut ref self` changes the value in place, through an owned `dyn` and
/// through a `mut ref dyn` view of a concrete value; `own self` consumes the
/// `dyn` value; a field, an `Option` and a `Vec` hold them; and each owned
/// `dyn` drops its concrete value, at the end of its owner's scope.
#[test]
fn mir_dyn_mutates_consumes_and_drops() {
    let src = r#"trait Counter {
    fn get(self) -> i64;
    fn bump(mut ref self);
    fn finish(own self) -> String;
}
struct Tally { n: i64, tag: String }
struct Loud { n: i64 }
impl Counter for Tally {
    fn get(self) -> i64 { self.n }
    fn bump(mut ref self) { self.n += 1; }
    fn finish(own self) -> String { f"{self.tag}={self.n}" }
}
impl Counter for Loud {
    fn get(self) -> i64 { self.n * 10 }
    fn bump(mut ref self) { self.n += 2; }
    fn finish(own self) -> String { f"loud {self.n}" }
}
impl Drop for Loud {
    fn drop(mut ref self) { println(f"drop loud {self.n}"); }
}
struct Holder { c: dyn Counter, label: String }
fn twice(c: mut ref dyn Counter) {
    c.bump();
    c.bump();
}
fn main() {
    let mut a: dyn Counter = Tally { n: 1, tag: "t" };
    a.bump();
    twice(mut a);
    println(f"{a.get()}");
    println(a.finish());
    let mut l = Loud { n: 1 };
    twice(mut l);
    println(f"{l.get()}");
    let mut v: Vec[dyn Counter] = Vec.new();
    v.push(Tally { n: 5, tag: "v" });
    v.push(Loud { n: 7 });
    for c in v.iter() {
        println(f"{c.get()}");
    }
    let h = Holder { c: Loud { n: 100 }, label: "h" };
    println(f"{h.label} {h.c.get()}");
    let o: Option[dyn Counter] = Some(Tally { n: 3, tag: "o" });
    match o {
        Some(c) => println(c.finish()),
        None => println("none"),
    }
    let gone: dyn Counter = Loud { n: 9 };
    println(gone.finish());
    println("end");
}
"#;
    runs(
        src,
        "4\nt=4\n50\n5\n70\nh 1000\no=3\ndrop loud 9\nloud 9\nend\ndrop loud 100\ndrop loud 7\ndrop loud 5\n",
    );
}

/// A supertrait's method is in the table, and a default method calling it
/// runs over the concrete value; a trait's argument and an associated type
/// bound in the `dyn` type give the calls their types; a closure captures a
/// `dyn` value.
#[test]
fn mir_dyn_reaches_supertraits_arguments_and_associated_types() {
    let src = r#"trait Named { fn name(self) -> String; }
trait Shape: Named {
    fn area(self) -> f64;
    fn describe(self) -> String { f"{self.name()}:{self.area()}" }
}
trait Conv[T] { fn conv(self, x: T) -> T; }
trait Gen { type Out; fn make(self) -> Self.Out; }
struct Sq { s: f64 }
impl Named for Sq { fn name(self) -> String { "sq" } }
impl Shape for Sq { fn area(self) -> f64 { self.s * self.s } }
struct Add1 {}
impl Conv[i64] for Add1 { fn conv(self, x: i64) -> i64 { x + 1 } }
struct Nums { n: i64 }
impl Gen for Nums { type Out = Vec[i64]; fn make(self) -> Vec[i64] { vec![self.n, self.n] } }
struct Count { i: i64, end: i64 }
impl Iterator for Count {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] {
        if self.i < self.end { self.i += 1; Some(self.i) } else { None }
    }
}
fn show(s: ref dyn Shape) -> String { s.describe() }
fn main() {
    println(show(Sq { s: 2.0 }));
    let s: dyn Shape = Sq { s: 3.0 };
    println(f"{s.name()} {s.describe()}");
    let c: dyn Conv[i64] = Add1 {};
    println(f"{c.conv(41)}");
    let g: dyn Gen[Out = Vec[i64]] = Nums { n: 7 };
    println(f"{g.make().len()}");
    let mut it: dyn Iterator[Item = i64] = Count { i: 0, end: 3 };
    while let Some(x) = it.next() {
        println(f"{x}");
    }
    let f = |k: f64| s.area() * k;
    println(f"{f(2.0)}");
}
"#;
    runs(src, "sq:4.0\nsq sq:9.0\n42\n2\n1\n2\n3\n18.0\n");
}

/// A value of a type that does not implement the trait, or implements it
/// with another associated type, a borrow into an owned `dyn`, a `dyn` type
/// leaving an associated type unbound, and a method the trait does not
/// have are errors. A default method with no slot, and a `dyn` of a
/// primitive, are refused until the builder builds them.
#[test]
fn mir_dyn_refuses_what_it_cannot_coerce_or_call() {
    let cases = [
        (
            "struct P { x: i64 }\nfn main() { let a: dyn Shape = P { x: 1 }; }",
            "expected 'dyn Shape', found 'P': `P` does not implement `Shape`",
        ),
        (
            "fn main() { let c = Circle { r: 1.0 }; let r: ref Circle = ref c; let d: dyn Shape = r; }",
            "expected 'dyn Shape', found 'ref Circle'",
        ),
        (
            "struct C { i: i64 }\nimpl Iterator for C { type Item = i64; fn next(mut ref self) -> Option[i64] { None } }\nfn main() { let c: dyn Iterator = C { i: 0 }; }",
            "`dyn Iterator` leaves the associated type `Item` unbound",
        ),
        (
            "struct C { i: i64 }\nimpl Iterator for C { type Item = i64; fn next(mut ref self) -> Option[i64] { None } }\nfn main() { let c: dyn Iterator[Item = bool] = C { i: 0 }; }",
            "`C` implements `Iterator` with `Item = i64`, not `bool`",
        ),
        (
            "fn main() { let g: dyn Shape = Circle { r: 1.0 }; g.perimeter(); }",
            "no method 'perimeter' on `dyn Shape`",
        ),
        (
            "struct C { i: i64 }\nimpl Iterator for C { type Item = i64; fn next(mut ref self) -> Option[i64] { None } }\nfn main() { let c: dyn Iterator[Item = i64] = C { i: 0 }; let v: Vec[i64] = c.map(|x| x + 1).collect(); }",
            "E_DYN_TRAIT_NOT_IMPLEMENTED_YET]: `map` cannot be called through `dyn Iterator[Item = i64]` yet",
        ),
        (
            "impl Shape for i64 { fn area(self) -> f64 { 1.0 } }\nfn main() { let a: dyn Shape = 5; }",
            "E_DYN_TRAIT_NOT_IMPLEMENTED_YET]: a `dyn` value of `i64` is not built yet",
        ),
    ];
    for (main, want) in cases {
        refused(&format!("{SHAPES}{main}\n"), want);
    }
}

const DEFAULT_COPIES: &str = r#"trait Named { fn id(self) -> i64; }
trait Show: Named {
    fn label(self) -> i64 { show(self) }
}
fn show(d: ref dyn Named) -> i64 { d.id() * 10 }
struct X {}
struct Y { n: i64 }
impl Named for X { fn id(self) -> i64 { 1 } }
impl Named for Y { fn id(self) -> i64 { self.n } }
"#;

/// A default body copied into two impls keeps the trait's spans, and each
/// copy coerces its own `Self` into the `dyn` slot: the checker records one
/// coercion per copy, with that copy's type, whichever impl comes first.
#[test]
fn mir_dyn_records_each_default_copys_own_coercion() {
    for impls in [
        "impl Show for X {}\nimpl Show for Y {}\n",
        "impl Show for Y {}\nimpl Show for X {}\n",
    ] {
        let src = format!(
            "{DEFAULT_COPIES}{impls}fn main() {{ let a = X {{}}.label(); let b = Y {{ n: 2 }}.label(); }}\n"
        );
        let mut program = karac::parse(&src).program;
        karac::prepare_for_resolve(&mut program);
        // As the MIR front end reads it: a bare parameter borrows (D5).
        karac::d5_params::borrow_bare_params(&mut program);
        let resolved = karac::resolve_v2(&program);
        let typed = karac::typecheck_with_library_source(&program, &resolved);
        assert!(typed.errors.is_empty(), "{:?}", typed.errors);
        let mut concrete: Vec<String> = typed
            .node_dyn_coercions
            .values()
            .map(|(c, _)| karac::typechecker::type_display(c))
            .collect();
        concrete.sort();
        assert_eq!(concrete, ["X", "Y"], "{impls}");
        runs(
            &format!(
                "{DEFAULT_COPIES}{impls}fn main() {{ println(f\"{{X {{}}.label()}} {{Y {{ n: 2 }}.label()}}\"); }}\n"
            ),
            "10 20\n",
        );
    }
}

/// A fresh binding lent into a `mut ref dyn` parameter takes the `mut`
/// marker, as into any `mut ref` parameter; a method naming `Self` inside
/// another type has no slot.
#[test]
fn mir_dyn_takes_the_mut_marker_and_leaves_nested_self_out() {
    let counter = r#"trait Counter { fn bump(mut ref self); fn get(self) -> i64; }
struct T { n: i64 }
impl Counter for T { fn bump(mut ref self) { self.n += 1; } fn get(self) -> i64 { self.n } }
fn g(x: mut ref dyn Counter) { x.bump(); }
"#;
    refused(
        &format!("{counter}fn main() {{ let mut s = T {{ n: 1 }}; g(s); }}\n"),
        "call with fresh binding requires a `mut` marker",
    );
    runs(
        &format!("{counter}fn main() {{ let mut s = T {{ n: 1 }}; g(mut s); println(f\"{{s.get()}}\"); }}\n"),
        "2\n",
    );
    refused(
        r#"trait Pair { fn pair(self) -> Vec[Self]; fn one(self) -> i64; }
struct P { n: i64 }
impl Pair for P { fn pair(self) -> Vec[P] { vec![P { n: self.n }] } fn one(self) -> i64 { self.n } }
fn main() { let d: dyn Pair = P { n: 1 }; let x = d.one(); let v = d.pair(); }
"#,
        "`pair` cannot be called through `dyn Pair` yet",
    );
}
