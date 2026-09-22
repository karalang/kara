//! generics, monomorphisation, traits, associated items -- fixtures for `tests/interpreter.rs`.
//!
//! Split out of `tests/interpreter.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test interpreter` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test interpreter generics::
//!
//! New fixtures about generics, monomorphisation, traits, associated items belong in this file.

use super::*;

/// B-2026-08-31-48 (oracle half) — one generic fn at several nameless-aggregate
/// type arguments.
///
/// The interpreter has no monomorphization at all, so it was correct throughout
/// and is the oracle here: the compiled side gave every `Array` / `Slice` /
/// `Vector` / tuple instantiation of one generic fn the SAME symbol and failed
/// module verification (`call [2 x i64] @"ident$opaque"([3 x i64] %a3)`).
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_generic_fn_at_nameless_aggregate_args_gets_distinct_monos`, pinned to
/// the same string.
#[test]
fn test_generic_fn_at_nameless_aggregate_args() {
    assert_eq!(
        run(r#"struct H { n: i64 }

impl H {
    fn pick[T](self, x: T) -> T { return x }
}

fn ident[T](x: T) -> T { return x }

fn main() {
    let a2: Array[i64, 2] = Array[3, 4];
    let a3: Array[i64, 3] = Array[7, 8, 9];
    let r2: Array[i64, 2] = ident(a2);
    let r3: Array[i64, 3] = ident(a3);
    println(f"len {r2[1]} {r3[2]}");

    let ai: Array[i64, 2] = Array[1, 2];
    let asx: Array[String, 2] = Array["a", "b"];
    let ri: Array[i64, 2] = ident(ai);
    let rs: Array[String, 2] = ident(asx);
    println(f"elem {ri[0]} {rs[1]}");

    let v: Vec[i64] = [5, 6];
    let u: Vec[String] = ["z"];
    let s1: Slice[i64] = v.as_slice();
    let s2: Slice[String] = u.as_slice();
    let q1: Slice[i64] = ident(s1);
    let q2: Slice[String] = ident(s2);
    println(f"slice {q1[1]} {q2[0]}");

    let v4: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);
    let v8: Vector[i64, 8] = Vector[i64, 8](1, 2, 3, 4, 5, 6, 7, 8);
    println(f"vector {ident(v4).reduce_sum()} {ident(v8).reduce_sum()}");

    let t1: (i64, i64) = (1, 2);
    let t2: (i64, String) = (3, "x");
    println(f"tuple {ident(t1).0} {ident(t2).0}");

    let m2: Array[i64, 2] = H { n: 0 }.pick(a2);
    let m3: Array[i64, 3] = H { n: 0 }.pick(a3);
    println(f"method {m2[0]} {m3[0]}");

    let st: String = "hi";
    let bv: Vec[i64] = [8, 9];
    let bu: Vec[String] = ["k"];
    let cs: String = ident(st);
    let cv: Vec[i64] = ident(bv);
    let cu: Vec[String] = ident(bu);
    println(f"prior {cs} {cv[0]} {cu[0]}");
}
"#),
        "len 4 9\nelem 1 b\nslice 6 z\nvector 10 36\ntuple 1 3\nmethod 3 7\nprior hi 8 k\n"
    );
}

/// B-2026-08-30-20 — the interpreter twin of `tests/codegen.rs`'s
/// `e2e_assoc_call_produced_argument_owns_its_value`.
///
/// This side had the SAME hole as codegen, one arm over: the fn-returned
/// Drop-temp classifier recognised a qualified UNIT VARIANT (`Sig.B`) but not
/// an associated fn (`H.mkr(1)`), so an argument produced by one carried no
/// owner here either. All four surfaces agreeing on the omission is exactly
/// what made the defect invisible to every A/B parity gate in the tree — the
/// only thing that can see it is an absolute expectation against the
/// free-function producer, which is what this asserts.
#[test]
fn test_assoc_call_produced_argument_owns_its_value() {
    let hdr = "struct R { id: i64, s: String }\n\
               impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
               fn mk(i: i64) -> String { return f\"pay-{i}-aaaa\" }\n\
               fn s1(a: R) -> i64 { return 7 }\n";
    let assoc = run(&format!(
        "{hdr}struct H {{ n: i64 }}\n\
         impl H {{ fn mkr(i: i64) -> R {{ return R {{ id: i, s: mk(i) }} }} }}\n\
         fn main() {{ let v = s1(H.mkr(1)); println(f\"v={{v}}\") }}"
    ));
    let free = run(&format!(
        "{hdr}fn mkr2(i: i64) -> R {{ return R {{ id: i, s: mk(i) }} }}\n\
         fn main() {{ let v = s1(mkr2(1)); println(f\"v={{v}}\") }}"
    ));
    assert_eq!(free, "dR1\nv=7\n", "the free-function oracle itself moved");
    assert_eq!(
        assoc, free,
        "an associated-call producer must own its argument exactly as a free one does"
    );
}

#[test]
fn fn_value_let_bound_then_called() {
    assert_eq!(
        run("fn doubler(n: i64) -> i64 { n * 2 }\n\
             fn main() { let f = doubler; println(f(21)); }\n"),
        "42\n"
    );
}

#[test]
fn tuple_type_arg_through_a_nested_generic_call_in_the_interpreter() {
    // B-2026-08-27-40 -- the ORACLE for
    // `test_e2e_tuple_type_arg_survives_a_nested_generic_call`, kept
    // row-for-row with it so each is the other's check.
    //
    // The interpreter was never wrong here, and that is the point: it carries
    // a generic binding as a `Value`, not as a NAME, so a type argument with
    // no name costs it nothing. The compiled backend threaded the binding
    // through `type_subst_names` and silently dropped the tuple at the first
    // nested call -- `T` fell to the `i64` default and a 16-byte element was
    // swapped 8 bytes at a time. This fixture is what pins the answer the
    // compiled twin has to match; without it, "both backends agree" could be
    // satisfied by both being wrong.
    let src = r#"
struct Bag[=T] { xs: Vec[T] }

impl[T] Bag[T] {
    fn sw(mut ref self, i: i64, j: i64) { self.xs.swap(i, j); }
    fn mid(mut ref self) { self.sw(0, 1); }
    fn outer(mut ref self) { self.mid(); }
    fn at(ref self, i: i64) -> T { return self.xs[i]; }
}

fn main() {
    let mut p: Bag[(i64, i64)] = Bag { xs: Vec.new() };
    p.xs.push((1, 10));
    p.xs.push((2, 20));
    p.outer();
    let p0 = p.at(0);
    let p1 = p.at(1);
    println(f"{p0.0}:{p0.1} {p1.0}:{p1.1}");

    let mut t: Bag[(i64, i64, i64)] = Bag { xs: Vec.new() };
    t.xs.push((1, 2, 3));
    t.xs.push((4, 5, 6));
    t.outer();
    let t0 = t.at(0);
    let t1 = t.at(1);
    println(f"{t0.0},{t0.1},{t0.2} {t1.0},{t1.1},{t1.2}");

    let mut s: Bag[(String, i64)] = Bag { xs: Vec.new() };
    s.xs.push(("x" + "y", 1));
    s.xs.push(("p" + "q", 2));
    s.outer();
    let s0 = s.at(0);
    let s1 = s.at(1);
    println(f"{s0.0}:{s0.1} {s1.0}:{s1.1}");

    let mut n: Bag[((i64, i64), i64)] = Bag { xs: Vec.new() };
    n.xs.push(((1, 2), 3));
    n.xs.push(((4, 5), 6));
    n.outer();
    let n0 = n.at(0);
    println(f"{n0.0.0},{n0.0.1},{n0.1}");

    let mut c: Bag[i64] = Bag { xs: Vec.new() };
    c.xs.push(1);
    c.xs.push(2);
    c.outer();
    println(f"{c.at(0)} {c.at(1)}");
}
"#;
    assert_eq!(
        run_no_errors(src),
        "2:20 1:10\n4,5,6 1,2,3\npq:2 xy:1\n4,5,6\n2 1\n"
    );
}

#[test]
fn test_user_impl_eq_drives_equality_operator() {
    // `impl Eq for Point` is registered, and `a == b` lowers to
    // `Point.eq(a, b)` — routed through the user-defined method rather than
    // any structural fallback. CR-202 slice 5b: companion
    // `impl PartialEq for Point` satisfies the new `Eq: PartialEq`
    // supertrait edge so the typecheck pass stays clean.
    assert_eq!(
        run("struct Point { x: i64, y: i64 }
             impl PartialEq for Point {
                 fn eq(ref self, other: ref Point) -> bool {
                     self.x == other.x and self.y == other.y
                 }
             }
             impl Eq for Point {
                 fn eq(self, other: Point) -> bool {
                     self.x == other.x and self.y == other.y
                 }
             }
             fn main() {
                 let a = Point { x: 1, y: 2 };
                 let b = Point { x: 1, y: 2 };
                 let c = Point { x: 9, y: 9 };
                 println(a == b);
                 println(a != c);
             }"),
        "true\ntrue\n"
    );
}

#[test]
fn test_user_impl_ord_cmp_body_drives_comparison_operators() {
    // B-2026-08-26-10 — the IDIOMATIC ordering impl: `trait Ord` declares
    // exactly one method, `cmp`, so this is the shape an author who reads
    // design.md writes. It used to lower to `Item.lt(a, b)` — a method the
    // trait does not declare and this impl does not define — and died with
    // `path 'Item.lt' has no interpreter evaluation rule`. It now lowers to
    // `Item.cmp(a, b).is_lt()`.
    //
    // The `cmp` REVERSES the order on purpose. That is what makes this a test
    // of DISPATCH rather than of compilation: field declaration order would
    // answer `true false true false` for ids 1 vs 5, and the body demands the
    // exact opposite. A `cmp` that merely agreed with declaration order would
    // pass this test while dispatching nowhere.
    assert_eq!(
        run("struct Item { id: i64 }
             impl PartialEq for Item { fn eq(ref self, other: ref Item) -> bool { self.id == other.id } }
             impl Eq for Item {}
             impl PartialOrd for Item {
                 fn partial_cmp(ref self, other: ref Item) -> Option[Ordering] { Some(other.id.cmp(self.id)) }
             }
             impl Ord for Item {
                 fn cmp(ref self, other: ref Item) -> Ordering { other.id.cmp(self.id) }
             }
             fn main() {
                 let a = Item { id: 1 };
                 let b = Item { id: 5 };
                 println(a < b);
                 println(a > b);
                 println(a <= b);
                 println(a >= b);
             }"),
        "false\ntrue\nfalse\ntrue\n"
    );
}

#[test]
fn test_user_impl_ord_drives_comparison_operators() {
    // `impl Ord for Point` with direct `lt`/`le`/`gt`/`ge` methods — `<`
    // lowers to `Point.lt(a, b)`, etc. Domain-specific ordering (here by x
    // only) rather than the interpreter's hardcoded primitive path.
    // CR-202 slice 5d: companion PartialEq/Eq/PartialOrd impls satisfy
    // the new `Ord: PartialOrd + Eq` supertrait edges (typecheck-clean;
    // interpreter execution behavior is unchanged).
    assert_eq!(
        run("struct Point { x: i64, y: i64 }
             impl PartialEq for Point {
                 fn eq(ref self, other: ref Point) -> bool { self.x == other.x and self.y == other.y }
             }
             impl Eq for Point {}
             impl PartialOrd for Point {
                 fn partial_cmp(ref self, other: ref Point) -> Option[Ordering] { Some(self.x.cmp(other.x)) }
             }
             impl Ord for Point {
                 fn lt(self, other: Point) -> bool { self.x < other.x }
                 fn le(self, other: Point) -> bool { self.x <= other.x }
                 fn gt(self, other: Point) -> bool { self.x > other.x }
                 fn ge(self, other: Point) -> bool { self.x >= other.x }
             }
             fn main() {
                 let a = Point { x: 1, y: 99 };
                 let b = Point { x: 5, y: 1 };
                 println(a < b);
                 println(a > b);
                 println(a <= b);
             }"),
        "true\nfalse\ntrue\n"
    );
}

// ── Generic Functions ──────────────────────────────────────────

#[test]
fn test_generic_identity_function() {
    assert_eq!(
        run("fn identity[T](x: T) -> T { x }\n\
             fn main() {\n\
                 println(identity(42));\n\
                 println(identity(true));\n\
             }"),
        "42\ntrue\n"
    );
}

#[test]
fn test_generic_pair() {
    assert_eq!(
        run("struct Pair[A, B] { first: A, second: B }\n\
             fn main() {\n\
                 let p = Pair { first: 1, second: true };\n\
                 println(p.first);\n\
                 println(p.second);\n\
             }"),
        "1\ntrue\n"
    );
}

// ── Trait associated function dispatch (List 1, item 5) ─────────

#[test]
fn test_bare_assoc_fn_with_concrete_expected_type() {
    // `let w: Foo = default()` resolves through `impl Default for Foo`.
    let output = run(r#"
trait Default {
    fn default() -> Self;
}

struct Foo { value: i64 }

impl Default for Foo {
    fn default() -> Foo { Foo { value: 42 } }
}

fn main() {
    let f: Foo = default();
    println(f.value);
}
"#);
    assert_eq!(output, "42\n");
}

#[test]
fn test_bare_assoc_fn_with_typeparam_expected() {
    // Bare `default()` inside a generic function with typeparam expected
    // type uses lowering + runtime substitution stack to dispatch.
    let output = run(r#"
trait Default {
    fn default() -> Self;
}

struct Foo { value: i64 }

impl Default for Foo {
    fn default() -> Foo { Foo { value: 99 } }
}

fn make[T: Default]() -> T {
    default()
}

fn main() {
    let f: Foo = make();
    println(f.value);
}
"#);
    assert_eq!(output, "99\n");
}

#[test]
fn test_assoc_fn_with_arg_through_typeparam() {
    // Trait method with a non-Self parameter dispatches through typeparam.
    let output = run(r#"
trait FromI64 {
    fn from_i64(n: i64) -> Self;
}

struct Wrap { v: i64 }

impl FromI64 for Wrap {
    fn from_i64(n: i64) -> Wrap { Wrap { v: n } }
}

fn make[T: FromI64](n: i64) -> T {
    T.from_i64(n)
}

fn main() {
    let w: Wrap = make(7);
    println(w.v);
}
"#);
    assert_eq!(output, "7\n");
}

#[test]
fn test_derive_default_user_impl_wins() {
    // A hand-written `default` on a `#[derive(Default)]` type is not
    // double-defined — the explicit impl is the one that runs.
    let output = run(r#"
#[derive(Default)]
struct C { x: i64 }

impl C { fn default() -> C { C { x: 99 } } }

fn main() {
    let c = C.default();
    println(c.x);
}
"#);
    assert_eq!(output, "99\n");
}

