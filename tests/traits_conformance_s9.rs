//! The design.md §9 (Traits) conformance pass (2026-11-16): each program
//! pins one claim the pass found the compiler breaking. `<` on a recursive
//! type whose ordering is derived, a NaN deciding a derived `<=` / `>=`, an
//! `impl Iterator[Item = ..]` return over literals, a derive whose field
//! lacks the trait, and a trait's associated function called on no type.

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-traits-s9-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `karac check` passes and `karac run --interp` prints `want`; with the
/// backend, the compiled program prints it too.
fn runs(tag: &str, src: &str, want: &str) {
    let dir = scratch(tag);
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let (ok, out, err) = karac(&dir, &["check", "main.kara"]);
    assert!(ok, "check refused:\n{out}{err}");
    let (ok, out, err) = karac(&dir, &["run", "--interp", "main.kara"]);
    assert!(ok, "interp failed:\n{out}{err}");
    assert_eq!(out, want, "interp\n{err}");
    #[cfg(feature = "llvm")]
    {
        let (ok, out, err) = karac(&dir, &["__mir-llvm-run", "main.kara"]);
        assert!(ok, "mir-llvm failed:\n{out}{err}");
        assert_eq!(out, want, "mir-llvm\n{err}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `karac check` refuses with each of `wants` in its output.
fn refused(tag: &str, src: &str, wants: &[&str]) {
    let dir = scratch(tag);
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let (ok, out, err) = karac(&dir, &["check", "main.kara"]);
    let text = format!("{out}{err}");
    assert!(!ok, "check accepted:\n{text}");
    for w in wants {
        assert!(text.contains(w), "missing `{w}` in:\n{text}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_recursive_type_orders_by_its_derive_without_overflowing_the_checker() {
    runs(
        "rec-ord",
        r#"#[derive(PartialEq, PartialOrd, Debug)]
enum Tree { Leaf(f64), Node(Vec[Tree]) }
#[derive(PartialEq, Eq, PartialOrd, Ord, Debug)]
enum T2 { L(i64), N(Vec[T2]) }
#[derive(PartialEq, PartialOrd)]
struct S { k: Vec[S], v: i64 }
fn main() {
    println(Tree.Leaf(1.0) < Tree.Node([]));
    println(Tree.Node([Tree.Leaf(2.0)]) < Tree.Node([Tree.Leaf(1.0)]));
    println(Tree.Node([Tree.Leaf(1.0)]) < Tree.Node([Tree.Leaf(1.0), Tree.Leaf(0.0)]));
    let nan = Tree.Node([Tree.Leaf(0.0 / 0.0), Tree.Leaf(1.0)]);
    let one = Tree.Node([Tree.Leaf(1.0), Tree.Leaf(0.0)]);
    println(nan < one);
    println(nan >= one);
    println(nan.partial_cmp(one) == None);
    println(T2.N([T2.L(3)]).cmp(T2.N([T2.L(2), T2.L(9)])) == Ordering.Greater);
    let mut v = [T2.N([T2.L(2)]), T2.L(5), T2.N([]), T2.N([T2.N([])])];
    v.sort();
    dbg(v);
    println(S { k: [S { k: [], v: 2 }], v: 0 } > S { k: [S { k: [], v: 1 }], v: 9 });
}
"#,
        "true\nfalse\ntrue\nfalse\nfalse\ntrue\ntrue\ntrue\n",
    );
}

#[test]
fn an_unordered_float_makes_every_derived_comparison_false() {
    runs(
        "nan-le",
        r#"#[derive(PartialEq, PartialOrd)]
struct P { x: f64 }
fn main() {
    let a = P { x: 0.0 / 0.0 };
    let b = P { x: 1.0 };
    println(f"{a < b} {a <= b} {a > b} {a >= b}");
    println(f"{(0.0 / 0.0, 1) >= (1.0, 1)} {[0.0 / 0.0] >= [1.0]} {[0.0 / 0.0] <= [1.0]}");
    println(f"{P { x: 1.0 } <= P { x: 1.0 }} {(2.0, 1) >= (1.0, 5)}");
}
"#,
        "false false false false\nfalse false false\ntrue true\n",
    );
}

#[test]
fn an_impl_iterator_return_types_its_literals_by_the_binding() {
    runs(
        "impl-lit",
        r#"fn f() -> impl Iterator[Item = i64] { 0..3 }
fn g() -> impl Iterator[Item = i32] { 0..3 }
fn h() -> impl Iterator[Item = u8] { [250, 255].into_iter() }
trait Src { fn items(self) -> impl Iterator[Item = i64]; }
struct A { n: i64 }
impl Src for A { fn items(self) -> impl Iterator[Item = i64] { 0..self.n } }
struct B {}
impl Src for B { fn items(self) -> impl Iterator[Item = i64] { [7, 8].into_iter() } }
fn sum[S: Src](s: S) -> i64 { s.items().sum() }
fn main() {
    let v: Vec[i64] = f().collect();
    println(v);
    for x in g() { let y: i32 = x; print(f"{y} "); }
    println("");
    let mut t: u32 = 0;
    for b in h() { t += b; }
    println(t);
    println(sum(A { n: 4 }));
    println(sum(B {}));
    let it: dyn Iterator[Item = i64] = [1, 2, 3].into_iter();
    let w: Vec[i64] = it.map(|x| x * 10).collect();
    println(w);
}
"#,
        "[0, 1, 2]\n0 1 2 \n505\n6\n15\n[10, 20, 30]\n",
    );
}

#[test]
fn an_impl_iterator_binding_is_kept_by_its_witness() {
    refused(
        "impl-u8-300",
        "fn h() -> impl Iterator[Item = u8] { [300].into_iter() }\nfn main() {}\n",
        &["integer literal 300 out of range for 'u8'"],
    );
    refused(
        "impl-u8-i64",
        "fn h() -> impl Iterator[Item = u8] { let v: Vec[i64] = [1]; v.into_iter() }\nfn main() {}\n",
        &["E_IMPL_TRAIT_ASSOC_MISMATCH", "`VecIntoIter[i64]` declares `Item = i64`"],
    );
    refused(
        "impl-two",
        "fn f(b: bool) -> impl Iterator[Item = i64] { if b { return 0..3; } [1, 2].into_iter() }\nfn main() {}\n",
        &["E_IMPL_TRAIT_MULTIPLE_WITNESSES", "Range[i64], VecIntoIter[i64]"],
    );
}

#[test]
fn a_derive_needs_the_trait_on_every_field() {
    // 0.8.x: a `deprecated` warning, as an implied derive is; the next minor
    // release makes it the error.
    let dir = scratch("derive-fields");
    let src = r#"import std.secret.{Secret};
struct A { n: i64 }
impl Display for A { fn to_string(self) -> String { "A!" } }
#[derive(Debug)]
struct B { a: A }
struct NoD { n: i64 }
#[derive(Display)]
struct C { a: NoD }
#[derive(Debug)]
enum E { V(NoD) }
#[derive(Clone)]
struct K { a: NoD }
#[derive(Clone)]
struct Raw { p: *mut u8 }
#[derive(Debug, Display)]
struct Login { token: Secret[String] }
fn main() {}
"#;
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let (ok, out, err) = karac(&dir, &["check", "main.kara"]);
    let text = format!("{out}{err}");
    assert!(ok, "check refused:\n{text}");
    for w in [
        "struct 'B' derives Debug but field 'a' has non-Debug type 'A'",
        "struct 'C' derives Display but field 'a' has non-Display type 'NoD'",
        "enum 'E' derives Debug but variant 'V' field '0' has non-Debug type 'NoD'",
        "struct 'K' derives Clone but field 'a' has non-Clone type 'NoD'",
    ] {
        assert!(text.contains(w), "missing `{w}` in:\n{text}");
    }
    assert_eq!(text.matches("warning[deprecated]").count(), 4, "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_trait_associated_function_is_called_on_a_type() {
    refused(
        "assoc-fn-no-type",
        r#"trait Make { fn name() -> String { "make" } }
struct P {}
impl Make for P {}
fn main() { println(Make.name()); println(P.name()); }
"#,
        &["`Make.name()` names no type", "`P.name(..)`"],
    );
}

/// `karac check` refuses, printing `want` exactly `n` times.
fn refused_n(tag: &str, src: &str, want: &str, n: usize) {
    let dir = scratch(tag);
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let (ok, out, err) = karac(&dir, &["check", "main.kara"]);
    let text = format!("{out}{err}");
    assert!(!ok, "check accepted:\n{text}");
    assert_eq!(text.matches(want).count(), n, "`{want}` in:\n{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn each_finding_is_reported_once() {
    // A marker trait's body item is one error, not one per token.
    refused_n(
        "marker-body",
        "marker trait P { fn f(self) { let x = 1; } type T; const N: i64 = 3; }\n\
         fn main() {}\n",
        "marker traits cannot",
        3,
    );
    // A `dyn` parameter type is lowered for the signature and the body.
    refused_n(
        "dyn-unbound",
        "fn f(x: dyn Iterator) {}\nfn main() {}\n",
        "leaves the associated type `Item` unbound",
        1,
    );
    // A derive beside a hand impl is one duplicate-impl error.
    refused_n(
        "derive-and-impl",
        "#[derive(Clone)]\nstruct S { a: i64 }\n\
         impl Clone for S { fn clone(self) -> S { S { a: self.a } } }\nfn main() {}\n",
        "error[",
        1,
    );
}

#[test]
fn a_trait_associated_function_takes_self_from_the_expected_type() {
    // Review-thread ruling 2026-11-16: `Make.name()` takes `Self` from the
    // expected type through the return type, reaching P's trait impl even
    // beside an inherent `P.name()` (B-2026-11-16-331).
    runs(
        "assoc-fn-expected",
        r#"trait Make { fn name() -> Self; fn tag() -> String; }
struct P { v: i64 }
impl P { fn name() -> P { P { v: 1 } } }
impl Make for P { fn name() -> P { P { v: 2 } } fn tag() -> String { "p" } }
struct Q { s: String }
impl Make for Q { fn name() -> Q { Q { s: "q" } } fn tag() -> String { "q" } }
trait Parse { fn parse_it(s: Str) -> Option[Self]; }
impl Parse for P { fn parse_it(s: Str) -> Option[P] { Some(P { v: s.len() }) } }
trait Build { fn build(n: i64) -> Self; }
struct W[T] { v: Vec[T] }
impl[T] Build for W[T] { fn build(n: i64) -> W[T] { W { v: Vec.new() } } }
impl Build for i64 { fn build(n: i64) -> i64 { n * 2 } }
fn mk() -> Q { Make.name() }
fn twice() -> i64 { Build.build(21) }
fn main() {
    let p: P = Make.name();
    let i: P = P.name();
    let o: Option[P] = Parse.parse_it("abc");
    let w: W[String] = Build.build(3);
    let k: i64 = Build.build(4);
    println(f"{p.v} {i.v} {mk().s} {o.unwrap().v}");
    println(f"{w.v.len()} {k} {twice()}");
}
"#,
        "2 1 q 3\n0 8 42\n",
    );
}

#[test]
fn a_trait_associated_function_with_nothing_to_fix_self_is_refused() {
    refused(
        "assoc-fn-unfixed",
        r#"trait Make { fn name() -> Self; fn tag() -> String; }
struct P { v: i64 }
impl Make for P { fn name() -> P { P { v: 2 } } fn tag() -> String { "p" } }
struct R { }
fn main() {
    let a = Make.name();
    let b = Make.tag();
    let c: R = Make.name();
}
"#,
        &[
            "`Make.name()` names no type",
            "or annotate the binding with the type, `let x: P = Make.name(..)`",
            "`Make.tag()` names no type",
            "`Make.name()` takes `Self` from the expected type, `R`, which does not implement `Make`",
        ],
    );
}

#[test]
fn a_type_parameter_no_call_can_decide_is_refused() {
    // Review-thread ruling 2026-11-16 (B-2026-11-16-333): a call takes no
    // type arguments, so a parameter named nowhere a call can see is refused
    // where it is declared; a trait impl's method is reported at the trait.
    refused_n(
        "uninferable",
        r#"trait Named { fn name() -> String; fn pick[U]() -> i64; }
struct P { }
impl Named for P { fn name() -> String { "p" } fn pick[U]() -> i64 { 1 } }
fn who[T: Named]() -> String { T.name() }
struct B { }
impl B { fn m[Q](ref self) -> i64 { 0 } }
fn main() { println(P.name()); }
"#,
        "so no call can decide it",
        3,
    );
    // Named in a parameter, the return type, another parameter's bound or a
    // where clause, it is decided there.
    runs(
        "inferable",
        r#"fn ok1[T](x: own T) -> T { x }
fn ok2[T]() -> Vec[T] { Vec.new() }
fn ok3[I: Iterator[Item = T], T](i: own I) -> i64 { 0 }
fn ok4[F: Fn(i64) -> R, R](f: F) -> i64 { 0 }
fn ok5[T, U](x: T) -> i64 where T: Into[U] { 0 }
fn main() {
    let v: Vec[i64] = ok2();
    println(f"{ok1(3)} {v.len()} {ok3([1, 2].into_iter())} {ok4(|n| n + 1)}");
}
"#,
        "3 0 0 0\n",
    );
}

#[test]
fn every_std_error_type_is_debug() {
    // Review-thread ruling 2026-11-16 (B-2026-11-16-334): check took no
    // `dbg(e)` of these while the backends rendered it.
    runs(
        "std-errors-debug",
        r#"import std.json.{Json, JsonError};

#[derive(Deserialize, Debug)]
struct N { title: String, n: i64 }

fn main() {
    let r: Result[i64, ParseError] = i64.parse("x");
    match r { Ok(_) => {}, Err(e) => { dbg(e); } }
    match Json.parse("{") { Ok(_) => {}, Err(e) => { dbg(e); } }
    let d: Result[N, DeserializeError] = Json.deserialize("{\"title\": 3}");
    match d { Ok(_) => {}, Err(e) => { dbg(e); } }
    let bad: Vec[u8] = [255, 0];
    match String.from_utf8(bad.as_slice()) { Ok(_) => {}, Err(e) => { dbg(e); } }
    println("done");
}
"#,
        "done\n",
    );
}
