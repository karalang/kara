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