#[test]
fn test_integration_generic_helper_chain() {
    // A generic helper calls another generic helper that dispatches via
    // typeparam — verifies the runtime substitution stack chains transitively
    // through multiple call frames.
    let output = run(r#"
trait Default {
    fn default() -> Self;
}

struct Box { value: i64 }

impl Default for Box {
    fn default() -> Box { Box { value: 100 } }
}

fn make[T: Default]() -> T {
    T.default()
}

fn outer[T: Default]() -> T {
    make()
}

fn main() {
    let b: Box = outer();
    println(b.value);
}
"#);
    assert_eq!(output, "100\n");
}

#[test]
fn test_integration_where_clause_bound_e2e() {
    // Full end-to-end with a where-clause bound (instead of inline) —
    // dispatch should land at the same impl method.
    let output = run(r#"
trait Default {
    fn default() -> Self;
}

struct Slot { v: i64 }

impl Default for Slot {
    fn default() -> Slot { Slot { v: 13 } }
}

fn make[T]() -> T where T: Default {
    default()
}

fn main() {
    let s: Slot = make();
    println(s.v);
}
"#);
    assert_eq!(output, "13\n");
}

#[test]
fn test_integration_assoc_fn_in_arg_position() {
    // Bare assoc fn call passed as a function argument. The expected type
    // is the parameter type at the call site.
    let output = run(r#"
trait Default {
    fn default() -> Self;
}

struct Foo { v: i64 }

impl Default for Foo {
    fn default() -> Foo { Foo { v: 21 } }
}

fn take(f: Foo) -> i64 { f.v * 2 }

fn main() {
    println(take(default()));
}
"#);
    assert_eq!(output, "42\n");
}

#[test]
fn test_for_over_unbounded_cycle_breaks_from_body() {
    // B-2026-07-14-22: the for-loop used to EAGERLY DRAIN `Value::Iterator`
    // into a Vec before running the body, so an UNBOUNDED `.cycle()` (no
    // `.take(n)` cap — the body's `break` is the only bound) hung forever
    // before the first body execution. The lazy pull-loop runs the body per
    // element, so the break lands after 7 pulls.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let mut n = 0;
    let mut a = 0;
    for x in v.iter().cycle() {
        if n == 7 { break; }
        n = n + 1;
        a = a + x;
    }
    println(a);
}
"#,
    );
    assert_eq!(output, "13\n");
}

#[test]
fn primitive_trait_impl_self_return() {
    // B-2026-07-03-5, `-> Self` shape: `self + self` in the body must see a
    // numeric `self` (pre-fix the hand-built `Named { "u8" }` self type errored
    // "arithmetic operator requires numeric type, found 'u8'"), and the
    // width-keyed dispatch selects the u8 impl for the u8 receiver.
    let src = "trait Dbl { fn dbl(self) -> Self; }
        impl Dbl for u8  { fn dbl(self) -> Self { self + self } }
        impl Dbl for i64 { fn dbl(self) -> Self { self + self } }
        fn main() {
            let a: u8 = 100;
            let b: i64 = 21;
            println(a.dbl());
            println(b.dbl());
        }";
    assert_eq!(run_no_errors(src), "200\n42\n");
}

/// B-2026-08-25-35 — a `#[derive(Ord)]` user type satisfies a `T: Ord` bound on
/// a GENERIC IMPL's method, not only on a free generic fn.
///
/// Two gates discharge bounds and they had different powers. `type_satisfies_bound`
/// (typechecker layer) recognizes `#[derive]` on a named type; `Env::bound_satisfied`
/// (the gate method resolution uses for a bound on a generic impl) did not, and its
/// own comment asserted the derive tables were unreachable from there — they are not,
/// `TypeEnv` owns `structs`/`enums`. So the SAME type against the SAME bound got two
/// answers depending on which side of the call the bound was written on: `fn
/// free[T: Ord](..)` accepted it, `impl[T: Ord] Holder[T] { fn tag(..) }` rejected it
/// with "`Item` does not implement `Ord`; trait `Ord` is implemented by: <primitives>".
///
/// `PriorityQueue[T]`'s whole surface is the second kind, which is why no derived
/// user type could be put in one — the point of the sibling test below.
///
/// Twin of `tests/codegen.rs`'s `e2e_derived_ord_satisfies_a_generic_impl_bound`.
/// PARITY test, NOT the regression oracle: this harness's `run()` executes and
/// joins output without asserting the typecheck is clean, and `karac run`
/// demotes typecheck errors to warnings, so this passes on the UNFIXED tree
/// too — the tree-walker never enforced the bound. The oracles that go red are
/// `tests/typechecker.rs`'s `derived_ord_satisfies_a_bound_on_a_generic_impl_method`
/// and the codegen twin. What this pins is that the two backends agree once the
/// program is accepted.
#[test]
fn test_derived_ord_satisfies_a_generic_impl_bound() {
    let out = run(r#"
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Item { id: i64 }
struct Holder[=T] { v: T }
impl[T: Ord] Holder[T] {
    fn tag(ref self) -> i64 { 7 }
}
fn free[T: Ord](a: T) -> i64 { 9 }
fn main() {
    let h = Holder { v: Item { id: 1 } };
    println(h.tag());
    println(free(Item { id: 1 }));
}
"#);
    assert_eq!(out, "7\n9\n");
}

/// B-2026-08-30-41 — A `T: PartialOrd` BOUND MUST BE RUNNABLE, FOR EVERY TYPE.
///
/// The bound was SATISFIABLE by every scalar (a structural rule in `env.rs`,
/// with no impl behind it) and runnable by none: lowering turns `a < b` under
/// it into `T.partial_cmp(a, b).is_lt()`, and no primitive had `partial_cmp`.
/// The tree-walk aborted with "method 'partial_cmp' not found on type 'i64'"
/// and `karac build` failed on the FOLLOW-ON predicate with "no handler for
/// method 'is_lt'". The identical body under a `T: Ord` bound worked on both,
/// which is the contrast that makes this a bound defect rather than a
/// comparison one.
///
/// `cmp` IS NOT AN ALTERNATIVE, and that is forced rather than preferred:
/// supertrait methods do not re-export through the requiring trait, so a
/// `T: PartialOrd` bound cannot call `cmp` at all — and routing to it would
/// fail for exactly the population the trait exists to serve, the bare floats,
/// which are `PartialOrd` and deliberately not `Ord`.
///
/// EVERY ROW IS A DIFFERENT DISPATCH PATH, not a restatement:
/// - `ord` — the `T: Ord` control, which worked before and must keep working.
/// - `i64` — all four operators, so a fix that wired only `is_lt` is caught.
/// - `u64` — an unsigned value above `i64::MAX` riding a signed carrier.
///   `partial_cmp` recovers signedness through the same span-hint path `cmp`
///   uses (B-2026-08-28-5); without it `u64::MAX < 1` answers `true`.
/// - `f64` — the motivating case: `PartialOrd` and NOT `Ord`, so `T: Ord`
///   rejects it outright and this is the only bound that admits it.
/// - `str` — a heap receiver, through `karac_string_cmp`.
/// - `user` — a struct deriving `PartialOrd`, through the aggregate comparator.
/// - `nan` — the reason the trait returns an `Option` at all. All four
///   predicates are `false` for an incomparable pair, per design.md
///   § Comparison Traits and IEEE-754.
///
/// Twin of `tests/codegen.rs`'s `e2e_partial_ord_bound_is_runnable`, pinned to
/// the same string.
#[test]
fn test_partial_ord_bound_is_runnable() {
    assert_eq!(
        run(r#"#[derive(PartialEq, PartialOrd)]
struct Po { v: i64 }

fn lt_ord[T: Ord](a: T, b: T) -> bool { return a < b }
fn lt_po[T: PartialOrd](a: T, b: T) -> bool { return a < b }
fn le_po[T: PartialOrd](a: T, b: T) -> bool { return a <= b }
fn gt_po[T: PartialOrd](a: T, b: T) -> bool { return a > b }
fn ge_po[T: PartialOrd](a: T, b: T) -> bool { return a >= b }

fn main() {
    // The seed is the literal 1 rather than `env.args().len()`: an IN-PROCESS
    // interpreter test sees the TEST binary's argv, which is 1 only when the
    // suite runs unfiltered and 2+ under `cargo test <filter>`. The codegen
    // twin keeps `env.args()` because it needs an opaque seed to survive -O2
    // folding, and 1 is what that yields under its harness.
    let n: i64 = 1;
    println(f"ord  {lt_ord(n, n + 1)} {lt_ord(f"a", f"b")}");
    println(f"i64  {lt_po(n, n + 1)} {le_po(n, n)} {gt_po(n + 1, n)} {ge_po(n, n + 1)}");
    let u: u64 = 18446744073709551615u64;
    println(f"u64  {lt_po(u, 1u64)} {gt_po(u, 1u64)}");
    let f: f64 = (n as f64) + 1.0;
    println(f"f64  {lt_po(f, f + 1.0)} {ge_po(f, f)}");
    println(f"str  {lt_po(f"a", f"b")} {gt_po(f"a", f"b")}");
    println(f"user {lt_po(Po { v: n }, Po { v: n + 1 })} {ge_po(Po { v: n }, Po { v: n })}");
    let z: f64 = (n as f64) - 1.0;
    let nan: f64 = z / z;
    println(f"nan  {lt_po(nan, f)} {le_po(nan, nan)} {gt_po(f, nan)} {ge_po(nan, nan)}");
}
"#),
        r#"ord  true true
i64  true true true false
u64  false true
f64  true true
str  true false
user true true
nan  false false false false
"#
    );
}

/// B-2026-08-29-3, interpreter leg — a GENERIC method that hands an owned
/// argument back runs its `Drop` body exactly once.
///
/// The twin of `e2e_generic_method_returned_param_body_runs_once` in
/// `tests/codegen.rs`, carrying the same cases with the same expected output.
/// Keep the two in step verbatim.
///
/// This backend was CORRECT before the fix, so these are controls: the defect
/// was a compiled-only double body, and what they pin is that codegen moved onto
/// the interpreter's answer rather than both moving.
#[test]
fn test_generic_method_returned_param_body_runs_once() {
    const DROPPER: &str = "struct R { id: i64, tag: String }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n\
         struct G1 { n: i64 }\n";
    for (label, body, want) in [
        (
            "generic-method-always-returns",
            "impl G1 { fn gm_ret[T](ref self, r: R, t: T) -> R { r } }\n\
             fn main() { let h = G1 { n: 1 }; \
             let b = h.gm_ret(R { id: 32, tag: f\"h\" }, 5); println(f\"{b.id}\") }\n",
            "32\ndrop 32\n",
        ),
        (
            "generic-method-param-dies",
            "impl G1 { fn gm_dies[T](ref self, r: R, t: T) -> i64 { 3 } }\n\
             fn main() { let g = G1 { n: 1 }; \
             let a = g.gm_dies(R { id: 31, tag: f\"g\" }, 5); println(f\"{a}\") }\n",
            "drop 31\n3\n",
        ),
        // The generic CONDITIONAL case that sat here is deliberately gone: it is
        // divergent in BOTH directions and is not what this row fixed. Measured —
        // param dies: interp 0 / compiled 1; param escapes: interp 1 / compiled 2.
        // The callee-side flip that would settle it is unavailable for generics
        // (B-2026-08-28-71), so it is tracked on its own row rather than pinned
        // here to either backend's answer.
        (
            "non-generic-twin",
            "impl G1 { fn id(ref self, r: R) -> R { r } }\n\
             fn main() { let g = G1 { n: 1 }; \
             let b = g.id(R { id: 32, tag: f\"h\" }); println(f\"{b.id}\") }\n",
            "32\ndrop 32\n",
        ),
        (
            "free-fn-oracle",
            "fn idf(r: R) -> R { r }\n\
             fn main() { let b = idf(R { id: 32, tag: f\"h\" }); println(f\"{b.id}\") }\n",
            "32\ndrop 32\n",
        ),
    ] {
        assert_eq!(run(&format!("{DROPPER}{body}")), want, "{label}");
    }
}

/// B-2026-08-13-9 — interpreter twin of `tests/codegen.rs`'s
/// `test_e2e_user_trait_bound_call_over_builtin_containers_dispatches`, same
/// source and expected string.
///
/// The interpreter answered this correctly all along — it dispatches on the
/// runtime value, so a bound call needs no monomorph identity — which is what
/// made it the oracle the compiled fix was measured against, and what made the
/// earlier reverted attempt visibly wrong (`m s m m` here would have read
/// `m s m s`). Pinning it keeps that oracle explicit rather than implied.
#[test]
fn test_user_trait_bound_call_over_builtin_containers() {
    assert_eq!(
        run("trait Zero { fn describe(ref self) -> String; }\n\
             impl Zero for Map[String, i64] { fn describe(ref self) -> String { return \"m\"; } }\n\
             impl Zero for Set[i64] { fn describe(ref self) -> String { return \"s\"; } }\n\
             impl Zero for Slice[i64] { fn describe(ref self) -> String { return \"l\"; } }\n\
             impl Zero for Vec[i64] { fn describe(ref self) -> String { return \"v\"; } }\n\
             impl Zero for String { fn describe(ref self) -> String { return \"t\"; } }\n\
             fn show[T: Zero](x: ref T) -> String { return x.describe(); }\n\
             fn outer[T: Zero](x: ref T) -> String { return show(x) + \"!\"; }\n\
             fn pair[A: Zero, B: Zero](x: ref A, y: ref B) -> String {\n\
                 return x.describe() + y.describe();\n\
             }\n\
             fn main() {\n\
                 let mut m: Map[String, i64] = Map.new();\n\
                 m.insert(\"k\", 1);\n\
                 let mut s: Set[i64] = Set.new();\n\
                 s.insert(7);\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 v.push(3);\n\
                 v.push(4);\n\
                 let t: String = \"hi\";\n\
                 let sl = v[0..2];\n\
                 println(show(m) + show(s) + show(sl) + show(v) + show(t));\n\
                 println(show(v[0..2]));\n\
                 println(outer(m) + outer(s));\n\
                 println(pair(m, s) + pair(s, m));\n\
                 println(m.describe() + s.describe() + v.describe() + t.describe());\n\
             }"),
        "mslvt\nl\nm!s!\nmssm\nmsvt\n"
    );
}

/// B-2026-08-14-3 — interpreter twin of `tests/codegen.rs`'s
/// `test_e2e_generic_at_unsigned_width_zero_extends`, same source and expected
/// string.
///
/// The interpreter was right on every one of these throughout — it carries the
/// value's own type rather than an `iN` whose signedness has to be recovered —
/// so this is the oracle the compiled backends now match. Pinning it keeps the
/// pair from drifting.
#[test]
fn test_generic_at_unsigned_width_zero_extends() {
    assert_eq!(
        run("struct Boxg[T] { v: T }\n\
             impl[T] Boxg[T] { fn get(ref self) -> T { return self.v; } }\n\
             fn idg[T](x: T) -> T { return x; }\n\
             fn main() {\n\
                 println(idg(200u8));\n\
                 println(idg(60000u16));\n\
                 println(idg(4000000000u32));\n\
                 println(idg(18446744073709551615u64));\n\
                 println(idg(-56i8));\n\
                 let g: Boxg[u8] = Boxg { v: 200u8 };\n\
                 println(g.v);\n\
                 println(g.get());\n\
                 let k: Boxg[i8] = Boxg { v: -56i8 };\n\
                 println(k.v);\n\
                 let mut vg: Vec[Boxg[u8]] = Vec.new();\n\
                 vg.push(Boxg { v: 200u8 });\n\
                 println(vg[0i64].v);\n\
                 println(idg(200u8) as i64);\n\
                 println(idg(-56i8) as i64);\n\
                 let direct = 200u8;\n\
                 println(direct);\n\
             }"),
        "200\n60000\n4000000000\n18446744073709551615\n-56\n200\n200\n-56\n200\n200\n-56\n200\n"
    );
}

/// B-2026-08-13-14 — interpreter twin of `tests/codegen.rs`'s
/// `test_e2e_field_bound_out_of_local_is_a_copy`, same source and expected
/// string.
///
/// The interpreter is the ORACLE for this pair: binding a field out of a local
/// leaves the source's contents intact, which is what the language's field-read
/// rule calls for and what the compiled backends did not do. Pinning the twin
/// keeps the two surfaces from drifting apart again — the whole failure was
/// that they disagreed silently, with `karac check` reporting the mismatch only
/// as an advisory `UseAfterMove` whose "codegen defensive-copies the reuse"
/// promise did not hold at a field-access consume site.
#[test]
fn test_field_bound_out_of_local_is_a_copy() {
    assert_eq!(
        run("struct S { name: String }\n\
             struct A { lines: Vec[String] }\n\
             struct B { a: A, n: i64 }\n\
             fn main() {\n\
                 let k = 1;\n\
                 let s = S { name: f\"hi{k}\" };\n\
                 let n1 = s.name;\n\
                 println(n1);\n\
                 println(s.name);\n\
                 let mut v: Vec[String] = Vec.new();\n\
                 v.push(f\"x{k}\");\n\
                 let d = A { lines: v };\n\
                 let t = d.lines;\n\
                 println(t.len());\n\
                 println(d.lines.len());\n\
                 println(d.lines[0]);\n\
                 let mut v2: Vec[String] = Vec.new();\n\
                 v2.push(f\"y{k}\");\n\
                 let b = B { a: A { lines: v2 }, n: 7 };\n\
                 let u = b.a;\n\
                 println(u.lines.len());\n\
                 let w = b.a;\n\
                 println(w.lines.len());\n\
                 println(w.lines[0]);\n\
                 println(b.n);\n\
             }"),
        "hi1\nhi1\n1\n1\nx1\n1\n1\ny1\n7\n"
    );
}

/// B-2026-08-14-15 — the interpreter oracle for the two nested-container
/// binding leaks fixed in codegen (`tests/codegen.rs::
/// test_e2e_bound_nested_container_reads`). The values were never in doubt on
/// this backend; what makes it an oracle is that codegen must match it for
/// BOTH spellings — the `let` binding and the inline read.
#[test]
fn test_bound_nested_container_reads() {
    assert_eq!(
        run("struct P { tag: String }\n\
             fn mk(k: i64) -> Option[Vec[P]] {\n\
                 let mut c: Vec[P] = Vec.new();\n\
                 c.push(P { tag: f\"alpha{k}\" });\n\
                 Some(c)\n\
             }\n\
             fn mkr(k: i64) -> Result[Vec[P], Vec[P]] {\n\
                 let mut c: Vec[P] = Vec.new();\n\
                 c.push(P { tag: f\"beta{k}\" });\n\
                 if k > 100 { return Err(c); }\n\
                 Ok(c)\n\
             }\n\
             fn main() {\n\
                 let k = 1;\n\
                 let mut vms: Vec[Map[String, i64]] = Vec.new();\n\
                 let mut inner: Map[String, i64] = Map.new();\n\
                 let _ = inner.insert(\"n\", k);\n\
                 vms.push(inner);\n\
                 let cur = vms[0];\n\
                 match cur.get(\"n\") { Some(x) => println(x), None => println(-1) }\n\
                 println(cur.contains_key(\"n\"));\n\
                 println(cur.len());\n\
                 match vms[0].get(\"n\") { Some(x) => println(x), None => println(-1) }\n\
                 let held = mk(k);\n\
                 match held { Some(v) => println(v[0].tag), None => println(\"none\") }\n\
                 match mk(k) { Some(v) => println(v[0].tag), None => println(\"none\") }\n\
                 let okr = mkr(k);\n\
                 match okr { Ok(v) => println(v[0].tag), Err(e) => println(e[0].tag) }\n\
                 let err = mkr(200);\n\
                 match err { Ok(v) => println(v[0].tag), Err(e) => println(e[0].tag) }\n\
             }\n"),
        "1\ntrue\n1\n1\nalpha1\nalpha1\nbeta1\nbeta200\n"
    );
}

/// B-2026-08-21-53 — the TYPE-QUALIFIED associated call `Type[Args].fn(a)`,
/// the spelling design.md § Generics settles on for explicit type selection.
///
/// The interpreter had no rule for it: a type is not a value, but the receiver
/// `Box[i64]` was evaluated as an ordinary expression and fell through
/// `eval_expr`'s `Path` arm to
/// "internal: path 'Box' has no interpreter evaluation rule". It now re-forms
/// the call as the two-segment callee the UNQUALIFIED `Box.make(7)` already
/// parses to, so both spellings share one dispatch and cannot drift.
#[test]
fn a_type_qualified_associated_call_evaluates() {
    let decls = "struct Box[T] { value: T }\n\
                 impl[T] Box[T] {\n\
                     fn make(v: T) -> Box[T] { return Box { value: v }; }\n\
                     fn zero() -> i64 { return 0i64; }\n\
                 }\n";
    // Qualified and unqualified must agree — the unqualified is the control.
    assert_eq!(
        run_no_errors(&format!(
            "{decls}fn main() {{ let b = Box[i64].make(7i64); println(f\"{{b.value}}\"); }}"
        )),
        "7\n"
    );
    assert_eq!(
        run_no_errors(&format!(
            "{decls}fn main() {{ let b = Box.make(7i64); println(f\"{{b.value}}\"); }}"
        )),
        "7\n"
    );
    // An associated fn whose return type is not the generic type.
    assert_eq!(
        run_no_errors(&format!(
            "{decls}fn main() {{ println(f\"{{Box[String].zero()}}\"); }}"
        )),
        "0\n"
    );
    // A generic ENUM receiver dispatches the same way.
    assert_eq!(
        run_no_errors(
            "enum Opt2[T] { Nothing, Just(T) }\n\
             impl[T] Opt2[T] { fn none() -> Opt2[T] { return Opt2.Nothing; } }\n\
             fn main() {\n\
                 let o = Opt2[i64].none();\n\
                 match o { Opt2.Nothing => println(\"nothing\"), Opt2.Just(v) => println(f\"just {v}\") }\n\
             }"
        ),
        "nothing\n"
    );
}

/// B-2026-09-06-9 — the INTERPRETER twin of
/// `e2e_param_through_returning_callee_bound_locally_runs_one_body`, same
/// program and the same expected string: the shape was agreed-wrong on all
/// four surfaces, and the two let-sites (`let_destructures_owned_param`'s
/// call arm here, the four registrations in `compile_let` there) read one
/// shared alias set, `fn_whole_param_aliases`.
#[test]
fn test_param_through_returning_callee_bound_locally_runs_one_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct H2 { pe: ((R, i64), i64) }
enum E { A(R), B }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
fn keep(t: (R, i64)) -> (R, i64) { return t; }
fn keeps(r: R) -> R { return r; }
fn keepg[T](x: T) -> T { return x; }
fn keepe(e: E) -> E { return e; }
fn t_keep_direct(t: (R, i64)) { let w: (R, i64) = keep(t); println(f"tkd {w.0.id}") }
fn t_keep_rebind(t: (R, i64)) { let z: (R, i64) = t; let w: (R, i64) = keep(z); println(f"tkr {w.0.id}") }
fn s_keep_direct(r: R) { let w: R = keeps(r); println(f"skd {w.id}") }
fn s_keep_rebind(r: R) { let z: R = r; let w: R = keeps(z); println(f"skr {w.id}") }
fn s_keep_unread(r: R) { let w: R = keeps(r); println("sku") }
fn s_keep_twice(r: R) { let w: R = keeps(r); let v: R = keeps(w); println(f"skt {v.id}") }
fn s_keep_generic(r: R) { let w: R = keepg(r); println(f"skg {w.id}") }
fn s_keep_assoc(r: R) { let w: R = K.id(r); println(f"ska {w.id}") }
fn s_keep_nested(r: R, k: bool) { if k { let w: R = keeps(r); println(f"skn {w.id}"); } println("skn-out") }
fn s_keep_arm(r: R, k: bool) -> i64 { let w: R = keeps(r); if k { return 1; } println(f"skm {w.id}"); return 0 }
fn e_keep(e: E) { let w: E = keepe(e); match w { E.A(x) => println(f"ek {x.id}"), E.B => println("ekB") } }
fn s_local() { let l = mk(30); let w: R = keeps(l); println(f"sl {w.id}") }
fn v_keep(h: H2) { let (inner, y) = h.pe; let z: (R, i64) = inner; let w: (R, i64) = keep(z); println(f"vk {w.0.id}") }
struct K { n: i64 }
impl K {
    fn id(r: R) -> R { return r; }
    fn m_keep(ref self, r: R) { let w: R = keeps(r); println(f"mk {w.id}") }
}
fn main() {
    let k = K { n: 0 };
    println("one"); t_keep_direct((mk(1), 1));
    println("two"); t_keep_rebind((mk(2), 2));
    println("three"); s_keep_direct(mk(3));
    println("four"); s_keep_rebind(mk(4));
    println("five"); s_keep_unread(mk(5));
    println("six"); s_keep_twice(mk(6));
    println("seven"); s_keep_generic(mk(7));
    println("eight"); s_keep_assoc(mk(8));
    println("nine-t"); s_keep_nested(mk(9), true);
    println("ten-f"); s_keep_nested(mk(10), false);
    println("eleven-t"); let _ = s_keep_arm(mk(11), true);
    println("twelve-f"); let _ = s_keep_arm(mk(12), false);
    println("thirteen"); e_keep(E.A(mk(13)));
    println("fourteen"); k.m_keep(mk(14));
    println("fifteen-named"); let a = mk(15); s_keep_direct(a);
    println("sixteen-mnamed"); let b = mk(16); k.m_keep(b);
    println("seventeen-tnamed"); let t: (R, i64) = (mk(17), 1); t_keep_direct(t);
    println("eighteen-local"); s_local();
    println("nineteen-leaf"); v_keep(H2 { pe: ((mk(19), 1), 2) });
    println("end");
}"#),
        "one\ntkd 1\ndR1\ntwo\ntkr 2\ndR2\nthree\nskd 3\ndR3\nfour\nskr 4\ndR4\nfive\nsku\ndR5\nsix\nskt 6\ndR6\nseven\nskg 7\ndR7\neight\nska 8\ndR8\nnine-t\nskn 9\nskn-out\ndR9\nten-f\nskn-out\ndR10\neleven-t\ndR11\ntwelve-f\nskm 12\ndR12\nthirteen\nek 13\ndR13\nfourteen\nmk 14\ndR14\nfifteen-named\nskd 15\ndR15\nsixteen-mnamed\nmk 16\ndR16\nseventeen-tnamed\ntkd 17\ndR17\neighteen-local\nsl 30\ndR30\nnineteen-leaf\nvk 19\ndR19\nend\n",
        "a by-value param rebound through an always-returning callee has one owner"
    );
}

#[test]
fn interp_unsigned_value_inside_a_generic_body_reads_unsigned() {
    // B-2026-08-30-44 — the interpreter carries every integer as a signed
    // `Value::Int` and recovers unsignedness from the type recorded at the
    // span. Inside `fn show[T](x: T)` that type is the PARAM `T`, so the
    // lookup answered `None` and the signed reading stood: `u64::MAX` printed
    // -1. The identical interpolation OUTSIDE the generic was correct, and so
    // were both compiled backends at every call site — codegen monomorphizes,
    // so its body is compiled at the concrete width and cannot have this hole.
    // That makes the compiled output the ORACLE here, and every expectation
    // below is what `karac build` already printed before the fix.
    //
    // The row describes the DISPLAY path only. The same lookup also feeds
    // `eval_binary`'s `unsigned_hint`, so the fix reaches ARITHMETIC too, and
    // that half is the more consequential one — it silently computes wrong
    // values rather than printing a wrong one. Measured on the pre-fix
    // interpreter, both inside a generic body at `u64`:
    //
    //     u64::MAX / 2   gave 0      (signed: -1 / 2)   should be 9223372036854775807
    //     u64::MAX < 2   gave true   (signed: -1 < 2)   should be false
    //
    // Neither appears in the row, and a display-only fix would have left both.
    //
    // Only u64 / usize / u128 can differ: a narrower unsigned width fits the
    // signed carrier non-negatively, so both readings coincide — which is why
    // `control-display-u8` and `control-display-u32` were green before the fix
    // and are controls rather than cases. `control-outside-generic` is the
    // localizer: the same value, the same interpolation, no generic frame.
    let cases: &[(&str, &str, &str)] = &[
        (
            "display-u64",
            "fn show[T](x: T) -> String { f\"{x}\" }\n\
             fn main() { let b: u64 = 18446744073709551615u64; println(show(b)); }",
            "18446744073709551615\n",
        ),
        (
            "display-u128",
            "fn show[T](x: T) -> String { f\"{x}\" }\n\
             fn main() { let b: u128 = 340282366920938463463374607431768211455u128; println(show(b)); }",
            "340282366920938463463374607431768211455\n",
        ),
        (
            "display-usize",
            "fn show[T](x: T) -> String { f\"{x}\" }\n\
             fn main() { let b: usize = 18446744073709551615u64 as usize; println(show(b)); }",
            "18446744073709551615\n",
        ),
        (
            "divide-u64",
            "fn divide[T: Div](a: T, b: T) -> T { a / b }\n\
             fn main() { let b: u64 = 18446744073709551615u64; let t: u64 = 2u64; println(divide(b, t)); }",
            "9223372036854775807\n",
        ),
        (
            "compare-u64",
            "fn less[T: Ord](a: T, b: T) -> bool { a < b }\n\
             fn main() { let b: u64 = 18446744073709551615u64; let t: u64 = 2u64; println(less(b, t)); }",
            "false\n",
        ),
        (
            "compare-u128",
            "fn less[T: Ord](a: T, b: T) -> bool { a < b }\n\
             fn main() { let b: u128 = 340282366920938463463374607431768211455u128; let t: u128 = 2u128; println(less(b, t)); }",
            "false\n",
        ),
        (
            "control-display-i64",
            "fn show[T](x: T) -> String { f\"{x}\" }\n\
             fn main() { let s: i64 = -9; println(show(s)); }",
            "-9\n",
        ),
        (
            "control-divide-i64",
            "fn divide[T: Div](a: T, b: T) -> T { a / b }\n\
             fn main() { let s: i64 = -9; println(divide(s, 3)); }",
            "-3\n",
        ),
        (
            "control-display-u8",
            "fn show[T](x: T) -> String { f\"{x}\" }\n\
             fn main() { let c: u8 = 200; println(show(c)); }",
            "200\n",
        ),
        (
            "control-display-u32",
            "fn show[T](x: T) -> String { f\"{x}\" }\n\
             fn main() { let d: u32 = 4294967295u32; println(show(d)); }",
            "4294967295\n",
        ),
        (
            "control-outside-generic",
            "fn main() { let b: u64 = 18446744073709551615u64; println(f\"{b}\"); }",
            "18446744073709551615\n",
        ),
    ];
    for (label, src, want) in cases {
        assert_eq!(run(src), *want, "{label}");
    }
}

#[test]
fn interp_generic_impl_method_resolves_its_own_type_param() {
    // B-2026-08-30-43 — only FREE-FUNCTION calls ever pushed a generic
    // substitution frame (`push_type_subs_for_call`, one call site). A method
    // on a generic `impl` binds `T` from the RECEIVER's type args, not from the
    // call's arguments, so nothing reached `call_type_subs` and
    // `resolve_type_param("T")` answered `None` for the whole body. Everything
    // the tree-walk recovers through that stack was skipped inside such a body.
    //
    // The typechecker already computed the binding (`recv_subs`, used to solve
    // the signature); it is now also recorded under `(span, method)` — the same
    // key `method_impl_dispatch` uses, because `MethodCall.span ==
    // receiver.span` aliases a chain — and pushed around the body.
    //
    // Codegen monomorphizes, so its bodies are compiled at the concrete type
    // and it is the oracle for the FLOAT cases below: each expected value is
    // what `karac build` already printed.
    //
    // `via-generic-caller` is the control that matters most. It was ALREADY
    // correct before this fix, because the enclosing free-function call pushes
    // a frame binding `T`, which the impl body then resolves through. It is
    // here to show the new frame COMPOSES with that one rather than shadowing
    // it — a fix that pushed an unresolved `T -> "T"` would break this case
    // while fixing the others, which is why the typechecker side drops any
    // entry that does not name a concrete type.
    //
    // `f64-combine` is the width control: f64 needs no narrowing, so it was
    // green before and stays green. `i64-field` and `string-field` are the
    // non-narrow, non-unsigned leaves.
    //
    // The u64 cases assert the UNSIGNED reading, which is what the interpreter
    // now prints and what the language defines. The compiled backends still
    // print -1 for them — that is B-2026-08-31-12's remaining half, filed
    // separately, and it is why those cases have no codegen twin here.
    let hdr = "struct Pair[T] { x: T, y: T }\n\
               impl[T: Add + Mul] Pair[T] {\n\
                   fn combine(ref self) -> T { self.x * self.y + self.x }\n\
                   fn twice(ref self) -> T { self.combine() + self.combine() }\n\
               }\n\
               struct Box[T] { v: T }\n\
               impl[T] Box[T] {\n\
                   fn get(ref self) -> String { f\"{self.v}\" }\n\
                   fn chained(ref self) -> String { self.get() }\n\
               }\n\
               struct Two[A, B] { a: A, b: B }\n\
               impl[A, B] Two[A, B] { fn show(ref self) -> String { f\"{self.a}/{self.b}\" } }\n\
               fn outer[T](b: Box[T]) -> String { b.get() }\n";
    let big = "18446744073709551615";
    let cases: &[(&str, String, String)] = &[
        (
            "f32-combine",
            format!(
                "{hdr}fn main() {{ let a: f32 = 0.1; let b: f32 = 0.3; let p = Pair[f32] {{ x: a, y: b }}; println(p.combine()); }}"
            ),
            "0.12999999523162842\n".to_string(),
        ),
        (
            "f32-method-calls-method",
            format!(
                "{hdr}fn main() {{ let a: f32 = 0.1; let b: f32 = 0.3; let p = Pair[f32] {{ x: a, y: b }}; println(p.twice()); }}"
            ),
            "0.25999999046325684\n".to_string(),
        ),
        (
            "u64-field",
            format!("{hdr}fn main() {{ let n: u64 = {big}u64; let bx = Box[u64] {{ v: n }}; println(bx.get()); }}"),
            format!("{big}\n"),
        ),
        (
            "u64-field-chained",
            format!("{hdr}fn main() {{ let n: u64 = {big}u64; let bx = Box[u64] {{ v: n }}; println(bx.chained()); }}"),
            format!("{big}\n"),
        ),
        (
            "two-param-impl",
            format!(
                "{hdr}fn main() {{ let n: u64 = {big}u64; let t = Two[u64, i64] {{ a: n, b: -5 }}; println(t.show()); }}"
            ),
            format!("{big}/-5\n"),
        ),
        (
            "control-via-generic-caller",
            format!("{hdr}fn main() {{ let n: u64 = {big}u64; println(outer(Box[u64] {{ v: n }})); }}"),
            format!("{big}\n"),
        ),
        (
            "control-f64-combine",
            format!(
                "{hdr}fn main() {{ let a: f64 = 0.1; let b: f64 = 0.3; let p = Pair[f64] {{ x: a, y: b }}; println(p.combine()); }}"
            ),
            "0.13\n".to_string(),
        ),
        (
            "control-i64-field",
            format!("{hdr}fn main() {{ let bi = Box[i64] {{ v: -7 }}; println(bi.get()); }}"),
            "-7\n".to_string(),
        ),
        (
            "control-string-field",
            format!("{hdr}fn main() {{ let bs = Box[String] {{ v: \"hi\" }}; println(bs.get()); }}"),
            "hi\n".to_string(),
        ),
    ];
    for (label, src, want) in cases {
        assert_eq!(run(src), *want, "{label}");
    }
}

#[test]
fn interp_transitive_generic_is_the_codegen_unsignedness_oracle() {
    // B-2026-08-31-11's oracle half. The DEFECT is on the compiled side: a
    // generic calling another generic whose type param has the SAME NAME
    // instantiated the callee at the signed sibling of the declared width, so
    // `u32::MAX` printed as -1 from both `karac run` and `karac build` while
    // `--interp` was correct.
    //
    // The interpreter is right for a structural reason rather than by luck: it
    // carries the declared type at runtime and recovers signedness from it
    // (B-2026-08-30-44), so it has no monomorph symbol to collide in the first
    // place. That makes it the reference, and every value below is what it
    // prints — the same table `e2e_transitive_generic_keeps_the_declared_-
    // unsignedness` in `tests/codegen.rs` asserts on the compiled side.
    //
    // Pinning it here is the point: without the pair, an interpreter regression
    // would silently move the reference the codegen test is measured against
    // and the two would agree on a wrong answer.
    //
    // The cases are the ones that diverged (15 of 22 pre-fix) plus the
    // localizer: `control-renamed-param` spells the callee's parameter `U`
    // instead of `T` and was always correct on every backend, which is what
    // separates a NAME COLLISION from nesting.
    let hdr = "fn show[T](x: T) -> String { f\"{x}\" }\n\
               fn showU[U](x: U) -> String { f\"{x}\" }\n\
               fn wrap[T](x: T) -> String { show(x) }\n\
               fn wrapU[T](x: T) -> String { showU(x) }\n\
               fn wrap2[T](x: T) -> String { wrap(x) }\n\
               fn wrapRef[T](x: ref T) -> String { show(x) }\n\
               fn dv[T: Div](a: T, b: T) -> T { a / b }\n\
               fn wdv[T: Div](a: T, b: T) -> T { dv(a, b) }\n\
               fn lt[T: PartialOrd](a: T, b: T) -> bool { a < b }\n\
               fn wlt[T: PartialOrd](a: T, b: T) -> bool { lt(a, b) }\n\
               fn add1[T: Add](x: T, o: T) -> T { x + o }\n\
               fn wInline[T: Add](x: T, o: T) -> String { show(add1(x, o)) }\n";
    let big = "let v: u64 = 18446744073709551614u64; let o: u64 = 1u64;";
    let cases: Vec<(&str, String, &str)> = vec![
        ("u8", "let v: u8 = 255u8; println(wrap(v));".into(), "255\n"),
        (
            "u16",
            "let v: u16 = 65535u16; println(wrap(v));".into(),
            "65535\n",
        ),
        (
            "u32",
            "let v: u32 = 4294967295u32; println(wrap(v));".into(),
            "4294967295\n",
        ),
        (
            "u64",
            "let v: u64 = 18446744073709551615u64; println(wrap(v));".into(),
            "18446744073709551615\n",
        ),
        (
            "u128",
            "let v: u128 = 340282366920938463463374607431768211455u128;\n\
             println(wrap(v));"
                .into(),
            "340282366920938463463374607431768211455\n",
        ),
        (
            "usize",
            "let v: usize = 18446744073709551615u64 as usize; println(wrap(v));".into(),
            "18446744073709551615\n",
        ),
        (
            "depth-3",
            "let v: u64 = 18446744073709551615u64; println(wrap2(v));".into(),
            "18446744073709551615\n",
        ),
        (
            "ref-param",
            "let v: u32 = 4294967295u32; println(wrapRef(v));".into(),
            "4294967295\n",
        ),
        (
            "arith-div",
            "let b: u64 = 18446744073709551615u64; let t: u64 = 2u64;\n\
             println(wdv(b, t));"
                .into(),
            "9223372036854775807\n",
        ),
        (
            "arith-cmp",
            "let b: u64 = 18446744073709551615u64; let t: u64 = 2u64;\n\
             println(wlt(b, t));"
                .into(),
            "false\n",
        ),
        (
            "inline-call-arg",
            format!("{big} println(wInline(v, o));"),
            "18446744073709551615\n",
        ),
        (
            "both-signs-one-program",
            "let u: u32 = 4294967295u32; let s: i32 = -1i32;\n\
             println(wrap(u)); println(wrap(s));"
                .into(),
            "4294967295\n-1\n",
        ),
        (
            "control-renamed-param",
            "let v: u32 = 4294967295u32; println(wrapU(v));".into(),
            "4294967295\n",
        ),
        (
            "control-direct",
            "let v: u64 = 18446744073709551615u64; println(show(v));".into(),
            "18446744073709551615\n",
        ),
        (
            "control-i64",
            "let s: i64 = -7; println(wrap(s));".into(),
            "-7\n",
        ),
        (
            "control-f64",
            "let f: f64 = 1.5; println(wrap(f));".into(),
            "1.5\n",
        ),
        (
            "control-string",
            "let s: String = \"hi\"; println(wrap(s));".into(),
            "hi\n",
        ),
    ];
    for (label, body, want) in cases {
        let src = format!("{hdr}fn main() {{\n    {body}\n}}");
        assert_eq!(run(&src), want, "{label}");
    }
}

#[test]
fn interp_generic_impl_field_is_the_codegen_unsignedness_oracle() {
    // B-2026-08-31-12's oracle half. That row was filed as wrong on ALL THREE
    // backends — which is what made it dangerous, since A/B agreement on a
    // wrong value hides in every comparison. B-2026-08-30-43 then gave a
    // generic `impl` body its substitution frame and fixed the interpreter,
    // leaving an ordinary run-vs-build divergence with the interpreter as the
    // reference.
    //
    // Every value below is what `--interp` prints, and
    // `e2e_generic_impl_field_keeps_the_declared_unsignedness` in
    // tests/codegen.rs asserts the same table on the compiled side. Pinning
    // both is the point: the row's whole history is two backends agreeing on
    // `-1`, so a regression on EITHER side must fail rather than restore the
    // agreement.
    //
    // The controls carry the row's other correction. Its scope note asks
    // whether ARITHMETIC on a `u64` field inside a generic impl is affected as
    // well as display; measured, it is not — `self.v / d` and `self.v < o`
    // were correct on every backend before the fix, as were a method PARAM of
    // type `T` and the field copied to a local first.
    let hdr = "struct Box[T] { v: T }\n\
               impl[T] Box[T] {\n\
               \x20   fn get(ref self) -> String { f\"{self.v}\" }\n\
               \x20   fn viaParam(ref self, x: T) -> String { f\"{x}\" }\n\
               \x20   fn viaLocal(ref self) -> String { let y = self.v; f\"{y}\" }\n\
               }\n\
               impl[T: Div] Box[T] { fn half(ref self, d: T) -> T { self.v / d } }\n\
               impl[T: PartialOrd] Box[T] {\n\
               \x20   fn under(ref self, o: T) -> bool { self.v < o }\n\
               }\n\
               struct Bag[T] { xs: Vec[T] }\n\
               impl[T] Bag[T] { fn first(ref self) -> String { f\"{self.xs[0]}\" } }\n\
               struct BoxU { v: u64 }\n\
               impl BoxU { fn get(ref self) -> String { f\"{self.v}\" } }\n";
    let big = "let big: u64 = 18446744073709551615u64; let b = Box[u64] { v: big };";
    let cases: Vec<(&str, String, &str)> = vec![
        (
            "field-u8",
            "let s: u8 = 255u8; let c = Box[u8] { v: s }; println(c.get());".into(),
            "255\n",
        ),
        (
            "field-u16",
            "let s: u16 = 65535u16; let c = Box[u16] { v: s }; println(c.get());".into(),
            "65535\n",
        ),
        (
            "field-u32",
            "let s: u32 = 4294967295u32; let c = Box[u32] { v: s }; println(c.get());".into(),
            "4294967295\n",
        ),
        (
            "field-u64",
            format!("{big} println(b.get());"),
            "18446744073709551615\n",
        ),
        (
            "field-u128",
            "let s: u128 = 340282366920938463463374607431768211455u128;\n\
             let c = Box[u128] { v: s }; println(c.get());"
                .into(),
            "340282366920938463463374607431768211455\n",
        ),
        (
            "vec-field-index",
            "let big: u64 = 18446744073709551615u64;\n\
             let g = Bag[u64] { xs: [big] }; println(g.first());"
                .into(),
            "18446744073709551615\n",
        ),
        (
            "control-arith-div",
            format!("{big} let d: u64 = 2u64; println(b.half(d));"),
            "9223372036854775807\n",
        ),
        (
            "control-arith-cmp",
            format!("{big} let d: u64 = 2u64; println(b.under(d));"),
            "false\n",
        ),
        (
            "control-method-param",
            format!("{big} println(b.viaParam(big));"),
            "18446744073709551615\n",
        ),
        (
            "control-field-via-local",
            format!("{big} println(b.viaLocal());"),
            "18446744073709551615\n",
        ),
        (
            "control-nongeneric-struct",
            "let big: u64 = 18446744073709551615u64;\n\
             let n = BoxU { v: big }; println(n.get());"
                .into(),
            "18446744073709551615\n",
        ),
        (
            "control-i64-field",
            "let s: i64 = -7; let c = Box[i64] { v: s }; println(c.get());".into(),
            "-7\n",
        ),
        (
            "control-f64-field",
            "let f: f64 = 1.5; let c = Box[f64] { v: f }; println(c.get());".into(),
            "1.5\n",
        ),
        (
            "control-string-field",
            "let s: String = \"hi\"; let c = Box[String] { v: s }; println(c.get());".into(),
            "hi\n",
        ),
    ];
    for (label, body, want) in cases {
        let src = format!("{hdr}fn main() {{\n    {body}\n}}");
        assert_eq!(run(&src), want, "{label}");
    }
}

/// B-2026-08-31-15 — the INTERPRETER oracle for the value-receiver comparison
/// methods, and specifically for the one thing the compiled tests cannot pin
/// from their own side: which of the builtin and a user `impl` wins.
///
/// The interpreter's `value_type_name` reads a type-ERASED `Value::Int`, so it
/// cannot tell an `i64` receiver from a `u32` one — both answer `i64`. A gate
/// keyed on that lookup therefore applied an `impl i64 { fn lt }` to a `u32`
/// receiver, printing `user` where the typechecker types the call `bool` and
/// the compiled backend printed `false`. The gate reads the typechecker's
/// `method_impl_dispatch` record instead, which is decided at the one point
/// that knows the static receiver type.
///
/// `target_span.is_some()` is what makes that record mean "a USER impl won":
/// `register_builtin_impl` puts the baked `Ord`/`Eq` through the same pick
/// branch, and recording those too made this gate fire for EVERY primitive
/// comparison and stand the builtin aside with nothing behind it — every case
/// below became "method 'lt' not found on type 'i64'".
#[test]
fn value_receiver_comparison_prefers_a_user_impl_over_the_builtin() {
    // No user impl in scope: the builtin answers, on every receiver class.
    for (recv, decl) in [
        ("i64", "let a: i64 = 3; let b: i64 = 4;"),
        ("u32", "let a: u32 = 3u32; let b: u32 = 4u32;"),
        ("bool", "let a: bool = false; let b: bool = true;"),
        ("char", "let a: char = 'x'; let b: char = 'y';"),
        ("String", "let a: String = \"x\"; let b: String = \"y\";"),
    ] {
        for (method, want) in [
            ("eq", "false\n"),
            ("ne", "true\n"),
            ("lt", "true\n"),
            ("le", "true\n"),
            ("gt", "false\n"),
            ("ge", "false\n"),
        ] {
            let src = format!("fn main() {{ {decl} println(a.{method}(b)); }}");
            assert_eq!(run(&src), want, "{recv}.{method}");
        }
    }

    // A user impl on the receiver's own type wins…
    assert_eq!(
        run(
            "impl i64 { fn lt(self, other: i64) -> String { \"user\" } }\n\
             fn main() { let a: i64 = 1; let b: i64 = 2; println(a.lt(b)); }"
        ),
        "user\n",
        "a user `impl i64` defining `lt` must beat the baked Ord for an i64 receiver"
    );

    // …and does NOT leak onto a different primitive. This is the erasure
    // control: `value_type_name` says `i64` for both receivers, so only a gate
    // that consults the typechecker can get this line right.
    assert_eq!(
        run(
            "impl i64 { fn lt(self, other: i64) -> String { \"user\" } }\n\
             fn main() { let c: u32 = 5u32; let d: u32 = 2u32; println(c.lt(d)); }"
        ),
        "false\n",
        "an `impl i64` must not answer for a u32 receiver — check types this `bool`"
    );

    // A user impl on a NON-comparison name was always reachable and must be
    // unaffected: it is the control that shows this change touched only the
    // seven names it meant to.
    assert_eq!(
        run("impl i64 { fn shout(self) -> String { \"hi\" } }\n\
             fn main() { let a: i64 = 1; println(a.shout()); }"),
        "hi\n",
        "control: a non-comparison user impl on a primitive"
    );
}

/// B-2026-09-01-20 — the same call-site default fill, reached through the
/// QUALIFIED `Type.assoc_fn(..)` spelling.
///
/// B-2026-08-17-19 shipped the fill for a bare identifier only and said so:
/// "Method / associated-function calls and module-qualified `Path` callees are
/// out of scope for this slice." design.md scopes the feature to no function
/// kind, though — "Parameters may have default values, allowing callers to omit
/// them" — so two of the three call forms did not have the feature the document
/// describes.
///
/// (This paragraph used to add that `runtime/stdlib/column.kara` "already
/// declares such a signature on an impl method, so the declaration form was in
/// use on a surface where the call form was an arity error". Measured against
/// the pre-fix tree, that is wrong: `Column.fillna` is `#[compiler_builtin]`
/// and dispatches through the builtin surface, so `c.fillna(0)` type-checked
/// and ran all along. It is also the only defaulted method signature in the
/// whole of `runtime/stdlib`, so the stdlib was never blocked by this. The row
/// stands on its actual symptom — a user-declared method.)
///
/// The four shapes are the free-function fixture's, verbatim, so the two read as
/// one oracle: omit every default, omit the tail, skip one by label, and mix a
/// positional override with two labels. Same values as
/// `test_e2e_default_parameter_call_site_fill`, because a fill that produced
/// anything else for the same signature would mean the two spellings disagree —
/// which is the whole complaint.
///
/// This fixture covers the ASSOCIATED half, which the pre-resolve pass fills
/// because `Type.assoc_fn` names its callee syntactically. The instance-method
/// half is the twin below: it cannot be filled here — picking the impl needs the
/// receiver's TYPE — so the typechecker plans it and `lowering` splices it.
#[test]
fn test_default_parameter_fill_through_an_associated_call_oracle() {
    assert_eq!(
        run(r#"
struct Server { id: i64 }

impl Server {
    fn create(host: i64, port: i64 = 8080, max_connections: i64 = 1000, timeout_ms: i64 = 5000) -> i64 {
        host + port + max_connections + timeout_ms
    }
}

fn main() {
    println(Server.create(1));
    println(Server.create(1, 9090));
    println(Server.create(1, max_connections: 100));
    println(Server.create(1, 9090, max_connections: 100, timeout_ms: 250));
}
"#),
        "14081\n15091\n13181\n9441\n"
    );
}

/// B-2026-09-01-40 — interpreter twin of `tests/codegen.rs`'s
/// `test_e2e_let_bound_scalar_field_read_runs_sibling_body_once`, same programs
/// and expectations.
///
/// This backend was CORRECT on every row; the compiled ones ran a sibling
/// field's body twice for a `let`-bound scalar field read. The twin is what
/// makes "both backends agree here" a property a test holds rather than one
/// only the fixed side asserts, and it pins the transcript this fix had to
/// converge ON.
#[test]
fn let_bound_scalar_field_read_runs_sibling_body_once() {
    const H: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         struct H { r: R, n: i64 }\n\
         impl Drop for H { fn drop(mut ref self) { println(\"dH\") } }\n\
         struct P { r: R, n: i64 }\n\
         struct H2 { r: R, r2: R, n: i64 }\n\
         impl Drop for H2 { fn drop(mut ref self) { println(\"dH2\") } }\n";
    for (label, body, want) in [
        (
            "let-bound scalar read",
            "let h = H { r: R { id: 3 }, n: 4 }; let q = h.n; println(f\"{q}\")",
            "dH\ndR3\n4\n",
        ),
        (
            "no field read (control)",
            "let h = H { r: R { id: 3 }, n: 4 }; println(\"mid\")",
            "dH\ndR3\nmid\n",
        ),
        (
            "inline read (control)",
            "let h = H { r: R { id: 3 }, n: 4 }; println(f\"{h.n}\")",
            "4\ndH\ndR3\n",
        ),
        (
            "two reads run one set of bodies",
            "let h = H { r: R { id: 3 }, n: 4 }; let q = h.n; let w = h.n; println(f\"{q}{w}\")",
            "dH\ndR3\n44\n",
        ),
        (
            "wrapper without its own Drop (control)",
            "let p = P { r: R { id: 3 }, n: 4 }; let q = p.n; println(f\"{q}\")",
            "dR3\n4\n",
        ),
        (
            "reading the Drop-bearing field (control)",
            "let h = H { r: R { id: 5 }, n: 4 }; let q = h.r; println(\"z\")",
            "dR5\ndH\nz\n",
        ),
        (
            "two Drop siblings each run once",
            "let h = H2 { r: R { id: 1 }, r2: R { id: 2 }, n: 4 }; let q = h.n; println(f\"{q}\")",
            "dH2\ndR2\ndR1\n4\n",
        ),
        (
            "field read inside an expression (control)",
            "let h = H { r: R { id: 4 }, n: 4 }; let q = h.n + 1; println(f\"{q}\")",
            "dH\ndR4\n5\n",
        ),
    ] {
        assert_eq!(
            run(&format!("{H}fn main() {{\n{body}\n}}\n")),
            want,
            "{label}"
        );
    }
}

// ── B-2026-09-03-17: user-defined associated functions on PRIMITIVE types ──
//
// `i64.zero()` where the program carries `impl Zero for i64` passed
// `karac check` clean and then failed on EVERY executor: the interpreter died
// with "name 'i64' resolved but has no binding at run time" and both compiled
// backends fell through method dispatch with "no handler for method 'zero' on
// variable 'i64'". Root cause was in the PARSER, upstream of all three — the
// `starts_upper` test that decides "this identifier is a type" is the parser's
// only notion of typehood, and every primitive name is lowercase, so
// `i64.zero()` parsed as a method call on a *value* named `i64`.

#[test]
fn test_assoc_fn_on_primitive_via_trait_impl() {
    // Deliberately NOT 0. Codegen's `compile_assoc_call` has a silent
    // `Ok(const 0)` tail for an unrecognized `Type.method`, so a `zero()` that
    // returns 0 passes whether it dispatched to the impl or fell through to
    // that default — the assertion would hold for the wrong reason.
    let out = run("trait Zero { fn zero() -> Self; }\n\
                   impl Zero for i64 { fn zero() -> i64 { return 41; } }\n\
                   fn main() { let x: i64 = i64.zero(); println(x.to_string()); }");
    assert_eq!(out.trim(), "41");
}

#[test]
fn test_assoc_fn_on_primitive_via_inherent_impl() {
    // The row's scoping (c): not trait-specific — an inherent `impl i64`
    // failed identically, so it is fixed and pinned identically.
    let out = run("impl i64 { fn two() -> i64 { return 42; } }\n\
                   fn main() { let x: i64 = i64.two(); println(x.to_string()); }");
    assert_eq!(out.trim(), "42");
}

#[test]
fn test_assoc_fn_on_primitive_is_uniform_across_primitives() {
    // The row measured the fall-through as uniform over i32/i64/u8/bool/f64.
    // Non-integer primitives matter most here: they are the ones codegen's
    // `const 0` tail would return the WRONG WIDTH for, not merely a wrong
    // integer, so they discriminate a real dispatch from a silent default.
    let out = run("trait Two { fn two() -> Self; }\n\
                   impl Two for u8 { fn two() -> u8 { return 44; } }\n\
                   impl Two for bool { fn two() -> bool { return true; } }\n\
                   impl Two for f64 { fn two() -> f64 { return 45.5; } }\n\
                   fn main() {\n\
                       println(u8.two().to_string());\n\
                       println(bool.two().to_string());\n\
                       println(f64.two().to_string());\n\
                   }");
    assert_eq!(out.trim(), "44\ntrue\n45.5");
}

#[test]
fn test_assoc_fn_on_primitive_chains_further_methods() {
    // The parse fix roots a `Path` and returns the `Call` straight out of
    // `parse_primary`, so a trailing `.to_string()` has to be picked up by the
    // caller's postfix loop rather than lost. Pins that it is.
    let out = run("trait Zero { fn zero() -> Self; }\n\
                   impl Zero for i64 { fn zero() -> i64 { return 41; } }\n\
                   fn main() { println(i64.zero().to_string()); }");
    assert_eq!(out.trim(), "41");
}

#[test]
fn test_primitive_assoc_constant_still_parses_as_field_access() {
    // NEGATIVE CONTROL, and the one the fix was most at risk of breaking. The
    // parser comment above this heuristic warns in as many words that a
    // primitive associated CONSTANT (`i64.MAX`, `f64.NAN` — lowercase head,
    // uppercase member, no parens) must stay a field access. The `(` in the
    // new lookahead is what keeps that true, including through a `.` chain
    // where the token after the member is a dot rather than a paren.
    let out = run("fn main() {\n\
                       println(i64.MAX.to_string());\n\
                       println(i64.MIN.to_string());\n\
                   }");
    assert_eq!(out.trim(), "9223372036854775807\n-9223372036854775808");
}

#[test]
fn test_builtin_primitive_assoc_fn_still_resolves() {
    // NEGATIVE CONTROL, and the one that decided WHERE this bug gets fixed.
    // The tempting fix is in the parser: root a `Path` for any
    // `<primitive>.<name>(`, so the call resolves as an associated function the
    // way `P.zero()` does on a user struct. That breaks this — the BUILT-IN
    // primitive associated functions reach their inference through the
    // identifier-receiver METHOD path, so re-shaping the AST takes them out of
    // the arm that serves them. This exact snippet, from
    // `docs/book/ch09b-strings-and-bytes.md`, is what caught it (the
    // `book_snippets` suite failed on it), and it is pinned here so the cheaper
    // fix cannot be reintroduced without a direct, named failure.
    let out = run("fn digit_char(d: i64) -> char {\n\
                       match char.try_from(b'0' + d as u8) {\n\
                           Ok(c)  => c,\n\
                           Err(_) => '?',\n\
                       }\n\
                   }\n\
                   fn main() { println(digit_char(5).to_string()); }");
    assert_eq!(out.trim(), "5");
}

/// B-2026-09-03-16 — A GENERIC PARENT'S PROJECTION DESTRUCTURE MUST HAND THE
/// ELEMENT'S `Drop` BODY TO THE LEAF, NOT LEAVE IT WITH THE SOURCE.
///
/// `struct G[T] { pe: (T, i64) }; let (r, k) = h.pe;` ran the element's body at
/// `h`'s NLL death on the three compiled surfaces and at the leaf's under
/// `--interp`. ONE body on both sides, on a LIVE value on both sides — a
/// PLACEMENT split with no husk, which is why a body COUNT cannot see it and
/// `marker` below is the cell that identifies the owner: it extends the SOURCE's
/// live range past the read (`z{h.z}`), so a body belonging to `h` prints AFTER
/// `z9` and a body belonging to the leaf prints before it.
///
/// `struct_field_type_exprs` is keyed by the DECLARATION name and stores fields
/// as written, so `G`'s element read back as the bare `T` however `G` was
/// instantiated; `T` names neither an enum nor a struct, so the leaf arm
/// declined and took neither body nor memory. The same name-keyed lookup fed the
/// LLVM layout, where the placeholder width made a load read `dR/​/0`.
/// `nongeneric` is the twin that was correct throughout and is what put the
/// fault on the missing substitution rather than on the projection source.
///
/// Every `Drop` body renders `tag` and `xs.len()`, so a body running on a
/// cap-zeroed husk prints `dRnn//0` and is distinguishable from a correct one —
/// the whole family asserts COUNTS over a payload that cannot render empty, and
/// this class of defect hides in exactly that gap.
///
/// OVER-REACH CONTROLS. `nodrop` instantiates the same generic at a
/// `Drop`-less type and must stay silent; `bothelems` and `secondpos` place the
/// parameter in either position; `nested` goes a level down; `wildcard`
/// discards the leaf; `paramroot` is the non-generic by-value param.
///
/// `genericfn` PINNED THE ONE SHAPE THIS FIX DECLINED. A by-value param of a
/// GENERIC function is emitted by `compile_generic_call`, which populated
/// neither `current_fn_param_names` nor `owned_struct_params`, so the ownership
/// gate could not tell it from a local and the leaf would take a body the
/// caller's copy also runs. The substitution made that shape reachable for the
/// first time, so it was guarded rather than answered wrongly. B-2026-09-03-23
/// gave the monomorph body its own param-ownership identity and REMOVED that
/// guard; this cell is the pin that it did not move when it came off, and it
/// still holds at ONE body matching its non-generic twin.
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_generic_parent_projection_destructure_hands_the_body_to_the_leaf`, pinned to the same string.
#[test]
fn test_generic_parent_projection_destructure_hands_the_body_to_the_leaf() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}/{self.xs.len()}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
struct G[T]  { pe: (T, i64), z: i64 }
struct Gn    { pe: (R, i64), z: i64 }
struct P2[A, B] { pe: (A, B), z: i64 }
struct Mix[T] { pe: (i64, T), z: i64 }
struct Nest[T] { pe: ((T, i64), i64), z: i64 }
struct Plain { a: i64, b: i64 }

fn n1() { let h = G[R] { pe: (mk(41), 0), z: 9 }; let (r, k) = h.pe; println(f"  b{r.id}/{r.tag}/{r.xs.len()}") }
fn n2() { let h = G[R] { pe: (mk(42), 0), z: 9 }; let (r, k) = h.pe; println(f"  b{r.id}/{r.tag}"); println(f"  z{h.z}") }
fn n3() { let h = Gn   { pe: (mk(43), 0), z: 9 }; let (r, k) = h.pe; println(f"  b{r.id}/{r.tag}") }
fn n4() { let h = G[R] { pe: (mk(44), 0), z: 9 }; let (r, k) = h.pe; let m = r; println(f"  b{m.id}/{m.tag}/{m.xs.len()}") }
fn n5() { let h = P2[R, R] { pe: (mk(45), mk(95)), z: 9 }; let (a, b) = h.pe; println(f"  b{a.id}/{b.id}") }
fn n6() { let h = Mix[R] { pe: (7, mk(46)), z: 9 }; let (a, r) = h.pe; println(f"  b{a}/{r.id}") }
fn n7() { let h = G[Plain] { pe: (Plain { a: 1, b: 2 }, 0), z: 9 }; let (p, k) = h.pe; println(f"  b{p.a}/{p.b}") }
fn n8() { let h = G[R] { pe: (mk(48), 0), z: 9 }; let (_, k) = h.pe; println(f"  b{k}") }
fn n9() { let h = Nest[R] { pe: ((mk(49), 1), 2), z: 9 }; let ((r, a), b) = h.pe; println(f"  b{r.id}/{a}/{b}") }
fn n10[T](h: G[T]) -> i64 { let (r, k) = h.pe; println("  in"); return k; }
fn n11() { let h = G[R] { pe: (mk(51), 5), z: 9 }; let k = n10(h); println(f"  b{k}") }
fn n12(h: Gn) { let (r, k) = h.pe; println(f"  b{r.id}/{r.tag}") }

fn main() {
    println("generic");    n1();                             println("generic end")
    println("marker");     n2();                             println("marker end")
    println("nongeneric"); n3();                             println("nongeneric end")
    println("rebind");     n4();                             println("rebind end")
    println("bothelems");  n5();                             println("bothelems end")
    println("secondpos");  n6();                             println("secondpos end")
    println("nodrop");     n7();                             println("nodrop end")
    println("wildcard");   n8();                             println("wildcard end")
    println("nested");     n9();                             println("nested end")
    println("genericfn");  n11();                            println("genericfn end")
    println("paramroot");  n12(Gn { pe: (mk(52), 0), z: 9 }); println("paramroot end")
    println("done")
}
"#),
        r#"generic
  b41/t41/1
dR41/t41/1
generic end
marker
  b42/t42
dR42/t42/1
  z9
marker end
nongeneric
  b43/t43
dR43/t43/1
nongeneric end
rebind
  b44/t44/1
dR44/t44/1
rebind end
bothelems
  b45/95
dR95/t95/1
dR45/t45/1
bothelems end
secondpos
  b7/46
dR46/t46/1
secondpos end
nodrop
  b1/2
nodrop end
wildcard
dR48/t48/1
  b0
wildcard end
nested
  b49/1/2
dR49/t49/1
nested end
genericfn
  in
dR51/t51/1
  b5
genericfn end
paramroot
  b52/t52
dR52/t52/1
paramroot end
done
"#
    );
}
