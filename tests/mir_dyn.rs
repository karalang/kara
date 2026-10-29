//! `dyn Trait` on the MIR interpreter (design.md § `dyn Trait`): the three
//! coercions (an owned value into `dyn Trait`, a borrow into `ref dyn` /
//! `mut ref dyn`, a view of an owned `dyn`), calls through the method table
//! with each receiver mode, supertrait and default methods in the table, a
//! trait's arguments and associated types, and the drop of an owned `dyn`
//! (the concrete value's drop, then the block). The check refuses what the
//! MIR builder does not build yet. Each program's output is worked out by
//! hand; the interpreter also reports any block left at exit.
//!
//! With `--features llvm` each program is also compiled, at -O0 and -O2,
//! and must print the same.

#[cfg(feature = "llvm")]
mod common;

use karac::mir::interp::Outcome;

/// The interpreter prints `want` and finishes with nothing leaked, and so
/// does the program compiled.
fn runs(src: &str, want: &str) {
    let run = karac::mir::lower::run_source(src).expect("MIR");
    assert!(
        matches!(run.outcome, Outcome::Returned(_)),
        "{:?}\n{}",
        run.outcome,
        run.output
    );
    assert_eq!(run.output, want);
    #[cfg(feature = "llvm")]
    for opt in [0, 2] {
        if let Some((ok, out)) = compiled::build_and_run(src, opt) {
            assert!(ok, "at -O{opt}: {out}");
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[cfg(feature = "llvm")]
mod compiled {
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// What the program prints compiled at `opt`, and whether it exited
    /// cleanly; `None` when there is no runtime archive to link.
    pub fn build_and_run(src: &str, opt: u8) -> Option<(bool, String)> {
        static N: AtomicUsize = AtomicUsize::new(0);
        let lowered = karac::mir::lower::build_source(src).expect("MIR");
        karac::mir::lower::compiled_check(&lowered).expect("checks");
        let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("karac-dyn-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let (obj, exe) = (dir.join("main.o"), dir.join("main"));
        karac::backend::mir_aot::compile_object(&ir, &obj, opt).expect("object");
        let linked = super::common::link_or_skip(karac::backend::driver::link_executable(
            &obj.to_string_lossy(),
            &exe.to_string_lossy(),
        ));
        let out = linked.map(|()| Command::new(&exe).output().expect("run"));
        let _ = std::fs::remove_dir_all(&dir);
        let out = out?;
        Some((
            out.status.success(),
            String::from_utf8(out.stdout).expect("utf-8"),
        ))
    }
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
/// have are errors. A `dyn` of a type parameter is refused until the
/// builder builds it.
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
            "fn wrap[T: Shape](t: own T) -> dyn Shape { t }\nfn main() { let a = wrap(Circle { r: 1.0 }); }",
            "E_DYN_TRAIT_NOT_IMPLEMENTED_YET]: a `dyn` value of the type parameter `T` is not built yet",
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
        "`pair` cannot be called through `dyn Pair`",
    );
}

const COUNT: &str = r#"struct Count { i: i64, end: i64 }
impl Iterator for Count {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] {
        if self.i < self.end { self.i += 1; Some(self.i) } else { None }
    }
}
fn make(n: i64) -> dyn Iterator[Item = i64] { Count { i: 0, end: n } }
"#;

/// A default method with no slot (`map`, `filter`, `sum`, `collect`, which
/// are generic or take `Self` by value) runs its body with `Self` the `dyn`
/// type, its calls to `next` dispatching; one with a slot (`any`) runs the
/// value's own copy. A `for` loop over an owned `dyn` iterator calls its
/// `next` slot, and one through `mut ref dyn` reads it in place.
#[test]
fn mir_dyn_runs_iterator_default_methods() {
    let src = format!(
        "{COUNT}{}",
        r#"fn drain(it: mut ref dyn Iterator[Item = i64]) -> i64 {
    let mut t = 0;
    while let Some(x) = it.next() { t += x; }
    t
}
fn main() {
    let s: i64 = make(4).sum();
    let c = make(5).count();
    let f = make(5).fold(0, |a, x| a + x * x);
    let v: Vec[i64] = make(6).filter(|x| *x % 2 == 0).collect();
    let m: Vec[bool] = make(3).map(|x| x == 2).collect();
    println(f"{s} {c} {f} {v} {m}");
    let mut it = make(5);
    let a = it.any(|x| x == 2);
    let n = it.next();
    for x in make(2) { println(f"x {x}"); }
    let mut total = 0;
    for x in it { total += x; }
    println(f"{a} {n} {total}");
    let mut d: dyn Iterator[Item = i64] = Count { i: 0, end: 3 };
    let mut k = Count { i: 0, end: 4 };
    println(f"{drain(mut d)} {drain(mut k)}");
    let words: dyn Iterator[Item = String] = vec!["a", "", "bc"].into_iter().map(|w| w.to_string());
    let kept: Vec[String] = words.filter(|w| w.len() > 0).collect();
    println(f"{kept}");
}
"#
    );
    runs(
        &src,
        "10 5 55 [2, 4, 6] [false, true, false]\nx 1\nx 2\ntrue Some(3) 9\n6 10\n[a, bc]\n",
    );
}

/// A generic function's `[I: Iterator]` takes a `dyn` iterator, and its
/// calls of default methods run them at the `dyn` type; a `Vec` of them
/// is consumed one by one.
#[test]
fn mir_dyn_fills_a_generic_parameter() {
    let src = format!(
        "{COUNT}{}",
        r#"fn total[I: Iterator[Item = i64]](it: own I) -> i64 { it.sum() }
fn main() {
    println(f"{total(make(4))}");
    let v: Vec[dyn Iterator[Item = i64]] = vec![make(2), make(3)];
    for it in v.into_iter() {
        let w: Vec[i64] = it.map(|x| x + 1).collect();
        println(f"{w}");
    }
}
"#
    );
    runs(&src, "10\n[2, 3]\n[2, 3, 4]\n");
}

/// A user trait's generic default methods run over a `dyn` value and a
/// view of one, a supertrait's included, and one default calls another
/// and the slots.
#[test]
fn mir_dyn_runs_generic_user_default_methods() {
    let src = r#"trait Named {
    fn id(self) -> i64;
    fn tag[T](self, t: own T) -> (i64, T) { (self.id(), t) }
}
trait Shape: Named {
    fn area(self) -> i64;
    fn both[U](self, u: own U) -> (i64, U) {
        let p: (i64, U) = self.tag(u);
        (p.0 + self.area(), p.1)
    }
}
struct Sq { s: i64 }
impl Named for Sq { fn id(self) -> i64 { 100 } }
impl Shape for Sq { fn area(self) -> i64 { self.s * self.s } }
struct Pt {}
impl Named for Pt { fn id(self) -> i64 { 7 } }
impl Shape for Pt { fn area(self) -> i64 { 0 } }
fn main() {
    let d: dyn Shape = Sq { s: 3 };
    let (a, b) = d.tag("x");
    let (c, e) = d.both(true);
    println(f"{a} {b} {c} {e}");
    let shapes: Vec[dyn Shape] = vec![Sq { s: 2 }, Pt {}];
    for s in shapes.iter() {
        let (n, w) = s.both("w");
        println(f"{n} {w}");
    }
}
"#;
    runs(src, "100 x 109 true\n104 w\n7 w\n");
}

