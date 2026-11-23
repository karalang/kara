//! B-2026-11-23-7: desugar copies a trait's default method into every
//! impl that does not override it, and the copies keep the trait's spans.
//! The typechecker checks each copy with its own `Self` and records
//! `self.step()`'s callee twice under one span, so the span table holds the
//! last impl's. The effect pass read that table: with two impls of the
//! trait, every copy was charged the LAST impl's callees. A program that
//! wrote Stderr through the first impl reported no `writes(Stderr)`, and
//! moving the impl blocks changed the answer.
//!
//! Each program here has a type whose method writes Stderr (`Loud`) and one
//! whose method does not (`Quiet`), inheriting one default body, in both
//! orders. `main` runs the default on one of them: `writes(Stderr)` is in
//! `program_effects` exactly when that one is `Loud`.

use std::process::{Command, Stdio};

const WALK: &str = "trait Walk {\n    fn step(self) -> i64;\n\n    fn run(self) -> i64 {\n        \
                    BODY\n    }\n}\n";
const LOUD: &str =
    "struct Loud { n: i64 }\n\nimpl Walk for Loud {\n    fn step(self) -> i64 {\n        \
                    eprintln(\"loud\");\n        self.n\n    }\n}\n";
const QUIET: &str =
    "struct Quiet { n: i64 }\n\nimpl Walk for Quiet {\n    fn step(self) -> i64 { self.n }\n}\n";

/// `(name, what comes before the two types, the Loud type, the Quiet type)`.
fn cases() -> Vec<(&'static str, String, String, String)> {
    let walk = |body: &str| WALK.replace("BODY", body);
    let plain =
        |name: &'static str, body: &str| (name, walk(body), LOUD.to_string(), QUIET.to_string());
    vec![
        plain("method", "self.step() + 1"),
        plain("ufcs", "Walk.step(self) + 1"),
        plain("closure", "let f = || self.step();\n        f() + 1"),
        (
            "sibling default",
            "trait Walk {\n    fn step(self) -> i64;\n\n    fn mid(self) -> i64 {\n        \
             self.step()\n    }\n\n    fn run(self) -> i64 {\n        self.mid() + 1\n    }\n}\n"
                .to_string(),
            LOUD.to_string(),
            QUIET.to_string(),
        ),
        (
            "generic helper",
            format!(
                "{}\nfn helper[T: Walk](x: T) -> i64 {{\n    x.step()\n}}\n",
                walk("helper(self) + 1")
            ),
            LOUD.to_string(),
            QUIET.to_string(),
        ),
        (
            "associated function",
            "trait Walk {\n    fn make() -> i64;\n\n    fn run(self) -> i64 {\n        Self.make() + \
             1\n    }\n}\n"
                .to_string(),
            "struct Loud { n: i64 }\n\nimpl Walk for Loud {\n    fn make() -> i64 {\n        \
             eprintln(\"loud\");\n        1\n    }\n}\n"
                .to_string(),
            "struct Quiet { n: i64 }\n\nimpl Walk for Quiet {\n    fn make() -> i64 { 2 }\n}\n"
                .to_string(),
        ),
        (
            "associated type",
            "trait Go {\n    fn go(self) -> i64;\n}\n\nstruct LoudIn { n: i64 }\n\nimpl Go for LoudIn \
             {\n    fn go(self) -> i64 {\n        eprintln(\"loud\");\n        self.n\n    }\n}\n\n\
             struct QuietIn { n: i64 }\n\nimpl Go for QuietIn {\n    fn go(self) -> i64 { self.n \
             }\n}\n\ntrait Walk {\n    type In: Go;\n\n    fn inner(self) -> Self.In;\n\n    fn \
             run(self) -> i64 {\n        self.inner().go() + 1\n    }\n}\n"
                .to_string(),
            "struct Loud { n: i64 }\n\nimpl Walk for Loud {\n    type In = LoudIn;\n    fn \
             inner(self) -> LoudIn { LoudIn { n: self.n } }\n}\n"
                .to_string(),
            "struct Quiet { n: i64 }\n\nimpl Walk for Quiet {\n    type In = QuietIn;\n    fn \
             inner(self) -> QuietIn { QuietIn { n: self.n } }\n}\n"
                .to_string(),
        ),
        (
            "operator",
            "struct Big { v: i64 }\n\nimpl Add[Big] for Big {\n    type Output = Big;\n    fn \
             add(self, other: Big) -> Big {\n        eprintln(\"loud\");\n        Big { v: self.v + \
             other.v }\n    }\n}\n\ntrait Walk[T: Add[T, Output = T]] {\n    fn get(self) -> T;\n\n    \
             fn run(self) -> T {\n        self.get() + self.get()\n    }\n}\n"
                .to_string(),
            "struct Loud { n: i64 }\n\nimpl Walk[Big] for Loud {\n    fn get(self) -> Big { Big { v: \
             self.n } }\n}\n"
                .to_string(),
            "struct Quiet { n: i64 }\n\nimpl Walk[i64] for Quiet {\n    fn get(self) -> i64 { self.n \
             }\n}\n"
                .to_string(),
        ),
    ]
}

