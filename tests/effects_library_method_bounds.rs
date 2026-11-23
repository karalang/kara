//! B-2026-11-23-17: a call to a generic method is charged the instance's
//! impl of each trait its signature bounds (design.md § Effects). `karac
//! check` checks a program against the library's signatures, so a library
//! method written in Kāra had nothing to follow: a `for` loop over a
//! program iterator, and `count`, `sum`, `collect`, `extend`, `zip` or a
//! `map` chain driving it, reported none of its `next`'s effects, and
//! neither did `sum` with a program `Add` or `extend` with a program
//! `IntoIterator`.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Two iterators, one whose `next` writes Stderr and one silent, both in
/// every program: a charge must follow the one the program drives.
const ITERS: &str = "pub struct Loud { n: i64 }

impl Iterator for Loud {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] {
        eprintln(\"loud\");
        if self.n > 0 {
            self.n = self.n - 1;
            Some(self.n)
        } else {
            None
        }
    }
}

pub struct Quiet { n: i64 }

impl Iterator for Quiet {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] {
        if self.n > 0 {
            self.n = self.n - 1;
            Some(self.n)
        } else {
            None
        }
    }
}

fn total[I: Iterator[Item = i64]](it: own I) -> i64 {
    it.count()
}

fn walk[I: Iterator[Item = i64]](it: own I) -> i64 {
    let mut t = 0;
    for x in it {
        t = t + x;
    }
    t
}

pub struct Wrap[I] { inner: I }

impl[I: Iterator[Item = i64]] Iterator for Wrap[I] {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] {
        self.inner.next()
    }
}
";

/// `check --output=json` of a one-file program: its error messages and
/// its `program_effects`.
fn check(tag: &str, src: &str) -> (Vec<String>, Vec<String>) {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("karac-lib-bounds-{tag}-{}", std::process::id()));
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

fn writes_stderr(effects: &[String]) -> bool {
    effects.iter().any(|e| e == "writes(Stderr)")
}

/// `body` over `Loud` reports `writes(Stderr)`, and over `Quiet` does not.
fn loud_not_quiet(tag: &str, items: &str, body: &str) {
    for (iter, loud) in [("Loud", true), ("Quiet", false)] {
        let src = format!(
            "{ITERS}\n{}\nfn main() {{\n    {}\n}}\n",
            items.replace("ITER", iter),
            body.replace("ITER", iter)
        );
        let (errors, effects) = check(&format!("{tag}-{iter}"), &src);
        assert!(errors.is_empty(), "{tag} over {iter}: {errors:?}");
        assert_eq!(
            writes_stderr(&effects),
            loud,
            "{tag} over {iter}: {effects:?}"
        );
    }
}

#[test]
fn a_loop_or_library_method_driving_an_iterator_is_charged_its_next() {
    for (tag, body) in [
        (
            "for",
            "let mut t = 0;\n    for x in (ITER { n: 2 }) {\n        t = t + x;\n    }\n    \
             println(t);",
        ),
        ("count", "println(ITER { n: 2 }.count());"),
        (
            "map-sum",
            "let s: i64 = ITER { n: 2 }.map(|x| x + 1).sum();\n    println(s);",
        ),
        (
            "stored-chain",
            "let it = ITER { n: 2 }.map(|x| x + 1);\n    let s: i64 = it.sum();\n    println(s);",
        ),
        (
            "collect",
            "let v: Vec[i64] = ITER { n: 2 }.collect();\n    println(v.len());",
        ),
        (
            "extend",
            "let mut v: Vec[i64] = Vec.new();\n    v.extend(ITER { n: 2 });\n    println(v.len());",
        ),
        (
            "zip",
            "let v = [1, 2, 3];\n    println(v.iter().zip(ITER { n: 2 }).count());",
        ),
        ("generic-count", "println(total(ITER { n: 2 }));"),
        ("generic-for", "println(walk(ITER { n: 2 }));"),
        (
            "program-adapter",
            "let mut t = 0;\n    for x in (Wrap { inner: ITER { n: 2 } }) {\n        t = t + x;\n    \
             }\n    println(t);",
        ),
        (
            "program-adapter-count",
            "println(Wrap { inner: ITER { n: 2 } }.count());",
        ),
    ] {
        loud_not_quiet(tag, "", body);
    }
}

#[test]
fn an_opaque_iterator_is_charged_the_next_behind_it() {
    loud_not_quiet(
        "opaque",
        "fn make() -> impl Iterator[Item = i64] {\n    ITER { n: 2 }\n}\n",
        "println(make().count());",
    );
}

#[test]
fn a_library_method_is_charged_each_trait_its_signature_bounds() {
    // `rev` and `next_back`.
    loud_not_quiet(
        "rev",
        "impl DoubleEndedIterator for ITER {\n    fn next_back(mut ref self) -> Option[i64] {\n        \
         self.next()\n    }\n}\n",
        "println(ITER { n: 2 }.rev().count());",
    );
    // `sum` and the item's `Add`.
    for (ty, loud) in [("Big", true), ("Small", false)] {
        let src = format!(
            "#[derive(Clone, Copy, Default)]
pub struct Big {{ v: i64 }}
impl Add[Big] for Big {{
    type Output = Big;
    fn add(self, o: Big) -> Big {{
        eprintln(\"loud\");
        Big {{ v: self.v + o.v }}
    }}
}}
#[derive(Clone, Copy, Default)]
pub struct Small {{ v: i64 }}
impl Add[Small] for Small {{
    type Output = Small;
    fn add(self, o: Small) -> Small {{
        Small {{ v: self.v + o.v }}
    }}
}}
fn main() {{
    let xs = [{ty} {{ v: 1 }}, {ty} {{ v: 2 }}];
    let t: {ty} = xs.iter().sum();
    println(t.v);
}}
"
        );
        let (errors, effects) = check(&format!("sum-{ty}"), &src);
        assert!(errors.is_empty(), "sum of {ty}: {errors:?}");
        assert_eq!(writes_stderr(&effects), loud, "sum of {ty}: {effects:?}");
    }
    // `extend` and the argument's `IntoIterator`.
    let src = "pub struct Bag { v: Vec[i64] }
impl IntoIterator for Bag {
    type Item = i64;
    fn into_iter(own self) -> impl Iterator[Item = i64] {
        eprintln(\"loud\");
        self.v.into_iter()
    }
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.extend(Bag { v: [1, 2] });
    println(v.len());
}
";
    let (errors, effects) = check("extend-into-iter", src);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(writes_stderr(&effects), "{effects:?}");
}

/// What the under-report let through: a public function that loops over a
/// program iterator owes its `next`'s effects, and `#[no_effect]` sees them.
#[test]
fn an_iterators_effects_reach_a_public_clause_and_no_effect() {
    let src = format!(
        "{ITERS}
pub fn sum_loud() -> i64 {{
    let mut t = 0;
    for x in (Loud {{ n: 2 }}) {{
        t = t + x;
    }}
    t
}}

pub fn sum_quiet() -> i64 {{
    let mut t = 0;
    for x in (Quiet {{ n: 2 }}) {{
        t = t + x;
    }}
    t
}}

#[no_effect(writes(Stderr))]
fn count_loud() -> i64 {{
    Loud {{ n: 2 }}.count()
}}

fn main() {{
    println(sum_loud() + sum_quiet() + count_loud());
}}
"
    );
    let (errors, _) = check("clause", &src);
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(
        errors
            .iter()
            .any(|m| m.contains("public function 'sum_loud' performs effects [writes(Stderr)]")),
        "{errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|m| m.contains("count_loud") && m.contains("writes(Stderr)")),
        "{errors:?}"
    );
}