/// A number, a `bool`, a `char`, a `String` and a `Vec` become `dyn`
/// values as a struct does, through their impls of a user trait: in a
/// `let`, an argument, a return, a collection's element, a map's value and
/// an `Option`, owned and borrowed, with default and `own self` methods.
/// The string's and the vector's blocks are dropped with the `dyn` value.
#[test]
fn mir_dyn_holds_numbers_strings_and_vectors() {
    runs(
        r#"trait Shape {
    fn area(ref self) -> i64;

    fn twice(ref self) -> i64 {
        self.area() * 2
    }
}

struct Sq {
    s: i64,
}

impl Shape for Sq {
    fn area(ref self) -> i64 {
        self.s * self.s
    }
}

impl Shape for i64 {
    fn area(ref self) -> i64 {
        *self + 1
    }
}

impl Shape for String {
    fn area(ref self) -> i64 {
        self.len()
    }
}

impl Shape for f64 {
    fn area(ref self) -> i64 {
        *self as i64
    }
}

fn total(v: ref Vec[dyn Shape]) -> i64 {
    let mut t = 0;
    for s in v.iter() {
        t += s.area() + s.twice();
    }
    t
}

fn main() {
    let mut v: Vec[dyn Shape] = Vec.new();
    v.push(Sq { s: 3 });
    v.push(4);
    v.push("hello");
    v.push(2.5);
    v.push(String.from("ab"));
    println(f"{total(v)}");
    let b: dyn Shape = 9;
    println(f"{b.area()}");
}
"#,
        "69\n10\n",
    );
    runs(
        r#"trait Show {
    fn show(ref self) -> String;

    fn eat(own self) -> i64 {
        self.show().len()
    }
}