/// What `karac check --output=json` reports for a one-file program: its
/// `program_effects`, sorted, and its errors.
fn check(src: &str) -> (Vec<String>, Vec<String>) {
    let dir = std::env::temp_dir().join(format!(
        "karac-default-copy-callees-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
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
    let mut effects: Vec<String> = v["program_effects"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|e| e.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    effects.sort();
    let errors = v["diagnostics"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|d| d["severity"] == "error")
                .map(|d| format!("{} {}", d["code"].as_str().unwrap_or(""), d["message"]))
                .collect()
        })
        .unwrap_or_default();
    (effects, errors)
}

#[test]
fn each_impls_copy_of_a_default_body_is_charged_its_own_callees() {
    let stderr = "writes(Stderr)".to_string();
    for (name, head, loud, quiet) in cases() {
        for (order, types) in [
            ("Loud first", format!("{loud}\n{quiet}")),
            ("Quiet first", format!("{quiet}\n{loud}")),
        ] {
            for runs in ["Loud", "Quiet"] {
                let src = format!(
                    "{head}\n{types}\nfn main() {{\n    let c = {runs} {{ n: 3 }};\n    let n = \
                     c.run();\n}}\n"
                );
                let (effects, errors) = check(&src);
                assert!(errors.is_empty(), "{name}, {order}, {runs}: {errors:?}");
                assert_eq!(
                    effects.contains(&stderr),
                    runs == "Loud",
                    "{name}, {order}, main runs {runs}: {effects:?}"
                );
            }
        }
    }
}

/// A default body with a clause, two quiet impls, and a type whose `step`
/// writes Stderr.
fn ceiling(noisy_impl: &str) -> String {
    format!(
        "trait Walk {{\n    fn step(self) -> i64;\n\n    fn run(self) -> i64 with writes(Stdout) \
         {{\n        println(\"run\");\n        self.step() + 1\n    }}\n}}\n\nstruct A {{ n: i64 \
         }}\n\nimpl Walk for A {{\n    fn step(self) -> i64 {{ self.n }}\n}}\n\nstruct B {{ n: i64 \
         }}\n\nimpl Walk for B {{\n    fn step(self) -> i64 {{ self.n + 1 }}\n}}\n\nstruct Noisy {{ \
         n: i64 }}\n\n{noisy_impl} {{\n    fn step(self) -> i64 {{\n        \
         eprintln(\"noisy\");\n        self.n\n    }}\n}}\n\nfn main() {{\n    let a = A {{ n: 1 \
         }};\n    let n = a.run();\n}}\n"
    )
}

/// The trait's own default body is checked against its clause with `self`
/// standing for every implementor: a type that does not implement the trait
/// is none of them, whatever its methods are called.
#[test]
fn a_default_bodys_clause_is_checked_against_the_traits_implementors() {
    let (effects, errors) = check(&ceiling("impl Noisy"));
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(effects, ["panics", "writes(Stdout)"]);
    // Once `Noisy` implements `Walk`, its copy of `run` exceeds the clause,
    // and so does the default body. The copies in `A` and `B` do not: each
    // was reported too while every copy read the last impl's callee.
    let (_, errors) = check(&ceiling("impl Walk for Noisy"));
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(
        errors[0].starts_with("E0230") && errors[0].contains("'Noisy.run'"),
        "{errors:?}"
    );
    assert!(
        errors[1].starts_with("E0231") && errors[1].contains("'Walk.run'"),
        "{errors:?}"
    );
}
