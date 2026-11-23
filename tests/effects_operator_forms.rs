//! B-2026-11-23-10: every operator is a call to a trait method (design.md
//! § Operator traits), and a program impl reached through `==`, `!=`, a
//! compound assignment, unary `-` / `~`, indexing or `assert_eq` was not
//! charged its effects: `a == b` with an `impl PartialEq` whose `eq` writes
//! Stderr reported no `writes(Stderr)`, while `a.eq(b)` and `a + b` did.
//! The typechecker now keys the impl each operator runs by the operator's
//! node, and the effect pass charges it.

use std::path::PathBuf;
use std::process::{Command, Stdio};

const LOUD: &str = "eprintln(\"loud\");";

const EQ: &str = "impl PartialEq for Big {
    fn eq(self, other: Big) -> bool {
        eprintln(\"loud\");
        self.v == other.v
    }
}
";

const ADD: &str = "impl Add[Big] for Big {
    type Output = Big;
    fn add(self, other: Big) -> Big {
        eprintln(\"loud\");
        Big { v: self.v + other.v }
    }
}
";

const MUL: &str = "impl Mul[i64] for Big {
    type Output = Big;
    fn mul(self, k: i64) -> Big {
        eprintln(\"loud\");
        Big { v: self.v * k }
    }
}
";

const NEG: &str = "impl Neg for Big {
    type Output = Big;
    fn neg(self) -> Big {
        eprintln(\"loud\");
        Big { v: 0 - self.v }
    }
}
";

const NOT: &str = "impl Not for Big {
    type Output = Big;
    fn not(self) -> Big {
        eprintln(\"loud\");
        Big { v: 0 - self.v }
    }
}
";

const INDEX: &str = "impl Index[i64] for Big {
    type Output = i64;
    fn index(self, i: i64) -> ref i64 {
        eprintln(\"loud\");
        ref self.v
    }
}
";