impl Show for i64 {
    fn show(ref self) -> String {
        f"int {self}"
    }
}

impl Show for bool {
    fn show(ref self) -> String {
        (if *self { "yes" } else { "no" }).to_string()
    }
}

impl Show for Vec[i64] {
    fn show(ref self) -> String {
        f"vec of {self.len()}"
    }
}

impl Show for char {
    fn show(ref self) -> String {
        f"char {self}"
    }
}

fn wrap(s: own String) -> dyn Show {
    s
}

impl Show for String {
    fn show(ref self) -> String {
        f"string {self}"
    }
}

fn view(s: ref dyn Show) -> String {
    s.show()
}

fn main() {
    let x = 41;
    let v = vec![1, 2, 3];
    println(view(x));
    println(view(v));
    let items: Vec[dyn Show] = vec![7, true, vec![5, 6], 'q'];
    for it in items.iter() {
        println(it.show());
    }
    let w = wrap("hi".to_string());
    println(w.show());
    let u: dyn Show = vec![9];
    println(f"{u.eat()}");
    let m: Map[String, dyn Show] = Map.new();
    let mut m = m;
    m.insert("a", 3);
    m.insert("b", false);
    println(m.get("a").unwrap().show());
    let o: Option[dyn Show] = Some(5);
    if let Some(s) = o {
        println(f"{s.eat()}");
    }
}
"#,
        "int 41\nvec of 3\nint 7\nyes\nvec of 2\nchar q\nstring hi\n8\nint 3\n5\n",
    );
}

/// A `dyn Display` (and a trait under `Display`) shows through its
/// `to_string` slot: a call, an f-string, `println` of the value, a
/// `ref dyn Display` parameter and a `Vec[dyn Display]`, for a struct with
/// its own `impl Display` and for a number, a string, a `bool` and a `char`,
/// whose `to_string` is the library's (B-2026-10-29-31). A generic
/// `[T: Display]` body calls a primitive's `to_string` too.
#[test]
fn mir_dyn_display_shows_through_its_to_string_slot() {
    runs(
        r#"
trait Shape: Display {
    fn area(ref self) -> i64;
}

struct Sq {
    s: i64,
}

impl Display for Sq {
    fn to_string(ref self) -> String {
        f"Sq({self.s})"
    }
}

impl Shape for Sq {
    fn area(ref self) -> i64 {
        self.s * self.s
    }
}

fn show(d: ref dyn Display) -> String {
    f"<{d}>"
}

fn stringify[T: Display](x: T) -> String {
    x.to_string()
}

fn main() {
    let a: dyn Display = 5;
    let b: dyn Display = "hi";
    let c: dyn Display = Sq { s: 3 };
    let e: dyn Display = true;
    println(f"{a} {b} {c} {e}");
    println(a);
    println(c.to_string());
    let s: dyn Shape = Sq { s: 4 };
    println(f"{s} has area {s.area()}");
    let mut v: Vec[dyn Display] = Vec.new();
    v.push(1.5);
    v.push(String.from("owned"));
    v.push(Sq { s: 2 });
    v.push('q');
    for d in v.iter() {
        println(show(d));
    }
    println(stringify(7));
    println(stringify('c'));
    println(stringify(Sq { s: 9 }));
}
"#,
        "5 hi Sq(3) true\n5\nSq(3)\nSq(4) has area 16\n<1.5>\n<owned>\n<Sq(2)>\n<q>\n7\nc\nSq(9)\n",
    );
}
