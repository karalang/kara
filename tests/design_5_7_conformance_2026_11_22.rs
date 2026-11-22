//! The 2026-11-22 conformance pass over design.md § 5 (Types) and § 7
//! (Functions and closures), /mnt/project-files/conformance/design-5-7/:
//!
//! - a type alias whose arguments name `dyn Trait` (`type Shapes =
//!   Vec[dyn Shape]`) is registered early, before the walk, and used to be
//!   lowered before any trait was, so it was refused as "`Shape` is not a
//!   trait" (B-2026-11-22-37);
//! - a missing call-site `mut` marker names the argument to mark, as § 7's
//!   own example message does, rather than `mut <expr>` (B-2026-11-22-38).

use std::process::{Command, Output};

fn karac(tag: &str, src: &str, args: &[&str]) -> Output {
    let dir = std::env::temp_dir().join(format!("karac-d57-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args(args)
        .arg("main.kara")
        .output()
        .unwrap()
}

const DYN_ALIAS: &str = "trait Shape { fn area(self) -> f64; }
struct Sq { s: f64 }
impl Shape for Sq { fn area(self) -> f64 { self.s * self.s } }
type Shapes = Vec[dyn Shape];
type OptShape = Option[dyn Shape];
fn total(v: Shapes) -> f64 { let mut t = 0.0; for s in v { t += s.area(); } t }
fn main() {
    let v: Shapes = [Sq { s: 2.0 }, Sq { s: 1.0 }];
    println(f\"{total(v)} {OptShape.size_of()}\");
}
";

#[test]
fn d57_a_type_alias_may_name_dyn_trait_in_its_arguments() {
    let check = karac("dyn-check", DYN_ALIAS, &["check"]);
    assert!(
        check.status.success(),
        "{}{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    let run = karac("dyn-run", DYN_ALIAS, &["__mir-run"]);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "5.0 16\n",
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn d57_a_missing_mut_marker_names_the_argument_to_mark() {
    let src = "struct S { n: i64 }
fn sort_in_place(xs: mut Slice[i64]) { }
fn reset(s: mut ref S) { s.n = 0; }
fn main() {
    let mut v = [3, 1];
    sort_in_place(v);
    let mut w = S { n: 1 };
    reset(w);
}
";
    let out = karac("marker", src, &["check"]);
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{text}");
    assert!(
        text.contains("parameter expects `mut Slice[i64]`"),
        "{text}"
    );
    assert!(text.contains("Write `mut v`."), "{text}");
    assert!(text.contains("Write `mut w`."), "{text}");
    assert!(!text.contains("mut <expr>"), "{text}");
}