/// `check --output=json` of a one-file program: its error messages, and its
/// `program_effects` when it has no error.
fn check(tag: &str, src: &str) -> (Vec<String>, Vec<String>) {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("karac-operator-fx-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "cell.kara", "--output=json"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    let errors = v["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .filter(|d| d["severity"] == "error")
        .map(|d| d["message"].as_str().unwrap_or_default().to_string())
        .collect();
    let effects = v["program_effects"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|e| e.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    (errors, effects)
}

fn program(impls: &str, body: &str) -> String {
    format!(
        "struct Big {{ v: i64 }}\n\n{impls}\nfn main() {{\n    let a = Big {{ v: 1 }};\n    let b = \
         Big {{ v: 2 }};\n    {body}\n}}\n"
    )
}

fn writes_stderr(effects: &[String]) -> bool {
    effects.iter().any(|e| e == "writes(Stderr)")
}

/// Each operator form on a type whose impl writes Stderr reports it, and
/// the same program with the write removed from the impl does not: the
/// charge is the impl's, not the operator's.
#[test]
fn an_operator_is_charged_the_program_impl_it_runs() {
    for (tag, impls, body) in [
        ("eq", EQ, "let e = a == b;"),
        ("ne", EQ, "let e = a != b;"),
        ("eq-ref", EQ, "let r = ref a;\n    let e = r == b;"),
        ("add", ADD, "let e = a + b;"),
        ("add-assign", ADD, "let mut c = a;\n    c += b;"),
        ("mul-mixed", MUL, "let e = a * 2;"),
        ("mul-assign-mixed", MUL, "let mut c = a;\n    c *= 2;"),
        ("neg", NEG, "let e = -a;"),
        ("not", NOT, "let e = ~a;"),
        ("index", INDEX, "let e = a[0];"),
    ] {
        let (errors, effects) = check(tag, &program(impls, body));
        assert!(errors.is_empty(), "{tag}: {errors:?}");
        assert!(writes_stderr(&effects), "{tag}: {effects:?}");
        let quiet = impls.replace(LOUD, "");
        let (errors, effects) = check(&format!("{tag}-quiet"), &program(&quiet, body));
        assert!(errors.is_empty(), "{tag} quiet: {errors:?}");
        assert!(!writes_stderr(&effects), "{tag} quiet: {effects:?}");
    }
}

/// `assert_eq(a, b)` compares with `eq` (and needs `Debug` to print a
/// failure).
#[test]
fn assert_eq_is_charged_the_operands_eq() {
    let src = format!(
        "#[derive(Debug)]\nstruct Big {{ v: i64 }}\n\n{EQ}\nfn main() {{\n    let a = Big {{ v: 1 \
         }};\n    let b = Big {{ v: 1 }};\n    assert_eq(a, b);\n    assert_ne(a, Big {{ v: 2 \
         }});\n}}\n"
    );
    let (errors, effects) = check("assert-eq", &src);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(writes_stderr(&effects), "{effects:?}");
    let (errors, effects) = check("assert-eq-quiet", &src.replace(LOUD, ""));
    assert!(errors.is_empty(), "{errors:?}");
    assert!(!writes_stderr(&effects), "{effects:?}");
}

/// What the under-report let through: `#[no_effect]` on a function that
/// writes Stderr through `==`, and a public function that owes the clause.
#[test]
fn an_operators_effects_reach_no_effect_and_a_public_clause() {
    let src = format!(
        "struct Big {{ v: i64 }}\n\n{EQ}\n#[no_effect(writes(Stderr))]\nfn same(a: Big, b: Big) \
         -> bool {{\n    a == b\n}}\n\nfn main() {{\n    println(same(Big {{ v: 1 }}, Big {{ v: \
         2 }}));\n}}\n"
    );
    let (errors, _) = check("no-effect", &src);
    assert!(
        errors.len() == 1 && errors[0].contains("writes(Stderr)"),
        "{errors:?}"
    );
    let src = format!(
        "pub struct Big {{ v: i64 }}\n\n{EQ}\npub fn same(a: Big, b: Big) -> bool {{\n    a == \
         b\n}}\n\nfn main() {{\n    println(same(Big {{ v: 1 }}, Big {{ v: 2 }}));\n}}\n"
    );
    let (errors, _) = check("pub", &src);
    assert!(
        errors.len() == 1 && errors[0].contains("'same' performs effects [writes(Stderr)]"),
        "{errors:?}"
    );
}

/// `c[k]` runs `index` where it is read and `index_mut` where it is
/// written, and is charged the one it runs.
#[test]
fn indexing_is_charged_index_for_a_read_and_index_mut_for_a_write() {
    let impls = "impl Index[i64] for Big {
    type Output = i64;
    fn index(self, i: i64) -> ref i64 {
        ref self.v
    }
}

impl IndexMut[i64] for Big {
    type Output = i64;
    fn index_mut(mut ref self, i: i64) -> mut ref i64 {
        eprintln(\"loud\");
        mut ref self.v
    }
}
";
    let (errors, effects) = check("index-read", &program(impls, "let e = a[0];"));
    assert!(errors.is_empty(), "{errors:?}");
    assert!(!writes_stderr(&effects), "{effects:?}");
    for (tag, write) in [
        ("store", "c[0] = 5;"),
        ("compound", "c[0] += 5;"),
        ("mut-ref", "let r = mut ref c[0];\n    *r = 5;"),
        ("mut-arg", "bump(mut c[0]);"),
    ] {
        let src = format!(
            "fn bump(n: mut ref i64) {{ *n += 1; }}\n\n{}",
            program(impls, &format!("let mut c = a;\n    {write}"))
        );
        let (errors, effects) = check(&format!("index-{tag}"), &src);
        assert!(errors.is_empty(), "{tag}: {errors:?}");
        assert!(writes_stderr(&effects), "{tag}: {effects:?}");
    }
}

/// In a trait default body two impls inherit, each copy's `==` is charged
/// the `eq` of its own operand type, whichever impl is written last.
#[test]
fn an_operator_in_a_shared_default_body_is_charged_per_copy() {
    let walk = "trait Walk {
    type In;

    fn inner(self) -> Self.In;

    fn same(self) -> bool {
        self.inner() == self.inner()
    }
}
";
    let loud = "struct LoudIn { n: i64 }

impl PartialEq for LoudIn {
    fn eq(self, other: LoudIn) -> bool {
        eprintln(\"loud\");
        self.n == other.n
    }
}

struct A { n: i64 }

impl Walk for A {
    type In = LoudIn;
    fn inner(self) -> LoudIn { LoudIn { n: self.n } }
}
";
    let quiet = "struct QuietIn { n: i64 }

impl PartialEq for QuietIn {
    fn eq(self, other: QuietIn) -> bool {
        self.n == other.n
    }
}

struct B { n: i64 }

impl Walk for B {
    type In = QuietIn;
    fn inner(self) -> QuietIn { QuietIn { n: self.n } }
}
";
    for (order, first, second) in [("lq", loud, quiet), ("ql", quiet, loud)] {
        for (runs, receiver, expect) in [("a", "A", true), ("b", "B", false)] {
            let src = format!(
                "{walk}\n{first}\n{second}\nfn main() {{\n    println({receiver} {{ n: 1 \
                 }}.same());\n}}\n"
            );
            let tag = format!("copy-{order}-{runs}");
            let (errors, effects) = check(&tag, &src);
            assert!(errors.is_empty(), "{tag}: {errors:?}");
            assert_eq!(writes_stderr(&effects), expect, "{tag}: {effects:?}");
        }
    }
}

/// An operator on a value of a type parameter is a call through its bound:
/// a call that instantiates the parameter at `Big` is charged `Big`'s impl,
/// as the method spelling `a.eq(b)` is.
#[test]
fn an_operator_on_a_type_parameter_is_charged_the_instances_impl() {
    for (tag, bound, impls, expr) in [
        ("eq", "PartialEq", EQ, "a == b"),
        ("ne", "PartialEq", EQ, "a != b"),
        ("eq-method", "PartialEq", EQ, "a.eq(b)"),
    ] {
        let src = format!(
            "struct Big {{ v: i64 }}\n\n{impls}\nfn same[T: {bound}](a: T, b: T) -> bool {{\n    \
             {expr}\n}}\n\nfn main() {{\n    let e = same(Big {{ v: 1 }}, Big {{ v: 2 }});\n}}\n"
        );
        let (errors, effects) = check(&format!("param-{tag}"), &src);
        assert!(errors.is_empty(), "{tag}: {errors:?}");
        assert!(writes_stderr(&effects), "{tag}: {effects:?}");
        let (errors, effects) = check(&format!("param-{tag}-quiet"), &src.replace(LOUD, ""));
        assert!(errors.is_empty(), "{tag} quiet: {errors:?}");
        assert!(!writes_stderr(&effects), "{tag} quiet: {effects:?}");
    }
}
