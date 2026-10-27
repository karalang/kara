//! B-2026-10-27-13: the library's operator traits are declared as design.md
//! § Operator traits writes them (`trait Add[Rhs = Self] { type Output; fn
//! add(own self, rhs: own Rhs) -> Self.Output; }`), so an operator impl's
//! signature is checked against its trait like any other impl's
//! (B-2026-10-27-9): `impl Add for I` is `impl Add[I] for I`, whose `add`
//! takes an `I`. A trait's own generic parameters take defaults (design.md
//! § Default generic arguments); a function's take none.

use std::process::Command;

fn karac(tag: &str, cmd: &str, src: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-operator-traits-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&file)
        .output()
        .unwrap();
    let fixed = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = if cmd == "fix" {
        fixed
    } else {
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    (
        out.status.success(),
        stdout,
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

const MIXED: &str = r#"
struct D { ns: i64 }
struct I { ns: i64 }
impl Add for I {
    type Output = I;
    fn add(own self, rhs: own D) -> I { I { ns: self.ns + rhs.ns } }
}
fn main() { let x = I { ns: 1 } + D { ns: 2 }; println(x.ns); }
"#;

#[test]
fn an_operator_impl_taking_another_rhs_must_name_it() {
    let (ok, _, err) = karac("mixed", "check", MIXED);
    assert!(!ok, "{err}");
    assert!(
        err.contains(
            "parameter 'rhs' of impl method 'I.add' is `D`, but trait 'Add' declares `I` \
             there: an impl method's signature is the trait's. To take a `D`, implement \
             `Add[D]`"
        ),
        "{err}"
    );
    let named = MIXED.replace("impl Add for I", "impl Add[D] for I");
    let (ok, out, err) = karac("mixed-named", "run", &named);
    assert!(ok, "{err}");
    assert_eq!(out, "3\n");
}

#[test]
fn an_operator_impl_must_bind_output_and_karac_fix_binds_it() {
    let src = r#"
struct M { c: i64 }
impl Neg for M {
    fn neg(own self) -> M { M { c: -self.c } }
}
impl Sub for M {
    type Output = Self;
    fn sub(own self, rhs: own M) -> Self { M { c: self.c - rhs.c } }
}
fn main() { let m = -M { c: 4 } - M { c: 1 }; println(m.c); }
"#;
    let (ok, _, err) = karac("output", "check", src);
    assert!(!ok);
    assert!(
        err.contains("impl of trait 'Neg' is missing associated type 'Output'"),
        "{err}"
    );
    let (_, fixed, err) = karac("output-fix", "fix", src);
    assert!(
        fixed.contains("impl Neg for M {\n    type Output = M;\n    fn neg"),
        "{fixed}\n{err}"
    );
    let (ok, out, err) = karac("output-fixed", "run", &fixed);
    assert!(ok, "{err}");
    assert_eq!(out, "-5\n");
}

#[test]
fn a_trait_parameter_default_is_the_omitted_argument() {
    let src = r#"
trait Plus[Rhs = Self] {
    type Output;
    fn plus(own self, rhs: own Rhs) -> Self.Output;
}
struct M { c: i64 }
impl Plus for M {
    type Output = M;
    fn plus(own self, rhs: own i64) -> M { M { c: self.c + rhs } }
}
fn main() { println(M { c: 1 }.plus(2).c); }
"#;
    let (ok, _, err) = karac("default", "check", src);
    assert!(!ok);
    assert!(
        err.contains(
            "parameter 'rhs' of impl method 'M.plus' is `i64`, but trait 'Plus' declares `M` there"
        ),
        "{err}"
    );
    let named = src.replace("impl Plus for M", "impl Plus[i64] for M");
    let (ok, out, err) = karac("default-named", "run", &named);
    assert!(ok, "{err}");
    assert_eq!(out, "3\n");
}

#[test]
fn a_default_generic_argument_is_a_traits_only() {
    let (ok, _, err) = karac(
        "fn-default",
        "check",
        "fn f[T = i64](x: T) -> T { x }\nfn main() { println(f(1)); }\n",
    );
    assert!(!ok);
    assert!(
        err.contains(
            "a default generic argument (`T = Type`) is allowed on a trait's type parameters only"
        ),
        "{err}"
    );
    let (ok, _, err) = karac(
        "order",
        "check",
        "trait P[A = Self, B] { fn p(self, a: A, b: B); }\nfn main() { }\n",
    );
    assert!(!ok);
    assert!(
        err.contains("a generic parameter without a default cannot follow one with a default"),
        "{err}"
    );
}

// B-2026-10-27-14: an operator on a bounded parameter calls the bound's
// impl, so its result is the bound's `Output` and its right operand the
// bound's `Rhs`. The result used to be the parameter whatever the impl's
// `Output`, so a `T: Sub` body read an `Instant - Instant` `Duration` as an
// `Instant`.

const INSTANT: &str = r#"
struct D { ns: i64 }
struct I { ns: i64 }
impl Sub for I {
    type Output = D;
    fn sub(own self, rhs: own I) -> D { D { ns: self.ns - rhs.ns } }
}
impl Add[D] for I {
    type Output = I;
    fn add(own self, rhs: own D) -> I { I { ns: self.ns + rhs.ns } }
}
"#;

#[test]
fn an_operator_on_a_bounded_parameter_is_its_bounds_output() {
    let gap = format!(
        "{INSTANT}fn gap[T: Sub](a: own T, b: own T) -> T.Output {{ a - b }}\n\
         fn main() {{ println(gap(I {{ ns: 7 }}, I {{ ns: 3 }}).ns); }}\n"
    );
    let (ok, out, err) = karac("output-proj", "run", &gap);
    assert!(ok, "{err}");
    assert_eq!(out, "4\n");

    let wrong = format!(
        "{INSTANT}fn gap[T: Sub](a: own T, b: own T) -> T {{ a - b }}\n\
         fn main() {{ println(gap(I {{ ns: 7 }}, I {{ ns: 3 }}).ns); }}\n"
    );
    let (ok, _, err) = karac("output-unfixed", "check", &wrong);
    assert!(!ok);
    assert!(
        err.contains(
            "expected 'T', found 'T.Output': the result of an operator on a 'T' is its \
             bound's `Output` (design.md § Operator traits), which is 'T' only where the \
             bound says so: write `Sub[Output = T]`"
        ),
        "{err}"
    );
    // The fix states it, and the program it describes is then refused at
    // the call, where `I - I` is a `D`.
    let (_, fixed, _) = karac("output-unfixed-fix", "fix", &wrong);
    assert!(
        fixed.contains("fn gap[T: Sub[Output = T]](a: own T"),
        "{fixed}"
    );
    let (ok, _, err) = karac("output-unfixed-fixed", "check", &fixed);
    assert!(!ok, "an `I` is not a `Sub[Output = I]`: {err}");
}

#[test]
fn a_bound_naming_output_t_keeps_arithmetic_on_t() {
    let src = "fn twice[T: Add](a: own T, b: own T, c: own T) -> T { a + b + c }\n\
               fn neg[T: Neg](a: own T) -> T { -a }\n\
               fn acc[T: Add + Copy](a: own T, b: own T) -> T { let mut x = a; x += b; x }\n\
               fn main() { println(f\"{twice(1, 2, 3)} {neg(4)} {acc(5, 6)}\"); }\n";
    let (ok, _, err) = karac("unfixed", "check", src);
    assert!(!ok);
    assert!(
        err.contains("'T.Output' has no operator here"),
        "a chain operates on an unfixed `T.Output`: {err}"
    );
    assert!(err.contains("write `Neg[Output = T]`"), "{err}");
    assert!(err.contains("write `Add[Output = T]`"), "{err}");
    let (_, fixed, err) = karac("unfixed-fix", "fix", src);
    assert!(
        fixed.contains("fn twice[T: Add[Output = T]]")
            && fixed.contains("fn neg[T: Neg[Output = T]]")
            && fixed.contains("fn acc[T: Add[Output = T] + Copy]"),
        "{fixed}\n{err}"
    );
    let (ok, out, err) = karac("unfixed-fixed", "run", &fixed);
    assert!(ok, "{err}");
    assert_eq!(out, "6 -4 11\n");
}

#[test]
fn a_bound_names_another_right_hand_type() {
    let src = format!(
        "{INSTANT}fn later[T: Add[D, Output = T]](a: own T, d: own D) -> T {{ a + d }}\n\
         fn main() {{ println(later(I {{ ns: 1 }}, D {{ ns: 2 }}).ns); }}\n"
    );
    let (ok, out, err) = karac("rhs", "run", &src);
    assert!(ok, "{err}");
    assert_eq!(out, "3\n");

    let same = format!(
        "{INSTANT}fn later[T: Add[D, Output = T]](a: own T, b: own T) -> T {{ a + b }}\n\
         fn main() {{ }}\n"
    );
    let (ok, _, err) = karac("rhs-wrong", "check", &same);
    assert!(!ok);
    assert!(err.contains("expected 'D', found 'T'"), "{err}");
    assert_eq!(
        err.matches("error[").count(),
        1,
        "one error, no cascade: {err}"
    );

    let unnamed = "fn f[T: Add, U: Add](a: own T, b: own U) -> T { a + b }\nfn main() { }\n";
    let (ok, _, err) = karac("rhs-unnamed", "check", unnamed);
    assert!(!ok);
    assert!(
        err.contains("found 'T' and 'U'; a bound names another right-hand type as `Add[R]`"),
        "{err}"
    );
    assert_eq!(
        err.matches("error[").count(),
        1,
        "one error, no cascade: {err}"
    );
}

#[test]
fn an_output_eq_picks_the_bounds_trait_and_reaches_trait_defaults() {
    // `V` has two operator impls with different `Output`s; `Add[Output = T]`
    // is discharged against `Add`'s, not `Mul`'s.
    let src = r#"
struct V { x: i64 }
impl Add for V {
    type Output = V;
    fn add(own self, rhs: own V) -> V { V { x: self.x + rhs.x } }
}
impl Mul for V {
    type Output = i64;
    fn mul(own self, rhs: own V) -> i64 { self.x * rhs.x }
}
fn sum2[T: Add[Output = T]](a: own T, b: own T) -> T { a + b }
trait Pair[T: Add[Output = T] + Copy] {
    fn a(ref self) -> T;
    fn b(ref self) -> T;
    fn both(ref self) -> T { self.a() + self.b() }
}
struct P { l: i64, r: i64 }
impl Pair[i64] for P {
    fn a(ref self) -> i64 { self.l }
    fn b(ref self) -> i64 { self.r }
}
fn main() {
    println(f"{sum2(V { x: 2 }, V { x: 3 }).x} {V { x: 2 } * V { x: 3 }} {P { l: 4, r: 5 }.both()}");
}
"#;
    let (ok, out, err) = karac("eq-trait", "run", src);
    assert!(ok, "{err}");
    assert_eq!(out, "5 6 9\n");
}

#[test]
fn a_generic_caller_passes_on_only_an_output_its_bound_fixes() {
    // `h`'s `T: Add` says nothing of `T.Output`, so it cannot pass its `T`
    // to `g`, whose bound requires `Output = T`: at `I` it would hand `g`'s
    // caller a `D` as an `I`.
    let src = format!(
        "{INSTANT}fn g[T: Add[Output = T]](a: own T, b: own T) -> T {{ a + b }}\n\
         fn h[T: Add + Mul](a: own T, b: own T) -> T {{ g(a, b) }}\n\
         fn main() {{ println(h(2, 3)); }}\n"
    );
    let (ok, _, err) = karac("caller", "check", &src);
    assert!(!ok);
    assert!(
        err.contains(
            "associated-type bound `T.Output = T` is not satisfied; 'T' is bounded without \
             fixing its `Output`: write `Add[Output = T]`"
        ),
        "the edit names `h`'s `Add`, not its `Mul`: {err}"
    );
    let (_, fixed, err) = karac("caller-fix", "fix", &src);
    assert!(
        fixed.contains("fn h[T: Add[Output = T] + Mul](a: own T"),
        "{fixed}\n{err}"
    );
    let (ok, out, err) = karac("caller-fixed", "run", &fixed);
    assert!(ok, "{err}");
    assert_eq!(out, "5\n");
}
