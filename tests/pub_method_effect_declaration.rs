//! B-2026-11-19-87: a public inherent method with no `with` clause declares
//! no effects (design.md §12, public items declare their effects), and its callers
//! are charged that empty set. Its body was never verified against it, so
//! `impl Job { pub fn go(self) -> i64 { println("x"); self.n } }` and a
//! `pub fn fire(j: Job) -> i64 { j.go() }` both checked clean. The body is
//! verified now, with a machine fix that writes the clause; `karac fix`
//! then reaches the public caller too.

use std::process::{Command, Output};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn scratch(name: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-pub-method-effects-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    path
}

const JOB: &str = "pub struct Job { n: i64 }
impl Job { pub fn go(self) -> i64 { println(\"x\"); self.n } }
pub fn fire(j: Job) -> i64 { j.go() }
fn main() { println(fire(Job { n: 3 })); }
";

#[test]
fn a_public_method_with_no_clause_is_verified_against_none() {
    let path = scratch("job.kara", JOB);
    let check = karac(&["check"], &path);
    assert!(!check.status.success(), "{}", text(&check));
    assert!(
        text(&check).contains(
            ":2:16: public method 'Job.go' performs writes(Stdout) (via call to 'println' at \
             line 2) but has no effect declaration, so its callers are charged none of it; add \
             `with writes(Stdout)` to its signature or remove the call"
        ),
        "{}",
        text(&check)
    );
}

#[test]
fn karac_fix_declares_the_method_and_then_its_public_caller() {
    let path = scratch("fixed.kara", JOB);
    let fix = karac(&["fix"], &path);
    assert!(fix.status.success(), "{}", text(&fix));
    let src = std::fs::read_to_string(&path).unwrap();
    assert!(
        src.contains("pub fn go(self) -> i64 with writes(Stdout) {"),
        "{src}"
    );
    assert!(
        src.contains("pub fn fire(j: Job) -> i64 with writes(Stdout) {"),
        "{src}"
    );
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "x\n3\n",
        "{}",
        text(&run)
    );
}

#[test]
fn private_declared_and_trait_methods_are_as_before() {
    let path = scratch(
        "others.kara",
        "pub struct Job { n: i64 }
impl Job {
    fn quiet(self) -> i64 { println(\"q\"); self.n }
    pub fn loud(self) -> i64 with writes(Stdout) { println(\"l\"); self.n }
    pub fn pure(self) -> i64 { self.n * 2 }
}
trait Show { fn show(self) -> i64 with writes(Stdout); }
impl Show for Job { fn show(self) -> i64 { println(\"s\"); self.n } }
fn main() {
    let j = Job { n: 1 };
    println(j.quiet() + j.loud() + j.pure() + j.show());
}
",
    );
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "q\nl\ns\n5\n",
        "{}",
        text(&run)
    );
}
